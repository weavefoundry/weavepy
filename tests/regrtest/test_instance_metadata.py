"""Instance metadata preserves slots, state, and cached weak-reference hashes."""

import copy
import gc
import pickle
import threading
import weakref


class Single:
    __slots__ = ("value",)


class Multiple:
    __slots__ = ("first", "second", "third", "fourth", "__weakref__")


class Derived(Single):
    __slots__ = ("other",)


class WithDict(Single):
    pass


def missing(obj, name):
    try:
        getattr(obj, name)
    except AttributeError:
        return
    raise AssertionError("deleted slot remained visible")


single = Single()
missing(single, "value")
for value in (None, 0, -0.0, "text", [1, 2], 2 ** 100):
    single.value = value
    assert single.value is value
    assert single.__getstate__() == (None, {"value": value})
    del single.value
    missing(single, "value")
single.value = [1, 2]
assert copy.copy(single).value is single.value
assert copy.deepcopy(single).value == single.value
assert copy.deepcopy(single).value is not single.value

multiple = Multiple()
names = ("first", "second", "third", "fourth")
for i, name in enumerate(names):
    setattr(multiple, name, i)
for name in reversed(names):
    delattr(multiple, name)
    missing(multiple, name)
for i, name in enumerate(reversed(names)):
    setattr(multiple, name, i + 10)
for i, name in enumerate(reversed(names)):
    assert getattr(multiple, name) == i + 10

derived = Derived()
derived.value = "base"
derived.other = "child"
assert derived.__getstate__() == (None, {"value": "base", "other": "child"})
with_dict = WithDict()
with_dict.value = 4
with_dict.extra = 5
assert vars(with_dict) == {"extra": 5}
assert with_dict.__getstate__() == ({"extra": 5}, {"value": 4})

for obj in (single, multiple, derived, with_dict):
    for protocol in range(pickle.HIGHEST_PROTOCOL + 1):
        # Older protocols require an explicit __getstate__ on slotted classes.
        if protocol < 2:
            continue
        restored = pickle.loads(pickle.dumps(obj, protocol))
        assert type(restored) is type(obj)
        assert restored.__getstate__() == obj.__getstate__()

# Slot edges remain visible to the cycle collector.
cycle = Multiple()
cycle.first = cycle
reference = weakref.ref(cycle)
del cycle
gc.collect()
assert reference() is None


class HashOwner:
    __slots__ = ("value", "calls", "__weakref__")

    def __init__(self, value):
        self.value = value
        self.calls = 0

    def __hash__(self):
        self.calls += 1
        return self.value


def read_hash(reference, output):
    output.append(hash(reference))


for value in (0, 1, -1, -2, -(2 ** 63), 2 ** 63 - 1):
    owner = HashOwner(value)
    reference = weakref.ref(owner)
    published = []
    thread = threading.Thread(target=read_hash, args=(reference, published))
    thread.start()
    thread.join()
    assert len(published) == 1
    cached = published[0]
    assert owner.calls == 1
    assert hash(reference) == cached
    assert owner.calls == 1
    del owner
    gc.collect()
    assert reference() is None
    observed = []
    thread = threading.Thread(target=read_hash, args=(reference, observed))
    thread.start()
    thread.join()
    assert observed == [cached]

print("ok")
