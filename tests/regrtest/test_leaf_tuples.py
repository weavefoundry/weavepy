"""Frameless leaves that build tuples (`return (x, 1)`, a sort key's
`(-x, x)`), called from compiled code and from native callers (`map`,
`sorted`, `min`), return the same tuples CPython does: fresh ones, holding
what they were given (big ints, strings, nested tuples, other leaves'
results), with no reference left over."""
import sys


def pair(x):
    return (x, 1)


def key(x):
    return (-x, x)


def nest(a, b):
    return (b, a, (a, b))


def wrap(x):
    return (pair(x), key(x))


def single(x):
    return (x,)


big = 1 << 80
word = "w" * 40


def check(i):
    assert list(map(pair, [1, 2])) == [(1, 1), (2, 1)]
    assert sorted([3, 1, 2], key=key) == [3, 2, 1]
    assert min([5, 9, 7], key=key) == 9
    assert nest(1, 2) == (2, 1, (1, 2)) and nest(word, big) == (big, word, (word, big))
    assert wrap(3) == ((3, 1), (-3, 3))
    assert single(big) == (big,) and type(single(1)) is tuple
    t = pair(word)
    assert t[0] is word and pair(word) is not t


before = sys.getrefcount(word)
# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    check(i)
assert sys.getrefcount(word) == before, (sys.getrefcount(word), before)
assert sorted(range(3000), key=lambda x: (x % 7, -x))[:3] == [2996, 2989, 2982]
print("ok")
