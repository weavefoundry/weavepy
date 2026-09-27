"""Replace the middle of a sixteen-field chain and collect during the loop.

The node after eight links carries a 64 KiB payload. This exposes ownership at
the guarded eight-field boundary while retaining a fixed live graph.
Allocation and collection are included in the measured workload.
"""

import gc
import os
import time


class Node:
    def __init__(self, next=None, value=3, payload=None):
        self.next = next
        self.value = value
        self.payload = payload


def make_tail(i):
    return Node(Node(Node(Node(Node(Node(Node(Node())))))), payload=bytes([i % 251]) * 65536)


def bench(n):
    root = Node(Node(Node(Node(Node(Node(Node(Node(make_tail(0)))))))))
    total = 0
    for i in range(n):
        total += root.next.next.next.next.next.next.next.next.next.next.next.next.next.next.next.value
        if i % 64 == 0:
            root.next.next.next.next.next.next.next.next = make_tail(i)
            gc.collect()
    assert total == n * 3
    return total


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "16384"))
    start = time.perf_counter_ns()
    bench(n)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
