"""Generators resumed lazily from compiled code (their activation bound only
when something needs it) behave as resumed ones: values, `gi_running`,
frames seen from inside, tracebacks, `StopIteration` values, `close` and
`throw`, `yield from` with a plain delegate, coroutines driven by `send`,
and inlined `list`/`tuple`/`all`/`any` of generator expressions that drain
their generator."""

import sys
import traceback

N = 3000
T8 = tuple(range(8))


def counter():
    i = 0
    while True:
        got = yield len(T8) + i
        i += 1 if got is None else got


def consume_for(n):
    total = 0
    g = counter()
    for k, x in enumerate(g):
        total += x
        if k == n:
            break
    return total


def consume_next(n):
    g = counter()
    return sum(next(g) for _ in range(n))


def consume_send(n):
    g = counter()
    next(g)
    total = 0
    for _ in range(n):
        total += g.send(2)
    return total


for _ in range(3):
    assert consume_for(N) == sum(8 + i for i in range(N + 1))
    assert consume_next(N) == sum(8 + i for i in range(N))
    assert consume_send(N) == sum(8 + 2 * i for i in range(1, N + 1))


# What the body sees of itself while it runs.
def introspect():
    me = None
    while True:
        frame = sys._getframe()
        me = yield (frame.f_code.co_name, frame.f_back.f_code.co_name, me is not None and me.gi_running)


def drive_introspect(n):
    g = introspect()
    out = next(g)
    for _ in range(n):
        out = g.send(g)
    return out


for _ in range(3):
    assert drive_introspect(N) == ("introspect", "drive_introspect", True)


# A raise inside a lazily resumed body: its traceback runs through the
# generator, which is finished afterwards.
def fails_at(k):
    i = 0
    while True:
        if i == k:
            raise ValueError(len(T8) + i)
        yield i
        i += 1


def catch_fail(k):
    g = fails_at(k)
    seen = []
    try:
        for x in g:
            seen.append(x)
    except ValueError as e:
        names = [f.name for f in traceback.extract_tb(e.__traceback__)]
        return seen, e.args[0], names, g.gi_frame
    return None


for _ in range(N // 10):
    seen, arg, names, frame = catch_fail(5)
    assert seen == [0, 1, 2, 3, 4], seen
    assert arg == 13
    assert names == ["catch_fail", "fails_at"], names
    assert frame is None


# `StopIteration` carries the return value; `next(g, default)` gives the
# default.
def returns(v):
    yield len(T8)
    return v


def stop_value(n):
    vals = []
    for i in range(n):
        g = returns(i)
        next(g)
        try:
            next(g)
        except StopIteration as e:
            vals.append(e.value)
        g = returns(i)
        assert next(g, None) == 8
        assert next(g, "dflt") == "dflt"
    return vals


assert stop_value(N) == list(range(N))


# `close` and `throw` after lazy resumes, with a `finally` that runs once.
log = []


def guarded():
    try:
        while True:
            yield len(T8)
    finally:
        log.append("closed")


def close_after(n):
    g = guarded()
    for _ in range(n):
        assert next(g) == 8
    g.close()
    assert g.gi_frame is None
    g = guarded()
    next(g)
    try:
        g.throw(KeyError("k"))
    except KeyError:
        pass
    return g.gi_frame is None


for _ in range(3):
    log.clear()
    assert close_after(N)
    assert log == ["closed", "closed"], log


# `yield from` of a plain delegate, its return value the result.
def leaf(k):
    yield len(T8) + k
    return k * 10


def mid(k):
    r = yield from leaf(k)
    yield r


def run_mid(n):
    out = []
    for k in range(n):
        out.extend(mid(k))
    return out


for _ in range(2):
    assert run_mid(N) == [x for k in range(N) for x in (8 + k, k * 10)]


# Coroutines driven by `send(None)`, suspending in an `await`.
class Suspend:
    def __await__(self):
        yield len(T8)


async def inner_coro(k):
    await Suspend()
    return k


async def outer_coro(n):
    total = 0
    for k in range(n):
        total += await inner_coro(k)
    return total


def drive(coro):
    steps = 0
    try:
        while True:
            assert coro.send(None) == 8
            steps += 1
    except StopIteration as e:
        return e.value, steps


for _ in range(3):
    assert drive(outer_coro(N)) == (sum(range(N)), N)


# Finalizers of what a lazily running body drops run promptly.
class Tracked:
    alive = 0

    def __init__(self):
        Tracked.alive += 1

    def __del__(self):
        Tracked.alive -= 1


def churn():
    while True:
        t = Tracked()
        yield len(T8)
        del t


def run_churn(n):
    g = churn()
    for _ in range(n):
        next(g)
        assert Tracked.alive == 1
    g.close()
    assert Tracked.alive == 0


run_churn(N)


# Inlined `list`/`tuple`/`all`/`any` of generator expressions.
class Truthy:
    def __init__(self, v):
        self.v = v

    def __bool__(self):
        return bool(self.v)


def drains(n):
    xs = list(range(20))
    for _ in range(n):
        assert tuple(x * 2 for x in xs) == tuple(range(0, 40, 2))
        assert list(x for x in xs if x % 3 == 0) == [0, 3, 6, 9, 12, 15, 18]
        assert all(x >= 0 for x in xs)
        assert not all(x < 10 for x in xs)
        assert any(x == 19 for x in xs)
        assert not any(x > 19 for x in xs)
        assert not all(v for v in (1, 2, None, 3))
        assert any(v for v in (0, None, "", Truthy(1)))
        assert not any(v for v in (0, None, "", Truthy(0)))
        # A body the drain can't finish (a division by zero) raises from the
        # loop with the items so far discarded.
        try:
            tuple(10 // (x - 5) for x in xs)
        except ZeroDivisionError:
            pass
        else:
            raise AssertionError("no ZeroDivisionError")
        # A body that stops partway (a call) goes on item by item.
        assert list(len(str(x)) for x in xs) == [1] * 10 + [2] * 10
        assert all(str(x) for x in xs)
    return True


assert drains(N // 3)
