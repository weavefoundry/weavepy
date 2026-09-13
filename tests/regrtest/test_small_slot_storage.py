"""Small and promoted slot storage preserves ownership and Python state."""

import copy
import gc
import pickle
import threading
import weakref


for count in (1, 2, 8, 9, 16):
    names = tuple("field%d" % i for i in range(count))
    cls = type("Slots%d" % count, (), {"__slots__": names + ("__weakref__",)})
    globals()[cls.__name__] = cls
    item = cls()
    values = [None, False, -0.0, 2 ** 100, "α", [], {}, (1, 2)]
    for i, name in enumerate(names):
        setattr(item, name, values[i % len(values)])
    expected = {name: values[i % len(values)] for i, name in enumerate(names)}
    assert item.__getstate__() == (None, expected)
    shallow = copy.copy(item)
    for name in names:
        assert getattr(shallow, name) is getattr(item, name)
    for protocol in (2, pickle.HIGHEST_PROTOCOL):
        restored = pickle.loads(pickle.dumps(item, protocol))
        assert type(restored) is cls
        assert restored.__getstate__() == (None, expected)
    for name in names[::2]:
        delattr(item, name)
        try:
            getattr(item, name)
        except AttributeError:
            pass
        else:
            raise AssertionError("deleted slot is still visible")
    for i, name in enumerate(reversed(names[::2])):
        setattr(item, name, i)
        expected[name] = i
    assert item.__getstate__() == (None, expected)
    assert copy.deepcopy(item).__getstate__() == (None, expected)
    # Both vector and table storage must expose all cycle edges to GC.
    cycle = cls()
    for name in names:
        setattr(cycle, name, cycle)
    reference = weakref.ref(cycle)
    del cycle
    gc.collect()
    assert reference() is None


class Shared:
    __slots__ = tuple("field%d" % i for i in range(9))


shared = Shared()
for i in range(9):
    setattr(shared, "field%d" % i, i)
errors = []


def worker(index):
    name = "field%d" % index
    for value in range(200):
        setattr(shared, name, value)
        if getattr(shared, name) != value:
            errors.append((index, value))


threads = [threading.Thread(target=worker, args=(i,)) for i in range(4)]
for thread in threads:
    thread.start()
for thread in threads:
    thread.join()
assert errors == []
for i in range(4):
    assert getattr(shared, "field%d" % i) == 199
for i in range(4, 9):
    assert getattr(shared, "field%d" % i) == i
print("ok")
