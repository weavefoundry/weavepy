"""`zip` over tuple and list cursors in compiled loops.

The adapter refills its result tuple (or the one before it) only when it
holds the tuple alone, so tuples a loop keeps never change; a `zip` whose
first source ends stops in place; sources that alias, mutation during the
loop, `strict=True`, and finalizers of items held only by the adapter
behave as in CPython. Run it past the compile threshold.
"""

import gc

log = []


class D:
    def __init__(self, n):
        self.n = n

    def __del__(self):
        log.append(self.n)


def kept(n):
    a = (1, 2, 3)
    b = ["x", "y", "z"]
    for _ in range(n):
        pairs = list(zip(a, b))
        assert pairs == [(1, "x"), (2, "y"), (3, "z")]
        assert len({id(p) for p in pairs}) == 3
        seen = []
        for t in zip(a, b):
            seen.append(t)
        assert seen == pairs
        last = None
        snap = []
        for t in zip(a, b, a):
            snap.append(tuple(t))
            if last is not None:
                assert last != t
            last = t
        assert snap == [(1, "x", 1), (2, "y", 2), (3, "z", 3)] and last == (3, "z", 3)
        for t in zip(a, b):
            pass
        assert t == (3, "z")
        n3 = 0
        for x, y, z in zip(a, b, b):
            n3 += x
            assert y == z
        assert n3 == 6
        assert list(zip(a, b[:2])) == [(1, "x"), (2, "y")]
        assert list(zip(a[:1], b)) == [(1, "x")]
        assert list(zip()) == [] and list(zip(a)) == [(1,), (2,), (3,)]


def aliasing(n):
    for _ in range(n):
        it = iter([1, 2, 3, 4, 5])
        assert list(zip(it, it)) == [(1, 2), (3, 4)]
        assert list(it) == []
        it = iter((1, 2, 3, 4, 5, 6, 7))
        assert list(zip(it, it, it)) == [(1, 2, 3), (4, 5, 6)]
        assert next(it, None) is None


def mutation(n):
    for _ in range(n):
        a = [1, 2, 3, 4]
        b = (5, 6, 7, 8)
        out = []
        for x, y in zip(a, b):
            out.append((x, y))
            a.pop()
        assert out == [(1, 5), (2, 6)]
        a = [1, 2]
        for t in zip(a, b):
            a.append(t[0])
            if len(a) > 6:
                break
        assert a == [1, 2, 1, 2, 1, 2]


def strict(n):
    for _ in range(n):
        try:
            for t in zip((1, 2), (1,), strict=True):
                pass
        except ValueError:
            pass
        else:
            raise AssertionError("strict zip of unequal lengths")
        assert list(zip((1, 2), [3, 4], strict=True)) == [(1, 3), (2, 4)]


def finalizers(n):
    for _ in range(n):
        log.clear()
        xs = [D(1), D(2)]
        ys = (0, 0)
        for t in zip(xs, ys):
            pass
        del t
        del xs
        assert sorted(log) == [1, 2], log
        log.clear()
        for t in zip([D(3)], (0,), [D(4)]):
            pass
        del t
        assert sorted(log) == [3, 4], log


for f in (kept, aliasing, mutation, strict, finalizers):
    f(400)
gc.collect()
print("ok")
