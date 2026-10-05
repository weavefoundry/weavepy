"""Run one benchmark module in this interpreter and print its timings.

Usage: harness.py NAME WORK REPS

Imports ``benchmarks/NAME.py``, calls ``bench(WORK)`` once untimed-for-
steady-state (its time is reported as ``cold``), then ``REPS`` more times,
and prints one JSON line with the cold time, every warm sample, the time
spent importing the module, and the process's peak resident memory.
"""

import importlib
import json
import os
import sys
import time


def peak_rss_bytes():
    try:
        import resource
    except ImportError:
        return 0
    rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    # Linux reports KiB, macOS bytes.
    return rss if sys.platform == "darwin" else rss * 1024


def main():
    name, work, reps = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
    sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "benchmarks"))
    t0 = time.perf_counter_ns()
    mod = importlib.import_module(name)
    t1 = time.perf_counter_ns()
    expected = mod.bench(work)
    t2 = time.perf_counter_ns()
    warm = []
    for _ in range(reps):
        s = time.perf_counter_ns()
        got = mod.bench(work)
        warm.append(time.perf_counter_ns() - s)
        if got != expected:
            raise SystemExit("%s: nondeterministic result %r != %r" % (name, got, expected))
    print(json.dumps({
        "name": name,
        "import_ns": t1 - t0,
        "cold_ns": t2 - t1,
        "warm_ns": warm,
        "rss": peak_rss_bytes(),
        "result": repr(expected)[:200],
    }))


if __name__ == "__main__":
    main()
