"""Keep nested native scratch regions valid across recursion and callbacks."""

import gc
import weakref


def leaf(value):
    return value + 1


def narrow(value):
    return leaf(value) + 2


def sum_eight(a, b, c, d, e, f, g, h):
    return a + b + c + d + e + f + g + h


def wide(value, depth):
    a = value + 1
    b = value + 2
    c = value + 3
    d = value + 4
    e = value + 5
    f = value + 6
    g = value + 7
    h = value + 8
    result = sum_eight(a, b, c, d, e, f, g, h)
    if depth:
        return result + narrow(depth) + wide(value + 1, depth - 1)
    return result


def wide_expected(value, depth):
    return (depth + 1) * (8 * value + 36) + 4 * depth * (depth + 1) + depth * (depth + 1) // 2 + 3 * depth


for _ in range(60):
    assert wide(3, 4) == wide_expected(3, 4)
assert wide(5, 80) == wide_expected(5, 80)
assert wide((1 << 63) - 4, 3) == wide_expected((1 << 63) - 4, 3)
assert wide(0.5, 5) == wide_expected(0.5, 5)


class Payload:
    pass


class Callback:
    def __init__(self):
        self.calls = 0
        self.fail = False

    def __call__(self, payload):
        self.calls += 1
        if self.fail:
            raise ValueError("nested callback failed")
        return payload


CALLBACK = Callback()


def object_child(payload, depth):
    result = CALLBACK(payload)
    if depth:
        return object_child(result, depth - 1)
    return result


def object_driver(payload, depth):
    return object_child(payload, depth)


payload = Payload()
for _ in range(60):
    assert object_driver(payload, 4) is payload
assert object_driver(payload, 80) is payload
before = CALLBACK.calls
CALLBACK.fail = True
try:
    object_driver(payload, 4)
except ValueError as error:
    assert str(error) == "nested callback failed"
else:
    raise AssertionError("the callback exception must propagate")
assert CALLBACK.calls == before + 1
CALLBACK.fail = False
assert object_driver(payload, 4) is payload
reference = weakref.ref(payload)
del payload
gc.collect()
assert reference() is None


def default_child(value, increment=4):
    return narrow(value) + increment


def default_driver(value):
    return default_child(value)


for _ in range(60):
    assert default_driver(3) == 10
default_child.__defaults__ = (8,)
assert default_driver(3) == 14
default_child.__defaults__ = (4,)
assert default_driver(3) == 10
assert wide(3, 4) == wide_expected(3, 4)
print("nested native scratch: semantics and lifetimes ok")
