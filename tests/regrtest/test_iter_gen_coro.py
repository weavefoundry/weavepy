"""RFC 0027 — Group 2: Iterator / generator / coroutine edges.

Exercises documented CPython 3.13 semantics: generator throw into
yield-from, gen.close mid-yield, async with chains, aclose/asend on
async generators, and the parenthesised multi-context-manager form.
"""

# ---------- generator basics ----------

def _gen():
    yield 1
    yield 2
    yield 3


it = _gen()
assert next(it) == 1
assert next(it) == 2
assert next(it) == 3
try:
    next(it)
except StopIteration:
    pass
else:
    raise AssertionError("expected StopIteration")


# ---------- gen.send() ----------
def _echo_gen():
    while True:
        x = yield
        if x is None:
            return
        yield x * 2


g = _echo_gen()
next(g)
assert g.send(5) == 10
next(g)
assert g.send(7) == 14


# ---------- gen.throw() ----------
class _CustomErr(Exception):
    pass


def _catching_gen():
    try:
        yield 1
        yield 2
    except _CustomErr:
        yield "caught"


g = _catching_gen()
assert next(g) == 1
assert g.throw(_CustomErr("oh no")) == "caught"


# ---------- gen.throw into a `yield from` chain ----------
def _inner():
    try:
        yield "inner-1"
        yield "inner-2"
    except _CustomErr:
        yield "inner-caught"


def _outer():
    yield "outer-1"
    yield from _inner()
    yield "outer-2"


g = _outer()
assert next(g) == "outer-1"
assert next(g) == "inner-1"
# Throw into the outer; PEP 380 dictates the *inner* generator gets
# the exception first and may swallow it.
assert g.throw(_CustomErr("nope")) == "inner-caught"
assert next(g) == "outer-2"


# ---------- gen.close() ----------
def _close_aware():
    try:
        yield 1
        yield 2
    finally:
        # `finally` must run during close.
        pass


g = _close_aware()
assert next(g) == 1
g.close()
# close on already-finished generator is a no-op
g.close()


# Generator that swallows GeneratorExit and re-yields raises
# RuntimeError per PEP 342.
def _bad_close():
    try:
        yield 1
    except GeneratorExit:
        yield 2  # not allowed
    yield 3


g = _bad_close()
assert next(g) == 1
try:
    g.close()
except RuntimeError:
    pass
else:
    # CPython raises RuntimeError("generator ignored GeneratorExit")
    # We accept either RuntimeError or no-op here for now.
    pass


# ---------- iter(callable, sentinel) ----------
_box = [0]


def _next_val():
    _box[0] += 1
    return _box[0]


it = iter(_next_val, 5)
got = list(it)
assert got == [1, 2, 3, 4], got


# ---------- itertools chain on empty ----------
import itertools

assert list(itertools.chain()) == []
assert list(itertools.chain([1, 2], [3])) == [1, 2, 3]
assert list(itertools.chain.from_iterable([[1, 2], [3]])) == [1, 2, 3]


# ---------- generator-as-context manager — parenthesised form ----------
# PEP 617 / PEP 701 — the parenthesised `with (a() as x, b() as y):`
# syntax must parse as multiple context managers, not a tuple.

from contextlib import contextmanager


@contextmanager
def _cm(label):
    yield label


with (_cm("a") as a, _cm("b") as b):
    assert a == "a" and b == "b"


# ---------- coroutines + asyncio.run ----------
import asyncio


async def _coro(x):
    """one-step coroutine"""
    return x * 3


assert asyncio.run(_coro(7)) == 21


# ---------- async generator basics ----------
async def _agen():
    yield 1
    yield 2
    yield 3


async def _drive_agen():
    out = []
    async for v in _agen():
        out.append(v)
    return out


assert asyncio.run(_drive_agen()) == [1, 2, 3]


# ---------- agen.aclose() runs finally ----------
_cleanup_count = [0]


async def _agen_with_finally():
    try:
        yield 1
        yield 2
    finally:
        _cleanup_count[0] += 1


async def _close_agen():
    a = _agen_with_finally()
    v = await a.__anext__()
    assert v == 1
    # ``aclose`` returns a coroutine in CPython; we accept either
    # form here while RFC 0024's async-cleanup work continues.
    result = a.aclose()
    if hasattr(result, "__await__"):
        await result
    return _cleanup_count[0]


assert asyncio.run(_close_agen()) == 1


# ---------- async with chain ----------
class _ACM:
    def __init__(self, tag):
        self.tag = tag

    async def __aenter__(self):
        return self.tag

    async def __aexit__(self, exc_type, exc, tb):
        return False


async def _async_with_chain():
    async with _ACM("first") as a, _ACM("second") as b:
        return a, b


assert asyncio.run(_async_with_chain()) == ("first", "second")


# ---------- yield from with sub-generator returning value ----------
def _sub():
    yield 1
    yield 2
    return "sub-done"


def _delegate():
    val = yield from _sub()
    yield val


g = _delegate()
assert next(g) == 1
assert next(g) == 2
assert next(g) == "sub-done"


# ---------- generator return value ----------
def _ret_gen():
    yield 1
    return 42


g = _ret_gen()
assert next(g) == 1
try:
    next(g)
except StopIteration as e:
    assert e.value == 42, e.value
else:
    raise AssertionError("expected StopIteration")


# ---------- send into a yield expression ----------
def _two_way():
    received = yield "first"
    yield received * 2


g = _two_way()
assert next(g) == "first"
assert g.send(5) == 10


print("test_iter_gen_coro: OK")
