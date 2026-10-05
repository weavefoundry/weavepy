"""Natively built `itertools` objects.

Calling an exact `itertools` class builds its native adapter directly;
the adapter is the instance. These checks pin what that must preserve:
type identity, `iter(x) is x`, repr, the classes' own methods (which
reach the state through `self._core`), subclassing with overrides, the
keyword forms the native path declines, and error messages.
"""

import itertools as it
import operator
import sys


def raises(exc, fn, *args, **kwargs):
    try:
        fn(*args, **kwargs)
    except exc as e:
        return str(e)
    raise AssertionError("%s not raised" % exc.__name__)


def gen(n):
    yield from range(n)


cases = [
    (lambda: it.chain([1, 2], (3,), "ab"), [1, 2, 3, "a", "b"]),
    (lambda: it.chain.from_iterable([[1], [2, 3]]), [1, 2, 3]),
    (lambda: it.islice(it.count(5, 2), 4), [5, 7, 9, 11]),
    (lambda: it.repeat("x", 3), ["x"] * 3),
    (lambda: it.islice(it.cycle("ab"), 5), list("ababa")),
    (lambda: it.accumulate([1, 2, 3]), [1, 3, 6]),
    (lambda: it.accumulate([1, 2, 3], initial=10), [10, 11, 13, 16]),
    (lambda: it.compress("abcd", [1, 0, 1, 0]), ["a", "c"]),
    (lambda: it.dropwhile(lambda x: x < 2, range(4)), [2, 3]),
    (lambda: it.takewhile(lambda x: x < 2, range(4)), [0, 1]),
    (lambda: it.filterfalse(None, [0, 1, 0, 2]), [0, 0]),
    (lambda: it.starmap(pow, [(2, 3), (3, 2)]), [8, 9]),
    (lambda: it.islice(range(10), 1, 8, 3), [1, 4, 7]),
    (lambda: it.islice(gen(10), 2, None), list(range(2, 10))),
    (lambda: it.pairwise("abc"), [("a", "b"), ("b", "c")]),
    (lambda: it.zip_longest("ab", [1], fillvalue=0), [("a", 1), ("b", 0)]),
    (lambda: it.product("ab", repeat=2), [("a", "a"), ("a", "b"), ("b", "a"), ("b", "b")]),
    (lambda: it.permutations(range(3), 2), [(0, 1), (0, 2), (1, 0), (1, 2), (2, 0), (2, 1)]),
    (lambda: it.combinations(range(4), 3), [(0, 1, 2), (0, 1, 3), (0, 2, 3), (1, 2, 3)]),
    (lambda: it.combinations_with_replacement("ab", 2), [("a", "a"), ("a", "b"), ("b", "b")]),
    (lambda: it.batched(range(5), 2), [(0, 1), (2, 3), (4,)]),
]
for make, expected in cases:
    obj = make()
    cls = getattr(it, type(obj).__name__)
    assert type(obj) is cls, obj
    assert isinstance(obj, cls)
    assert iter(obj) is obj
    assert list(obj) == expected, (obj, expected)
    assert list(obj) == []
    loop = [x for x in make()]
    assert loop == expected
    first = next(make(), "empty")
    assert first == (expected[0] if expected else "empty")

assert repr(it.count()) == "count(0)"
assert repr(it.count(5, 2)) == "count(5, 2)"
assert repr(it.repeat(1, 3)) == "repeat(1, 3)"
assert repr(it.chain()).startswith("<itertools.chain object at ")
assert operator.length_hint(it.repeat(1, 3)) == 3
assert list(it.islice(it.count(sys.maxsize - 1), 3)) == [
    sys.maxsize - 1,
    sys.maxsize,
    sys.maxsize + 1,
]
assert list(it.repeat(1, -5)) == []
assert list(zip(it.count(), "abc")) == [(0, "a"), (1, "b"), (2, "c")]

# A partially consumed source is left where the adapter stopped.
src = iter(range(10))
assert list(it.islice(src, 3)) == [0, 1, 2]
assert next(src) == 3

# Errors keep CPython's messages (the native path declines and the
# class's own `__new__` raises).
raises(ValueError, it.islice, range(3), -1)
raises(ValueError, it.islice, range(3), 0, 1, 0)
raises(TypeError, it.islice, range(3))
raises(TypeError, it.count, "a")
raises(TypeError, it.chain, x=1)
raises(ValueError, it.permutations, range(3), -1)
raises(TypeError, it.combinations, range(3))
raises(ValueError, it.batched, range(3), 0)
raises(ValueError, it.product, "ab", repeat=-1)
# `chain` calls `iter()` on each argument only when it reaches it.
lazy = it.chain([1], 1)
assert next(lazy) == 1
assert raises(TypeError, next, lazy).endswith("is not iterable")


# Subclasses keep the class's own construction and honor overrides.
class C(it.chain):
    pass


c = C([1], [2])
assert type(c) is C and list(c) == [1, 2]
assert type(C.from_iterable([[1]])) is C


class Cy(it.cycle):
    def __next__(self):
        return "x"


assert next(Cy("ab")) == "x"

assert [list(x) for x in it.tee(it.chain([1, 2], [3]), 2)] == [[1, 2, 3], [1, 2, 3]]
assert [(k, list(g)) for k, g in it.groupby("aabbc")] == [
    ("a", ["a", "a"]),
    ("b", ["b", "b"]),
    ("c", ["c"]),
]

# groupby: groups go stale once the groupby advances, and keys run once
# per element.
g = it.groupby("aabbcc")
k1, g1 = next(g)
k2, g2 = next(g)
assert (k1, list(g1), k2, list(g2)) == ("a", [], "b", ["b", "b"])
assert type(g) is it.groupby and iter(g) is g
assert type(g2).__name__ == "_grouper"
calls = []
keys = [k for k, _ in it.groupby(range(6), lambda x: calls.append(x) or x // 2)]
assert keys == [0, 1, 2] and calls == list(range(6))
assert [(k, list(v)) for k, v in it.groupby(range(4), key=lambda x: x % 2 == 0)] == [
    (True, [0]),
    (False, [1]),
    (True, [2]),
    (False, [3]),
]
raises(ZeroDivisionError, list, it.groupby([1, 0], lambda x: 1 // x))


class GB(it.groupby):
    pass


assert [(k, list(v)) for k, v in GB("aab")] == [("a", ["a", "a"]), ("b", ["b"])]

print("itertools native ok")
