import weakref
import gc


class Box:
    def __init__(self, value):
        self.value = value


b = Box(42)
r = weakref.ref(b)
print("alive:", r() is b)
print("value:", r().value)

released = []
def on_die(_):
    released.append("died")

b2 = Box("two")
r2 = weakref.ref(b2, on_die)
del b2
gc.collect()
print("after release:", r2())
print("callback fired:", released)


s = weakref.WeakSet([b])
print("in set:", b in s, len(s))
# A WeakSet only retains members while something else holds a strong
# reference, so adding a bare temporary makes len(s) depend on GC timing
# (the entry can be reclaimed before the read, especially after earlier
# fixtures churn the shared process heap). Pin it to keep size deterministic.
third = Box("three")
s.add(third)
print("size:", len(s))


d = weakref.WeakValueDictionary()
d["k"] = b
print("k:", d["k"] is b)
print("keys:", list(d.keys()))


# gc shims should never raise.
print("gc enabled:", gc.isenabled())
gc.disable()
print("disabled:", gc.isenabled())
gc.enable()
print("re-enabled:", gc.isenabled())
# `collect()` now traces real cycles; only assert that it
# returns an int (the exact count depends on what the
# import graph dragged in earlier).
print("collected is int:", isinstance(gc.collect(), int))
print("threshold:", gc.get_threshold())
gc.set_threshold(800, 12, 14)
print("threshold:", gc.get_threshold())
print("count len:", len(gc.get_count()))
print("objects is list:", isinstance(gc.get_objects(), list))
print("debug:", gc.get_debug())
gc.set_debug(gc.DEBUG_STATS)
print("debug:", gc.get_debug())
gc.set_debug(0)
