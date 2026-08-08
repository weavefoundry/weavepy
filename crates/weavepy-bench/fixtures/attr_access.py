"""Attribute get/set on plain and __slots__ instances + method calls."""

import os


class Plain:
    def __init__(self):
        self.a = 1
        self.b = 2.0
        self.c = "x"

    def tick(self):
        self.a += 1
        return self.a


class Slotted:
    __slots__ = ("a", "b", "c")

    def __init__(self):
        self.a = 1
        self.b = 2.0
        self.c = "x"

    def tick(self):
        self.a += 1
        return self.a


def bench(n):
    p = Plain()
    s = Slotted()
    total = 0
    for _ in range(n):
        total += p.a + s.a
        p.b = p.b + 0.5
        s.b = s.b + 0.5
        total += p.tick() + s.tick()
        if p.c == s.c:
            total += 1
    return total


if __name__ == "__main__":
    import time

    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "50000"))
    _t0 = time.perf_counter_ns()
    bench(n)
    _t1 = time.perf_counter_ns()
    print("WEAVEPY_BENCH_NS=%d" % (_t1 - _t0))
