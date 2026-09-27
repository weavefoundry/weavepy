"""Measure conditional methods shared by three ordinary instance classes.

The same method reads fields at different dictionary indices and returns a
heap object. Each timed run includes object construction and cache warmup.
"""

import os
import time


def selected(self):
    if self.flag:
        return self.value
    return self.other


class First:
    selected = selected


class Second:
    selected = selected


class Third:
    selected = selected


class Payload:
    def __init__(self, value):
        self.value = value


def bench(n):
    first, second, third = First(), Second(), Third()
    second.padding = 1
    third.padding = 1
    third.another_padding = 2
    for obj in (first, second, third):
        obj.flag = True
        obj.value = Payload(7)
        obj.other = Payload(11)
    second.flag = False
    total = 0
    for _ in range(n):
        total += first.selected().value
        total += second.selected().value
        total += third.selected().value
    assert total == n * 25
    return total


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "300000"))
    start = time.perf_counter_ns()
    bench(n)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
