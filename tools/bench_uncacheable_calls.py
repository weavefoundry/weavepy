"""Distinct wrapper code objects calling a variable-argument function.

Compilation, first calls, and repeated calls are timed together. Wrappers stay
alive through process resource accounting. This is an allocation diagnostic,
not a replacement for the unchanged application benchmark suite.
"""
import os
import time


def flexible(*args):
    return len(args)


def bench(n):
    lines = []
    for i in range(n):
        lines.append("def wrapper_%d(callee):\n    return callee(1)\n" % i)
    scope = {"flexible": flexible}
    exec("\n".join(lines), scope)
    wrappers = [scope["wrapper_%d" % i] for i in range(n)]
    total = 0
    for _ in range(4):
        for wrapper in wrappers:
            total += wrapper(flexible)
    assert total == n * 4
    return scope, wrappers


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "2000"))
    start = time.perf_counter_ns()
    retained = bench(n)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
