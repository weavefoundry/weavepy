"""Measure long chains that require native class/module/property fallback.

WEAVEPY_CHAIN_FALLBACK selects class, module, or property at the eighth link.
The same sixteen field reads run in every mode; setup is included in bench.
"""
import math as module_link
import os
import time


class Node:
    def __init__(self, next=None, value=3):
        self.next = next
        self.value = value


class Constant:
    next = None


class Property:
    def __init__(self, child):
        self.child = child

    @property
    def next(self):
        return self.child


kind = os.environ.get("WEAVEPY_CHAIN_FALLBACK", "class")
if kind not in ("class", "module", "property"):
    raise ValueError("unsupported fallback kind")


def bench(n):
    tail = Node(Node(Node(Node(Node(Node(Node()))))))
    if kind == "class":
        Constant.next = tail
        link = Constant
    elif kind == "module":
        module_link.next = tail
        link = module_link
    else:
        link = Property(tail)
    root = Node(Node(Node(Node(Node(Node(Node(Node(link))))))))
    total = 0
    for _ in range(n):
        total += root.next.next.next.next.next.next.next.next.next.next.next.next.next.next.next.value
    assert total == n * 3
    return total


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "1000000"))
    start = time.perf_counter_ns()
    bench(n)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
