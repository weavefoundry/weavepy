"""Float keys and tuples of plain values, hashed and probed natively, find
what CPython finds: numbers that are equal across types share a key, zeros
of either sign are one key, a NaN is found only as itself, and hashes agree
with the ones equal values have."""

import math

N = 3000
nan = float("nan")

d = {2.5: "a", 1: "int one", 0.0: "zero", "s": "str", (1, 2.5): "tuple", nan: "nan"}
for _ in range(N):
    assert d[2.5] == "a" and d[5 / 2] == "a"
    assert d[1.0] == "int one" and d[True] == "int one" and d[1] == "int one"
    assert d[-0.0] == "zero" and d[0] == "zero" and d[False] == "zero"
    assert d[(1, 2.5)] == "tuple" and d[(1.0, 2.5)] == "tuple" and d[(True, 2.5)] == "tuple"
    assert d[nan] == "nan" and float("nan") not in d and math.nan not in d or math.nan is nan
    assert 3.5 not in d and d.get(3.5) is None and d.get(2.5, "x") == "a"

f = {1.0: "float one", 2.0**70: "big", float("inf"): "inf", -float("inf"): "-inf"}
for _ in range(N):
    assert f[1] == "float one" and f[True] == "float one" and f[2**70] == "big"
    assert f[float("inf")] == "inf" and f[-math.inf] == "-inf" and 2 not in f

# Stores through a float key land on the equal key already there.
g = {}
for i in range(N):
    g[i * 0.5] = i
    g[i] = -i
assert len(g) == N + N // 2 and g[1.0] == 2 and g[1] == 2 and g[0.5] == 1 and g[1.5] == 3
keys = list(g)
# (The key that got there first stays: `0.0` came before `0`, `1` before `1.0`.)
assert keys[:4] == [0.0, 0.5, 1, 2] and type(keys[0]) is float and type(keys[2]) is int

s = {1.5, 2, (3, 4.0), b"b", None}
for _ in range(N):
    assert 1.5 in s and 2.0 in s and (3.0, 4) in s and (3, 4.0) in s and b"b" in s
    assert None in s and 2.5 not in s and (3, 4.5) not in s
    s.add(2.0)
    s.add((3.0, 4.0))
assert len(s) == 5

for _ in range(N):
    assert hash(2.0) == hash(2) and hash(0.0) == hash(-0.0) == hash(0) == hash(False)
    assert hash((1, 2.0, "x")) == hash((1.0, 2, "x")) == hash((True, 2, "x"))
    assert hash((1, (2.5, None), b"b")) == hash((1, (2.5, None), b"b"))
    assert hash((1, 2.5)) != hash((1, 2.75))
    assert hash(b"abc") == hash(bytes([97, 98, 99])) and hash(nan) == hash(nan)
try:
    hash((1, [2]))
except TypeError:
    pass
else:
    raise AssertionError("a tuple holding a list isn't hashable")
print("ok")
