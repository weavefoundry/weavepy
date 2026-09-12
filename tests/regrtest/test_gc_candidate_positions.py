"""Account for temporary snapshot buffers and repeated edges during collection."""

import gc
import weakref


class Node:
    __slots__ = ("tag", "parts", "__weakref__")

    def __init__(self, tag):
        self.tag = tag

    def neighbor(self):
        return self.parts[0]


def connect(nodes):
    for index, node in enumerate(nodes):
        following = nodes[(index + 1) % len(nodes)]
        node.parts = (
            following,
            iter(frozenset((following,))),
            {index: following}.values(),
            iter({index: following}.items()),
            slice(following, None, following),
            node.neighbor,
            (following, following),
        )


def check_live(nodes):
    for index, node in enumerate(nodes):
        following = nodes[(index + 1) % len(nodes)]
        parts = node.parts
        assert parts[0] is following
        assert next(parts[1]) is following
        assert list(parts[2]) == [following]
        assert next(parts[3]) == (index, following)
        assert parts[4].start is following and parts[4].step is following
        assert parts[5]() is following
        assert parts[6][0] is following and parts[6][1] is following


def make_cycle(count, generation):
    nodes = [Node(index) for index in range(count)]
    references = [weakref.ref(node) for node in nodes]
    connect(nodes)
    gc.collect(generation)
    check_live(nodes)
    # Restore consumed iterators so all temporary edges remain in the
    # unreachable graph. No Python root retains their backing containers.
    connect(nodes)
    return references


enabled = gc.isenabled()
gc.disable()
try:
    for generation in (0, 1, 2):
        for count in (1, 17, 257):
            references = make_cycle(count, generation)
            gc.collect(2)
            assert all(reference() is None for reference in references), (generation, count)
finally:
    if enabled:
        gc.enable()

print("collector temporary positions: ok")
