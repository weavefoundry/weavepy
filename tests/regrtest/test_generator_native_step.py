"""Generators stepped by native callers, and the collector's young sets.

A native caller's step of a compiled generator body (`map(next, ...)`,
`enumerate`, `zip`, an asyncio task's step) enters the body's native code
straight from the resume; everything a body can do from there must look as
it does from a resume in the interpreter. Young instances, functions and
generators sit in sets by their own slot, without a weak handle: they must
be enumerated, tracked and collected exactly as registered ones are.

Each program runs in fresh interpreters with every tier on and with a low
frame-JIT threshold, and its output must be CPython's.
"""

import os
import subprocess
import sys
import unittest

STEP_PROGRAM = r'''
import gc, sys, weakref, asyncio, itertools

N = 3000

def plain(n):
    for i in range(n):
        yield len([i])

def calls(n):
    total = 0
    for i in range(n):
        total += helper(i)
        yield total

def helper(i):
    return abs(i) + 1

def catches(n):
    for i in range(n):
        try:
            if i % 7 == 0:
                raise ValueError(i)
            yield i
        except ValueError as e:
            yield -e.args[0]

def raises_at(n):
    for i in range(n):
        yield str(i)
    raise KeyError("done")

def returns(v):
    yield str(v)
    return v

def delegating(n):
    r = yield from plain(n)
    s = yield from returns(7)
    yield (r, s)

def frames():
    me = sys._getframe()
    yield me.f_code.co_name, me.f_lineno
    yield me.f_back is not None, me.f_lineno

def observed(g_box):
    g = g_box[0]
    yield g.gi_running, g.gi_frame is not None, g.gi_yieldfrom
    yield g.gi_code.co_name

def finalized(log):
    try:
        yield str(1)
        yield str(2)
    finally:
        log.append("finally")

def warm(fn, *args):
    for _ in range(600):
        for _ in fn(*args):
            pass

warm(plain, 3)
warm(calls, 3)
warm(catches, 3)
warm(returns, 1)
warm(delegating, 2)

# A native caller steps each of them.
print(sum(map(lambda x: x, list(map(next, [plain(5) for _ in range(N)])))))
print(list(enumerate(calls(4))))
print(list(zip(catches(8), plain(8))))
print(list(itertools.islice(calls(10), 3)))
print(list(map(lambda p: p, delegating(2))))

# A raise leaves through the native caller.
it = map(lambda x: x, raises_at(2))
print(next(it), next(it))
try:
    next(it)
except KeyError as e:
    print("KeyError", e.args, e.__traceback__.tb_next is not None)

# A return value.
g = returns(9)
print(next(map(lambda x: x, [next(g)])))
try:
    next(g)
except StopIteration as e:
    print("stop", e.value)

# The frame while running, from a native caller.
for _ in range(600):
    list(map(lambda p: p, frames()))
print(list(map(lambda p: p, frames())))

# gi_* from inside, stepped natively.
box = []
g = observed(box)
box.append(g)
for _ in range(2):
    print(next(map(next, [g])))

# close, throw and finalizers around native steps.
log = []
for _ in range(600):
    g = finalized(log)
    next(map(next, [g]))
    g.close()
print(len(log), log[0])
g = finalized(log)
print(next(map(next, [g])))
try:
    g.throw(ValueError("x"))
except ValueError as e:
    print("thrown", e.args)
print(g.gi_frame, g.gi_running)

# Tracing switched on between native steps.
events = []
def tracer(frame, event, arg):
    if frame.f_code.co_name == "traced":
        events.append(event)
    return tracer
def traced():
    x = str(1)
    yield x
    x = str(2)
    yield x
for _ in range(1200):
    list(map(lambda p: p, traced()))
g = traced()
print(next(map(next, [g])))
sys.settrace(tracer)
print(next(map(next, [g])))
sys.settrace(None)
print(events)

# Asyncio tasks stepping coroutines that suspend, raise, return and cancel.
async def leaf(i):
    await asyncio.sleep(0)
    if i % 5 == 4:
        raise ValueError(i)
    return i

async def node(n):
    out = []
    for i in range(n):
        try:
            out.append(await leaf(i))
        except ValueError as e:
            out.append(-e.args[0])
    return out

async def canceller():
    t = asyncio.ensure_future(asyncio.sleep(100))
    await asyncio.sleep(0)
    t.cancel()
    try:
        await t
    except asyncio.CancelledError:
        return "cancelled"

async def main():
    a = await asyncio.gather(*[node(6) for _ in range(300)])
    b = await canceller()
    return a[0], len(a), b

print(asyncio.run(main()))
'''

