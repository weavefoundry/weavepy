"""Compare checked eight-thread work with an explicit timed start gate."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import runpy
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[4]
helpers = runpy.run_path(str(ROOT / "tools/bench_compare.py"))
SOURCE = '''
import threading, time
THREADS = 8
N = WORK
def kernel(n):
    total = 0
    for i in range(n):
        total = (total + i * i) % 1000000007
    return total
expected = (N * (N - 1) * (2 * N - 1) // 6) % 1000000007
assert kernel(N) == expected
t = threading.Thread(target=kernel, args=(1,))
t.start()
t.join()
start = time.perf_counter_ns()
cpu = time.process_time_ns()
serial = [kernel(N) for _ in range(THREADS)]
serial_cpu = time.process_time_ns() - cpu
serial_ns = time.perf_counter_ns() - start
assert serial == [expected] * THREADS
ready = threading.Barrier(THREADS + 1)
go = threading.Event()
results = []
errors = []
def worker():
    try:
        ready.wait(timeout=120)
        assert go.wait(timeout=120)
        results.append(kernel(N))
    except BaseException as error:
        errors.append(repr(error))
threads = [threading.Thread(target=worker) for _ in range(THREADS)]
for thread in threads:
    thread.start()
ready.wait(timeout=120)
start = time.perf_counter_ns()
cpu = time.process_time_ns()
go.set()
for thread in threads:
    thread.join(timeout=120)
parallel_cpu = time.process_time_ns() - cpu
parallel_ns = time.perf_counter_ns() - start
assert not errors, errors
assert all(not thread.is_alive() for thread in threads)
assert results == [expected] * THREADS
for name, value in (("serial_ns", serial_ns), ("parallel_ns", parallel_ns),
                    ("serial_cpu_ns", serial_cpu), ("parallel_cpu_ns", parallel_cpu)):
    print("MEASURE", name, value)
'''


def measure(binary, gil, work):
    env = {**os.environ, "WEAVEPY_JIT": "1"}
    with tempfile.TemporaryFile() as output:
        start = time.perf_counter_ns()
        child = subprocess.Popen([binary, "-X", f"gil={gil}", "-c",
                                  SOURCE.replace("N = WORK", f"N = {work}")],
                                 env=env, stdin=subprocess.DEVNULL,
                                 stdout=output, stderr=subprocess.STDOUT)
        _, status, usage = os.wait4(child.pid, 0)
        wall = time.perf_counter_ns() - start
        child.returncode = os.waitstatus_to_exitcode(status)
        output.seek(0)
        text = output.read().decode(errors="replace")
    if child.returncode:
        raise RuntimeError(f"{binary} gil={gil}: {text}")
    values = {name: int(value) for name, value in
              re.findall(r"MEASURE (\w+) (\d+)", text)}
    if set(values) != {"serial_ns", "parallel_ns", "serial_cpu_ns", "parallel_cpu_ns"}:
        raise RuntimeError(f"Missing measurements: {text}")
    return {**values, "wall_ns": wall,
            "cpu_ns": round((usage.ru_utime + usage.ru_stime) * 1e9),
            "rss_bytes": usage.ru_maxrss * (1 if sys.platform == "darwin" else 1024)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True)
    parser.add_argument("--new", required=True)
    parser.add_argument("--python", default="python3.14")
    parser.add_argument("--samples", type=int, default=5)
    parser.add_argument("--work", type=int, default=1000000)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if args.samples < 1 or args.work < 1:
        parser.error("samples and work must be positive")
    for binary in (args.base, args.new):
        helpers["verify_runtime"](binary)
    report = {"work_per_thread": args.work, "threads": 8,
              "samples": args.samples, "platform": platform.platform(),
              "cpython": args.python, "cpython_gil": True,
              "jit_requested": True, "binaries": {
                  label: {"path": binary, "sha256": hashlib.sha256(Path(binary).read_bytes()).hexdigest()}
                  for label, binary in (("base", args.base), ("new", args.new))}, "modes": {}}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    for gil in (1, 0):
        variants = [("base", args.base, gil), ("new", args.new, gil),
                    ("cpython", args.python, 1)]
        samples = {label: [] for label, _, _ in variants}
        for cycle in range(args.samples + 1):
            for label, binary, mode in variants if cycle % 2 == 0 else reversed(variants):
                values = measure(binary, mode, args.work)
                if cycle:
                    samples[label].append(values)
            print("GIL", gil, "completed cycle", cycle, flush=True)
        comparisons = {label: helpers["relative_metrics"](samples["new"], samples[label])
                       for label in ("base", "cpython")}
        report["modes"][str(gil)] = {"samples": samples, "comparisons": comparisons}
        args.out.write_text(json.dumps(report, indent=2) + "\n")
        print("GIL", gil, comparisons, flush=True)


if __name__ == "__main__":
    main()
