#!/usr/bin/env python3
"""Compare WeavePy releases in both execution modes on the same host.

Use the existing benchmark fixtures and work sizes. Alternate measurement order,
discard one warmup cycle, and retain every timing, CPU-time, and peak-RSS sample
in JSON. Run with an otherwise idle machine; compilation and other tests should
finish first. CPU time and RSS include startup; fixture timers exclude it except
for the dedicated startup fixture.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = ROOT / "crates/weavepy-bench/fixtures"


def execution_context() -> dict:
    """Record launch conditions when measuring, independently of the build.

    A controller can't reliably detect its enclosing filesystem sandbox. The
    caller declares that condition; an absent declaration remains unspecified.
    """
    return {
        "declared_launch_context": os.environ.get(
            "WEAVEPY_BENCH_LAUNCH_CONTEXT", "unspecified"
        ),
        "environment": {
            key: os.environ.get(key)
            for key in ("TZ", "LANG", "LC_ALL", "LC_TIME", "PYTHON_JIT", "PYTHON_GIL")
        },
    }


def verify_runtime(binary: str) -> None:
    """Refuse WeavePy's reduced fallback runtime in standard-library comparisons."""
    source = '''
import sys, os
if sys.implementation.name == "weavepy":
    assert sys._stdlib_dir and os.path.isdir(sys._stdlib_dir), (
        "Standard library was not staged; set WEAVEPY_STDLIB_CACHE to a writable directory")
    assert getattr(os, "__file__", None), "os must come from the staged standard library"
'''
    subprocess.run([binary, "-c", source], check=True, capture_output=True,
                   text=True, timeout=60)


def paired_ratio(after: list[dict], before: list[dict], metric: str) -> float:
    """Keep interleaved samples paired when summarizing relative performance.

    A host-speed change can put the two marginal medians in different cycles.
    Their quotient loses the drift protection that pairing provides.
    """
    return statistics.median(
        new[metric] / old[metric]
        for new, old in zip(after, before, strict=True)
    )


def relative_metrics(after: list[dict], before: list[dict]) -> dict:
    """Report every measured metric, including CPU time and process memory."""
    if not after or not before:
        raise ValueError("Relative metrics require measured samples")
    if any(sample.keys() != after[0].keys() for sample in after + before):
        raise ValueError("Relative metrics require identical measurement fields")
    return {metric: paired_ratio(after, before, metric) for metric in after[0]}


def measure(binary: str, jit: str | None, name: str, work: int, warm: bool,
            *, fixture_root: Path | None = None,
            frozen_cache: Path | None = None) -> dict:
    env = os.environ.copy()
    env["WEAVEPY_BENCH_WORK"] = str(work)
    if jit is None:
        env.pop("WEAVEPY_JIT", None)
    else:
        env["WEAVEPY_JIT"] = jit
    if frozen_cache is not None:
        env["WEAVEPY_FROZEN_CACHE"] = str(frozen_cache.resolve())

    fixture_root = FIXTURES if fixture_root is None else fixture_root
    fixture = str(fixture_root / f"{name}.py")
    command = [binary, fixture]
    if warm and name != "startup":
        # Register the module so functions and classes remain pickleable.
        # The original __main__ timer is skipped; both runs use bench(n).
        driver = f'''
import sys, time, types
sys.path[0] = {str(fixture_root)!r}
sys.argv[0] = {fixture!r}
module = types.ModuleType("_weavepy_bench")
module.__file__ = {fixture!r}
sys.modules[module.__name__] = module
with open(module.__file__) as source:
    code = compile(source.read(), module.__file__, "exec")
exec(code, module.__dict__)
module.bench({work})
start = time.perf_counter_ns()
cpu_start = time.process_time_ns()
module.bench({work})
cpu_elapsed = time.process_time_ns() - cpu_start
elapsed = time.perf_counter_ns() - start
print("WEAVEPY_BENCH_NS=%d" % elapsed)
print("WEAVEPY_BENCH_CPU_NS=%d" % cpu_elapsed)
'''
        command = [binary, "-c", driver]

    # A file avoids pipe deadlocks without a reader thread. Reap with wait4
    # instead of Popen.wait(), which discards the child's resource usage.
    with tempfile.TemporaryFile() as output:
        start = time.perf_counter_ns()
        child = subprocess.Popen(
            command,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=output,
            stderr=subprocess.STDOUT,
        )
        _, status, usage = os.wait4(child.pid, 0)
        wall = time.perf_counter_ns() - start
        child.returncode = os.waitstatus_to_exitcode(status)
        output.seek(0)
        text = output.read().decode(errors="replace")
    if child.returncode:
        raise RuntimeError(f"{binary} {name}: {text}")
    timed = re.findall(r"WEAVEPY_BENCH_NS=(\d+)", text)
    if not timed and name != "startup":
        raise RuntimeError(f"Missing benchmark timer for {name}: {text}")
    result = {
        "ns": wall if name == "startup" else int(timed[-1]),
        "wall_ns": wall,
        "cpu_ns": round((usage.ru_utime + usage.ru_stime) * 1e9),
        "rss_bytes": usage.ru_maxrss * (1 if sys.platform == "darwin" else 1024),
    }
    if warm and name != "startup":
        cpu_timed = re.findall(r"WEAVEPY_BENCH_CPU_NS=(\d+)", text)
        if not cpu_timed:
            raise RuntimeError(f"Missing workload CPU timer for {name}: {text}")
        result["work_cpu_ns"] = int(cpu_timed[-1])
    return result


