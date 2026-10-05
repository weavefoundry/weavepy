"""Opaque values in hot loops: float speculation and the generic lanes.

Native code guesses a float for an object meeting a float (and for a local
the live frame holds a float in), and runs every other operation through
the interpreter's own dispatch. Each case warms the loop with the expected
types, then changes them mid-loop and checks the result, the exception,
and the traceback line against what the interpreter computes.
"""

import functools
import math
import random
import sys
import traceback
from math import cos, sin, sqrt


def source(values):
    it = iter(values)
    return lambda: next(it)


def float_peer(values):
    nxt = source(values)
    total = 0.0
    for _ in range(len(values)):
        total += nxt() * 0.5
    return total


floats = [float(i % 17) for i in range(4000)]
assert float_peer(floats) == sum(v * 0.5 for v in floats)
# An int, a big int, a bool, and a float subclass among the floats.


class F(float):
    def __mul__(self, other):
        return 1000.0


mixed = list(floats)
mixed[3000] = 3
mixed[3100] = 2 ** 80
mixed[3200] = True
mixed[3300] = F(2.0)
assert float_peer(mixed) == sum(v * 0.5 for v in mixed)


def compares(values, limit):
    nxt = source(values)
    hits = 0
    for _ in range(len(values)):
        if nxt() < limit:
            hits += 1
    return hits


assert compares(floats, 8.5) == sum(v < 8.5 for v in floats)
# Exact comparison of an int beyond 2**53 with a float.
big = [2 ** 53 + 1] * 10 + floats
assert compares(big, 9007199254740992.0) == sum(v < 9007199254740992.0 for v in big)


def stored(rng, n):
    # The locals take the float the live frame holds; a guard at each
    # store checks it.
    under = 0
    for _ in range(n):
        x = rng()
        y = rng()
        if x * x + y * y <= 1.0:
            under += 1
    return under


r = random.Random(5)
expect = 0
r2 = random.Random(5)
for _ in range(20000):
    a = r2.random()
    b = r2.random()
    if a * a + b * b <= 1.0:
        expect += 1
assert stored(r.random, 20000) == expect
# The producer starts returning ints (and then an object) mid-loop.
def produced(k):
    if k < 3000:
        return 0.25
    if k < 3500:
        return 1 if k % 2 else 0
    return F(0.5)


seq = [produced(k) for k in range(8000)]
expect = sum(1 for k in range(0, 8000, 2) if seq[k] * seq[k] + seq[k + 1] * seq[k + 1] <= 1.0)
assert stored(source(seq), 4000) == expect


# Math intrinsics bound to globals, with domain errors at the right line.
def norms(points):
    total = 0.0
    for p in points:
        total += sqrt(p) + sin(p) * cos(p)
    return total


pts = [float(i) for i in range(3000)]
assert norms(pts) == functools.reduce(lambda acc, p: acc + (sqrt(p) + sin(p) * cos(p)), pts, 0.0)
pts[2500] = -1.0
try:
    norms(pts)
except ValueError as exc:
    assert "math domain error" in str(exc) or "expected a nonnegative input" in str(exc), exc
    assert traceback.extract_tb(sys.exc_info()[2])[-1].line == \
        "total += sqrt(p) + sin(p) * cos(p)"
else:
    raise AssertionError("expected ValueError")


# Rebinding the global mid-loop is seen at once.
def rebinding(n):
    global sqrt
    total = 0.0
    for i in range(n):
        if i == n // 2:
            sqrt = lambda v: -1.0  # noqa: E731
        total += sqrt(4.0)
    sqrt = math.sqrt
    return total


assert rebinding(4000) == 2000 * 2.0 + 2000 * -1.0


# Generic binary operations: user dunders, NotImplemented, in-place.
class Vec:
    __slots__ = ("x", "y")

    def __init__(self, x, y):
        self.x = x
        self.y = y

    def __add__(self, other):
        if not isinstance(other, Vec):
            return NotImplemented
        return Vec(self.x + other.x, self.y + other.y)

    def __radd__(self, other):
        if other == 0:
            return self
        return NotImplemented

    def __sub__(self, other):
        if other.x == 999:
            raise ArithmeticError("bad operand")
        return Vec(self.x - other.x, self.y - other.y)


def vec_sum(vs):
    acc = Vec(0, 0)
    for v in vs:
        acc = acc + v - Vec(1, 1)
    return acc.x, acc.y


vs = [Vec(i, 2 * i) for i in range(3000)]
assert vec_sum(vs) == (sum(range(3000)) - 3000, 2 * sum(range(3000)) - 3000)
vs[2000] = Vec(5, 5)
vs[2001] = Vec(999, 0)


def vec_sub_all(vs):
    acc = Vec(0, 0)
    for v in vs:
        acc = acc - v  # the raising line
    return acc.x


try:
    vec_sub_all(vs)
except ArithmeticError as exc:
    assert str(exc) == "bad operand"
    tb = traceback.extract_tb(sys.exc_info()[2])
    assert [f.name for f in tb[-2:]] == ["vec_sub_all", "__sub__"], tb
    assert tb[-2].line == "acc = acc - v  # the raising line"
else:
    raise AssertionError("expected ArithmeticError")


def mixed_ops(items):
    out = []
    total = 0
    for a, b in items:
        total = total + (a ** b)
        out += [a // b if b else 0]
    return total, out


items = [(i % 7, i % 3) for i in range(3000)]
exp_total = sum(a ** b for a, b in items)
exp_out = [a // b if b else 0 for a, b in items]
assert mixed_ops(items) == (exp_total, exp_out)
items[2900] = (2.5, -2)
items[2901] = ("ab", 3)
try:
    mixed_ops(items)
except TypeError:
    pass
else:
    raise AssertionError("expected TypeError")


# Rich comparisons returning non-bool objects, assigned and tested.
class Rich:
    def __init__(self, v):
        self.v = v

    def __lt__(self, other):
        return [self.v] if self.v < other.v else []  # a truthy or falsy list


def rich(xs, ys):
    hits = 0
    kept = None
    for i in range(len(xs)):
        r = xs[i] < ys[i]
        kept = r
        if xs[i] < ys[i]:
            hits += 1
    return hits, kept


xs = [Rich(i % 5) for i in range(3000)]
ys = [Rich(2) for _ in range(3000)]
assert rich(xs, ys) == (sum(1 for x in xs if x.v < 2), [xs[-1].v] if xs[-1].v < 2 else [])

print("JIT numeric speculation: ok")
