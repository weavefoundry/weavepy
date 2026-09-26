"""Compiled constructor stores preserve dictionary and callback semantics."""

import gc
import math
import sys
import weakref


class Record:
    def __init__(self, value):
        # Math calls keep this a compiled constructor body rather than the
        # simpler constructor shortcut that copies arguments into fields.
        self.measurement_value = math.sin(value)
        self.other_value = math.cos(value)


def build(n):
    records = [None] * n
    for i in range(n):
        records[i] = Record(i)
    return records


records = build(1500)
for i, record in enumerate(records):
    assert record.measurement_value == math.sin(i)
    assert record.other_value == math.cos(i)
    assert list(vars(record)) == ["measurement_value", "other_value"]
    assert next(iter(vars(record))) is sys.intern("measurement_value")

# Re-enter the same compiled body with existing keys, reversed insertion
# order, or a replaced dictionary. Keep the exported dictionary's identity.
record = records[-1]
record.__dict__ = {"other_value": -2.0, "measurement_value": -1.0, "extra": 17}
namespace = vars(record)
record.__init__(0)
assert vars(record) is namespace
assert list(namespace) == ["other_value", "measurement_value", "extra"]
assert namespace == {"other_value": 1.0, "measurement_value": 0.0, "extra": 17}
del record.measurement_value
record.__init__(1)
assert list(namespace) == ["other_value", "extra", "measurement_value"]

# Replacing a last reference must still run its finalizer and weakref
# callback. The finalizer observes the new value, as a normal store does.
events = []


class Finalized:
    def __del__(self):
        events.append(("del", record.measurement_value))


value = Finalized()
ref = weakref.ref(value, lambda _: events.append(("weak", record.measurement_value)))
record.measurement_value = value
del value
record.__init__(0)
assert events == [("del", 0.0), ("weak", 0.0)], events
assert ref() is None

# A dictionary can contain non-string keys equal to attribute names. Native
# probes must resume before equality invokes Python.
events.clear()


class Name:
    def __hash__(self):
        return hash("measurement_value")

    def __eq__(self, other):
        events.append(other)
        return other == "measurement_value"


key = Name()
namespace = {key: -1.0}
record.__dict__ = namespace
namespace = vars(record)
record.__init__(0)
assert events == ["measurement_value"], events
assert list(namespace)[0] is key
assert list(namespace.values()) == [0.0, 1.0]


# A data descriptor installed after compilation invalidates the class guard.
record.__dict__ = {}
events.clear()


class Descriptor:
    def __set__(self, instance, value):
        events.append((instance, value))


Record.measurement_value = Descriptor()
record.__init__(0)
assert events == [(record, 0.0)]
assert vars(record) == {"other_value": 1.0}
del Record.measurement_value
record.__init__(1)
assert record.measurement_value == math.sin(1)

del records, record, namespace
gc.collect()
print("JIT attribute insertion: ok")
