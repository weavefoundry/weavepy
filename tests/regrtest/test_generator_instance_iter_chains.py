"""Generators made by `iter(obj)` / `await obj` from a generator
`__iter__` / `__await__`, and recursive `yield from` chains.

`iter(obj)` builds the generator of a generator-function `__iter__`
without the call machinery; the receiver goes straight into the frame,
defaults fill any later parameters, and a body with cells of its own
takes the general frame. Resuming a suspended chain of `yield from`s
resumes its innermost generator directly; levels that share code and a
resume point are walked from what the previous level showed. Values,
`send`, `throw`, `close`, `gi_yieldfrom` and `gi_running` must come out
as CPython's.
"""

import sys


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


tree = build(0, 200)
for _ in range(30):
    assert list(tree) == list(range(200))
    assert sum(tree) == sum(range(200))
    assert max(x for x in tree if x % 7 == 3) == 199 - (199 - 3) % 7


# Defaults after `self`, and a body with a cell of its own.
class WithDefault:
    def __iter__(self, step=2, start=1):
        yield from range(start, 7, step)


class WithCell:
    def __init__(self, n):
        self.n = n

    def __iter__(self):
        n = self.n

        def bump(x):
            return x + n

        for i in range(3):
            yield bump(i)


for _ in range(50):
    assert list(WithDefault()) == [1, 3, 5]
    assert list(WithCell(10)) == [10, 11, 12]


# `__await__` as a generator function.
class Ready:
    def __init__(self, v):
        self.v = v

    def __await__(self):
        yield None
        return self.v


async def use(n):
    total = 0
    for i in range(n):
        total += await Ready(i)
    return total


def drive(c):
    try:
        while True:
            c.send(None)
    except StopIteration as e:
        return e.value


for _ in range(20):
    assert drive(use(50)) == sum(range(50))


# A deep chain: send and throw reach the innermost level; the levels in
# between report the delegation as CPython does.
def leaf(log):
    while True:
        try:
            got = yield "leaf"
            log.append(got)
        except KeyError as e:
            log.append(("caught", e.args[0]))
            yield "after-throw"


def level(n, log):
    if n == 0:
        r = yield from leaf(log)
    else:
        r = yield from level(n - 1, log)
    return r


for depth in (1, 2, 5, 12):
    log = []
    g = level(depth, log)
    assert next(g) == "leaf"
    for i in range(20):
        assert g.send(i) == "leaf"
    assert log == list(range(20)), log
    # Walk the chain: every level but the innermost delegates.
    inner, levels = g, 0
    while inner.gi_yieldfrom is not None:
        assert not inner.gi_running
        inner = inner.gi_yieldfrom
        levels += 1
    assert levels == depth + 1, (levels, depth)
    assert inner.gi_code.co_name == "leaf"
    assert g.throw(KeyError("k")) == "after-throw"
    assert log[-1] == ("caught", "k")
    assert next(g) == "leaf"
    g.close()
    assert g.gi_frame is None


# A level touched from inside the innermost one is running.
def outer_check():
    me = yield
    yield from inner_check(me)


def inner_check(top):
    assert top.gi_running
    try:
        next(top)
    except ValueError as e:
        assert "already executing" in str(e)
    else:
        raise AssertionError("re-entered a running generator")
    yield "ok"


for _ in range(10):
    g = outer_check()
    next(g)
    assert g.send(g) == "ok"
    g.close()

print("ok")
