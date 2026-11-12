"""Generators drained by `list()`, `tuple()`, `set()`, `sum()`, `all()` and
`any()` from compiled code, their yields folded into the consumer in place:
CPython's results, every value kept (big ints, strings, objects), a value
the consumer declines (a non-scalar in `sum()`, the deciding item of
`all()`) handed back as it was, and no reference left over."""
import sys


def f(x):
    return x + 1


big = 1 << 80
word = "w" * 40


class Box:
    def __init__(self, v):
        self.v = v

    def __add__(self, other):
        return Box(self.v + (other.v if isinstance(other, Box) else other))

    __radd__ = __add__


def plain(n):
    for i in range(n):
        yield i


def calls(n):
    # A body with calls: resumed by the core loop, not a fast step.
    for i in range(n):
        yield f(i)


def check(i):
    xs = list(range(8))
    assert list(x * 2 for x in xs) == [0, 2, 4, 6, 8, 10, 12, 14]
    assert tuple(x for x in xs if x % 2) == (1, 3, 5, 7)
    assert set(x % 3 for x in xs) == {0, 1, 2}
    assert sum(x for x in xs) == 28 and sum(f(x) for x in xs) == 36
    assert sum(x + 0.5 for x in xs) == 32.0
    assert sum((big for _ in xs), 0) == 8 * big
    assert sum(Box(x) for x in xs).v == 28
    assert all(x >= 0 for x in xs) and not all(x < 7 for x in xs)
    assert any(x == 7 for x in xs) and not any(x > 7 for x in xs)
    assert all(f(x) for x in xs) and any(f(x) - 1 == 3 for x in xs)
    assert list(word for _ in range(3)) == [word] * 3
    assert list(plain(5)) == [0, 1, 2, 3, 4] and sum(plain(5)) == 10
    assert list(calls(4)) == [1, 2, 3, 4] and sum(calls(4)) == 10
    assert all(calls(4)) and not any(x > 4 for x in calls(4))
    assert list(x for x in [big, word, None, (1, 2)]) == [big, word, None, (1, 2)]
    assert sorted(x for x in (3, 1, 2)) == [1, 2, 3]
    assert max(x for x in xs) == 7 and min(f(x) for x in xs) == 1


before = sys.getrefcount(word), sys.getrefcount(big)
# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    check(i)
assert (sys.getrefcount(word), sys.getrefcount(big)) == before
# A consumer that stops early leaves the generator suspended where it was.
g = (x for x in range(10))
assert any(x == 3 for x in g) and next(g) == 4
print("ok")
