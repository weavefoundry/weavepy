"""Tuples made and freed in loops, wherever they die (an exception's
`args`, a `*args` tuple, a dict's key, a tuple inside a container): a
tuple in a reused allocation hashes and compares as its new items, a dying
tuple releases its items last first (as CPython's does), and a deeply
nested chain frees without exhausting the stack."""

import gc
import sys


class Noisy:
    log = []

    def __init__(self, name):
        self.name = name

    def __del__(self):
        Noisy.log.append(self.name)


# A reused allocation forgets the hash it had.
for n in range(1, 20):
    t = tuple(range(n))
    h = hash(t)
    del t
    u = tuple(range(100, 100 + n))
    assert hash(u) == hash(tuple(range(100, 100 + n))), n
    assert u == tuple(range(100, 100 + n)), n
    assert hash(u) != h or n == 0

# Keys built and dropped in a loop still find their entries.
d = {}
for i in range(2000):
    d[(i, str(i))] = i
for i in range(2000):
    assert d[(i, str(i))] == i
    assert (i, str(i + 1)) not in d

# Items die when their tuple does, last first.
Noisy.log.clear()
t = (Noisy("a"), Noisy("b"), Noisy("c"))
del t
assert Noisy.log == ["c", "b", "a"], Noisy.log


# `*args` tuples and exception arguments come and go.
def spread(*args):
    return args


total = 0
for i in range(5000):
    a = spread(i, i + 1, i + 2)
    total += a[2]
    try:
        raise ValueError(i, "x")
    except ValueError as e:
        total += e.args[0]
assert total == sum(i + 2 for i in range(5000)) + sum(range(5000)), total

# Tuples that held containers, and tuples in cycles, are collected.
for i in range(200):
    lst = [i]
    t = (lst, i)
    lst.append(t)
    del lst, t
gc.collect()

# A long chain of nested tuples frees in bounded stack.
chain = ()
for i in range(200000):
    chain = (chain, i)
del chain

# Empty tuples stay the one interned object.
EMPTY = ()
assert tuple() is EMPTY
assert tuple([]) is EMPTY

# Long tuples (past the free list's lengths) still work.
big = tuple(range(1000))
assert sum(big) == 499500
del big

print("ok")
