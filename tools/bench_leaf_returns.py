#!/usr/bin/env python3
"""Time simple Python return bodies; use WEAVEPY_BENCH_WORK to scale the loop.

Run with each interpreter being compared, both with the default JIT setting
and WEAVEPY_JIT=0. The timer excludes imports and checks the result afterward.
"""

import os
import time


class Record:
    def __init__(self, value):
        self.value = value

    def get(self):
        return self.value

    def truth(self):
        return True


def identity(value):
    return value


def bench(n):
    obj = Record(3)
    total = 0
    for _ in range(n):
        total += obj.get()
        total += identity(2)
        total += obj.truth()
    return total


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "500000"))
    start = time.perf_counter_ns()
    result = bench(n)
    elapsed = time.perf_counter_ns() - start
    assert result == n * 6
    print("WEAVEPY_BENCH_NS=%d" % elapsed)
