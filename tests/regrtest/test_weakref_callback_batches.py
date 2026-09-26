"""Mixed weakref batches clear every reference before newest-first callbacks."""
import gc
import weakref


class Target:
    pass


def exercise(cyclic, with_callbacks):
    target = Target()
    if cyclic:
        target.edge = target
    references = []
    events = []

    def record(reference, index):
        assert all(item() is None for item in references)
        assert reference is references[index]
        events.append(index)
        # Clearing and collection can reenter through a queued callback.
        nested = Target()
        nested_ref = weakref.ref(nested)
        del nested
        gc.collect()
        assert nested_ref() is None

    for index in range(12):
        callback = (lambda reference, index=index: record(reference, index)
                    ) if with_callbacks and index % 3 else None
        references.append(weakref.ref(target, callback))
    cancelled = weakref.ref(target, lambda reference: events.append('cancelled'))
    del cancelled, callback
    assert {id(reference) for reference in weakref.getweakrefs(target)} == {
        id(reference) for reference in references
    }
    del target
    gc.collect()
    expected = [index for index in reversed(range(12))
                if with_callbacks and index % 3]
    assert events == expected, events
    assert all(reference() is None for reference in references)
    # CPython retains callback attributes after cyclic collection. Check
    # callback-attribute cleanup on the acyclic path shared by both runtimes.
    if not cyclic:
        assert all(reference.__callback__ is None for reference in references)
    gc.collect()
    assert events == expected


for unused in range(8):
    for cyclic in (False, True):
        for with_callbacks in (False, True):
            exercise(cyclic, with_callbacks)
print('weakref callback batches preserve clearing and callback order')
