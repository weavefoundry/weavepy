"""Ecosystem probe: greenlet — the bundled native implementation
(RFC 0066 WS4). Exercises symmetric switching with value plumbing,
exception and GreenletExit propagation, parent chains, gr_frame,
contextvars isolation, and the dist-info visibility contract that
lets dependents (SQLAlchemy asyncio) resolve greenlet as installed."""

import contextvars

import greenlet
from greenlet import GreenletExit, getcurrent
from greenlet import greenlet as glet

main = getcurrent()

# Producer/consumer ping-pong through symmetric switches.
log = []


def producer():
    for i in range(3):
        log.append(("produced", i))
        consumer_g.switch(i)
    consumer_g.switch(None)


def consumer():
    while True:
        item = producer_g.switch()
        if item is None:
            break
        log.append(("consumed", item))
    main.switch("done")


producer_g = glet(producer)
consumer_g = glet(consumer)
assert consumer_g.switch() == "done"
assert log == [
    ("produced", 0), ("consumed", 0),
    ("produced", 1), ("consumed", 1),
    ("produced", 2), ("consumed", 2),
], log

# Exception propagation to the parent; GreenletExit becomes a value.
def boom():
    raise KeyError("k")

g = glet(boom)
try:
    g.switch()
    raise AssertionError("KeyError should propagate")
except KeyError:
    pass

def parked():
    main.switch("parked")

g2 = glet(parked)
assert g2.switch() == "parked"
assert isinstance(g2.throw(), GreenletExit)
assert g2.dead

# gr_frame on a suspended greenlet.
g3 = glet(parked)
g3.switch()
assert g3.gr_frame.f_code.co_name == "parked"
g3.switch()

# contextvars isolation (greenlet >= 1.0 semantics).
cv = contextvars.ContextVar("cv", default="fresh")
cv.set("main")
g4 = glet(lambda: main.switch(cv.get()))
assert g4.switch() == "fresh"
g4.switch()

# Deep recursion on the dedicated stack.
def deep(n):
    return "bottom" if n == 0 else deep(n - 1)

assert glet(lambda: deep(900)).switch() == "bottom"

# The bundled distribution is a real installed distribution.
from importlib.metadata import version
assert version("greenlet") == greenlet.__version__

print("greenlet ok", greenlet.__version__)