GC_PROGRAM = r'''
import gc, weakref, sys

gc.collect()

class C:
    pass

def gen():
    yield 1

def mk():
    return lambda: 0

# Young objects are tracked, enumerated and untracked by their own place.
keep = []
for _ in range(200):
    c = C()
    c.me = c
    keep.append(c)
print(all(gc.is_tracked(x) for x in keep))
ids = {id(o) for o in gc.get_objects()}
print(all(id(x) in ids for x in keep))
for x in keep:
    x.me = None
del x, c, keep

g = gen()
f = mk()
print(gc.is_tracked(g), gc.is_tracked(f))
ids = {id(o) for o in gc.get_objects()}
print(id(g) in ids, id(f) in ids)
next(g)
print(gc.is_tracked(g))
del g, f, ids

# Many births and deaths in any order leave the set whole.
live = []
for i in range(5000):
    o = C()
    o.x = []
    live.append(o)
    if i % 3 == 0:
        live.pop(i % len(live))
    if i % 7 == 0 and live:
        live.pop(0)
print(len(live), all(gc.is_tracked(o) for o in live))
print(gc.collect() >= 0, all(isinstance(o.x, list) for o in live))
del live, o

# Cycles of young objects, each kind, collected once found.
class D:
    def __del__(self):
        dead.append("del")
dead = []
d = D()
d.me = d
r = weakref.ref(d, lambda _: dead.append("cb"))
del d
n = gc.collect()
print(n >= 1, sorted(dead), r())

def self_ref():
    def inner():
        return inner
    return inner
w = weakref.ref(self_ref())
gc.collect()
print(w())

def gen_cycle():
    def g():
        yield me
    me = g()
    next(me)
    return weakref.ref(me)
w = gen_cycle()
gc.collect()
print(w())

# A cycle through a bound method nothing else holds.
class H:
    def m(self):
        pass
h = H()
h.cb = h.m
w = weakref.ref(h)
del h
print(gc.collect() >= 1, w())

# A weak reference on a bound method in a cycle: only the external one fires.
alist = []
class E:
    def cb(self, ignore):
        alist.append("cb called")
c, d = E(), E()
c.other = d
d.other = c
callback = c.cb
c.wr = weakref.ref(d, callback)
d.wr = weakref.ref(callback, d.cb)
external = weakref.ref(callback, lambda _: alist.append("safe"))
del callback, c, d, E
gc.collect()
print(alist, external())

# A closure cell held only by its function.
class F:
    pass
def closes():
    o = F()
    def fn():
        return o
    o.fn = fn
    return weakref.ref(o)
w = closes()
gc.collect()
print(w())

# The automatic pace: collections that find nothing wait longer, and an
# explicit collection or a new threshold starts over, so garbage made after
# a long idle stretch is still found by the automatic trigger.
gc.collect()
gc.set_threshold(100, 10, 10)
stay = []
for i in range(20000):
    o = C()
    o.v = []
    stay.append(o)
del stay, o
gc.collect()
w = []
for i in range(5000):
    o = C()
    o.me = o
    if i == 0:
        w.append(weakref.ref(o))
del o
print(w[0]())
gc.set_threshold(2000, 10, 10)
'''

STEP_EXPECTED = """3000
[(0, 1), (1, 3), (2, 6), (3, 10)]
[(0, 1), (1, 1), (2, 1), (3, 1), (4, 1), (5, 1), (6, 1), (-7, 1)]
[1, 3, 6]
[1, 1, '7', (None, 7)]
0 1
KeyError ('done',) True
9
stop 9
[('frames', 44), (True, 45)]
(True, True, None)
observed
600 finally
1
thrown ('x',)
None False
1
2
['call', 'line', 'line', 'return']
([0, 1, 2, 3, -4, 5], 300, 'cancelled')
"""
GC_EXPECTED = """True
True
True True
True True
True
2619 True
True True
True ['cb', 'del'] None
None
None
True None
['safe'] None
None
None
"""

SETTINGS = (
    {},
    {"WEAVEPY_FRAME_JIT_HOT": "20", "WEAVEPY_JIT_SYNC": "1"},
)


def run(program, env):
    return subprocess.run(
        [sys.executable, "-c", program],
        capture_output=True,
        text=True,
        env=dict(os.environ, **env),
        timeout=300,
    )


class NativeStepTests(unittest.TestCase):
    def test_steps_look_alike_under_every_tier(self):
        for env in SETTINGS:
            r = run(STEP_PROGRAM, env)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(r.stdout, STEP_EXPECTED)

    def test_young_sets_under_every_tier(self):
        for env in SETTINGS:
            r = run(GC_PROGRAM, env)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(r.stdout, GC_EXPECTED)


if __name__ == "__main__":
    unittest.main()
