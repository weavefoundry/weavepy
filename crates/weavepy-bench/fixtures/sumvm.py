"""Pure dispatch-loop benchmark — a tight `total += i` loop that
exercises the hot path the BINARY_OP / FOR_ITER specializations
target most directly."""

import os


def bench(n):
    total = 0
    for i in range(n):
        total = total + i
    return total


if __name__ == "__main__":
    import time

    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "10000"))
    _t0 = time.perf_counter_ns()
    bench(n)
    _t1 = time.perf_counter_ns()
    print("WEAVEPY_BENCH_NS=%d" % (_t1 - _t0))
