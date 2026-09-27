"""Attribute-chain replacement with bounded, retained payloads.

Measure the loop including allocations and collection; verify identical work.
Payloads make intermediate ownership visible without a large memory demand.
"""
import gc
import os
import time


class Node:
    def __init__(self, next=None, value=3, payload=None):
        self.next = next
        self.value = value
        self.payload = payload


def make_chain(i):
    return Node(Node(Node()), payload=bytes([i % 251]) * 65536)


def bench(n):
    root = Node(make_chain(0))
    total = 0
    for i in range(n):
        total += root.next.next.next.value
        if i % 64 == 0:
            root.next = make_chain(i)
            gc.collect()
    assert total == n * 3
    return total


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "16384"))
    start = time.perf_counter_ns()
    bench(n)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
