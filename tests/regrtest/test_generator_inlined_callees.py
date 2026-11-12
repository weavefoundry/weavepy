"""A generator whose body calls small functions, which compiled code
lowers in line, keeps CPython's results: each yield, a method's call and
a property's read on the generator's values, a callee that raises
(with its frame in the traceback), one that leaves its inlined body partway
(a builtin it can't run in line, a frame it looks at), a generator nested
in another and delegated to, and `throw` and `close` while suspended."""
import sys
import traceback


def f(x):
    if x > 100:
        return [x]
    return x + 1


def g(x):
    if x == 7:
        raise ValueError(x)
    return x * 2


def h(x):
    if x % 3 == 0:
        return sorted([x, 1, 2])[0] + len(str(x))
    return x


def who(x):
    if x == 5:
        return sys._getframe(1).f_code.co_name
    return x


class Acc:
    def __init__(self, k):
        self.k = k

    def add(self, x):
        return x + self.k

    @property
    def twice(self):
        return self.k * 2


def inner(xs):
    for x in xs:
        yield f(x) + h(x)


def outer(xs):
    yield from inner(xs)
    yield sum(f(x) for x in xs)


def run(i):
    xs = list(range(i % 4, i % 4 + 9))
    assert list(f(x) for x in xs) == [x + 1 for x in xs]
    assert [h(x) for x in xs] == list(h(x) for x in xs)
    try:
        list(g(x) for x in range(10))
    except ValueError as e:
        names = [fr.name for fr in traceback.extract_tb(e.__traceback__)]
        assert names[-2:] == ["<genexpr>", "g"], names
    else:
        raise AssertionError("the callee's raise was lost")
    assert list(who(x) for x in range(7))[5] == "<genexpr>"
    acc = Acc(i)
    assert list(acc.add(x) for x in xs) == [x + i for x in xs]
    assert sum(Acc(x).add(1) + Acc(x).twice for x in xs) == sum(3 * x + 1 for x in xs)
    assert list(outer(xs)) == [f(x) + h(x) for x in xs] + [sum(x + 1 for x in xs)]
    gen = (f(x) for x in xs)
    assert next(gen) == xs[0] + 1
    try:
        gen.throw(KeyError("k"))
    except KeyError:
        pass
    gen2 = inner(xs)
    next(gen2)
    gen2.close()
    assert list(gen2) == []


# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(2000):
    run(i)
print("ok")
