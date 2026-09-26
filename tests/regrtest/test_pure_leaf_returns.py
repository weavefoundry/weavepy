"""Tiny return bodies retain lookup, call, and observation semantics."""

import gc
import sys
import weakref


def identity(value):
    return value


def constant():
    return ("kept", 123)


def small_integer():
    return 7


def get_value(obj):
    return obj.value


class Record:
    def __init__(self, value):
        self.value = value

    def get(self):
        return self.value


class Other:
    def __init__(self, value):
        self.padding = None
        self.value = value


class Slotted:
    __slots__ = ("value",)


def check(obj, expected):
    for _ in range(200):
        assert get_value(obj) is expected


value = Record(None)
record = Record(value)
for _ in range(200):
    assert identity(value) is value
    assert constant() is constant()
    assert small_integer() == 7
    assert record.get() is value
check(record, value)
check(Other(value), value)
slot = Slotted()
slot.value = value
check(slot, value)

# Reorder and delete dictionary entries after a site has become hot.
del record.value
record.padding = 1
record.value = value
check(record, value)
del record.value
try:
    get_value(record)
except AttributeError:
    pass
else:
    raise AssertionError("a removed attribute was cached")
record.value = value

# A class mutation must replace the cached instance-dictionary lookup.
calls = []


def descriptor(self):
    calls.append(self)
    return "descriptor"


Record.value = property(descriptor)
check(record, "descriptor")
assert len(calls) == 200
del Record.value
check(record, value)
calls.clear()


class Custom:
    def __getattribute__(self, name):
        assert name == "value"
        return value


check(Custom(), value)

# Return ownership must survive the receiver, without retaining it.
owner = Record(Record(None))
ref = weakref.ref(owner)
result = get_value(owner)
del owner
gc.collect()
assert ref() is None
assert isinstance(result, Record)


def defaulted(arg=value):
    return arg


for _ in range(200):
    assert defaulted() is value
replacement = Record(None)
defaulted.__defaults__ = (replacement,)
for _ in range(200):
    assert defaulted() is replacement
    assert defaulted(arg=value) is value


def changed(obj):
    return "changed"


get_value.__code__ = changed.__code__
assert get_value(record) == "changed"

# An observer installed after warmup still sees the real Python call.
events = []


def trace(frame, event, arg):
    if frame.f_code is identity.__code__:
        events.append(event)
    return trace


sys.settrace(trace)
try:
    assert identity(value) is value
finally:
    sys.settrace(None)
assert "call" in events and "return" in events and "line" in events, events
print("pure leaf returns: ok")
