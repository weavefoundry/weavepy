"""User-defined weakref slots must not alias the native wrapper state."""
import copy
import gc
import weakref
class Owner:
    pass
class SlotRef(weakref.ref):
    __slots__ = ('__callback__', '__weakref_get__')
owner = Owner()
events = []
def callback(ref):
    events.append(ref)
ref = SlotRef(owner, callback)
ref.__callback__ = 23
ref.__weakref_get__ = len
assert ref.__callback__ == 23
assert ref.__weakref_get__ is len
assert ref() is owner
plain = weakref.ref(owner)
assert copy.copy(plain) is plain
assert copy.deepcopy(plain) is plain
assert weakref.ref.__callback__.__get__(ref, SlotRef) is callback
saved = ref.__call__
del owner
gc.collect()
assert ref() is saved() is None
assert events == [ref]
assert ref.__callback__ == 23
assert weakref.ref.__callback__.__get__(ref, SlotRef) is None
print('Subclass slots remain separate from internal weakref state')
