"""Exact dynamic modules use native pickle; lookup hooks remain observable."""

import pickle
import sys
import types


def observed(function):
    calls = []

    def profile(frame, event, arg):
        filename = frame.f_code.co_filename.replace("\\", "/")
        if event == "call" and (filename.endswith("/pickle.py") or filename == "pickle.py"):
            calls.append(frame.f_code.co_name)

    sys.setprofile(profile)
    try:
        result = function()
    finally:
        sys.setprofile(None)
    return result, calls


module = types.ModuleType("pickle_dynamic_fixture")
sys.modules[module.__name__] = module
exec("""
class Record:
    pass

class Outer:
    class Inner:
        __slots__ = ('value',)
""", module.__dict__)
record_class = module.Record
record = record_class()
record.a = ["shared", 1]
record.b = record.a
inner = module.Outer.Inner()
inner.value = record

# CPython 3.14's C-pickler bytes, including the shared child's memo index.
expected = (
    b'\x80\x04\x95F\x00\x00\x00\x00\x00\x00\x00'
    b'\x8c\x16pickle_dynamic_fixture\x94\x8c\x06Record\x94\x93\x94)\x81\x94}'
    b'\x94(\x8c\x01a\x94]\x94(\x8c\x06shared\x94K\x01e\x8c\x01b\x94h\x06ub.'
)
for protocol in (4, 5):
    blob, calls = observed(lambda: pickle.dumps(record, protocol=protocol))
    assert calls == [], calls[:10]
    assert blob == expected[:1] + bytes([protocol]) + expected[2:]
    back, calls = observed(lambda: pickle.loads(blob))
    assert calls == [], calls[:10]
    assert type(back) is record_class
    assert back.a is back.b and back.a == ["shared", 1]
    nested_blob, calls = observed(lambda: pickle.dumps(inner, protocol=protocol))
    assert calls == [], calls[:10]
    nested, calls = observed(lambda: pickle.loads(nested_blob))
    assert calls == [], calls[:10]
    assert type(nested) is module.Outer.Inner
    assert type(nested.value) is record_class
    assert nested.value.a is nested.value.b

events = []


class Watched(types.ModuleType):
    def __getattribute__(self, name):
        if name == "Record":
            events.append(name)
        return super().__getattribute__(name)


module.__class__ = Watched
pickle.dumps(record)
assert events
events.clear()
assert type(pickle.loads(blob)) is record_class
assert events
module.__class__ = types.ModuleType

# Missing globals can invoke a module-level hook, even on an exact module.
del module.Record
events.clear()


def missing(name):
    events.append(name)
    if name == "Record":
        return record_class
    raise AttributeError(name)


module.__getattr__ = missing
pickle.dumps(record)
assert "Record" in events
events.clear()
assert type(pickle.loads(blob)) is record_class
assert "Record" in events
del module.__getattr__
module.Record = record_class

# A rebound global must fail encoding and affect subsequent decoding.
class Replacement:
    pass


module.Record = Replacement
try:
    pickle.dumps(record)
except pickle.PicklingError:
    pass
else:
    raise AssertionError("a rebound class was accepted")
assert type(pickle.loads(blob)) is Replacement
module.Record = record_class
_, calls = observed(lambda: pickle.dumps(record))
assert calls == [], calls[:10]

del sys.modules[module.__name__]
print("pickle dynamic modules: ok")
