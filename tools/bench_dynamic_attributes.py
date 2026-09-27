"""Measure plain and callback attribute reads on an object parameter.

WEAVEPY_DYNAMIC_ATTRIBUTE_KIND selects class, module, property, or getattr.
Each case reads two distinct names through the same driver. Receiver setup
happens before timing. The normal benchmark includes the first compilation;
bench_compare.py --warm also measures the already-called driver.
"""

import math
import os
import time


class Constants:
    value = 7
    other = 11


class Properties:
    @property
    def value(self):
        return 7

    @property
    def other(self):
        return 11


class Missing:
    def __getattr__(self, name):
        if name == "value":
            return 7
        if name == "other":
            return 11
        raise AttributeError(name)


kind = os.environ.get("WEAVEPY_DYNAMIC_ATTRIBUTE_KIND", "class")
if kind == "class":
    receiver = Constants
elif kind == "module":
    math.value = 7
    math.other = 11
    receiver = math
elif kind == "property":
    receiver = Properties()
elif kind == "getattr":
    receiver = Missing()
else:
    raise ValueError("unsupported dynamic attribute kind")


def read(root, n):
    total = 0
    for _ in range(n):
        total += root.value
        total += root.other
    return total


def bench(n):
    total = read(receiver, n)
    assert total == 18 * n
    return total


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "1000000"))
    start = time.perf_counter_ns()
    bench(n)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
