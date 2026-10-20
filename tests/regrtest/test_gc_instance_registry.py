"""Instances that record their own collector slot: registration, death, and reuse."""
import gc
import weakref

previous_enabled = gc.isenabled()
gc.disable()
gc.collect()


class Plain:
    pass


class Node:
    def __init__(self, tag):
        self.tag = tag
        self.other = None


finalized = []


class Finalized:
    def __init__(self, tag):
        self.tag = tag
        self.me = self

    def __del__(self):
        finalized.append(self.tag)


# A plain instance answers as tracked, and keeps answering through churn
# that frees and reuses the slots around it.
keep = Plain()
assert gc.is_tracked(keep)
for _ in range(3):
    churn = [Node(i) for i in range(500)]
    for node in churn:
        node.other = churn
    del churn
    assert gc.is_tracked(keep)
assert gc.collect() >= 500
assert any(o is keep for o in gc.get_objects())


# Instances that die by reference count leave the registry at once: a
# collection afterwards finds nothing.
def make_and_drop(n):
    for i in range(n):
        a = Node(i)
        a.other = [a.tag]
        del a


make_and_drop(1000)
assert gc.collect() == 0


# Self-cycles are reclaimed, counted, and their weakref callbacks run.
callbacks = []
refs = []
for i in range(200):
    n = Node(i)
    n.other = n
    refs.append(weakref.ref(n, lambda r: callbacks.append(1)))
del n
collected = gc.collect()
assert collected >= 200, collected
assert all(r() is None for r in refs)
assert len(callbacks) == 200, len(callbacks)


# Finalizers run once; a cycle whose __del__ ran is reclaimed by a later
# collection, and the count only covers objects actually reclaimed.
for i in range(50):
    Finalized(i)
gc.collect()
assert sorted(finalized) == list(range(50)), finalized
assert gc.collect() == 0
assert sorted(finalized) == list(range(50))


# Resurrection: a finalizer that stores its object keeps it alive and
# registered, and never runs again.
saved = []


class Phoenix:
    def __init__(self):
        self.me = self

    def __del__(self):
        saved.append(self)


Phoenix()
gc.collect()
assert len(saved) == 1
phoenix = saved.pop()
assert gc.is_tracked(phoenix)
del phoenix
gc.collect()
assert saved == []


# An older instance that dies while a younger cycle is being cleared.
class Holder:
    pass


old = Holder()
gc.collect()
gc.collect()
young = Node("young")
young.other = young
young.tag = old
del old
del young
assert gc.collect() >= 1


# Frozen instances survive collections and rejoin generation 0 on unfreeze,
# including ones that died while frozen.
frozen_cycle = Node("frozen")
frozen_cycle.other = frozen_cycle
transient = Node("transient")
wr = weakref.ref(frozen_cycle)
gc.freeze()
del transient
del frozen_cycle
gc.collect()
assert wr() is not None
gc.unfreeze()
gc.collect()
assert wr() is None


# gc.get_referrers sees an instance holding a list.
target = []
holder = Node("holder")
holder.other = target
assert any(r is holder or (isinstance(r, dict) and r.get("other") is target)
           for r in gc.get_referrers(target))

if previous_enabled:
    gc.enable()
print("instance registry: ok")
