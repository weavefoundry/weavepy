"""Class-resolution tokens guard caches through mutation and replacement."""

import gc
import threading
import weakref


class First:
    value = 11

    def read(self):
        return self.value * 2


class Second:
    value = 23

    def read(self):
        return self.value * 3


def read_total(item, n):
    total = 0
    for _ in range(n):
        total += item.value + item.read()
    return total


def write_total(item, n):
    for i in range(n):
        item.marker = i
    return item.marker


item = First()
for _ in range(80):
    assert read_total(item, 50) == 1650
    assert write_total(item, 50) == 49
for _ in range(20):
    item.__class__ = Second
    assert read_total(item, 50) == 4600
    item.__class__ = First
    assert read_total(item, 50) == 1650

item.__class__ = Second
events = []


def set_marker(self, value):
    events.append(value)


Second.marker = property(lambda self: 1001, set_marker)
assert write_total(item, 50) == 1001
assert events == list(range(50))
del Second.marker
assert write_total(item, 50) == 49
assert events == list(range(50))

# Class methods, instance shadows, and MRO replacement all invalidate caches.
original = Second.read
Second.read = lambda self: 7
assert read_total(item, 50) == 1500
Second.read = original
item.read = lambda: 5
assert read_total(item, 50) == 1400
del item.read
assert read_total(item, 50) == 4600


class Child(First):
    pass


child = Child()
for _ in range(80):
    assert read_total(child, 50) == 1650
Child.__bases__ = (Second,)
assert read_total(child, 50) == 4600
Second.value = 31
assert read_total(child, 50) == 6200
Child.__bases__ = (First,)
assert read_total(child, 50) == 1650


class SlotFirst:
    __slots__ = ("value",)


class SlotSecond:
    __slots__ = ("value",)

    def __getattribute__(self, name):
        value = object.__getattribute__(self, name)
        return value + 100 if name == "value" else value


def slot_total(item, n):
    total = 0
    for _ in range(n):
        total += item.value
    return total


slot = SlotFirst()
slot.value = 9
for _ in range(80):
    assert slot_total(slot, 50) == 450
slot.__class__ = SlotSecond
assert slot_total(slot, 50) == 5450
slot.__class__ = SlotFirst
assert slot_total(slot, 50) == 450

# Instance attributes let the native method driver infer scalar return lanes.
# The class-attribute read_total above also exercises the interpreter caches.
class NativeFirst:
    def __init__(self):
        self.value = 11

    def read(self):
        return self.value * 2


class NativeSecond:
    def read(self):
        return self.value * 3


def native_total(item, n):
    total = 0
    for _ in range(n):
        total += item.value + item.read()
    return total


native = NativeFirst()
for _ in range(80):
    assert native.read() == 22
for _ in range(80):
    assert native_total(native, 50) == 1650
native.__class__ = NativeSecond
assert native_total(native, 50) == 2200
native.__class__ = NativeFirst
assert native_total(native, 50) == 1650
saved_read = NativeFirst.read
NativeFirst.read = lambda self: 7
assert native_total(native, 50) == 900
NativeFirst.read = saved_read
native.read = lambda: 5
assert native_total(native, 50) == 800
del native.read
assert native_total(native, 50) == 1650

# Successive short-lived classes share access sites but never cache identity.
def read_value(item):
    return item.value


references = []
for value in range(512):
    cls = type("Transient", (), {"value": value})
    transient = cls()
    references.append(weakref.ref(cls))
    for _ in range(80):
        assert read_value(transient) == value
    del cls, transient
gc.collect()
assert all(reference() is None for reference in references)

errors = []


def worker(offset):
    try:
        for i in range(64):
            value = offset + i
            cls = type("Concurrent", (), {"value": value})
            current = cls()
            for _ in range(80):
                assert read_value(current) == value
            cls.value = -value
            assert read_value(current) == -value
    except BaseException as error:
        errors.append(repr(error))


threads = [threading.Thread(target=worker, args=(i * 1000,)) for i in range(4)]
for thread in threads:
    thread.start()
for thread in threads:
    thread.join()
assert errors == [], errors
print("ok")
