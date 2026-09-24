"""Allocate a polymorphic table only after a field needs another shape."""
import gc
import weakref


class First:
    def __init__(self, value):
        self.value = value


class Second:
    def __init__(self, value):
        self.padding = "other index"
        self.value = value


class Property:
    @property
    def value(self):
        return 17


def read(obj, skip=False):
    if skip:
        return 0
    return obj.value


def read_property(obj):
    return obj.value


def warm_first(first, property_obj):
    for iteration in range(300):
        # Reach the field only after this conditional reader is warm.
        skip = iteration < 150
        assert read(first, skip) == (0 if skip else 11)
        assert read_property(property_obj) == 17


first = First(11)
property_obj = Property()
warm_first(first, property_obj)

# POLYMORPHIC SECOND SHAPE: inspect the compact cache and absent table first.
second = Second(13)
for _ in range(300):
    assert read(first) == 11
    assert read(second) == 13

# POLYMORPHIC MUTATIONS: inspect populated table and matching entries first.
del second.value
second.extra = "shifted"
second.value = 19
assert read(second) == 19
assert read(first) == 11
seen = []


def descriptor(self):
    seen.append(self)
    return self.__dict__["value"] + 100


Second.value = property(descriptor)
assert read(second) == 119
assert seen == [second]
del Second.value
assert read(second) == 19

payload = First(23)
first.value = payload
held = read(first)
watch = weakref.ref(payload)
first.value = None
del payload
gc.collect()
assert held is watch()
assert held.value == 23
assert read(first) is None
del held
gc.collect()
assert watch() is None
for receiver in (first, second):
    del receiver.value
    try:
        read(receiver)
    except AttributeError as exc:
        assert "value" in str(exc)
    else:
        raise AssertionError("deleted field didn't raise")
print("polymorphic cache allocation: ok")
