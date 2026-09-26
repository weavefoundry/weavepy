"""Replacing a subclass dictionary must not confuse native field detection."""
import gc
import weakref

class Target:
    pass

class Ref(weakref.ref):
    __slots__ = ('__callback__', '__weakref_get__', '__dict__')

target = Target()
events = []
plain = weakref.ref(target)
callback = lambda ref: events.append(ref)
ref = Ref(target, callback)
ref.__callback__ = 23
ref.__weakref_get__ = len
# On WeavePy this permanently clears the inline-values flag. The user slots
# still must not be interpreted as the two native weakref fields.
ref.__dict__ = ref.__dict__
ref.label = 'retained'
assert ref() is plain() is target
assert ref.__callback__ == 23
assert ref.__weakref_get__ is len
assert weakref.ref.__callback__.__get__(ref, Ref) is callback
saved = ref.__call__
del target
gc.collect()
assert saved() is ref() is plain() is None
assert events == [ref]
assert ref.label == 'retained'
assert ref.__callback__ == 23
assert weakref.ref.__callback__.__get__(ref, Ref) is None
print('replaced subclass dictionary preserves weakref native state separation')
