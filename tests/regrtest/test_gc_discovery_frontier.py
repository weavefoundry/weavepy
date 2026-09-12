"""Keep discovered GC candidates alive through traversal and resurrection."""

import gc
import weakref


class Node:
    __slots__ = ("value", "edge", "__weakref__")

    def __init__(self, value):
        self.value = value


def wrap(value, kind, depth=96):
    for level in range(depth):
        if kind == "tuple" or kind == "mixed" and level % 2:
            value = (value,)
        else:
            value = iter((value,))
    return value


def unwrap(value, kind, depth=96):
    for level in reversed(range(depth)):
        if kind == "tuple" or kind == "mixed" and level % 2:
            value = value[0]
        else:
            value = next(value)
    return value


def make_cycle(kind, generation):
    nodes = [Node(i) for i in range(12)]
    for i in range(len(nodes)):
        nodes[i].edge = wrap(nodes[(i + 1) % len(nodes)], kind)
    references = [weakref.ref(node) for node in nodes]
    gc.collect(generation)
    for i in range(len(nodes)):
        assert unwrap(nodes[i].edge, kind) is nodes[(i + 1) % len(nodes)]
    # Reading the iterators consumes their edges. Rebuild the graph so the
    # next collection must discover the whole unrooted cycle from scratch.
    for i in range(len(nodes)):
        nodes[i].edge = wrap(nodes[(i + 1) % len(nodes)], kind)
    return references


resurrected = []
events = []


class RevivingNode(Node):
    __slots__ = ()

    def __del__(self):
        # The entire temporary tuple chain must remain intact when a
        # finalizer runs, and when the collector marks resurrected roots.
        assert unwrap(self.edge, "tuple") is self
        events.append(self.value)
        resurrected.append(self)


def make_reviving_cycle(value):
    node = RevivingNode(value)
    node.edge = wrap(node, "tuple")
    return weakref.ref(node)


was_enabled = gc.isenabled()
gc.disable()
try:
    for kind in ("tuple", "iterator", "mixed"):
        for generation in (0, 1, 2):
            references = make_cycle(kind, generation)
            # The live collection above may have promoted the anchors.
            gc.collect()
            assert all(reference() is None for reference in references), kind

    for value in range(3):
        reference = make_reviving_cycle(value)
        gc.collect()
        assert reference() is None
        assert events == list(range(value + 1)), events
        assert len(resurrected) == 1
        assert unwrap(resurrected[0].edge, "tuple") is resurrected[0]
        replacement = weakref.ref(resurrected[0])
        gc.collect()
        assert replacement() is resurrected[0]
        resurrected.clear()
        gc.collect()
        assert replacement() is None
        assert events == list(range(value + 1)), events
finally:
    if was_enabled:
        gc.enable()

print("collector discovery frontier: ok")
