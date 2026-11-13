"""A tuple's last release gives up its items' references exactly once:
items other owners keep stay alive, an item the tuple owns alone (once or
twice over) is finalized with it, a weakly referenced or deeply nested
tuple still dies, and its items' finalizers run in CPython's order."""

import gc
import sys
import weakref


class Tracked:
    live = 0
    log = []

    def __init__(self, name):
        self.name = name
        Tracked.live += 1

    def __del__(self):
        Tracked.live -= 1
        Tracked.log.append(self.name)


def shared_items(n):
    keep = [Tracked("k%d" % i) for i in range(3)]
    before = [sys.getrefcount(k) for k in keep]
    for i in range(n):
        t = (keep[0], i, keep[1], None, keep[2], 1.5, "s")
        u = (t, t)
        del t, u
    assert [sys.getrefcount(k) for k in keep] == before
    assert Tracked.live == 3
    del keep


def owned_items(n):
    for i in range(n):
        t = (Tracked("a"), i, Tracked("b"))
        del t
        assert Tracked.live == 0, Tracked.live
    for i in range(n):
        x = Tracked("twice")
        t = (i, x, x, [x])
        del x
        assert Tracked.live == 1
        del t
        assert Tracked.live == 0, Tracked.live


def mixed_items(n):
    keep = Tracked("kept")
    for i in range(n):
        t = (keep, Tracked("own"), keep, (keep, Tracked("inner")))
        del t
        assert Tracked.live == 1, Tracked.live
    del keep


def nested(depth):
    t = ()
    for _ in range(depth):
        t = (t, None)
    del t
    t = (Tracked("deep"),)
    for _ in range(depth):
        t = (t,)
    del t
    assert Tracked.live == 0


def varargs(*args):
    return len(args)


def calls(n):
    keep = Tracked("arg")
    total = 0
    for i in range(n):
        total += varargs(keep, i, keep) + varargs(*(keep, Tracked("t")))
    assert Tracked.live == 1
    del keep
    return total


for _ in range(3):
    Tracked.log.clear()
    shared_items(3000)
    assert Tracked.live == 0
    owned_items(2000)
    mixed_items(2000)
    nested(50000)
    assert calls(3000) == 5 * 3000
    gc.collect()
    assert Tracked.live == 0, Tracked.live

# Finalizers of a dying tuple's own items run last to first.
Tracked.log.clear()
t = (Tracked("first"), 0, Tracked("second"), Tracked("third"))
del t
assert Tracked.log == ["third", "second", "first"], Tracked.log


class Node:
    pass


n = Node()
r = weakref.ref(n)
t = (n, 1)
del n
assert r() is t[0]
del t
assert r() is None
