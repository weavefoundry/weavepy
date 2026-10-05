"""Allocate many small objects holding floats, normalize them, and reduce."""

from math import cos, sin, sqrt

WORK = 100000


class Point:
    def __init__(self, i):
        self.x = x = sin(i)
        self.y = cos(i) * 3
        self.z = (x * x) / 2

    def normalize(self):
        x = self.x
        y = self.y
        z = self.z
        norm = sqrt(x * x + y * y + z * z)
        self.x /= norm
        self.y /= norm
        self.z /= norm

    def maximize(self, other):
        self.x = self.x if self.x > other.x else other.x
        self.y = self.y if self.y > other.y else other.y
        self.z = self.z if self.z > other.z else other.z
        return self


def bench(n):
    points = [Point(i) for i in range(n)]
    for p in points:
        p.normalize()
    nxt = points[0]
    for p in points[1:]:
        nxt = nxt.maximize(p)
    return (round(nxt.x, 9), round(nxt.y, 9), round(nxt.z, 9))
