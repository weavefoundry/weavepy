"""RFC 0066 WS4 — the native greenlet semantics matrix.

Pins the upstream-greenlet behaviours the bundled native `_greenlet`
implements over real stack switching (upstream ``greenlet/greenlet.c``
is the spec):

* ``switch()`` value plumbing: single value, tuple, kwargs-dict, and
  the ``(args, kwargs)`` pairing rules;
* return-to-parent and exception-to-parent propagation, including
  through a parent *chain*;
* ``GreenletExit`` does not propagate — it becomes the parent's switch
  value; ``throw()`` defaults to ``GreenletExit``;
* throwing into a never-started greenlet kills it silently;
* settable ``parent`` with cycle rejection;
* thread-boundness (``greenlet.error`` on cross-thread switch);
* ``getcurrent()``, ``gr_frame``, ``gr_context``, ``__bool__``/``dead``;
* GC of unstarted and *suspended* greenlets (the latter gets
  ``GreenletExit`` thrown in on its own stack);
* exception-state and contextvars isolation across switches;
* deep recursion on a greenlet's dedicated stack;
* the bundled dist-info: ``importlib.metadata`` sees greenlet installed.
"""

import contextvars
import sys
import threading

import greenlet
from greenlet import GreenletExit, getcurrent
from greenlet import greenlet as glet

main = getcurrent()

# ---------------------------------------------------------------------------
# Identity of the main greenlet.
# ---------------------------------------------------------------------------
assert getcurrent() is main
assert main.parent is None
assert bool(main)
assert not main.dead

# ---------------------------------------------------------------------------
# switch() value plumbing (upstream single/tuple/dict rules).
# ---------------------------------------------------------------------------
def echo():
    got = main.switch()            # no args -> ()
    got2 = main.switch(got)        # single positional -> that value
    got3 = main.switch(got2)
    main.switch(got3)

g = glet(echo)
assert g.switch() == ()            # child immediately parked: first switch in
assert g.switch(1) == 1            # single value passes through unchanged
assert g.switch(1, 2) == (1, 2)    # multiple positionals -> tuple
assert g.switch(a=1) == {"a": 1}   # kwargs only -> dict
# args + kwargs -> (args, kwargs) pair
def pairer():
    v = main.switch()
    main.switch(v)
g2 = glet(pairer)
g2.switch()
assert g2.switch(1, a=2) == ((1,), {"a": 2})

# ---------------------------------------------------------------------------
# Return value and exception both land in the parent.
# ---------------------------------------------------------------------------
g3 = glet(lambda: "finished")
assert g3.switch() == "finished"
assert g3.dead
assert not bool(g3)

g4 = glet(lambda: (_ for _ in ()).throw(ValueError("boom")))
try:
    g4.switch()
except ValueError as e:
    assert str(e) == "boom"
else:
    raise AssertionError("ValueError should propagate to the parent")
assert g4.dead

# Parent chain: a greenlet's uncaught exception unwinds to *its* parent.
order = []
def grandchild():
    order.append("gc-run")
    raise KeyError("gk")
def child():
    inner = glet(grandchild)  # parent = child
    try:
        inner.switch()
    except KeyError:
        order.append("child-caught")
    return "child-done"
c = glet(child)
assert c.switch() == "child-done"
assert order == ["gc-run", "child-caught"]

# ---------------------------------------------------------------------------
# GreenletExit: throw() default, non-propagation, silent kill of unstarted.
# ---------------------------------------------------------------------------
def parked():
    main.switch("parked")
    return "unreached"

g5 = glet(parked)
assert g5.switch() == "parked"
res = g5.throw()                       # defaults to GreenletExit
assert isinstance(res, GreenletExit)   # returned to parent, not raised
assert g5.dead

g6 = glet(parked)
res = g6.throw()                       # never started: dies silently
assert isinstance(res, GreenletExit)
assert g6.dead

# Raising GreenletExit *inside* the run is also a normal death.
def exits():
    raise GreenletExit("bye")
g7 = glet(exits)
res = g7.switch()
assert isinstance(res, GreenletExit) and res.args == ("bye",)

# throw() with an explicit exception type raises in the parent.
g8 = glet(parked)
g8.switch()
try:
    g8.throw(KeyError, "k")
except KeyError:
    pass
else:
    raise AssertionError("throw(KeyError) should raise in the parent")

