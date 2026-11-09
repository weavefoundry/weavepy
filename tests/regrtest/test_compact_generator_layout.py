"""A generator keeps its code, names and origin through its life and after
it finishes (its object may be reused for another generator), deep operand
stacks grow and drain correctly, and the cycle collector finds cycles
through generators and instances in every generation."""

import gc
import sys
import types
import weakref


def gen(n):
    for i in range(n):
        yield i


def other():
    yield "other"


# `gi_code` before, during and after the body runs; a finished
# generator's object may come back as another one.
for _ in range(200):
    g = gen(2)
    assert g.gi_code is gen.__code__
    assert list(g) == [0, 1]
    assert g.gi_code is gen.__code__, "a finished generator keeps its code"
    del g
    h = other()
    assert h.gi_code is other.__code__
    assert h.__name__ == "other" and h.__qualname__ == "other"
    assert next(h) == "other"

# Names set on a generator stay its own.
g = gen(3)
g.__name__ = "renamed"
g.__qualname__ = "Outer.renamed"
assert (g.__name__, g.__qualname__) == ("renamed", "Outer.renamed")
assert list(g) == [0, 1, 2]
assert (g.__name__, g.__qualname__) == ("renamed", "Outer.renamed")
fresh = gen(1)
assert fresh.__name__ == "gen", "another generator's names don't leak"
del g, fresh


# A coroutine's origin, when tracking is on.
async def coro():
    return 1


sys.set_coroutine_origin_tracking_depth(2)
try:
    c = coro()
    assert c.cr_origin is not None and len(c.cr_origin) >= 1
    assert c.cr_origin[0][2] == "<module>"
    c.close()
finally:
    sys.set_coroutine_origin_tracking_depth(0)
c = coro()
assert c.cr_origin is None
c.close()


# Deep operand stacks: many arguments, starred calls and big displays grow
# the stack past its first capacity and drain it into the callee.
def many(*args, **kwargs):
    return len(args), sorted(kwargs)


args = list(range(300))
assert many(*args) == (300, [])
assert many(*args, *args, a=1, b=2) == (600, ["a", "b"])
assert many(**{"k%d" % i: i for i in range(50)})[1][:2] == ["k0", "k1"]
big = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
    20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37,
]
assert sum(big) == sum(range(38))
assert (*big, *big)[-1] == 37


def deep_gen():
    # Suspended with live operands of its own below each yield.
    yield ((yield 0), (yield 1), [(yield 2), (yield 3)], (yield 4))


d = deep_gen()
assert next(d) == 0
for i in range(1, 5):
    assert d.send("s%d" % (i - 1)) == i
assert d.send("s4") == ("s0", "s1", ["s2", "s3"], "s4")


# Cycles through generators and instances, in each generation.
class Node:
    def __init__(self):
        self.peer = None
        self.it = None


def make_cycles(n):
    out = []
    for _ in range(n):
        a, b = Node(), Node()
        a.peer, b.peer = b, a

        def loop(node):
            yield node

        a.it = loop(a)
        next(a.it)
        out.append(types.SimpleNamespace(alive=a))
    return out


gc.collect()
for generation in (0, 1, 2):
    keep = make_cycles(50)
    drop = make_cycles(300)
    refs = [weakref.ref(ns.alive) for ns in drop]
    for ns in drop:
        ns.alive = None
    del drop
    gc.collect(generation)
    gc.collect()
    assert all(r() is None for r in refs), generation
    # What is still referenced survived, intact.
    for ns in keep:
        a = ns.alive
        assert a.peer.peer is a
        assert a.it.gi_frame is not None
    del keep
    gc.collect()

print("ok")
