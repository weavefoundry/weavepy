"""List builders preserve scalar types and side effects across JIT exits."""

import builtins
import math


def build_int(n, start):
    out = []
    for i in range(n):
        out.append(start + i)
    return out


def build_float(n, value):
    out = []
    for i in range(n):
        out.append(value)
    return out


def build_bool(n):
    out = []
    for i in range(n):
        out.append(i < 2)
    return out


def build_mixed(n):
    out = []
    for i in range(n):
        out.append(i)
        out.append(i + 0.5)
        out.append(i < 2)
    return out


def byte_scramble(plain, key):
    out = []
    klen = len(key)
    for i, value in enumerate(plain):
        out.append((value ^ key[i % klen]) & 255)
    return bytes(out)


def typed_comprehension(n):
    return [i + 0.5 for i in range(n)]


def append_existing(out, n, start):
    for i in range(n):
        out.append(start + i)
    return out


plain = builtins.bytes(range(256)) * 4
key = b"abc"
expected = builtins.bytes(value ^ key[i % 3] for i, value in enumerate(plain))
for _ in range(60):
    assert build_int(20, -3) == list(range(-3, 17))
    assert build_float(20, 1.5) == [1.5] * 20
    flags = build_bool(20)
    assert flags == [True, True] + [False] * 18
    assert all(type(value) is bool for value in flags)
    mixed = build_mixed(20)
    assert mixed == [value for i in range(20) for value in (i, i + 0.5, i < 2)]
    assert [type(value) for value in mixed] == [int, float, bool] * 20
    assert byte_scramble(plain, key) == expected
    assert typed_comprehension(20) == [i + 0.5 for i in range(20)]
    existing = [0]
    assert append_existing(existing, 20, 1) is existing
    assert existing == list(range(21))

# An arithmetic overflow must resume before the failing append. The
# prefix already appended by native code must appear exactly once.
start = 2 ** 63 - 4
assert build_int(20, start) == list(range(start, start + 20))
existing = [0]
assert append_existing(existing, 20, start) is existing
assert existing == [0] + list(range(start, start + 20))
assert build_int(0, 0) == []
assert build_int(70000, -5) == list(range(-5, 69995))
assert byte_scramble(b"", b"a") == b""
try:
    byte_scramble(b"a", b"")
except ZeroDivisionError:
    pass
else:
    raise AssertionError("empty key did not raise")

for value in (-0.0, float("inf"), -float("inf"), float("nan")):
    result = build_float(20, value)
    assert len(result) == 20
    assert all(type(item) is float for item in result)
    if math.isnan(value):
        assert all(math.isnan(item) for item in result)
    else:
        assert all(repr(item) == repr(value) for item in result)


class SpecialList(list):
    def append(self, value):
        super().append(value * 2)


# Entry guards must preserve subclass dispatch after exact-list warmup.
special = SpecialList()
assert append_existing(special, 8, 1) is special
assert special == list(range(2, 18, 2))


class ChangingList(list):
    def append(self, value):
        calls.append(value)
        if value == 4:
            ChangingList.append = replacement_append
        super().append(value)


def replacement_append(self, value):
    calls.append(value)
    list.append(self, -value)


calls = []
changing = ChangingList()
append_existing(changing, 8, 1)
assert calls == list(range(1, 9))
assert changing == [1, 2, 3, 4, -5, -6, -7, -8]

# The conversion at the end of the compiled byte builder remains a
# normal global lookup, including a replacement that returns a scalar.
bytes = lambda values: sum(values)
assert byte_scramble(plain, key) == sum(expected)
bytes = builtins.bytes
assert byte_scramble(plain, key) == expected
print("ok")
