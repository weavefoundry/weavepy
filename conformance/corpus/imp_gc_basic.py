"""RFC 0024 — `gc` module surface."""

import gc

print(gc.isenabled())
gc.disable()
print(gc.isenabled())
gc.enable()

t = gc.get_threshold()
print(len(t))

c = gc.get_count()
print(len(c))

print(isinstance(gc.collect(), int))

print(isinstance(gc.get_objects(), list))
print(isinstance(gc.get_referrers([1]), list))
print(isinstance(gc.get_referents([1]), list))

gc.set_debug(0)
print(gc.get_debug())

print(gc.get_freeze_count())
gc.freeze()
gc.unfreeze()
print(gc.get_freeze_count())

print(gc.DEBUG_STATS)
print(isinstance(gc.garbage, list))
print(isinstance(gc.callbacks, list))
