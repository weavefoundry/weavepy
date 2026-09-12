"""Checked integer call results preserve arithmetic and callback behavior."""

import math


def add_results(n, callback):
    total = 0
    for i in range(n):
        total += callback(i)
    return total


def left_results(n, callback):
    total = 0
    for i in range(n):
        total += callback(i) + 1
    return total


def divide_results(n, callback):
    total = 0.0
    for i in range(n):
        total += 10 / callback(i)
    return total


def xor_results(n, callback):
    total = 0
    for i in range(n):
        total ^= callback(i)
    return total


def identity(i):
    return i


def positive(i):
    return i + 1


for _ in range(60):
    assert add_results(20, identity) == 190
    assert left_results(20, identity) == 210
    assert math.isclose(divide_results(20, positive), sum(10 / (i + 1) for i in range(20)))
    assert xor_results(20, identity) == 0

events = []


def counted_result(i):
    events.append(i)
    return i


# Cross the native pin-table limit while retaining observable call order.
# A completed result may be parked for the interpreter, but never recomputed.
assert add_results(100000, counted_result) == 4999950000
assert events == list(range(100000))
events.clear()


def near_limit(i):
    events.append(i)
    return 2**63 - 4


# The exact-integer check succeeds, then arithmetic overflow must restore
# both operands without repeating the callback that produced the result.
assert add_results(20, near_limit) == 20 * (2**63 - 4)
assert events == list(range(20))
events.clear()


def wide_integer(i):
    events.append(i)
    return 2**100 + i


assert left_results(20, wide_integer) == 20 * (2**100 + 1) + 190
assert events == list(range(20))
events.clear()


def floating(i):
    events.append(i)
    return i + 0.5


assert add_results(20, floating) == 200.0
assert events == list(range(20))
events.clear()


class IntegerSubclass(int):
    def __radd__(self, left):
        events.append(("radd", int(self)))
        return left + int(self) + 100


def subclass_result(i):
    events.append(i)
    return IntegerSubclass(i)


assert add_results(8, subclass_result) == 828
assert events == [value for i in range(8) for value in (i, ("radd", i))]
events.clear()


class Reflected:
    def __radd__(self, left):
        events.append("reflected")
        return left + 7


def reflected_result(i):
    events.append(i)
    return Reflected()


assert add_results(8, reflected_result) == 56
assert events == [value for i in range(8) for value in (i, "reflected")]
events.clear()


def invalid_result(i):
    events.append(i)
    if i == 5:
        return None
    return i


try:
    add_results(20, invalid_result)
except TypeError:
    pass
else:
    raise AssertionError("invalid result did not raise")
assert events == list(range(6))
events.clear()


def raising_result(i):
    events.append(i)
    if i == 5:
        raise ValueError("callback failed")
    return i


try:
    add_results(20, raising_result)
except ValueError as error:
    assert str(error) == "callback failed"
else:
    raise AssertionError("callback did not raise")
assert events == list(range(6))
events.clear()


def zero_result(i):
    events.append(i)
    return 0


try:
    divide_results(20, zero_result)
except ZeroDivisionError:
    pass
else:
    raise AssertionError("zero divisor did not raise")
assert events == [0]
events.clear()
assert add_results(0, raising_result) == 0
assert not events
assert xor_results(3, lambda i: True) == 1
assert type(xor_results(3, lambda i: True)) is int


def exact_division(numerator, callback, n):
    numerator = numerator + 0
    value = 0.0
    for i in range(n):
        value = numerator / callback(i)
    return value


for _ in range(60):
    assert exact_division(10, lambda i: 3, 20) == 10 / 3

# Rounding the operands to floats before division loses a bit for these
# machine-sized integers. The interpreter's integer quotient must handle
# them, preserving callbacks and signed zero across the native guard.
for numerator, denominator, expected in (
    (10, 9007199254740993, "0x1.3ffffffffffffp-50"),
    (9007199254740993, 3, "0x1.5555555555556p+51"),
    (-9007199254740993, 3, "-0x1.5555555555556p+51"),
    (9223372036854775807, 9007199254740993, "0x1.fffffffffffffp+9"),
    (0, -9007199254740993, "-0x0.0p+0"),
):
    events.clear()

    def denominator_result(i):
        events.append(i)
        return denominator

    assert exact_division(numerator, denominator_result, 20).hex() == expected
    assert events == list(range(20))


def with_defaults(a, b=10, c=20):
    return a + b + c


def default_gap_calls(n, callback):
    total = 0
    for i in range(n):
        total += with_defaults(callback(i), c=callback(i + 100))
    return total


for _ in range(60):
    assert default_gap_calls(20, identity) == 2580
events.clear()
assert default_gap_calls(20, counted_result) == 2580
assert events == [value for i in range(20) for value in (i, i + 100)]
with_defaults.__defaults__ = (30, 40)
events.clear()
assert default_gap_calls(20, counted_result) == 2980
assert events == [value for i in range(20) for value in (i, i + 100)]
with_defaults.__defaults__ = (1.5, 40)
events.clear()
assert default_gap_calls(20, counted_result) == 2410.0
assert events == [value for i in range(20) for value in (i, i + 100)]
with_defaults.__defaults__ = ()
events.clear()
try:
    default_gap_calls(20, counted_result)
except TypeError:
    pass
else:
    raise AssertionError("missing default did not raise")
assert events == [0, 100]
print("ok")
