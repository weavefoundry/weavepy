"""RFC 0039 WS4 — cycle-GC heuristics + deterministic finalization.

Pins the CPython-faithful collector behaviour this wave landed, all driven by
*explicit* ``gc.collect()`` so there is no collector-timing flakiness:

* the older-generation refcount cascade — clearing young cyclic garbage that
  held the last reference to an object pinned in an older generation reaps that
  object in the same pass and fires its weakref callback (``test_gc``'s
  ``test_bug1055820c``);
* ``gc.callbacks`` fire ``("start", info)`` / ``("stop", info)`` in
  registration order around each collection;
* generational ``get_threshold``/``set_threshold`` round-trip;
* PEP 442 finalization: ``__del__`` on cyclic garbage runs exactly once;
* ``weakref.finalize`` and a plain weakref callback fire on collection.
"""

import gc
import weakref


# ---------------------------------------------------------------------------
# Older-generation refcount cascade (bug1055820c, explicit-collect variant).
# c0 is moved to the oldest generation, then orphaned by clearing the young
# cyclic object c1 that referenced it. Collecting must reap c0 too and fire
# its weakref callback, which materialises c2 (already cleared) as None.
# ---------------------------------------------------------------------------

class C:
    def __init__(self, i):
        self.i = i
        self.loop = self  # self-cycle ⇒ cyclic garbage


c0 = C(0)
gc.collect()  # promote c0 toward the oldest generation

c1 = C(1)
c1.keep_c0_alive = c0
del c0.loop  # now only c1 keeps c0 alive

c2 = C(2)
c2wr = weakref.ref(c2)  # no callback

ouch = []


def callback(ignored):
    ouch[:] = [c2wr()]


c0wr = weakref.ref(c0, callback)
c0 = c1 = c2 = None

n = gc.collect()
assert len(ouch) == 1, "older-generation weakref callback did not fire (%r)" % (ouch,)
assert ouch[0] is None, "callback resurrected a cleared object: %r" % (ouch[0],)
assert c0wr() is None, "orphaned old-generation object was not reaped"


# ---------------------------------------------------------------------------
# gc.callbacks fire ("start"|"stop", info) in registration order.
# ---------------------------------------------------------------------------

events = []


def cb1(phase, info):
    events.append(("cb1", phase))


def cb2(phase, info):
    events.append(("cb2", phase))


gc.callbacks.append(cb1)
gc.callbacks.append(cb2)
try:
    events.clear()
    gc.collect()
finally:
    gc.callbacks.remove(cb1)
    gc.callbacks.remove(cb2)

assert ("cb1", "start") in events and ("cb1", "stop") in events, events
assert ("cb2", "start") in events and ("cb2", "stop") in events, events
# Registration order within a phase: cb1 precedes cb2.
assert events.index(("cb1", "start")) < events.index(("cb2", "start")), events
assert events.index(("cb1", "stop")) < events.index(("cb2", "stop")), events
# Each phase precedes its stop.
assert events.index(("cb1", "start")) < events.index(("cb1", "stop")), events


# ---------------------------------------------------------------------------
# Generational thresholds round-trip (CPython default is (700, 10, 10)).
# ---------------------------------------------------------------------------

saved = gc.get_threshold()
try:
    gc.set_threshold(700, 10, 10)
    assert gc.get_threshold() == (700, 10, 10)
    gc.set_threshold(123, 4, 5)
    assert gc.get_threshold() == (123, 4, 5)
finally:
    gc.set_threshold(*saved)


# ---------------------------------------------------------------------------
# PEP 442: __del__ on cyclic garbage runs exactly once.
# ---------------------------------------------------------------------------

dellog = []


class WithDel:
    def __del__(self):
        dellog.append(1)


d = WithDel()
d.loop = d  # cyclic, so only the collector can reclaim it
del d
gc.collect()
assert dellog == [1], "cyclic __del__ should run exactly once, got %r" % (dellog,)


# ---------------------------------------------------------------------------
# weakref.finalize and a plain weakref callback both fire on collection.
# ---------------------------------------------------------------------------

finlog = []


class E:
    def __init__(self):
        self.loop = self


e = E()
weakref.finalize(e, lambda: finlog.append("finalize"))
ewr = weakref.ref(e, lambda ref: finlog.append("callback"))
del e
gc.collect()
assert "finalize" in finlog, "weakref.finalize did not run: %r" % (finlog,)
assert "callback" in finlog, "weakref callback did not run: %r" % (finlog,)
assert ewr() is None


print("WS4 cycle-GC cascade + finalization ok")
