"""Compile and execute a generated sequence of simple statements.

Source construction, compilation, and execution are all timed. The checked
result prevents a faster parser from hiding missing or reordered statements.
"""
import os
import time


def bench(n):
    source = "total = 0\n" + "total += 1\n" * n
    code = compile(source, "<compile-statements>", "exec")
    namespace = {}
    exec(code, namespace)
    assert namespace["total"] == n
    return code, namespace


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "10000"))
    start = time.perf_counter_ns()
    retained = bench(n)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
