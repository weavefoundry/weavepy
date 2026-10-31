"""`yield from` chains resumed at their innermost generator from compiled
code (the levels staying collapsed while it runs) behave as resumed level
by level: values in order, return values landing in each level's `yield
from`, chains that grow as an inner generator starts delegating, a chain's
head driven by `for`, `next()`, `next(g, default)` and `send(None)`,
`gi_yieldfrom` and `gi_running` seen from inside, `throw` and `close` into
a head, and errors raised deep in a chain."""

import os
import subprocess
import sys
import traceback


class Tree:
    __slots__ = ("left", "value", "right")

    def __init__(self, left, value, right):
        self.left = left
        self.value = value
        self.right = right

    def __iter__(self):
        if self.left:
            yield from self.left
        yield self.value
        if self.right:
            yield from self.right


def build(lo, hi):
    if lo >= hi:
        return None
    mid = (lo + hi) // 2
    return Tree(build(lo, mid), mid, build(mid + 1, hi))


def pairs(it):
    it = iter(it)
    for a in it:
        b = next(it, -1)
        yield a, b


def walk(tree):
    out = []
    for x in tree:
        out.append(x)
    return out


tree = build(0, 600)
for _ in range(4):
    assert walk(tree) == list(range(600))
    assert sum(a * b for a, b in pairs(tree)) == sum(k * (k + 1) for k in range(0, 600, 2))
    assert max(x for x in tree if x % 7 == 3) == 598


# Return values travel up each level's `yield from`.
def leaf(k):
    yield k
    return k + 1


def level(depth, k):
    if depth == 0:
        r = yield from leaf(k)
    else:
        r = yield from level(depth - 1, k)
    yield -r
    return r * 2


def run_levels(n):
    out = []
    for k in range(n):
        g = level(4, k)
        while True:
            try:
                out.append(next(g))
            except StopIteration as e:
                out.append(("ret", e.value))
                break
    return out


def expect_levels(k):
    out = [k]
    r = k + 1
    for _ in range(5):
        out.append(-r)
        r *= 2
    return out + [("ret", r)]


for _ in range(3):
    assert run_levels(300) == [x for k in range(300) for x in expect_levels(k)]


# A head driven by `send(None)` and `next(g, default)`, with the chain's
# view from inside.
def inner_view():
    me = sys._getframe()
    for _ in range(3):
        yield me.f_code.co_name
    return "done"


def middle():
    return (yield from inner_view())


def outer():
    r = yield from middle()
    yield r


def drive(n):
    seen = []
    for _ in range(n):
        g = outer()
        seen.append(g.send(None))
        assert g.gi_yieldfrom is not None and g.gi_yieldfrom.gi_yieldfrom is not None
        assert not g.gi_running
        seen.append(next(g, "dflt"))
        seen.append(next(g))
        seen.append(g.send(None))
        seen.append(next(g, "dflt"))
    return seen


for _ in range(3):
    got = drive(300)
    assert got == ["inner_view", "inner_view", "inner_view", "done", "dflt"] * 300, got[:10]


# `throw` and `close` into a head whose chain ran collapsed.
closed = []


def guarded_leaf():
    try:
        while True:
            yield 1
    finally:
        closed.append("leaf")


def guarded_mid():
    try:
        yield from guarded_leaf()
    finally:
        closed.append("mid")


def throw_close(n):
    for _ in range(n):
        g = guarded_mid()
        for _ in range(5):
            assert next(g) == 1
        try:
            g.throw(KeyError("x"))
        except KeyError:
            pass
        else:
            raise AssertionError("no KeyError")
        g = guarded_mid()
        for _ in range(5):
            next(g)
        g.close()


closed.clear()
throw_close(300)
assert closed == ["leaf", "mid"] * 600, closed[:6]


# An error deep in a chain unwinds through every level.
def deep_fail(depth):
    if depth == 0:
        yield 1
        raise ZeroDivisionError("deep")
    yield from deep_fail(depth - 1)


def catch_deep(n):
    for _ in range(n):
        names = None
        try:
            for _ in deep_fail(5):
                pass
        except ZeroDivisionError as e:
            names = [f.name for f in traceback.extract_tb(e.__traceback__)]
        assert names == ["catch_deep"] + ["deep_fail"] * 6, names


catch_deep(300)


# Again with bodies compiling at once, and without tier 2, so the lazy
# paths run from the start (CPython ignores the settings).
if not os.environ.get("_LAZY_GEN_CHILD"):
    for extra in (
        {"WEAVEPY_FRAME_JIT_HOT": "5"},
        {"WEAVEPY_FRAME_JIT_HOT": "5", "WEAVEPY_TIER2": "0"},
    ):
        env = dict(os.environ, _LAZY_GEN_CHILD="1", **extra)
        r = subprocess.run(
            [sys.executable, __file__], env=env, capture_output=True, text=True, timeout=300
        )
        assert r.returncode == 0, (extra, r.stderr[-3000:])
