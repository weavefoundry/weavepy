"""Adjacent scalar reads preserve mutations, callbacks, and fallback values."""
import math
import sys


class DictPoint:
    def __init__(self, value):
        self.value = value


class SlotPoint:
    __slots__ = ("value", "other")

    def __init__(self, value):
        self.value = value


def square(point):
    return point.value * point.value


def sum_squares(point, n):
    total = 0
    for _ in range(n):
        total += point.value * point.value
    return total


proof_point = DictPoint(3)
assert sum_squares(proof_point, 3000) == 27000
assert sum_squares(proof_point, 3001) == 27009
for _ in range(100):
    assert square(proof_point) == 9

# REPEATED ATTRIBUTE MUTATIONS:
for cls in (DictPoint, SlotPoint):
    point = cls(3)
    assert sum_squares(point, 3000) == 27000
    for _ in range(100):
        assert square(point) == 9
    point.value = 7
    assert sum_squares(point, 100) == 4900
    point.value = 1.5
    assert square(point) == 2.25
    point.value = 1 << 70
    assert square(point) == 1 << 140
    point.value = True
    assert square(point) == 1
    point.value = float("nan")
    assert math.isnan(square(point))
    del point.value
    try:
        square(point)
    except AttributeError:
        pass
    else:
        raise AssertionError("missing attribute must raise")
    point.other = 123
    point.value = 11
    assert square(point) == 121

point = DictPoint(4)
assert sum_squares(point, 3000) == 48000
point.__dict__ = {"padding": 123, "value": 6}
assert sum_squares(point, 100) == 3600

reads = []
def property_get(self):
    reads.append(len(reads) + 1)
    return reads[-1]
DictPoint.value = property(property_get)
assert square(point) == 2
assert reads == [1, 2]
del DictPoint.value
assert square(point) == 36


class DynamicPoint:
    def __init__(self):
        self.reads = 0

    def __getattribute__(self, name):
        if name == "value":
            count = object.__getattribute__(self, "reads") + 1
            object.__setattr__(self, "reads", count)
            return count
        return object.__getattribute__(self, name)

point = DynamicPoint()
assert square(point) == 2
assert point.reads == 2


def mutate(point):
    point.value += 1
    return point.value


def around_call(point):
    return point.value * mutate(point) + point.value

point = SlotPoint(3)
for i in range(3000):
    old = point.value
    assert around_call(point) == old * (old + 1) + old + 1


def around_write(point, alias):
    first = point.value
    alias.value += 1
    return first * point.value

for _ in range(100):
    old = point.value
    assert around_write(point, point) == old * (old + 1)


def distinct(point):
    return point.value * point.other

point.other = 2
assert distinct(point) == point.value * 2

traced = []
def trace(frame, event, arg):
    if frame.f_code is square.__code__:
        traced.append(event)
    return trace

sys.settrace(trace)
try:
    assert square(point) == point.value * point.value
finally:
    sys.settrace(None)
assert "call" in traced and "return" in traced
print("repeated scalar attribute reads: ok")
