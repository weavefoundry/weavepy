"""The builtin `map`, `filter`, `zip`, and `enumerate` objects.

WeavePy builds exact instances as native adapters and steps them without
a Python frame; these checks pin the CPython-visible surface: the types
themselves, subclassing (with overrides honored), pickling, keyword
rules, `strict=`, lazy error timing, and exceptions raised by callbacks.
"""

import copy
import gc
import operator
import pickle


def raises(exc, fn, *args, **kwargs):
    try:
        fn(*args, **kwargs)
    except exc as e:
        return str(e)
    raise AssertionError("%s not raised" % exc.__name__)


def gen(n):
    yield from range(n)


# Real types, not functions.
for ty in (map, filter, zip, enumerate):
    assert isinstance(ty, type), ty
assert type(map(abs, [1])) is map
assert type(filter(None, [1])) is filter
assert type(zip([1])) is zip
assert type(enumerate(gen(1))) is enumerate
assert type(enumerate([1])) is enumerate
assert map.__module__ == "builtins" and map.__name__ == "map"
assert repr(zip()).startswith("<zip object at ")
m = map(abs, [1])
assert iter(m) is m and isinstance(m, map)
assert (map | int) == (map | int)

# Results, over native containers and interpreter-driven sources.
assert list(map(abs, [-1, -2])) == [1, 2]
assert list(map(operator.add, [1, 2, 3], (10, 20))) == [11, 22]
assert list(map(lambda x: x * 2, gen(3))) == [0, 2, 4]
assert list(filter(None, [0, 1, "", "a"])) == [1, "a"]
assert list(filter(lambda x: x % 2, gen(6))) == [1, 3, 5]
assert list(filter(bool, [0, 2])) == [2]
assert list(zip("ab", range(5), (7, 8, 9))) == [("a", 0, 7), ("b", 1, 8)]
assert list(zip()) == []
assert list(zip(gen(2), "xyz")) == [(0, "x"), (1, "y")]
assert list(enumerate(gen(2), 5)) == [(5, 0), (6, 1)]
assert list(enumerate(iterable=gen(1), start=3)) == [(3, 0)]
assert [a + b for a, b in zip(range(3), range(3))] == [0, 2, 4]
assert [i * x for i, x in enumerate((4, 5))] == [0, 5]
assert list(reversed(range(4))) == [3, 2, 1, 0]
assert list(reversed(range(0, 10, 3))) == [9, 6, 3, 0]
assert type(reversed(range(3))) is type(iter(range(3)))
assert sum(map(abs, [-1, -2])) == 3
assert sorted(map(abs, [-3, 1])) == [1, 3]
assert ",".join(map(str, (1, 2))) == "1,2"
assert dict(zip("ab", [1, 2])) == {"a": 1, "b": 2}
assert bytes(map(int, ["1", "2"])) == b"\x01\x02"

# A source shared by several arguments is consumed in argument order.
it = iter(range(5))
assert list(zip(it, it)) == [(0, 1), (2, 3)]
assert next(it, "end") == "end"
it = iter(range(5))
assert [(a, b) for a, b in zip(it, it)] == [(0, 1), (2, 3)]

# The zip result tuple may be reused, but never while it is referenced.
pairs = list(zip(range(3), range(3)))
assert pairs == [(0, 0), (1, 1), (2, 2)]
held = []
for t in zip(range(3), "abc"):
    held.append(t)
assert held == [(0, "a"), (1, "b"), (2, "c")]

# Argument checks at construction; keyword rules (bpo-43413).
assert raises(TypeError, map, abs) == "map() must have at least two arguments."
raises(TypeError, map, abs, 5)
raises(TypeError, map, abs, [1], foo=1)
raises(TypeError, filter, None)
raises(TypeError, filter, None, [], x=1)
raises(TypeError, zip, [1], foo=1)
raises(TypeError, enumerate, gen(1), "a")
raises(TypeError, zip, 5)

# Errors from callbacks surface at the step, not at construction.
m = map(lambda x: 1 / x, [1, 0])
assert next(m) == 1.0
raises(ZeroDivisionError, next, m)
f = filter(lambda x: 1 / x, [0])
raises(ZeroDivisionError, list, f)
calls = []
m = map(calls.append, [1, 2])
assert calls == []
next(m)
assert calls == [1]

# A callback that fails consumes its item exactly once, however the loop
# steps it (warm loops evaluate simple callbacks in place).
for _ in range(200):
    m = map(lambda x: 10 // x, [1, 2, 0, 5])
    out = []
    try:
        for v in m:
            out.append(v)
    except ZeroDivisionError:
        pass
    assert out == [10, 5] and next(m) == 2
    f = filter(lambda x: 1 // x, [1, 0, 1])
    got = []
    try:
        for v in f:
            got.append(v)
    except ZeroDivisionError:
        pass
    assert got == [1] and list(f) == [1]
assert [x for x in filter(lambda x: x % 3, range(10))] == [1, 2, 4, 5, 7, 8]
assert [y for y in map(lambda x: x * x, range(5))] == [0, 1, 4, 9, 16]

# strict=True (map since 3.14).
assert list(zip([1, 2], [3, 4], strict=True)) == [(1, 3), (2, 4)]
assert raises(ValueError, list, zip([1, 2], [1], strict=True)) == (
    "zip() argument 2 is shorter than argument 1"
)
assert raises(ValueError, list, zip([1], [1, 2], [3], strict=True)) == (
    "zip() argument 2 is longer than argument 1"
)
assert raises(ValueError, list, map(operator.add, [1], [2, 3], strict=True)) == (
    "map() argument 2 is longer than argument 1"
)

# Pickling and copying, including the strict state.
for obj in (
    map(abs, [-1, -2]),
    filter(None, [0, 1, 2]),
    zip([1, 2], "ab"),
    enumerate("ab", 3),
):
    expected = list(copy.deepcopy(obj))
    for proto in range(pickle.HIGHEST_PROTOCOL + 1):
        clone = pickle.loads(pickle.dumps(copy.deepcopy(obj), proto))
        assert type(clone) is type(obj)
        assert list(clone) == expected
s = pickle.loads(pickle.dumps(zip([1, 2], [3], strict=True)))
raises(ValueError, list, s)
assert map(abs, [1]).__reduce__()[0] is map
assert enumerate(gen(1)).__reduce__()[0] is enumerate
assert list(copy.copy(map(abs, [-3]))) == [3]


# Subclasses: the instance is the subclass, overrides win, and the
# inherited protocol still steps the native state.
class M(map):
    def __next__(self):
        return ("M", super().__next__())


m = M(abs, [-1, -2])
assert type(m) is M and isinstance(m, map)
assert list(m) == [("M", 1), ("M", 2)]
assert M(abs, [1]).__reduce__()[0] is M


class Z(zip):
    pass


assert list(Z([1, 2], [3, 4])) == [(1, 3), (2, 4)]
assert type(pickle.loads(pickle.dumps(Z([1], [2])))) is Z


class F(filter):
    def __init__(self, *args, extra=None):
        self.extra = extra


f = F(None, [0, 1], extra=3)
assert list(f) == [1] and f.extra == 3


class E(enumerate):
    pass


e = E(gen(2))
assert next(e) == (0, 0)
assert list(e) == [(1, 1)]
raises(StopIteration, next, E(gen(0)))
assert list(map.__new__(map, abs, [-1])) == [1]

# The adapters are visible to the cycle collector.
class Node:
    pass


n = Node()
n.m = map(lambda x: n, [1])
del n
gc.collect()

print("seqiter native ok")
