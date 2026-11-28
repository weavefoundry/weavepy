"""Tuple and list repetition in compiled code.

`argparse`'s metavar formatter returns `(result,) * tuple_size`. Compiled
code repeats a short tuple or list in place; a long result, a list's
in-place `*=`, and `t * 1` (the same tuple) keep the full handler's
behavior. Items are heap objects, so a wrong count shows. Run it past
the compile threshold.
"""

import sys


class Item:
    pass


def repeat(seq, n):
    return seq * n


def rrepeat(seq, n):
    return n * seq


def irepeat(seq, n):
    seq *= n
    return seq


item = Item()
base = sys.getrefcount(item)
for _ in range(3000):
    t = (item, "a")
    assert repeat(t, 3) == (item, "a") * 3
    assert rrepeat(t, 2) == (item, "a", item, "a")
    assert repeat(t, 1) is t and rrepeat(t, 1) is t
    assert repeat(t, 0) == () and repeat(t, -2) == ()
    assert irepeat(t, 2) == (item, "a", item, "a")
    l = [item]
    r = repeat(l, 2)
    assert r == [item, item] and r is not l and l == [item]
    assert rrepeat(l, 3) == [item] * 3
    m = irepeat(l, 2)
    assert m is l and l == [item, item]
    assert len(repeat((0,), 5000)) == 5000
    assert len(repeat([None], 5000)) == 5000
    del t, l, r, m
assert sys.getrefcount(item) == base, (sys.getrefcount(item), base)

try:
    repeat((1,), sys.maxsize)
except MemoryError:
    pass
else:
    raise AssertionError("no MemoryError")
