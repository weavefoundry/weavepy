"""Set comprehensions in compiled code (`SET_ADD`).

A value the native probe settles is added in place; an equal element
keeps the first object; unhashable values raise; values with a Python
`__eq__` or `__hash__`, `True` against `1`, and a discarded duplicate's
finalizer behave as in CPython. Run it past the compile threshold.
"""


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

    def __hash__(self):
        return 7

    def __eq__(self, other):
        return isinstance(other, D)

    def __del__(self):
        D.log.append(self.n)


def run(n):
    xs = (1, 2, 3, 2, 1)
    for i in range(n):
        s = {x + i % 2 for x in xs}
        assert s == {1 + i % 2, 2 + i % 2, 3 + i % 2}
        t = {v for v in (1, True, 1.0, "a", "a", None, None)}
        assert t == {1, "a", None} and [type(v) for v in t if v == 1] == [int]
        k = {K(x) for x in xs}
        assert len(k) == 3
        try:
            {[x] for x in xs}
        except TypeError:
            pass
        else:
            raise AssertionError("unhashable")
        D.log.clear()
        d = {D(j) for j in (1, 2)}
        assert D.log == [2] and next(iter(d)).n == 1, D.log
        del d
        D.log.clear()
        assert {c for c in "hello"} == set("helo")


run(3000)
print("ok")
