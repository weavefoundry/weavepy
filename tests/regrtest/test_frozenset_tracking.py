"""Collector tracking of frozensets.

A frozenset of atomic values can't be part of a cycle; `gc.is_tracked`
still answers `True` for every frozenset, as CPython's does. A frozenset
holding an object that can close a cycle is tracked from birth, and the
cycle through it is collected.
"""

import gc
import weakref


class Node:
    def __hash__(self):
        return id(self)


def build(n):
    out = []
    for i in range(n):
        out.append(frozenset((i, "a", None, 2.5, b"x")))
    return out


for fs in build(3000)[:3]:
    assert gc.is_tracked(fs)
assert gc.is_tracked(frozenset())
assert gc.is_tracked(frozenset([(1, 2)]))

collected = []
for _ in range(200):
    node = Node()
    node.ref = frozenset([node])
    weakref.finalize(node, collected.append, 1)
    assert gc.is_tracked(node.ref)
    del node
gc.collect()
assert len(collected) == 200, len(collected)
assert frozenset((1, 2)) | {3} == {1, 2, 3}
print("ok")
