"""Collecting an `itertools.islice` over a list or tuple at once.

`list(islice(...))`, `tuple(...)` and their kin take the slice in one
pass when the islice holds its sequence's iterator alone, in CPython's
order (the skipped items first, then the stop bound), and leave it
finished with its source released; a shared iterator, a partly consumed
islice, and other sources take the general path.
"""

import gc
import weakref
from itertools import islice

lst = list(range(20))
tup = tuple(lst)
for seq in (lst, tup):
    assert list(islice(seq, 5)) == lst[:5]
    assert list(islice(seq, 3, 8)) == lst[3:8]
    assert list(islice(seq, 1, 15, 4)) == lst[1:15:4]
    assert list(islice(seq, 2, None, 3)) == lst[2::3]
    assert list(islice(seq, 18, 40)) == [18, 19]
    assert list(islice(seq, 25, 30)) == []
    assert list(islice(seq, 0)) == [] and list(islice(seq, 4, 4)) == []
    assert tuple(islice(seq, None)) == tup
    s = islice(seq, 2, 9, 2)
    assert next(s) == 2
    assert list(s) == [4, 6, 8] and list(s) == [] and next(s, "end") == "end"
    s = islice(seq, 3)
    assert list(s) == [0, 1, 2] and list(s) == []

# A shared source iterator advances as CPython's does.
it = iter(lst)
assert list(islice(it, 3, 6)) == [3, 4, 5]
assert next(it) == 6
it = iter(tup)
assert list(islice(it, 2, 4)) == [2, 3] and list(it) == lst[4:]
it = iter([1, 2, 3])
assert list(islice(it, 5, 9)) == [] and list(it) == []

# Other sources.
assert list(islice((x * 2 for x in range(10)), 1, 6, 2)) == [2, 6, 10]
assert list(islice("abcdef", 1, None, 2)) == ["b", "d", "f"]
assert list(islice(range(10), 3)) == [0, 1, 2]


# The finished islice releases its source.
class Box(list):
    pass


b = Box([1, 2, 3])
r = weakref.ref(b)
s = islice(b, 1, None)
del b
assert list(s) == [2, 3]
gc.collect()
assert r() is None


def loop(n):
    for i in range(n):
        assert list(islice(lst, i % 7, 12, 2)) == lst[i % 7:12:2]


loop(3000)
print("ok")