def frozen_cache_snapshot(root: Path) -> dict[str, str]:
    """Identify frozen artifacts without changing them between timed runs."""
    return {str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in sorted(root.rglob("*")) if path.is_file()}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True, help="Unmodified release binary")
    parser.add_argument("--new", required=True, help="Modified release binary")
    parser.add_argument("--previous", help="Optional preceding release for paired incremental comparisons")
    parser.add_argument("--python", default="python3.14")
    parser.add_argument("--samples", type=int, default=5)
    parser.add_argument("--fixtures", nargs="*", help="Optional fixture names")
    parser.add_argument("--probe", type=Path,
                        help="Standalone Python fixture exposing bench(n); requires --work")
    parser.add_argument("--work", type=int, help="Override work for a single fixture")
    parser.add_argument(
        "--warm", action="store_true",
        help="Run bench(n) once before timing it; also record workload CPU time",
    )
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument(
        "--frozen-cache-root", type=Path,
        help="Fresh directory for separate per-binary frozen caches; verify warm artifacts stay unchanged",
    )
    args = parser.parse_args()
    verify_runtime(args.base)
    verify_runtime(args.new)
    if args.previous:
        verify_runtime(args.previous)
    if args.samples < 1:
        parser.error("--samples must be positive")
    if not hasattr(os, "wait4"):
        parser.error("CPU and peak-RSS measurements require a Unix host with wait4")

    # Read the harness's authoritative work values instead of maintaining a
    # second, silently diverging list. Refuse unknown fixture names.
    source = (ROOT / "crates/weavepy-bench/src/fixtures.rs").read_text()
    work = {
        name: int(n.replace("_", ""))
        for name, n in re.findall(r'"(\w+)" => ([\d_]+),', source)
    }
    if args.fixtures:
        unknown = set(args.fixtures) - work.keys()
        if unknown:
            parser.error(f"Unknown fixtures: {', '.join(sorted(unknown))}")
        work = {name: work[name] for name in args.fixtures}
    fixture_root = None
    if args.probe is not None:
        if args.fixtures or args.work is None:
            parser.error("--probe requires --work and cannot be combined with --fixtures")
        args.probe = args.probe.resolve()
        if args.probe.suffix != ".py" or not args.probe.is_file():
            parser.error("--probe must name an existing Python file")
        fixture_root = args.probe.parent
        work = {args.probe.stem: args.work}
    if args.work is not None:
        if len(work) != 1 or args.work < 1:
            parser.error("--work requires one fixture and a positive work value")
        work = {name: args.work for name in work}

    variants = [
        ("base", args.base, "1"),
        ("new", args.new, "1"),
        ("base_interp", args.base, "0"),
        ("new_interp", args.new, "0"),
        ("cpython", args.python, None),
    ]
    if args.previous:
        variants[2:2] = [("previous", args.previous, "1")]
        variants[-1:-1] = [("previous_interp", args.previous, "0")]
    binaries = [("base", args.base), ("new", args.new)]
    if args.previous:
        binaries.append(("previous", args.previous))
    report = {
        "execution_context": execution_context(),
        "samples": args.samples,
        "warm": args.warm,
        "platform": f"{sys.platform}-{platform.machine()}",
        "binaries": {
            label: {"path": path, "bytes": Path(path).stat().st_size,
                    "sha256": hashlib.sha256(Path(path).read_bytes()).hexdigest()}
            for label, path in binaries
        },
        "rows": {},
    }
    if args.probe is not None:
        report["probe"] = {
            "path": str(args.probe),
            "sha256": hashlib.sha256(args.probe.read_bytes()).hexdigest(),
        }
    caches = {}
    if args.frozen_cache_root is not None:
        args.frozen_cache_root.mkdir(parents=True, exist_ok=False)
        identities = {info["path"]: info["sha256"] for info in report["binaries"].values()}
        caches = {label: args.frozen_cache_root / identities[binary]
                  for label, binary, jit in variants if jit is not None}
        report["frozen_caches"] = {label: str(path.resolve()) for label, path in caches.items()}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    for name, n in work.items():
        samples = {label: [] for label, _, _ in variants}
        frozen_before = None
        for run in range(args.samples + 1):
            order = variants if run % 2 == 0 else list(reversed(variants))
            for label, binary, jit in order:
                sample = measure(binary, jit, name, n, args.warm,
                                 fixture_root=fixture_root,
                                 frozen_cache=caches.get(label))
                if run:
                    samples[label].append(sample)
            if run == 0 and args.frozen_cache_root is not None:
                frozen_before = frozen_cache_snapshot(args.frozen_cache_root)
        row = {
            label: {
                "samples": values,
                **{
                    key: statistics.median(value[key] for value in values)
                    for key in values[0]
                },
            }
            for label, values in samples.items()
        }
        comparisons = {
            mode: relative_metrics(samples[new], samples[base])
            for mode, new, base in [
                ("jit", "new", "base"),
                ("interp", "new_interp", "base_interp"),
            ]
        }
        cpython_comparisons = {
            mode: relative_metrics(samples[label], samples["cpython"])
            for mode, label in [("jit", "new"), ("interp", "new_interp")]
        }
        report["rows"][name] = {
            "work": n, **row, "comparisons": comparisons,
            "cpython_comparisons": cpython_comparisons,
        }
        if args.frozen_cache_root is not None:
            frozen_after = frozen_cache_snapshot(args.frozen_cache_root)
            report["rows"][name]["frozen_cache"] = {
                "before": frozen_before, "after": frozen_after,
                "unchanged": frozen_before == frozen_after,
            }
            if frozen_before != frozen_after:
                args.out.write_text(json.dumps(report, indent=2) + "\n")
                raise RuntimeError(f"Frozen cache changed during measured cycles for {name}")
        if args.previous:
            report["rows"][name]["previous_comparisons"] = {
                mode: relative_metrics(samples[new], samples[previous])
                for mode, new, previous in [
                    ("jit", "new", "previous"),
                    ("interp", "new_interp", "previous_interp"),
                ]
            }
        # Preserve completed fixtures if a subsequent workload fails.
        args.out.write_text(json.dumps(report, indent=2) + "\n")
        previous_text = ""
        if args.previous:
            paired = report["rows"][name]["previous_comparisons"]
            previous_text = (f"  previous JIT {paired['jit']['ns']:.3f}x / "
                             f"interp {paired['interp']['ns']:.3f}x")
        print(
            f"{name:16s} "
            f"JIT {comparisons['jit']['ns']:.3f}x  "
            f"interp {comparisons['interp']['ns']:.3f}x  "
            f"RSS {comparisons['jit']['rss_bytes']:.3f}x  "
            f"CPython {cpython_comparisons['jit']['ns']:.3f}x time / "
            f"{cpython_comparisons['jit']['rss_bytes']:.3f}x RSS" + previous_text,
            flush=True,
        )


if __name__ == "__main__":
    main()
