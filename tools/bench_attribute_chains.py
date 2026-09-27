"""Repeated reads through linked instance dictionaries."""

import os
import time


class Node:
    def __init__(self, next=None, value=0):
        self.next = next
        self.value = value


def bench(n):
    root = Node(Node(Node(Node(value=3))))
    total = 0
    for _ in range(n):
        total += root.next.next.next.value
    assert total == 3 * n
    return total


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "1000000"))
    start = time.perf_counter_ns()
    bench(n)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
