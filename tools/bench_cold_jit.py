#!/usr/bin/env python3
"""Diagnose cold JIT costs separately from benchmark admission.

Fresh-process and warmed-function samples use unchanged fixtures. Compilation
traces are separate instrumented runs. This tool never changes gate results.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import statistics
import subprocess
import time


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def snapshot(root):
    return {str(p.relative_to(root)): digest(p)
            for p in sorted(root.rglob("*")) if p.is_file()}


def binary_record(binary):
    path = Path(shutil.which(binary) or binary).resolve(strict=True)
    files = [path]
    runtime = path.with_name("python314.dll")
    if runtime.is_file():
        files.append(runtime)
    version = subprocess.check_output([str(path), "--version"], text=True, timeout=30).strip()
    return {"path": str(path), "version": version,
            "files": {str(p): digest(p) for p in files}}


def driver(path, work, trace=False):
    # Avoid runpy/types imports that could themselves warm the compiler.
    return f'''
import sys, time
scope = {{"__name__": "_weave_cold_jit", "__file__": {str(path)!r}}}
with open({str(path)!r}) as source:
    code = compile(source.read(), {str(path)!r}, "exec")
exec(code, scope)
if {trace!r}: print("JIT_DIAGNOSTIC_FIRST_BEGIN", file=sys.stderr, flush=True)
scope["bench"]({work})
if {trace!r}: print("JIT_DIAGNOSTIC_FIRST_END", file=sys.stderr, flush=True)
start = time.perf_counter_ns()
if {trace!r}: print("JIT_DIAGNOSTIC_SECOND_BEGIN", file=sys.stderr, flush=True)
scope["bench"]({work})
if {trace!r}: print("JIT_DIAGNOSTIC_SECOND_END", file=sys.stderr, flush=True)
print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
'''


def run(binary, path, work, cache, warm, trace=False):
    env = os.environ.copy()
    for name in ("WEAVEPY_JIT", "WEAVEPY_JIT_TRACE", "WEAVEPY_VM_STATS"):
        env.pop(name, None)
    env["WEAVEPY_BENCH_WORK"] = str(work)
    if cache is not None:
        env["WEAVEPY_JIT"] = "1"
        env["WEAVEPY_FROZEN_CACHE"] = str(cache)
        env.setdefault("WEAVEPY_STDLIB_CACHE", str(cache.parent.parent / "stdlib" / cache.name))
    if trace:
        env["WEAVEPY_JIT_TRACE"] = "1"
    command = [binary, "-c", driver(path, work, trace)] if warm else [binary, str(path)]
    start = time.perf_counter_ns()
    completed = subprocess.run(command, env=env, stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                               timeout=180, check=False)
    if completed.returncode:
        raise RuntimeError(f"{binary} exited {completed.returncode}: {completed.stderr}\n{completed.stdout}")
    wall = time.perf_counter_ns() - start
    values = re.findall(r"^WEAVEPY_BENCH_NS=(\d+)$", completed.stdout, re.MULTILINE)
    if not values:
        raise RuntimeError("missing workload timer: " + completed.stdout)
    return {"ns": int(values[-1]), "wall_ns": wall}, completed.stdout, completed.stderr


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True)
    parser.add_argument("--new", required=True)
    parser.add_argument("--python", default="python")
    parser.add_argument("--fixture-root", type=Path, default=Path("crates/weavepy-bench/fixtures"))
    parser.add_argument("--samples", type=int, default=7)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if args.samples < 1:
        parser.error("samples must be positive")
    args.out = args.out.resolve()
    args.out.parent.mkdir(parents=True, exist_ok=True)
    cache_root = args.out.parent / "frozen"
    cache_root.mkdir(exist_ok=False)
    raw = args.out.parent / "traces"
    raw.mkdir(exist_ok=False)
    binaries = {name: binary_record(path) for name, path in
                [("base", args.base), ("new", args.new), ("cpython", args.python)]}
    variants = [(name, record["path"], cache_root / name if name != "cpython" else None)
                for name, record in binaries.items()]
    report = {"diagnostic_only": True, "platform": platform.platform(),
              "samples": args.samples, "binaries": binaries, "rows": {},
              "environment": {name: os.environ.get(name) for name in
                              ("PYTHONHASHSEED", "PYTHONDONTWRITEBYTECODE", "PYTHON_JIT", "PYTHON_GIL", "WEAVEPY_STDLIB_CACHE")}}
    args.out.write_text(json.dumps(report, indent=2) + "\n")
    for name, work in [("sumvm", 2000000), ("nested_loops", 120), ("jitloop", 1000)]:
        path = (args.fixture_root / (name + ".py")).resolve(strict=True)
        row = {"work": work, "fixture": str(path), "sha256": digest(path), "modes": {}}
        for warm in (False, True):
            mode = "warm" if warm else "cold"
            samples = {label: [] for label, _, _ in variants}
            for cycle in range(args.samples + 1):
                ordered = variants if cycle % 2 == 0 else list(reversed(variants))
                for label, binary, cache in ordered:
                    result, _, _ = run(binary, path, work, cache, warm)
                    if cycle:
                        samples[label].append(result)
                if cycle == 0:
                    before = snapshot(cache_root)
                    for label in ("base", "new"):
                        if not any(Path(p).parts[0] == label for p in before):
                            raise RuntimeError("empty warmed frozen cache for " + label)
            after = snapshot(cache_root)
            if after != before:
                raise RuntimeError("frozen cache changed during " + name + " " + mode)
            row["modes"][mode] = {"samples": samples, "frozen_cache": before,
                "new_over_base": {metric: statistics.median(v[metric] / u[metric]
                for u, v in zip(samples["base"], samples["new"])) for metric in ("ns", "wall_ns")}}
        report["rows"][name] = row
        args.out.write_text(json.dumps(report, indent=2) + "\n")
        for label, binary, cache in variants[:2]:
            _, stdout, stderr = run(binary, path, work, cache, True, True)
            (raw / (name + "-" + label + ".log")).write_text(stderr + "\nSTDOUT\n" + stdout)
        report["rows"][name] = row
        args.out.write_text(json.dumps(report, indent=2) + "\n")
        print(name, {mode: values["new_over_base"] for mode, values in row["modes"].items()}, flush=True)
    for record in binaries.values():
        for path, expected in record["files"].items():
            if digest(Path(path)) != expected:
                raise RuntimeError("binary changed: " + path)


if __name__ == "__main__":
    main()
