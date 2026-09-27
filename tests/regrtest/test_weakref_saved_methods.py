"""Exact weakref methods preserve their wrapper and callback owners."""
import gc
import threading
import weakref


class Node:
    pass


def explicit_new(owner, callback):
    return weakref.ref.__new__(weakref.ref, owner, callback)


def saved_owner(method_name, construct=weakref.ref):
    events = []
    owner = Node()

    def callback(reference):
        assert reference() is None
        events.append('cleared')

    reference = construct(owner, callback)
    method = getattr(reference, method_name)
    del reference
    gc.collect()
    if method_name == '__call__':
        assert method() is owner
    else:
        assert 'dead' not in method()
    assert events == []
    del owner
    gc.collect()
    assert events == ['cleared'], (method_name, events)
    if method_name == '__call__':
        assert method() is None
    else:
        assert 'dead' in method()
    assert method.__self__.__callback__ is None
    del method
    gc.collect()


for construct in (weakref.ref, explicit_new):
    for name in ('__call__', '__repr__'):
        saved_owner(name, construct)


# Explicit methods bind the same receiver and agree with implicit operations.
owner = Node()
reference = weakref.ref(owner)
assert weakref.ref(owner) is reference
original_hash = hash(reference)
for _ in range(20):
    call = reference.__call__
    representation = reference.__repr__
    assert call.__self__ is representation.__self__ is reference
    assert call.__name__ == '__call__'
    assert representation.__name__ == '__repr__'
    assert call() is reference() is weakref.ref.__call__(reference) is owner
    assert representation() == repr(reference) == weakref.ref.__repr__(reference)
    assert "to 'Node'" in representation()
del owner
gc.collect()
assert reference() is call() is None
assert representation() == repr(reference) and 'dead' in representation()
assert hash(reference) == original_hash
del reference, call, representation
gc.collect()


# A retained method owns the callback even while the referent remains alive.
for method_name in ('__call__', '__repr__'):
    finalized = []
    events = []

    class Callback:
        def __call__(self, reference):
            events.append('called')

        def __del__(self):
            finalized.append('released')

    owner = Node()
    callback = Callback()
    witness = weakref.ref(callback)
    reference = weakref.ref(owner, callback)
    method = getattr(reference, method_name)
    del callback, reference
    gc.collect()
    assert witness() is not None and finalized == []
    del method
    gc.collect()
    assert witness() is None and finalized == ['released']
    assert events == []
    del owner
    gc.collect()
    assert events == []


# Saving the bound method on its referent must not turn the weak edge strong.
for method_name in ('__call__', '__repr__'):
    owner = Node()
    witness = weakref.ref(owner)
    reference = weakref.ref(owner, lambda reference: None)
    owner.saved = getattr(reference, method_name)
    del owner, reference
    gc.collect()
    assert witness() is None


# Shared type methods also work when an existing wrapper crosses a thread.
owner = Node()
events = []
reference = weakref.ref(owner, lambda reference: events.append('cleared'))
methods = (reference.__call__, reference.__repr__)
del reference
errors = []


def worker(alive):
    try:
        call, representation = methods
        assert call.__self__ is representation.__self__
        if alive:
            assert call() is owner
            assert 'dead' not in representation()
        else:
            assert call() is None
            assert 'dead' in representation()
        for name in ('__call__', '__repr__'):
            saved_owner(name)
    except BaseException as error:
        errors.append(error)


thread = threading.Thread(target=worker, args=(True,))
thread.start()
thread.join()
assert not errors, errors
del owner
gc.collect()
assert events == ['cleared']
thread = threading.Thread(target=worker, args=(False,))
thread.start()
thread.join()
assert not errors, errors
del methods, thread
gc.collect()
print('Saved exact weakref methods preserve binding, callbacks, and lifetimes')
