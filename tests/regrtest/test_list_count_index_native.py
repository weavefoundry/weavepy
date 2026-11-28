"""`list.count(x)`, `list.index(x)` and the tuple's when no comparison runs code.

Mixed numbers compare across `int`, `float` and `bool`; `nan` matches
itself by identity; strings, bytes, `None` and tuples of those compare
natively; an item with its own `__eq__`, bounds, and a missing value
take the full path; a missing value's error reads as CPython's. Run it
past the compile threshold.
"""


class Eq:
    calls = 0

    def __eq__(self, other):
        Eq.calls += 1
        return other == "eq"


nan = float("nan")


def run(n):
    for i in range(n):
        xs = [1, 2.0, True, "a", b"b", None, (1, "a"), 2, nan]
        assert xs.count(1) == 2 and xs.count(2) == 2 and xs.count(True) == 2
        assert xs.count("a") == 1 and xs.count(b"b") == 1 and xs.count(None) == 1
        assert xs.count((1, "a")) == 1 and xs.count(nan) == 1 and xs.count(3) == 0
        assert xs.index(2) == 1 and xs.index("a") == 3 and xs.index(nan) == 8
        assert xs.index((1, "a")) == 6 and xs.index(1.0) == 0
        try:
            xs.index("zz")
        except ValueError as e:
            assert str(e) == "list.index(x): x not in list"
        else:
            raise AssertionError("found a missing value")
        assert xs.index(2, 2) == 7 and xs.index(True, 1, 3) == 2
        ys = [Eq(), "eq", 1]
        before = Eq.calls
        assert ys.count("eq") == 2 and Eq.calls == before + 1
        assert ys.index(1) == 2
        t = tuple(xs)
        assert t.count(1) == 2 and t.count(nan) == 1 and t.index(2) == 1
        assert t.index("a") == 3 and t.index(2, 2) == 7
        assert (Eq(), "eq").count("eq") == 2


run(3000)
print("ok")
