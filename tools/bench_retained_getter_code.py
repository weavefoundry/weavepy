"""Retain distinct field/property reader functions after their first calls.

Compilation, object construction, and four calls per reader are timed together.
This diagnoses cold code-cache allocation; it doesn't replace application tests.
"""
import os
import time


class Record:
    def __init__(self):
        self.value = 11


class Property:
    @property
    def value(self):
        return 13


def bench(n):
    source = "\n".join(
        "def read_%d(obj):\n    return obj.value\n" % i for i in range(n)
    )
    scope = {}
    exec(source, scope)
    readers = [scope["read_%d" % i] for i in range(n)]
    receivers = (Record(), Property())
    total = 0
    for _ in range(4):
        for i, read in enumerate(readers):
            total += read(receivers[i & 1])
    assert total == 4 * (11 * ((n + 1) // 2) + 13 * (n // 2))
    return scope, readers


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "2000"))
    start = time.perf_counter_ns()
    retained = bench(n)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
