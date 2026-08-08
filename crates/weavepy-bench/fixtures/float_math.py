"""Float-heavy point normalization — pyperformance `float` shape.

Exercises float arithmetic, math-module calls, and per-iteration
object construction.
"""

import math
import os


class Point:
    def __init__(self, i):
        self.x = math.sin(i)
        self.y = math.cos(i) * 3.0
        self.z = (self.x * self.x) / 2.0

    def normalize(self):
        norm = math.sqrt(self.x * self.x + self.y * self.y + self.z * self.z)
        self.x /= norm
        self.y /= norm
        self.z /= norm

    def maximize(self, other):
        self.x = self.x if self.x > other.x else other.x
        self.y = self.y if self.y > other.y else other.y
        self.z = self.z if self.z > other.z else other.z
        return self


def bench(n):
    points = [None] * n
    for i in range(n):
        points[i] = Point(i)
    for p in points:
        p.normalize()
    nxt = points[0]
    for p in points[1:]:
        nxt = nxt.maximize(p)
    return nxt.x + nxt.y + nxt.z


if __name__ == "__main__":
    import time

    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "10000"))
    _t0 = time.perf_counter_ns()
    bench(n)
    _t1 = time.perf_counter_ns()
    print("WEAVEPY_BENCH_NS=%d" % (_t1 - _t0))
