"""Distinct scalar range loops, called once or repeatedly.

The timer includes source generation, compilation, all calls, and result checks.
Work controls iterations per call; WEAVEPY_RANGE_CALLS controls repeat calls.
The unchanged worker-churn and application fixtures remain separate checks.
"""
import os
import time


def bench(n, calls):
    source = "\n".join(
        "def kernel%d(n):\n    total = %d\n    for i in range(n):\n        total += i\n    return total\n" % (i, i)
        for i in range(32)
    )
    namespace = {}
    exec(source, namespace)
    results = []
    expected = n * (n - 1) // 2
    for i in range(32):
        kernel = namespace["kernel%d" % i]
        for _ in range(calls):
            value = kernel(n)
            assert value == expected + i
            results.append(value)
    return namespace, results


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "2000"))
    calls = int(os.environ.get("WEAVEPY_RANGE_CALLS", "1"))
    start = time.perf_counter_ns()
    retained = bench(n, calls)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
