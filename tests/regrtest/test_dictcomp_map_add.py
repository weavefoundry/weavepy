"""Dict comprehensions in compiled code (`MAP_ADD`).

A key the native probe settles is stored in place; a duplicate key keeps
the first key object and takes the last value; unhashable keys raise;
keys with a Python `__eq__` or `__hash__`, `True` against `1`, and a
replaced value's finalizer behave as in CPython. Run it past the compile
threshold.
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

    def __del__(self):
        D.log.append(self.n)


def run(n):
    xs = (1, 2, 3, 2, 1)
    for i in range(n):
        d = {x: x * 10 + i % 3 for x in xs}
        assert d == {1: 10 + i % 3, 2: 20 + i % 3, 3: 30 + i % 3}
        assert list(d) == [1, 2, 3]
        e = {k: v for k, v in [(1, "a"), (True, "b"), (1.0, "c")]}
        assert e == {1: "c"} and type(next(iter(e))) is int
        s = {str(x): None for x in xs}
        assert list(s) == ["1", "2", "3"]
        kd = {K(x): x for x in xs}
        assert len(kd) == 3 and kd[K(2)] == 2
        try:
            {[x]: x for x in xs}
        except TypeError:
            pass
        else:
            raise AssertionError("unhashable key")
        D.log.clear()
        dd = {k: D(k) for k in (5, 5)}
        assert D.log == [5], D.log
        assert dd[5].n == 5
        del dd
        D.log.clear()


run(3000)
print("ok")
