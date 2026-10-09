"""Generators the collector holds as young (not yet registered) still
answer gc.is_tracked, are found in cycles, and are finalized promptly."""

import gc
import weakref


def counter(n):
    for i in range(n):
        yield i


g = counter(3)
assert gc.is_tracked(g)
assert list(g) == [0, 1, 2]

# Many short-lived generators, then one that closes a cycle through its
# own frame: the collector must still find and free it.
for _ in range(10000):
    assert sum(counter(4)) == 6


def selfish():
    me = yield
    yield me


cyc = selfish()
next(cyc)
assert cyc.send(cyc) is cyc
ref = weakref.ref(cyc)
del cyc
gc.collect()
assert ref() is None, "generator cycle survived a collection"

# A suspended generator dropped without a cycle runs its finally block
# right away.
log = []


def cleanup():
    try:
        yield 1
    finally:
        log.append("closed")


c = cleanup()
next(c)
del c
assert log == ["closed"], log


# Coroutines take the same path.
async def coro():
    return 5


co = coro()
assert gc.is_tracked(co)
try:
    co.send(None)
except StopIteration as e:
    assert e.value == 5
print("ok")
