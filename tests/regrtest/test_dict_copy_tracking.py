"""`dict.copy()` of a dict whose values can and can't close a cycle.

The copy is deferred for the collector without a scan of its values; a
copy that then joins a cycle is still collected, and `gc.is_tracked`
answers as CPython's does.
"""

import gc
import weakref


class Node:
    pass


plain = {"k%d" % i: i for i in range(20)}
for _ in range(3000):
    c = plain.copy()
    assert c == plain and c is not plain
    assert gc.is_tracked(c) == gc.is_tracked(plain.copy())

n = Node()
holder = {"n": n}
c = holder.copy()
assert gc.is_tracked(c)
c["self"] = c
r = weakref.ref(n)
del n, holder, c
gc.collect()
assert r() is None
