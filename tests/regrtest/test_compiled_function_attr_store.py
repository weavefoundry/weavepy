"""Attribute stores into a function or a plain class held in a local, run
from compiled code (`wrapper.__wrapped__ = f`, as `functools.wraps` and
`contextlib.ExitStack.callback` do, and `Cls.count = n`), keep CPython's
results: the attribute lands in the function's `__dict__`, `inspect.unwrap`
follows it, and a function's own attributes (`__name__`, `__doc__`) still
take their setters."""
import functools
import inspect


def mk(cb, *args, **kwds):
    def w(a, b, c):
        return cb(*args, **kwds)
    w.__wrapped__ = cb
    w.calls = 0
    w.__doc__ = "wrapped"
    w.__name__ = "renamed"
    return w


class Counter:
    count = 0


def bump(n):
    cls = Counter
    cls.count = n
    return cls.count


def run(i):
    w = mk(len, (1, 2))
    assert w.__wrapped__ is len and w.calls == 0
    assert w.__doc__ == "wrapped" and w.__name__ == "renamed"
    assert w.__dict__ == {"__wrapped__": len, "calls": 0}
    assert w(0, 0, 0) == 2
    assert inspect.unwrap(w) is len
    assert bump(i) == i == Counter.count


for i in range(3000):
    run(i)


@functools.wraps(len)
def g(*a):
    return len(*a)


assert g.__wrapped__ is len and g.__name__ == "len"
print("ok")
