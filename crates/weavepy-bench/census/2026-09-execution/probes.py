"""Paired startup, compilation, and allocation probes; run from the repo root."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import statistics
import subprocess
import sys
import tempfile
import time


KERNELS = {
    "compile_small": '''
source = """class Point:
    def __init__(self, x, y):
        self.x, self.y = x, y
    def length2(self):
        return self.x * self.x + self.y * self.y

def process(n):
    return [Point(i, i + 1).length2() for i in range(n) if i % 2]
"""
def bench():
    for _ in range(1500):
        compile(source, "<compile-bench>", "exec")
''',
    "compile_6000": '''
source = "\\n".join("name_%d = %d" % (i, i) for i in range(6000))
def bench():
    for _ in range(3):
        compile(source, "<compile-bench>", "exec")
''',
    "compile_20000": '''
source = "\\n".join("name_%d = %d" % (i, i) for i in range(20000))
def bench():
    compile(source, "<compile-bench>", "exec")
''',
    "join_large": '''
items = ["abcdefgh" * 8] * 100000
def bench():
    for _ in range(10):
        result = "|".join(items)
    assert len(result) == 6499999
''',
    "split_bounded": '''
text = "word \\u2003" * 200000
def bench():
    for _ in range(10):
        result = text.split(None, 1)
    assert result[0] == "word" and len(result) == 2
''',
    "case_ascii": '''
text = "The QUICK brown Fox 123; " * 40000
def bench():
    for _ in range(10):
        result = text.lower().upper().title()
    assert result.startswith("The Quick Brown Fox")
''',
    "case_unicode": '''
text = "Straße ΟΣ ΟΣΑ İ ǳ \\u0301; " * 10000
def bench():
    for _ in range(3):
        result = text.lower().upper().title()
    assert result.startswith("Strasse")
''',
}

TIMER = '''
import time
start = time.perf_counter_ns()
cpu = time.process_time_ns()
bench()
cpu = time.process_time_ns() - cpu
elapsed = time.perf_counter_ns() - start
print("TIMING", elapsed, cpu)
'''


def measure(binary, flags, source, timed, cache=None):
    env = {**os.environ, "WEAVEPY_JIT": "0"}
    if cache is not None:
        env["WEAVEPY_STDLIB_CACHE"] = cache
    with tempfile.TemporaryFile() as output:
        start = time.perf_counter_ns()
        child = subprocess.Popen(
            [binary, *flags, "-c", source],
            env=env,
            stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.STDOUT,
        )
        _, status, usage = os.wait4(child.pid, 0)
        wall = time.perf_counter_ns() - start
        child.returncode = os.waitstatus_to_exitcode(status)
        output.seek(0)
        text = output.read().decode(errors="replace")
    if child.returncode:
        raise RuntimeError(f"{binary}: {text}")
    result = {
        "wall_ns": wall,
        "cpu_ns": round((usage.ru_utime + usage.ru_stime) * 1e9),
        "rss_bytes": usage.ru_maxrss * (1 if sys.platform == "darwin" else 1024),
    }
    if timed:
        match = re.search(r"TIMING (\d+) (\d+)", text)
        if match is None:
            raise RuntimeError(f"Missing timing: {text}")
        result.update(ns=int(match[1]), work_cpu_ns=int(match[2]))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True)
    parser.add_argument("--new", required=True)
    parser.add_argument("--python", default="python3.14")
    parser.add_argument("--samples", type=int, default=7)
    parser.add_argument("--only", nargs="*")
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if args.samples < 1:
        parser.error("--samples must be positive")
    variants = [("base", args.base), ("new", args.new), ("cpython", args.python)]
    report = {
        "platform": platform.platform(),
        "jit": False,
        "binaries": {
            label: {"path": binary, "sha256": hashlib.sha256(Path(binary).read_bytes()).hexdigest(),
                    "bytes": Path(binary).stat().st_size}
            for label, binary in variants[:2]
        },
        "rows": {},
    }
    probes = {
        "startup": ([], "pass", False),
        "startup_no_site": (["-S"], "pass", False),
        "startup_fresh_cache": ([], "pass", False),
        "imports": ([], "import json, datetime, collections, pathlib", False),
        **{name: ([], source + TIMER, True) for name, source in KERNELS.items()},
    }
    if args.only:
        probes = {name: probes[name] for name in args.only}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    for name, (flags, source, timed) in probes.items():
        fresh = name == "startup_fresh_cache"
        count = args.samples if timed or fresh else max(args.samples, 31)
        # Extraction of WeavePy's embedded library has no CPython equivalent.
        active = variants[:2] if fresh else variants
        samples = {label: [] for label, _ in active}
        for cycle in range(count + 1):
            for label, binary in active if cycle % 2 == 0 else reversed(active):
                if fresh:
                    with tempfile.TemporaryDirectory(prefix="weavepy-perf-cache-") as cache:
                        value = measure(binary, flags, source, timed, cache)
                else:
                    value = measure(binary, flags, source, timed)
                if cycle:
                    samples[label].append(value)
        metrics = samples["new"][0]
        ratios = {
            key: statistics.median(new[key] / base[key] for new, base in
                                   zip(samples["new"], samples["base"], strict=True))
            for key in metrics
        }
        report["rows"][name] = {"samples": samples, "comparisons": ratios}
        args.out.write_text(json.dumps(report, indent=2) + "\n")
        print(name, {key: round(value, 3) for key, value in ratios.items()}, flush=True)


if __name__ == "__main__":
    main()
