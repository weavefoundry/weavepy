"""`weakref.ref(x, callback)` and `KeyedRef` built in compiled code.

`WeakValueDictionary` makes a `KeyedRef` (a `ref` subclass with a key)
for every item. A ref with a callback is made in place like one without;
its callback runs when the referent dies, a callback-less ref is still
shared, and a subclass's ref keeps its key and callback. Run it past the
compile threshold.
"""

import weakref


class N:
    pass


fired = []


def make(n):
    return weakref.ref(n, fired.append)


def keyed(n, i):
    return weakref.KeyedRef(n, fired.append, i)


for i in range(3000):
    del fired[:]
    n = N()
    r = make(n)
    assert r() is n and r.__callback__ == fired.append
    assert weakref.ref(n) is weakref.ref(n) and weakref.ref(n) is not r
    k = keyed(n, i)
    assert k() is n and k.key == i and type(k) is weakref.KeyedRef
    del n
    assert r() is None and k() is None
    assert fired == [r, k] or fired == [k, r], fired

wd = weakref.WeakValueDictionary()
nodes = [N() for _ in range(50)]
for j, n in enumerate(nodes):
    wd[j] = n
assert len(wd) == 50
del nodes[:25]
assert len(wd) == 25
