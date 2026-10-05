"""Context variables (PEP 567) over the native `contextvars` bodies.

`ContextVar.get`/`set`/`reset`, `Context.run`/`copy` and `copy_context`
are native, and a context's mapping is shared copy-on-write between a
context and its copies. These checks pin the observable semantics:
copies are independent snapshots, `run` switches the current context and
restores it (also on error), a `set` inside `run` changes that context,
tokens enforce their error taxonomy, threads start empty, and asyncio
tasks and callbacks run in the context they captured.
"""

import asyncio
import contextvars
import gc
import threading

v = contextvars.ContextVar("v", default=0)
w = contextvars.ContextVar("w")


# Copies are snapshots: writes after the copy don't leak either way.
t1 = v.set(1)
snap = contextvars.copy_context()
v.set(2)
assert snap[v] == 1 and v.get() == 2
assert snap.run(v.get) == 1


def write_inside():
    v.set(3)
    w.set("x")
    return v.get(), w.get()


assert snap.run(write_inside) == (3, "x")
assert snap[v] == 3 and snap[w] == "x"
assert v.get() == 2 and w.get("none") == "none"
snap2 = snap.copy()
snap.run(v.set, 4)
assert snap[v] == 4 and snap2[v] == 3
assert len(snap2) == 2 and set(snap2) == {v, w}
assert dict(snap2.items()) == {v: 3, w: "x"}
assert snap2 == snap2.copy() and snap2 != snap


# `run` restores the previous context even when the callable raises,
# and passes positional and keyword arguments through.
def boom():
    v.set(99)
    raise KeyError("k")


try:
    snap.run(boom)
except KeyError:
    pass
else:
    raise AssertionError("expected KeyError")
assert v.get() == 2 and snap[v] == 99
assert snap.run(lambda a, b=0: (a, b), 1, b=2) == (1, 2)

# A context can't be entered twice.
try:
    snap.run(snap.run, v.get)
except RuntimeError as e:
    assert "is already entered" in str(e), e
else:
    raise AssertionError("expected RuntimeError")


# Token semantics.
v.reset(t1)
assert v.get() == 0
try:
    v.reset(t1)
except RuntimeError as e:
    assert "has already been used once" in str(e), e
else:
    raise AssertionError("expected RuntimeError")
tw = w.set(1)
try:
    v.reset(tw)
except ValueError as e:
    assert "different ContextVar" in str(e), e
else:
    raise AssertionError("expected ValueError")
tv = v.set(5)
try:
    contextvars.Context().run(v.reset, tv)
except ValueError as e:
    assert "different Context" in str(e), e
else:
    raise AssertionError("expected ValueError")
assert tv.var is v and tv.old_value is contextvars.Token.MISSING
assert w.set(2).old_value == 1
assert contextvars.ContextVar("fresh").set(1).old_value is contextvars.Token.MISSING
with v.set(6):
    assert v.get() == 6
assert v.get() == 5
try:
    contextvars.Token()
except RuntimeError:
    pass
else:
    raise AssertionError("Token() should be refused")
try:
    contextvars.ContextVar("u").get()
except LookupError:
    pass
else:
    raise AssertionError("expected LookupError")
try:
    v.get(1, 2)
except TypeError:
    pass
else:
    raise AssertionError("expected TypeError")


# A new thread starts with an empty context, and its writes stay there.
seen = []


def in_thread():
    seen.append((v.get(), w.get(None)))
    v.set(100)


th = threading.Thread(target=in_thread)
th.start()
th.join()
assert seen == [(0, None)], seen
assert v.get() == 5


# Contexts that reference themselves are collectable.
c = contextvars.Context()
c.run(w.set, c)
del c
gc.collect()


# asyncio: each task runs in a copy of its creator's context, and
# callbacks run in the context they were scheduled with.
async def child(n, start=5):
    assert v.get() == start
    v.set(n)
    await asyncio.sleep(0)
    assert v.get() == n
    return n


async def main():
    results = await asyncio.gather(*(child(i) for i in range(10)))
    assert results == list(range(10))
    assert v.get() == 5
    loop = asyncio.get_running_loop()
    got = []
    ctx = contextvars.copy_context()
    ctx.run(v.set, 7)
    loop.call_soon(lambda: got.append(v.get()), context=ctx)
    loop.call_soon(lambda: got.append(v.get()))
    await asyncio.sleep(0)
    assert got == [7, 5], got
    t = asyncio.create_task(child(3, 7), context=ctx.copy())
    assert await t == 3
    assert t.get_context()[v] == 3 and ctx[v] == 7


asyncio.run(main())


# A re-imported `contextvars` defines fresh classes; contexts and
# variables made from the first import (and `threading`, which runs every
# thread body through `_contextvars.Context().run`) keep working.
import sys

old_ctx = contextvars.copy_context()
del sys.modules["contextvars"]
import contextvars as fresh  # noqa: E402

assert old_ctx.run(v.get) == 5
fw = fresh.ContextVar("fw")
fw.set(1)
assert fresh.copy_context().run(fw.get) == 1
assert old_ctx.run(fw.get, None) is None
with v.set(8):
    assert v.get() == 8
seen.clear()
th = threading.Thread(target=in_thread)
th.start()
th.join()
assert seen == [(0, None)], seen
print("ok")
