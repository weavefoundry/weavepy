"""Keyword calls of native functions and methods from compiled loops give
the interpreter's results: `list.sort(key=...)` (whose key runs Python),
`str.split(maxsplit=...)`, `int(base=...)`, `sorted(reverse=...)`,
`super().__init__(...)` of a native base, a bad keyword's `TypeError`, an
error from the key function, and `sys.getrefcount` of an argument."""

import os
import signal
import sys


class Base(dict):
    pass


class Child(Base):
    def __init__(self, **kw):
        super().__init__(**kw)


def run(n):
    total = 0
    for i in range(n):
        data = [3, 1, 2, i % 5]
        data.sort(key=lambda x: -x)
        total += data[0]
        total += len("a b c d".split(maxsplit=1))
        total += int("ff", base=16)
        total += sorted([1, 3, 2], reverse=True)[0]
        total += len(Child(a=i, b=2))
    return total


def expected(n):
    total = 0
    for i in range(n):
        total += max(3, i % 5) + 2 + 255 + 3 + 2
    return total


for _ in range(4):
    assert run(300) == expected(300)


def bad(n):
    seen = 0
    for i in range(n):
        try:
            "abc".split(bogus=1)
        except TypeError:
            seen += 1
        try:
            [1, 2].sort(key=lambda x: 1 / 0)
        except ZeroDivisionError:
            seen += 1
    return seen


for _ in range(3):
    assert bad(300) == 600


def counts(n):
    obj = object()
    out = set()
    for i in range(n):
        out.add(sys.getrefcount(obj))
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
        def kill_kw(n):
            for i in range(n):
                if i == n - 1:
                    sorted([os.kill(os.getpid(), signal.SIGUSR1)], key=None)
            return n

        for _ in range(3):
            try:
                kill_kw(300)
            except Tripped:
                pass
            else:
                raise AssertionError("expected Tripped")
    finally:
        signal.signal(signal.SIGUSR1, old)
print("ok")
