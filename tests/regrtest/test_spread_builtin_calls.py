"""Spread calls (`f(*args)`, `f(*args, **{})`) of builtins and of native
methods bound to their receivers from compiled loops give the interpreter's
results: leaf builtins (`len`, `isinstance`, `abs`), bodies that run Python
code (`max` over instances with `__lt__`, `sorted` with a key), a saved
`log.clear` from a closure (`ExitStack.callback`'s shape), list spreads, a
non-empty mapping, keywords to a builtin that takes none, wrong argument
counts, errors raised from the body, a finalizer of a spread item, a signal
the body trips, and `sys.getrefcount` of an argument."""

import contextlib
import gc
import os
import signal
import sys


class V:
    def __init__(self, v):
        self.v = v

    def __lt__(self, other):
        return self.v < other.v


def make(cb, *args, **kwds):
    def wrapper(a, b, c):
        return cb(*args, **kwds)

    return wrapper


def run(n):
    total = 0
    log = []
    for i in range(n):
        a = ([1, 2, i],)
        total += len(*a)
        total += isinstance(*(i, int))
        total += abs(*[-i])
        total += max(*(V(1), V(i), V(2))).v
        total += sorted(*([3, i, 1],), key=lambda x: -x)[0]
        log.append(i)
        w = make(log.clear)
        w(None, None, None)
        total += len(log)
        add = log.append
        add(*(i,), **{})
        total += log[-1]
        log.clear()
        total += int(*("7",))
        total += divmod(*[i, 3])[1]
        total += len(str.join(*(",", ["a", "b"])))
    return total


def expected(n):
    total = 0
    for i in range(n):
        total += 3 + 1 + i + max(2, i) + max(3, i) + 0 + i + 7 + i % 3 + 3
    return total


for _ in range(4):
    assert run(300) == expected(300), (run(300), expected(300))


def stack(n):
    seen = 0
    log = []
    for i in range(n):
        with contextlib.ExitStack() as st:
            st.callback(log.append, i)
            st.callback(log.clear)
        seen += len(log)
    return seen


for _ in range(3):
    assert stack(300) == 300 * 1


def errors(n):
    seen = 0
    for i in range(n):
        try:
            len(*())
        except TypeError as e:
            seen += "len()" in str(e)
        try:
            len(*(1,))
        except TypeError:
            seen += 1
        try:
            max(*(V(1), 2))
        except AttributeError:
            seen += 1
        try:
            sorted(*([1, 2],), key=lambda x: 1 / 0)
        except ZeroDivisionError:
            seen += 1
        try:
            [].pop(*())
        except IndexError:
            seen += 1
        try:
            abs(*(1,), **{"x": 1})
        except TypeError:
            seen += 1
        try:
            len([], x=i)
        except TypeError as e:
            seen += "keyword" in str(e)
    return seen


for _ in range(3):
    assert errors(300) == 2100


class Mortal:
    log = []

    def __del__(self):
        Mortal.log.append(1)


def finalizers(n):
    seen = 0
    for i in range(n):
        Mortal.log.clear()
        id(*[Mortal()])
        seen += len(Mortal.log)
    return seen


gc.collect()
for _ in range(3):
    assert finalizers(300) == 300


def counts(n):
    obj = object()
    a = (obj,)
    out = set()
    for i in range(n):
        out.add(sys.getrefcount(*a))
    return out


for _ in range(3):
    assert len(counts(300)) == 1

if hasattr(signal, "SIGUSR1"):

    class Tripped(Exception):
        pass

    def handler(signum, frame):
        raise Tripped

    old = signal.signal(signal.SIGUSR1, handler)
    try:

        def kill_spread(n):
            args = (os.getpid(), signal.SIGUSR1)
            for i in range(n):
                if i == n - 1:
                    os.kill(*args)
            return n

        for _ in range(3):
            try:
                kill_spread(300)
            except Tripped:
                pass
            else:
                raise AssertionError("expected Tripped")
    finally:
        signal.signal(signal.SIGUSR1, old)
print("ok")
