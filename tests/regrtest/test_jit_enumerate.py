"""Enumeration keeps values, exceptions, and callbacks across JIT exits."""

import builtins


def enum_checksum(data):
    total = 0
    for index, value in enumerate(data):
        total = total + (index ^ value)
    return total


def sum_indices(data):
    total = 0
    for index, value in enumerate(data):
        total = total + index
    return total


def last_item(data):
    result = None
    for index, value in enumerate(data):
        result = value
    return result


LOCAL_ITEMS = tuple(range(30))


def local_indices():
    items = LOCAL_ITEMS
    total = 0
    for index, value in enumerate(items):
        total = total + index
    return total


data = bytes(range(256)) * 4
for _ in range(60):
    assert enum_checksum(data) == 393216
    assert sum_indices(tuple(range(30))) == 435
    assert last_item(tuple(range(30))) == 29
    assert local_indices() == 435

LOCAL_ITEMS = tuple(range(10))
assert local_indices() == 45

data = bytes(range(256)) * 300
assert enum_checksum(data) == sum(index ^ value for index, value in builtins.enumerate(data))

for value in (True, 42, 2 ** 100, -0.0, "text", "\ud800", b"bytes", None):
    result = last_item((None, value))
    assert type(result) is type(value)
    assert repr(result) == repr(value)

# Mutable tuple members retain the generic path and their original identity.
for value in ([1, 2], {"key": 3}, {4}, object()):
    assert last_item((None, value)) is value
    assert sum_indices((value, None, 42)) == 3

# A long generic loop exhausts the bounded runtime pin table. The value
# consumed at that boundary must survive the return to the interpreter.
assert sum_indices(tuple(range(70000))) == 2449965000
assert last_item(tuple(range(70000))) == 69999

for data in (b"", b"a", bytes(range(256)), [2 ** 100, -2 ** 80, 7], (1, 2, 3)):
    expected = sum(index ^ value for index, value in builtins.enumerate(data))
    assert enum_checksum(data) == expected


class Bytes(bytes):
    def __iter__(self):
        return iter((500, 600))


assert enum_checksum(Bytes(b"ignored")) == (0 ^ 500) + (1 ^ 600)


class Tuple(tuple):
    def __iter__(self):
        return iter((100, 200, 300))


assert sum_indices(Tuple((1,))) == 3
assert last_item(Tuple((1,))) == 300


def replacement(data):
    return iter(((10, 2), (20, 3)))


# A rebound global must invalidate the compiled builtin assumption.
enumerate = replacement
assert enum_checksum(b"ignored") == (10 ^ 2) + (20 ^ 3)
enumerate = builtins.enumerate
assert enum_checksum(bytes(range(256)) * 4) == 393216


class MutatingIterator:
    def __init__(self):
        self.position = 0
        self.calls = []

    def __iter__(self):
        return self

    def __next__(self):
        global enumerate
        position = self.position
        self.calls.append(position)
        if position == 10:
            raise StopIteration
        self.position += 1
        if position == 4:
            enumerate = replacement
        return object()


# The in-flight iterator remains the original builtin enumerate. A guard
# failure after __next__ must preserve the consumed pair without repeating it.
iterator = MutatingIterator()
assert sum_indices(iterator) == 45
assert iterator.calls == list(range(11))
enumerate = builtins.enumerate


class BrokenIterator:
    def __init__(self):
        self.calls = 0

    def __iter__(self):
        return self

    def __next__(self):
        self.calls += 1
        if self.calls == 4:
            raise ValueError("iteration failed")
        return None


iterator = BrokenIterator()
try:
    sum_indices(iterator)
except ValueError as error:
    assert str(error) == "iteration failed"
else:
    raise AssertionError("iterator exception was lost")
assert iterator.calls == 4
print("ok")
