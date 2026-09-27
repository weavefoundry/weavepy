"""Retain, promote, and collect populations with observable lifetime checks."""
import gc
import os
import time
import weakref

KIND = os.environ.get('WEAVEPY_INSTANCE_KIND', 'gc-cycle')
if KIND not in ('gc-cycle', 'gc-finalizer', 'gc-weakref', 'gc-callback', 'gc-frozen'):
    raise ValueError('unknown WEAVEPY_INSTANCE_KIND: ' + KIND)

FINALIZED = 0
CALLBACKS = 0


class Node:
    __slots__ = ('value', 'edge', '__weakref__')

    def __init__(self, value):
        self.value = value
        self.edge = None


class FinalNode(Node):
    __slots__ = ()

    def __del__(self):
        global FINALIZED
        FINALIZED += 1


def on_collect(reference):
    global CALLBACKS
    CALLBACKS += 1


FACTORY = FinalNode if KIND == 'gc-finalizer' else Node
CALLBACK = on_collect if KIND == 'gc-callback' else None
CYCLIC = KIND in ('gc-cycle', 'gc-frozen')


def bench(n):
    global FINALIZED, CALLBACKS
    FINALIZED = CALLBACKS = 0
    values = []
    for i in range(n):
        value = FACTORY(i)
        if CYCLIC:
            value.edge = value
        values.append(value)
    if n:
        del value
    references = [weakref.ref(value, CALLBACK) for value in values]
    # Exercise candidate promotion and position updates with live owners.
    gc.collect(0)
    gc.collect(1)
    if n:
        for i in (0, n // 2, n - 1):
            assert references[i]() is values[i]
            assert values[i].value == i
            if CYCLIC:
                assert values[i].edge is values[i]
    assert FINALIZED == CALLBACKS == 0
    if KIND == 'gc-frozen':
        gc.freeze()
        try:
            del values
            gc.collect(2)
            assert all(reference() is not None for reference in references)
        finally:
            gc.unfreeze()
    else:
        del values
    gc.collect(2)
    assert all(reference() is None for reference in references)
    assert FINALIZED == (n if KIND == 'gc-finalizer' else 0), FINALIZED
    assert CALLBACKS == (n if KIND == 'gc-callback' else 0), CALLBACKS
    del references
    return n


if __name__ == '__main__':
    start = time.perf_counter_ns()
    bench(int(os.environ.get('WEAVEPY_BENCH_WORK', '10000')))
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns() - start))
