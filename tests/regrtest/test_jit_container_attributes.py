"""Container attributes retain identity, mutations, and lifetime semantics."""
import gc
import weakref


class Box:
    def __init__(self, value):
        self.value = value


class Slotted:
    __slots__ = ('value',)

    def __init__(self, value):
        self.value = value


def read(box, n):
    result = box.value
    for _ in range(n):
        result = box.value
    return result


def append_all(box, value, n):
    for _ in range(n):
        box.value.append(value)


def replace_mid_loop(box, replacement, n):
    result = box.value
    for i in range(n):
        if i == n // 2:
            box.value = replacement
        result = box.value
    return result


for cls in (Box, Slotted):
    for value in ([1, None, 'three'], {'first': 1, 'second': None}):
        box = cls(value)
        for _ in range(80):
            assert read(box, 300) is value
        equal = value.copy()
        assert equal == value and equal is not value
        box.value = equal
        assert read(box, 20000) is equal
        assert replace_mid_loop(box, value, 20000) is value
        box.value = None
        assert read(box, 20000) is None
        box.value = [value]
        assert read(box, 20000) is box.value
        del box.value
        try:
            read(box, 20000)
        except AttributeError:
            pass
        else:
            raise AssertionError('deleted attribute did not raise')

box = Box([])
marker = object()
for _ in range(80):
    append_all(box, marker, 200)
assert box.value == [marker] * 16000
original = box.value
replacement = []
box.value = replacement
append_all(box, marker, 20000)
assert box.value is replacement and replacement == [marker] * 20000
assert original == [marker] * 16000

# A callee result that crosses a native boundary must not be recomputed.
class Counted:
    def __init__(self, value):
        self.value = value
        self.calls = 0

    def get(self):
        self.calls += 1
        return self.value


def call_getter(box, n):
    result = None
    for _ in range(n):
        result = box.get()
    return result


for value in ([], {}):
    box = Counted(value)
    for _ in range(80):
        assert call_getter(box, 200) is value
    assert box.calls == 16000
    equal = value.copy()
    box.value = equal
    assert call_getter(box, 20000) is equal
    assert box.calls == 36000

# Inherited code must revalidate descriptors and custom lookup hooks.
box = Box({'value': 1})
for _ in range(80):
    assert read(box, 300) is box.value
expected = [7]
Box.value = property(lambda self: expected)
assert read(box, 20000) is expected
del Box.value
assert read(box, 20000) is box.__dict__['value']

# A reused result hint cannot own a dead container or its contents.
class Payload:
    pass

for make in (lambda obj: [obj], lambda obj: {'payload': obj}):
    payload = Payload()
    watched = weakref.ref(payload)
    box = Box(make(payload))
    assert read(box, 20000) is box.value
    del box, payload
    gc.collect()
    assert watched() is None

# Dead native temporaries must not retain the contents of a replaced field.
# Check while the activation is still running, before its exit drains pins.
def release_during_loop(box, watched, n):
    current = box.value
    alive = True
    for i in range(n):
        current = box.value
        if i == 1000:
            box.value = None
        if i == 1001:
            gc.collect()
            alive = watched() is not None
    return alive


for cls in (Box, Slotted):
    for make in (lambda obj: [obj], lambda obj: {'payload': obj}):
        payload = Payload()
        watched = weakref.ref(payload)
        box = cls(make(payload))
        del payload
        assert not release_during_loop(box, watched, 10000)
        assert watched() is None

print('container attributes: ok')
