"""Dict union and equality, set comparisons, keys-view algebra, and bytes
repetition and formatting in compiled code.

Plain keys and values take the native paths; a key or value with a
Python `__eq__` takes the full path, which runs it. Results that are heap
values keep compiled code from leaving at the next instruction. Run it
past the compile threshold.
"""


class E:
    eqs = 0

    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return hash(self.v)

    def __eq__(self, other):
        E.eqs += 1
        return isinstance(other, E) and self.v == other.v


nan = float("nan")


def dicts(n):
    d1 = {"a": 1, "b": [1, 2]}
    d2 = {"b": 3, "c": 4}
    for _ in range(n):
        u = d1 | d2
        assert u == {"a": 1, "b": 3, "c": 4} and list(u) == ["a", "b", "c"]
        assert u is not d1 and d1 == {"a": 1, "b": [1, 2]}
        assert d1 | {} == d1 and {} | d2 == d2
        assert {"x": 1, "y": 2} == {"y": 2, "x": 1}
        assert {"x": 1} != {"x": 1.5} and {"x": 1} == {"x": 1.0}
        assert {"x": 1} != {"y": 1} and {"x": 1} != {"x": 1, "y": 2}
        assert {"x": {"y": (1, [2])}} == {"x": {"y": (1, [2])}}
        d = {"n": nan}
        assert d == d and d != {"n": float("nan")}
        assert {"n": nan} == {"n": nan}
        E.eqs = 0
        assert {"e": E(1)} == {"e": E(1)} and E.eqs == 1
        assert {E(1): 1} == {E(1): 1} and E.eqs >= 2


def sets(n):
    s = {"a", "c"}
    f = frozenset({"a", "c", "d"})
    for i in range(n):
        assert s <= {"a", "c", "d"} and s < f and not s > f and f >= s
        assert not {"a", "z"} <= {"a", "c", "d"}
        assert s == frozenset({"c", "a"}) and s != f
        assert {1, 2} == {2.0, 1} and {i} <= {i, i + 1}
        E.eqs = 0
        assert {E(1)} <= {E(1), E(2)} and E.eqs >= 1


def views(n):
    d = {"a": 1, "b": 2, 1: 3}
    k = d.keys()
    for _ in range(n):
        r = k & {"a", "z"}
        assert r == {"a"} and type(r) is set
        assert k | {"z"} == {"a", "b", 1, "z"} and type(k | frozenset()) is set
        assert k - {"a"} == {"b", 1} and k ^ {"a", "z"} == {"b", 1, "z"}
        assert k & frozenset({"b"}) == {"b"} and type(k & frozenset()) is set
        # (A tie keeps the view's element, as `set.intersection(view)`.)
        tie = k & {1.0, "a", "b"}
        assert tie == {1, "a", "b"} and [x for x in tie if x == 1][0].__class__ is int
        bigger = d.keys() & {1.0}
        assert [type(x) for x in bigger] == [float]
        E.eqs = 0
        assert {E(1): 0}.keys() & {E(1)} == {E(1)} and E.eqs >= 1


def byte_ops(n):
    bs = b"ab"
    for i in range(n):
        assert bs * 3 == b"ababab" and 2 * bs == b"abab"
        assert bs * 0 == b"" and bs * -1 == b""
        assert b"%d-%s" % (i, b"x") == b"%d-x" % i
        assert b"%x" % 255 == b"ff" and b"%%%d" % 5 == b"%5"
        assert b"%5d" % 3 == b"    3"
        try:
            b"%d" % "s"
        except TypeError:
            pass
        else:
            raise AssertionError("bytes %d of a str")
        try:
            b"%d %d" % (1,)
        except TypeError:
            pass
        else:
            raise AssertionError("too few arguments")


for f in (dicts, sets, views, byte_ops):
    f(1500)

# Self-referential dicts compare by identity first, and two distinct
# ones recurse until the full path raises.
a = {}
a["a"] = a
assert a == a
b = {}
b["a"] = b
try:
    a == b
except RecursionError:
    pass
else:
    raise AssertionError("expected RecursionError")
print("ok")
