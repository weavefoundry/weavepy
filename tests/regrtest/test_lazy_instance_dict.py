"""Dictionary materialization preserves lookup, ownership, and shared state."""
import copy
import gc
import pickle
import threading
import types
import weakref


class Plain:
    value = 17

    def read(self):
        return self.value


class EmptySlots:
    __slots__ = ()

    def read(self):
        return 23


class Slots:
    __slots__ = ("value", "__weakref__")


class Mixed(Slots):
    pass


def missing(instance, name):
    try:
        getattr(instance, name)
    except AttributeError:
        return
    raise AssertionError((type(instance).__name__, name))


plain = Plain()
empty = EmptySlots()
slotted = Slots()
for i in range(1000):
    assert plain.read() == 17
    assert empty.read() == 23
    missing(slotted, "value")
    missing(slotted, "__dict__")
    missing(empty, "__dict__")

namespace = vars(plain)
assert namespace is plain.__dict__
assert namespace == {}
namespace["value"] = 29
for i in range(1000):
    assert plain.read() == 29
del plain.value
assert plain.read() == 17
assert namespace is vars(plain)
assert namespace == {}
plain.extra = [1, 2]
assert namespace["extra"] is plain.extra
independent = Plain()
assert vars(independent) is not namespace
assert vars(independent) == {}

# A dictionary exported before the owner dies remains a valid independent
# Python object. Its values may keep other objects alive.
payload = Plain()
reference = weakref.ref(payload)
plain.payload = payload
del payload, plain
gc.collect()
assert reference() is namespace["payload"]
del namespace["payload"]
gc.collect()
assert reference() is None

mixed = Mixed()
mixed.value = "slot"
mixed.other = "dict"
assert vars(mixed) == {"other": "dict"}
assert mixed.__getstate__() == ({"other": "dict"}, {"value": "slot"})
for obj in (independent, empty, mixed):
    shallow = copy.copy(obj)
    assert type(shallow) is type(obj)
    assert shallow.__getstate__() == obj.__getstate__()
    for protocol in (2, 4, pickle.HIGHEST_PROTOCOL):
        restored = pickle.loads(pickle.dumps(obj, protocol))
        assert type(restored) is type(obj)
        assert restored.__getstate__() == obj.__getstate__()


class Number(int):
    pass


class Namespace(types.SimpleNamespace):
    pass


number = Number(31)
assert number + 2 == 33
number.extra = "number"
assert vars(number) == {"extra": "number"}
wrapped_namespace = Namespace(one=1)
assert vars(wrapped_namespace) == {"one": 1}
vars(wrapped_namespace)["two"] = 2
assert wrapped_namespace.two == 2

# Class reassignment between compatible layouts must not replace the dict.
class OtherPlain:
    pass


before = vars(independent)
independent.__class__ = OtherPlain
assert vars(independent) is before
independent.changed = 42
assert before == {"changed": 42}

# Concurrent first exports and writes must all share the one published dict.
shared = Plain()
barrier = threading.Barrier(4)
namespaces = [None] * 4
errors = []


def worker(index):
    try:
        barrier.wait(timeout=30)
        current = vars(shared)
        current["worker%d" % index] = index
        namespaces[index] = current
    except BaseException as error:
        errors.append(repr(error))


threads = [threading.Thread(target=worker, args=(i,)) for i in range(4)]
for thread in threads:
    thread.start()
for thread in threads:
    thread.join(timeout=30)
assert not errors, errors
assert all(not thread.is_alive() for thread in threads)
assert all(current is vars(shared) for current in namespaces)
assert vars(shared) == {"worker%d" % i: i for i in range(4)}

# GC must traverse populated dicts and slots while tolerating cold instances.
cycle = Plain()
cycle.self = cycle
reference = weakref.ref(cycle)
del cycle
gc.collect()
assert reference() is None
cycle = Slots()
cycle.value = cycle
reference = weakref.ref(cycle)
del cycle
gc.collect()
assert reference() is None

events = []
class Finalized:
    value = 19
    def __del__(self):
        events.append(self.value)

finalized = Finalized()
del finalized
gc.collect()
assert events == [19]

print("lazy instance dictionary semantics: ok")
