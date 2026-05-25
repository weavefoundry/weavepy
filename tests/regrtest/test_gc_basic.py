"""RFC 0024 — `gc` module surface.

Exercises the public surface of the `gc` module without making any
strong claim about the underlying collector's heuristics. These
assertions only check that the API is shaped right and returns the
expected types — actual cycle collection is covered separately by
the run-fixtures harness.
"""

import gc


# ---------------------------------------------------------------------------
# enable / disable / isenabled.
# ---------------------------------------------------------------------------

was_enabled = gc.isenabled()
gc.disable()
assert gc.isenabled() is False
gc.enable()
assert gc.isenabled() is True

if not was_enabled:
    gc.disable()
    gc.enable()


# ---------------------------------------------------------------------------
# Threshold + counters.
# ---------------------------------------------------------------------------

t = gc.get_threshold()
assert isinstance(t, tuple)
assert len(t) == 3
gc.set_threshold(10, 5, 2)
assert gc.get_threshold() == (10, 5, 2)
gc.set_threshold(700, 10, 10)

c = gc.get_count()
assert isinstance(c, tuple)
assert len(c) == 3


# ---------------------------------------------------------------------------
# collect / get_objects / get_referrers / get_referents / is_tracked.
# ---------------------------------------------------------------------------

assert isinstance(gc.collect(), int)
assert isinstance(gc.collect(0), int)

assert isinstance(gc.get_objects(), list)
assert isinstance(gc.get_referrers([1]), list)
assert isinstance(gc.get_referents([1]), list)


class Cycle:
    def __init__(self):
        self.self = self


cyc = Cycle()
# RFC 0024 auto-tracks every fresh user-defined instance, so
# `is_tracked` should return True for cycles.
assert gc.is_tracked(cyc) is True
del cyc
gc.collect()


# ---------------------------------------------------------------------------
# debug / freeze / stats / constants / rooted lists.
# ---------------------------------------------------------------------------

prev_debug = gc.get_debug()
gc.set_debug(0)
assert gc.get_debug() == 0
gc.set_debug(prev_debug)

assert isinstance(gc.get_freeze_count(), int)
gc.freeze()
gc.unfreeze()

stats = gc.get_stats()
assert isinstance(stats, list)
assert all(isinstance(s, dict) for s in stats)

assert hasattr(gc, "DEBUG_STATS")
assert hasattr(gc, "DEBUG_COLLECTABLE")
assert hasattr(gc, "DEBUG_UNCOLLECTABLE")
assert hasattr(gc, "DEBUG_SAVEALL")
assert hasattr(gc, "DEBUG_LEAK")

assert isinstance(gc.garbage, list)
assert isinstance(gc.callbacks, list)


print("gc surface ok")
