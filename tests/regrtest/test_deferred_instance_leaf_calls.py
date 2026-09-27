"""Small calls preserve cleanup when instance tracking changes."""
import gc
import weakref


class Item:
    def __init__(self, left, right):
        self.left, self.right = left, right


def read(item):
    return item.left


def compare(a, b):
    return a.left < b.right


def add(item):
    return item.left + item.right


def exercise(a, b):
    for _ in range(4000):
        assert read(a) == 3
        assert compare(a, b) is True
        assert compare(a, a) is True
        assert add(a) == 8


a, b = Item(3, 5), Item(7, 11)
exercise(a, b)
# Explicit tracking queries, weakrefs, and non-atomic stores revoke deferral.
assert gc.is_tracked(a)
exercise(a, b)
events = []
refs = [weakref.ref(a, lambda ref: events.append('a')),
        weakref.ref(b, lambda ref: events.append('b'))]
exercise(a, b)
a.payload = [1, 2, 3]
b.peer = a
exercise(a, b)
del a
assert refs[0]() is not None
del b
gc.collect()
assert all(ref() is None for ref in refs)
assert sorted(events) == ['a', 'b'], events

# Tracked cycles still become collectible after the optimized calls.
a, b = Item(3, 5), Item(7, 11)
a.peer, b.peer = b, a
exercise(a, b)
refs = [weakref.ref(a), weakref.ref(b)]
del a, b
gc.collect()
assert all(ref() is None for ref in refs)

# Temporary owners with finalizers retain the ordinary cleanup path.
class Finalized(Item):
    def __del__(self):
        events.append(self.left)


a, b = Finalized(3, 5), Finalized(7, 11)
exercise(a, b)
events.clear()
del a, b
gc.collect()
assert sorted(events) == [3, 7], events
events.clear()
assert read(Finalized(13, 17)) == 13
gc.collect()
assert events == [13], events
print('Deferred instance leaf calls: ok')