# ---------------------------------------------------------------------------
# Settable parent, cycle rejection.
# ---------------------------------------------------------------------------
hub = glet(lambda: None)
worker = glet(parked)
worker.parent = hub
assert worker.parent is hub
try:
    # worker's parent is hub, so making hub's parent worker is a cycle.
    hub.parent = worker
    raise AssertionError("parent cycle should be rejected")
except ValueError:
    pass
try:
    main.parent = hub
    raise AssertionError("setting the main greenlet's parent should fail")
except (AttributeError, ValueError):
    pass

# ---------------------------------------------------------------------------
# Thread-boundness: switching a foreign thread's greenlet raises error.
# ---------------------------------------------------------------------------
box = {}
def other_thread():
    their_main = getcurrent()
    box["g"] = glet(lambda: their_main.switch())
    box["g"].switch()  # start it; it parks by switching to that thread's main
t = threading.Thread(target=other_thread)
t.start()
t.join()
try:
    box["g"].switch()
except greenlet.error:
    pass
else:
    raise AssertionError("cross-thread switch must raise greenlet.error")

# ---------------------------------------------------------------------------
# gr_frame: a suspended greenlet exposes its parked top frame.
# ---------------------------------------------------------------------------
def with_frames():
    def innermost():
        main.switch("suspended")
    innermost()
g9 = glet(with_frames)
assert g9.switch() == "suspended"
f = g9.gr_frame
assert f is not None and f.f_code.co_name == "innermost"
assert main.gr_frame is None        # running greenlet: None
g9.switch()  # let it finish
assert g9.gr_frame is None          # dead: None

# ---------------------------------------------------------------------------
# Exception-state isolation across switches.
# ---------------------------------------------------------------------------
def excstate():
    try:
        raise RuntimeError("inner")
    except RuntimeError:
        main.switch(sys.exc_info()[0])
        # back in the greenlet: our handled-exception context is restored
        main.switch(sys.exc_info()[0])

g10 = glet(excstate)
try:
    raise OSError("outer")
except OSError:
    assert g10.switch() is RuntimeError
    assert sys.exc_info()[0] is OSError   # ours untouched
    assert g10.switch() is RuntimeError   # theirs restored

# ---------------------------------------------------------------------------
# contextvars: each greenlet starts in a fresh implicit context
# (greenlet >= 1.0 semantics), gr_context is inspectable while parked.
# ---------------------------------------------------------------------------
cv = contextvars.ContextVar("cv", default="default")
cv.set("main-value")

def ctxprobe():
    seen = cv.get()
    cv.set("greenlet-value")
    main.switch(seen)
    main.switch(cv.get())

g11 = glet(ctxprobe)
assert g11.switch() == "default"          # not "main-value": fresh context
ctx = g11.gr_context
assert ctx is not None and ctx[cv] == "greenlet-value"
assert cv.get() == "main-value"           # ours untouched
assert g11.switch() == "greenlet-value"   # theirs restored

# ---------------------------------------------------------------------------
# GC: collecting a *suspended* greenlet throws GreenletExit into it on
# its own stack; collecting an unstarted one just drops it.
# ---------------------------------------------------------------------------
import gc

cleanup = []
def gc_victim():
    try:
        main.switch("gc-parked")
    except GreenletExit:
        cleanup.append("greenlet-exit")
        raise

gv = glet(gc_victim)
assert gv.switch() == "gc-parked"
del gv
gc.collect()
assert cleanup == ["greenlet-exit"], cleanup

unstarted = glet(parked)
del unstarted
gc.collect()  # must not crash or run the body

# ---------------------------------------------------------------------------
# Deep recursion on the greenlet's dedicated stack.
# ---------------------------------------------------------------------------
def deep(n):
    if n == 0:
        return "bottom"
    return deep(n - 1)

g12 = glet(lambda: deep(500))
assert g12.switch() == "bottom"

# ---------------------------------------------------------------------------
# Subclass with `run` as a method; switch into a dead greenlet goes to
# its parent (here: main) and returns ().
# ---------------------------------------------------------------------------
class Sub(glet):
    def run(self, *args):
        self.ran = args
        return "sub-done"

s = Sub()
assert s.switch(1, 2) == "sub-done"
assert s.ran == (1, 2)
assert s.dead
assert s.switch() == ()  # dead target: delegates to parent (main)

# ---------------------------------------------------------------------------
# The bundled distribution is visible to importlib.metadata.
# ---------------------------------------------------------------------------
from importlib.metadata import version
assert version("greenlet") == greenlet.__version__

print("greenlet native semantics matrix ok")
