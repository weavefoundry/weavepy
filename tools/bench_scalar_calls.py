#!/usr/bin/env python3
"""Measure dynamic scalar calls whose integer results feed an accumulator.

WEAVEPY_SCALAR_CALL_KIND selects one, two, constant, global, default, or branch.
WEAVEPY_BENCH_WORK controls iterations; setup and result validation stay timed.
"""

import os
import time

KIND = os.environ.get("WEAVEPY_SCALAR_CALL_KIND", "one")
if KIND not in ("one", "two", "constant", "global", "default", "branch"):
    raise ValueError("unknown WEAVEPY_SCALAR_CALL_KIND: " + KIND)

OFFSET = 1


def add_one(value):
    return value + 1


def add_two(value, amount):
    return value + amount


def constant(value):
    return 1


def add_global(value):
    return value + OFFSET


def add_default(value, amount=1):
    return value + amount


def add_branch(value):
    if value < 0:
        return value - 1
    return value + 1


def sum_one(callback, n):
    total = 0
    for i in range(n):
        total += callback(i)
    return total


def sum_two(callback, n):
    total = 0
    for i in range(n):
        total += callback(i, 1)
    return total


def bench(n):
    if KIND == "one":
        result = sum_one(add_one, n)
    elif KIND == "two":
        result = sum_two(add_two, n)
    elif KIND == "constant":
        result = sum_one(constant, n)
    elif KIND == "global":
        result = sum_one(add_global, n)
    elif KIND == "default":
        result = sum_one(add_default, n)
    else:
        result = sum_one(add_branch, n)
    expected = n if KIND == "constant" else n * (n + 1) // 2
    assert result == expected, result
    return result


if __name__ == "__main__":
    work = int(os.environ.get("WEAVEPY_BENCH_WORK", "200000"))
    start = time.perf_counter_ns()
    bench(work)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
