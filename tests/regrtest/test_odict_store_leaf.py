"""`od[k] = v` on an `OrderedDict` in compiled code.

A store by a `str` or `int` key goes straight to the class's own
`__setitem__`; new keys append to the order, existing keys keep their
place; a subclass's Python `__setitem__` (defined up front or added
mid-loop) still runs; unhashable keys raise; keys with a Python `__eq__`
or `__hash__`, and a displaced value's finalizer, behave as in CPython.
Run it past the compile thresholds, in a loop and in a called function.
"""

from collections import OrderedDict


class Logged(OrderedDict):
    calls = 0

    def __setitem__(self, k, v):
        Logged.calls += 1
        super().__setitem__(k, v)


class Plain(OrderedDict):
    pass


class K:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return hash(self.v)

    def __eq__(self, other):
        return isinstance(other, K) and self.v == other.v


class D:
    log = []

    def __init__(self, n):
        self.n = n

    def __del__(self):
        D.log.append(self.n)


def store(od, k, v):
    od[k] = v


def loop(od, n):
    for i in range(n):
        od[i % 7] = i
        od["k" + str(i % 3)] = i


def check(cls, n):
    od = cls((j, None) for j in range(5))
    od.move_to_end(0)
    loop(od, n)
    for i in range(n):
        store(od, i % 7, i)
    last = n - 1
    expect = [1, 2, 3, 4, 0, "k0", "k1", "k2", 5, 6]
    assert list(od) == expect, list(od)
    assert od[last % 7] == last
    return od


N = 3000
check(OrderedDict, N)
check(Plain, N)
Logged.calls = 0
check(Logged, N)
assert Logged.calls == 3 * N + 5, Logged.calls


class Late(OrderedDict):
    pass


late = Late()
seen = []
for i in range(N):
    if i == N // 2:
        Late.__setitem__ = lambda self, k, v: seen.append(k)
    store(late, i % 5, i)
assert len(seen) == N - N // 2 and list(late) == [0, 1, 2, 3, 4], (len(seen), list(late))

od = OrderedDict()
for i in range(N):
    store(od, K(i % 4), i)
    try:
        store(od, [i], i)
    except TypeError:
        pass
    else:
        raise AssertionError("unhashable key")
assert [k.v for k in od] == [0, 1, 2, 3]

od = OrderedDict()
for i in range(N):
    D.log.clear()
    store(od, "x", D(i))
    if i:
        assert D.log == [i - 1], (i, D.log)
print("ok")
