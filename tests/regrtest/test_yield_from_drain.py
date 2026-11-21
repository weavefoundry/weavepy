"""`list(gen())` where the generator delegates to a tuple, list or range.

A collecting consumer takes a `yield from` over a plain sequence iterator
in one run instead of one yield at a time; the yields around it, partial
consumption, other consumers, and sources other code holds behave as in
CPython. Run it past the compile threshold.
"""

tup = (1, 2, 3)
lst = ["a", "b"]


def g1():
    yield 0
    yield from tup
    yield from lst
    yield from range(4, 7)
    yield from range(10, 0, -3)
    yield "end"


def g2(it):
    r = yield from it
    yield ("returned", r)


def g3(xs):
    for x in xs:
        yield from (x, x)


expected = [0, 1, 2, 3, "a", "b", 4, 5, 6, 10, 7, 4, 1, "end"]
big = list(range(70000))


def run(n):
    for _ in range(n):
        assert list(g1()) == expected
        assert tuple(g1()) == tuple(expected)
        g = g1()
        assert next(g) == 0 and next(g) == 1
        assert list(g) == expected[2:] and list(g) == []
        it = iter(lst)
        next(it)
        assert list(g2(it)) == ["b", ("returned", None)] and list(it) == []
        it = iter(tup)
        assert list(g2(it)) == [1, 2, 3, ("returned", None)]
        assert list(g3([1, 2])) == [1, 1, 2, 2]
        assert sum(g1() if False else g3([1, 2, 3])) == 12
        assert any(g3([0, 5])) and not all(g3([1, 0]))


run(400)
assert list(g2(iter(big))) == big + [("returned", None)]
print("ok")
