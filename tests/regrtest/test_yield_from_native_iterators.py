"""`yield from` a range, tuple or list iterator steps it in place.

A suspended generator delegating to one of those iterators hands out its
next item without resuming its frame, and the delegation's end leaves the
`SEND` with `None` as CPython's does. A list's end goes through the general
path: it detaches the list, and a subclass's instance it frees runs its
`__del__` there.
"""

import gc


def from_range(n):
    r = yield from range(n)
    yield ("end", r)


def from_tuple(t):
    yield from t
    yield "after"


def from_list(xs):
    yield from xs


for _ in range(2000):
    assert list(from_range(4)) == [0, 1, 2, 3, ("end", None)]
    assert list(from_tuple((1, "a", [2]))) == [1, "a", [2], "after"]

# A list changed while it's delegated to: the iterator reads it live.
xs = [1, 2, 3]
g = from_list(xs)
assert next(g) == 1
xs.append(4)
xs[1] = "two"
assert list(g) == ["two", 3, 4]

# A list subclass freed at the delegation's end runs its __del__.
freed = []


class L(list):
    def __del__(self):
        freed.append(len(self))


for _ in range(50):
    assert list(from_list(L([7, 8, 9]))) == [7, 8, 9]
gc.collect()
assert len(freed) == 50, freed

# Next, send, throw and close of a generator resting in the delegation.
g = from_range(10)
assert next(g) == 0 and g.send(None) == 1 and next(g) == 2
try:
    g.send(5)  # a range iterator has no send()
except AttributeError:
    pass
else:
    raise AssertionError("send of a value went through")
g = from_range(10)
next(g)
try:
    g.throw(ValueError("x"))
except ValueError:
    pass
else:
    raise AssertionError("throw did not propagate")
closed = []


def guarded():
    try:
        yield from range(5)
    finally:
        closed.append(True)


g = guarded()
next(g)
g.close()
assert closed == [True]
