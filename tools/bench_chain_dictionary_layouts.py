"""Alternate one class's dictionary layouts in a two-field chain."""

import os
import time


class Node:
    pass


def bench(n):
    one = Node()
    one.child = Node()
    one.child.value = 3
    other = Node()
    other.padding = 0
    other.child = Node()
    other.child.padding = 0
    other.child.value = 5
    total = 0
    for i in range(n):
        if i & 1:
            root = other
        else:
            root = one
        total += root.child.value
    assert total == (n // 2) * 8 + (n % 2) * 3
    return total


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "1000000"))
    start = time.perf_counter_ns()
    bench(n)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
