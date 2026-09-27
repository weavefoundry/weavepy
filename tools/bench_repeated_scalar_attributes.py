"""Adjacent scalar reads on ordinary dictionaries or slots.

WEAVEPY_REPEATED_SCALAR_KIND selects dict_int, dict_float, slots_int,
or slots_float. Instance construction and all loop work are timed.
"""
import os
import time

KIND = os.environ.get("WEAVEPY_REPEATED_SCALAR_KIND", "dict_float")


class DictPoint:
    def __init__(self, value):
        self.value = value


class SlotPoint:
    __slots__ = ("value",)

    def __init__(self, value):
        self.value = value


def squared(point, n):
    total = 0
    for _ in range(n):
        total += point.value * point.value
    return total


def bench(n):
    if KIND not in ("dict_int", "dict_float", "slots_int", "slots_float"):
        raise ValueError("unknown repeated scalar kind: " + KIND)
    cls = SlotPoint if KIND.startswith("slots_") else DictPoint
    value = 1.5 if KIND.endswith("_float") else 3
    total = squared(cls(value), n)
    assert total == n * value * value, (total, n)
    return total


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "1000000"))
    start = time.perf_counter_ns()
    bench(n)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
