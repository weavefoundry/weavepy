"""Keep cyclic roots, frozen objects, weak references, and finalizers intact."""
import gc
import weakref

previous_enabled = gc.isenabled()
previous_thresholds = gc.get_threshold()
gc.disable()
gc.collect(2)
finalized = []


class Node:
    __slots__ = ('other', 'tag', '__weakref__')

    def __init__(self, tag):
        self.tag = tag

    def __del__(self):
        finalized.append(self.tag)


def ring(first, count):
    nodes = [Node(first + i) for i in range(count)]
    for i, node in enumerate(nodes):
        node.other = nodes[(i + 1) % count]
    return nodes


def rooted_cycles():
    roots = ring(0, 48)
    references = [weakref.ref(node) for node in roots]
    for generation in [0, 1, 2, 2, 0, 2]:
        gc.collect(generation)
        assert all(reference() is root for reference, root in zip(references, roots))
        assert roots[-1].other is roots[0]
        assert not finalized
    return references


references = rooted_cycles()
gc.collect(2)
assert all(reference() is None for reference in references)
assert sorted(finalized) == list(range(48)), finalized
finalized.clear()


def frozen_cycles():
    roots = ring(100, 48)
    references = [weakref.ref(node) for node in roots]
    gc.freeze()
    assert gc.get_freeze_count() >= 48
    return references


references = frozen_cycles()
for generation in [0, 1, 2]:
    gc.collect(generation)
    assert all(reference() is not None for reference in references)
    assert not finalized
gc.unfreeze()
assert gc.get_freeze_count() == 0
gc.collect(2)
assert all(reference() is None for reference in references)
assert sorted(finalized) == list(range(100, 148)), finalized
for generation in [0, 1, 2, 2]:
    gc.collect(generation)
assert sorted(finalized) == list(range(100, 148)), finalized

gc.set_threshold(*previous_thresholds)
if previous_enabled:
    gc.enable()
print('collector metadata lifecycle: ok')
