"""Compiled `x.m(...)` calls of leaf methods keep Python's semantics.

The frame JIT runs a leaf method (`return self.v`, `return env[self.k]`,
a setter) straight from the call site, with the callee cached by the
receiver's class version. Each of the things that version doesn't cover
must still be seen: an instance attribute shadowing the method, the
method's `__code__` rebound, a class changed, many receiver classes at
one site, a receiver that isn't an instance, a temporary receiver whose
release runs `__del__`, and the recursion limit.
"""

import sys


class Num:
    def __init__(self, v):
        self.v = v

    def ev(self, env):
        return self.v


class Var:
    def __init__(self, k):
        self.k = k

    def ev(self, env):
        return env[self.k]


class Box:
    def __init__(self, item):
        self.item = item

    def set(self, v):
        self.item = v


class Holder:
    def __init__(self, node):
        self.node = node


def total(nodes, env, n):
    t = 0
    for _ in range(n):
        for node in nodes:
            t += node.ev(env)
    return t


def through(holders, env, n):
    t = 0
    for _ in range(n):
        for h in holders:
            t += h.node.ev(env)
    return t


env = {"x": 4, "y": 5}
nodes = [Num(3), Var("x"), Num(1), Var("y")]
assert total(nodes, env, 3000) == 3000 * 13
assert through([Holder(n) for n in nodes], env, 3000) == 3000 * 13

# An instance attribute shadows the method.
shadowed = Num(3)
shadowed.ev = lambda env: 100
assert total([shadowed], env, 10) == 1000
assert through([Holder(shadowed)], env, 10) == 1000

# The method's code rebound.
saved = Num.ev.__code__
Num.ev.__code__ = (lambda self, env: self.v * 2).__code__
assert total([Num(3)], env, 10) == 60
assert through([Holder(Num(3))], env, 10) == 60
Num.ev.__code__ = saved
assert total([Num(3)], env, 10) == 30

# The class changed.
Num.ev = lambda self, env: -self.v
assert total([Num(3)], env, 10) == -30
del Num.ev
Num.ev = lambda self, env: self.v
assert total([Num(3)], env, 10) == 30


# Many receiver classes at one site.
def make(i):
    def ev(self, env):
        return self.v

    return type("K%d" % i, (), {"__init__": Num.__init__, "ev": ev})


many = [make(i)(i) for i in range(20)]
assert total(many, env, 200) == 200 * sum(range(20))
assert through([Holder(m) for m in many], env, 200) == 200 * sum(range(20))


# A receiver that isn't an instance.
class Meta:
    @staticmethod
    def ev(env):
        return 7


assert total([Meta, Meta()], env, 3000) == 3000 * 14

# A setter (an effect leaf).
boxes = [Box(0) for _ in range(4)]
for i in range(3000):
    for b in boxes:
        b.set(i)
assert all(b.item == 2999 for b in boxes)

# A temporary receiver released by the call runs `__del__` at once.
deleted = []


class Temp:
    def __init__(self, v):
        self.v = v

    def ev(self, env):
        return self.v

    def __del__(self):
        deleted.append(self.v)


def temps(n):
    t = 0
    for i in range(n):
        t += Temp(i).ev(None)
        assert deleted[-1] == i, (deleted[-3:], i)
    return t


assert temps(3000) == sum(range(3000))


# The recursion limit still applies.
def deep(n, node):
    if n == 0:
        return node.ev(env)
    return deep(n - 1, node)


old = sys.getrecursionlimit()
for _ in range(100):
    deep(5, Num(1))
sys.setrecursionlimit(60)
try:
    deep(200, Num(1))
except RecursionError:
    pass
else:
    raise AssertionError("no RecursionError")
finally:
    sys.setrecursionlimit(old)

# A surplus positional argument raises.
try:
    for _ in range(3000):
        Num(1).ev(env)
    Num(1).ev(env, 2)
except TypeError as e:
    assert "positional argument" in str(e), e
else:
    raise AssertionError("a surplus positional argument bound")

print("ok")
