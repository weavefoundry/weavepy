"""Repeated native math calls across a Python function boundary."""

import math
import os
import time


def wave(x):
    return math.sin(x) + math.cos(x)


def bench(n):
    total = 0.0
    for i in range(n):
        total += wave(i)
    assert -4.0 < total < 4.0
    return total


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "500000"))
    start = time.perf_counter_ns()
    bench(n)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
