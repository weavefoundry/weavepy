"""RFC 0024 — real cycle GC."""

import gc


# Default state.
print("enabled:", gc.isenabled())

# Toggle.
gc.disable()
print("after disable:", gc.isenabled())
gc.enable()
print("after enable:", gc.isenabled())


# Thresholds.
gc.set_threshold(800, 12, 14)
print("threshold:", gc.get_threshold())
gc.set_threshold(700, 10, 10)
print("default threshold:", gc.get_threshold())


# Counts (per-generation tracker).
counts = gc.get_count()
print("count tuple length:", len(counts))


# Tracking — RFC 0024 auto-tracks every fresh user instance.
class Box:
    pass


b = Box()
gc.collect()  # warm up
print("box tracked initially:", gc.is_tracked(b))


# Cycle reclamation: a → b → a (instance cycle).
class Node:
    pass


a = Node()
n = Node()
a.next = n
n.next = a
del a
del n
recovered = gc.collect()
print("cycle collect returned int:", isinstance(recovered, int))


# Cycle creation: a self-referential dict — should be reclaimable.
d = {}
d["self"] = d
del d
collected = gc.collect()
print("collect returned int:", isinstance(collected, int))


# Debug flags.
print("debug:", gc.get_debug())
gc.set_debug(gc.DEBUG_STATS)
print("debug after set:", gc.get_debug() == gc.DEBUG_STATS)
gc.set_debug(0)


# get_objects returns a list.
objs = gc.get_objects()
print("get_objects is list:", isinstance(objs, list))


# get_referrers / get_referents don't crash.
referents = gc.get_referents([1, 2, 3])
print("get_referents is list:", isinstance(referents, list))
referrers = gc.get_referrers([1, 2, 3])
print("get_referrers is list:", isinstance(referrers, list))


# freeze / unfreeze.
print("freeze count:", gc.get_freeze_count())
gc.freeze()
gc.unfreeze()
print("freeze count after unfreeze:", gc.get_freeze_count())


# get_stats returns a list of dicts.
stats = gc.get_stats()
print("stats list:", isinstance(stats, list))
print("stats has 3 entries:", len(stats) == 3)


# Constants.
print("DEBUG_STATS:", gc.DEBUG_STATS)
print("DEBUG_LEAK:", gc.DEBUG_LEAK)


# garbage / callbacks lists exist.
print("garbage is list:", isinstance(gc.garbage, list))
print("callbacks is list:", isinstance(gc.callbacks, list))
