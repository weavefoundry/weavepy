"""Guarded comparisons preserve producers, rich results, and caller frames."""
import gc
import math
import sys

OPERATORS = [
    ("<", lambda a, b: a < b),
    ("<=", lambda a, b: a <= b),
    ("==", lambda a, b: a == b),
    ("!=", lambda a, b: a != b),
    (">", lambda a, b: a > b),
    (">=", lambda a, b: a >= b),
]
comparisons = {}
for symbol, operation in OPERATORS:
    for side in ("left", "right"):
        expression = ("producer(i) " + symbol + " 0" if side == "left"
                      else "0 " + symbol + " producer(i)")
        namespace = {}
        exec("def compare(producer, n):\n"
             "    marker = 211\n"
             "    total = 0\n"
             "    for i in range(n):\n"
             "        if " + expression + ":\n"
             "            total += 1\n"
             "    return total\n", namespace)
        compare = namespace["compare"]
        comparisons[symbol, side] = compare
        expected = sum(operation(i - 2, 0) if side == "left"
                       else operation(0, i - 2) for i in range(4000))
        assert compare(lambda i: i - 2, 4000) == expected

events = []
for value in [-2, -1, 0, 1, 2, -(2**100), 2**100, 0.5, math.nan, True, False]:
    def producer(i):
        events.append(i)
        return value
    for symbol, operation in OPERATORS:
        for side in ("left", "right"):
            events.clear()
            expected = operation(value, 0) if side == "left" else operation(0, value)
            assert comparisons[symbol, side](producer, 3) == (3 if expected else 0)
            assert events == [0, 1, 2], events


class Result:
    def __bool__(self):
        frame = sys._getframe(1)
        events.append(("bool", frame.f_code.co_name,
                       frame.f_locals.get("marker"), frame.f_locals.get("i")))
        return True


result_token = Result()


class Rich:
    def record(self, method, other):
        frame = sys._getframe(2)
        events.append((method, other, frame.f_code.co_name,
                       frame.f_locals.get("marker"), frame.f_locals.get("i")))
        return result_token
    def __lt__(self, other):
        return self.record("lt", other)
    def __le__(self, other):
        return self.record("le", other)
    def __eq__(self, other):
        return self.record("eq", other)
    def __ne__(self, other):
        return self.record("ne", other)
    def __gt__(self, other):
        return self.record("gt", other)
    def __ge__(self, other):
        return self.record("ge", other)


rich = Rich()

def rich_producer(i):
    events.append(i)
    return rich


for symbol, methods in [("<", ("lt", "gt")), ("<=", ("le", "ge")),
                        ("==", ("eq", "eq")), ("!=", ("ne", "ne")),
                        (">", ("gt", "lt")), (">=", ("ge", "le"))]:
    for side, method in zip(("left", "right"), methods):
        events.clear()
        assert comparisons[symbol, side](rich_producer, 3) == 3
        expected = []
        for i in range(3):
            expected.extend([i, (method, 0, "compare", 211, i),
                             ("bool", "compare", 211, i)])
        assert events == expected, (symbol, side, events)


def raw_comparison(producer, n):
    marker = 311
    result = False
    for i in range(n):
        result = producer(i) < 0
    return result


assert raw_comparison(lambda i: -1, 4000) is True
events.clear()
assert raw_comparison(rich_producer, 3) is result_token
assert events == [value for i in range(3)
                  for value in (i, ("lt", 0, "raw_comparison", 311, i))], events


class SpecialInt(int):
    def __lt__(self, other):
        events.append("int-lt")
        return True
    def __gt__(self, other):
        events.append("int-gt")
        return False


def subclass_producer(i):
    events.append(i)
    return SpecialInt(10)


events.clear()
assert comparisons["<", "left"](subclass_producer, 3) == 3
assert events == [value for i in range(3) for value in (i, "int-lt")], events
events.clear()
assert comparisons["<", "right"](subclass_producer, 3) == 0
assert events == [value for i in range(3) for value in (i, "int-gt")], events


class Broken:
    def __lt__(self, other):
        events.append("raise")
        raise ValueError("comparison failed")


def broken_producer(i):
    events.append(i)
    return Broken()


events.clear()
try:
    comparisons["<", "left"](broken_producer, 5)
except ValueError as error:
    assert str(error) == "comparison failed"
else:
    raise AssertionError("missing comparison error")
assert events == [0, "raise"], events


class Box:
    def __init__(self):
        self.value = -1


def attribute_predicate(box, n):
    marker = 411
    total = 0
    for i in range(n):
        if box.value < 0:
            total += 1
    return total


box = Box()
assert attribute_predicate(box, 4000) == 4000

def read_value(self):
    frame = sys._getframe(1)
    events.append(("read", frame.f_code.co_name,
                   frame.f_locals.get("marker"), frame.f_locals.get("i")))
    return rich


Box.value = property(read_value)
events.clear()
assert attribute_predicate(box, 3) == 3
assert events == [value for i in range(3) for value in (
    ("read", "attribute_predicate", 411, i),
    ("lt", 0, "attribute_predicate", 411, i),
    ("bool", "attribute_predicate", 411, i))], events

finalized = []

class Temporary:
    def __init__(self, value):
        self.value = value
    def __lt__(self, other):
        return True
    def __del__(self):
        finalized.append(self.value)


def after_comparison(producer, n):
    total = 0
    for i in range(n):
        if producer(i) < 0:
            total += len(finalized)
    return total


assert after_comparison(lambda i: -1, 4000) == 0
assert after_comparison(Temporary, 20) == 210
assert finalized == list(range(20)), finalized
# An opaque result must not force an untyped threshold parameter onto
# the object lane before the compiler consults its observed value.
def parameter_left(producer, threshold, n):
    total = 0
    for i in range(n):
        if producer(i) >= threshold:
            total += 1
    return total


def parameter_right(producer, threshold, n):
    total = 0
    for i in range(n):
        if threshold <= producer(i):
            total += 1
    return total


for reader in [parameter_left, parameter_right]:
    assert reader(lambda i: i, 2000, 4000) == 2000
    for threshold in [0, 2, 2.5, -(2**100), 2**100, math.nan, True, False]:
        events.clear()
        def produce_parameter(i):
            events.append(i)
            return i - 2
        expected = sum(i - 2 >= threshold for i in range(5))
        assert reader(produce_parameter, threshold, 5) == expected
        assert events == list(range(5)), events


class Threshold:
    def __le__(self, other):
        frame = sys._getframe(1)
        events.append(("threshold", other, frame.f_code.co_name, frame.f_locals["i"]))
        return True


for reader in [parameter_left, parameter_right]:
    events.clear()
    assert reader(produce_parameter, Threshold(), 3) == 3
    assert events == [entry for i in range(3) for entry in
                      (i, ("threshold", i - 2, reader.__name__, i))], events

# A completed producer can change a later operand through writable caller
# locals. The left operand of the reflected form is already evaluated.
def writable_left(producer, threshold, n):
    total = 0
    for i in range(n):
        if producer(i) >= threshold:
            total += 1
    return total


def writable_right(producer, threshold, n):
    total = 0
    for i in range(n):
        if threshold <= producer(i):
            total += 1
    return total


def identity_producer(i):
    return i


def write_threshold(i):
    sys._getframe(1).f_locals["threshold"] = i + 1
    return i


assert writable_left(identity_producer, 0, 4000) == 4000
assert writable_right(identity_producer, 0, 4000) == 4000
assert writable_left(write_threshold, 0, 5) == 0
assert writable_right(write_threshold, 0, 5) == 5

gc.collect()
print("Guarded integer comparisons: ok")
