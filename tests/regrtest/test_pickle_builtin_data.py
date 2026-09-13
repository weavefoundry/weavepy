"""Preserve built-in pickle values, aliases, and fallback side effects."""

import math
import gc
import pickle
import struct
import weakref


def same(actual, expected):
    assert type(actual) is type(expected)
    if isinstance(expected, float):
        assert struct.pack(">d", actual) == struct.pack(">d", expected)
    elif isinstance(expected, (list, tuple)):
        assert len(actual) == len(expected)
        for left, right in zip(actual, expected):
            same(left, right)
    elif isinstance(expected, dict):
        assert list(actual) == list(expected)
        for key in expected:
            same(actual[key], expected[key])
    else:
        assert actual == expected


values = [
    None, True, False, 0, 255, 256, 65535, 65536, -1, -(1 << 31),
    1 << 31, (1 << 63) - 1, -(1 << 63), 1 << 63, -(1 << 100),
    (1 << 2500) + 17, 0.0, -0.0, math.inf, -math.inf, math.nan,
    "", "x", "plain text", "\0 embedded", "\u00e9 \u4e2d \U0001f680",
    "x" * 255, "y" * 256, "z" * 70000, "\ud800",
    b"", b"\0\xff", bytes(range(256)) * 300,
    (), (1,), (1, "a"), (None, False, b"a"), tuple(range(5)),
    [], list(range(2200)), {}, {"a": 1, 42: [True, None], b"b": (1, 2)},
    {(1, 2): "tuple key"}, {1.5: "float key"}, {None: "none", False: "bool"},
    {"nested": [{"item": (i, i + 1)} for i in range(50)]},
    set(range(5)), frozenset(["a", "b"]), bytearray(b"abc"),
]
for protocol in (4, 5):
    for value in values:
        blob = pickle.dumps(value, protocol=protocol)
        same(pickle.loads(blob), value)
        same(pickle.loads(blob + b"ignored trailing data"), value)
        same(pickle.loads(bytearray(blob)), value)

    shared = ["a mutable list"]
    value = {"a": shared, "b": shared, "tuple": (shared, shared)}
    back = pickle.loads(pickle.dumps(value, protocol=protocol))
    assert back["a"] is back["b"] is back["tuple"][0] is back["tuple"][1]
    back["a"].append(7)
    assert back["b"] == ["a mutable list", 7]

    recursive = []
    recursive.append(recursive)
    back = pickle.loads(pickle.dumps(recursive, protocol=protocol))
    assert back[0] is back
    recursive_dict = {}
    recursive_dict["self"] = recursive_dict
    back = pickle.loads(pickle.dumps(recursive_dict, protocol=protocol))
    assert back["self"] is back
    indirect = []
    indirect.append((indirect,))
    back = pickle.loads(pickle.dumps(indirect, protocol=protocol))
    assert back[0][0] is back

    # Exercise deep decoding without depending on the recursive Python
    # encoder or comparison function's recursion budget.
    deep_blob = bytes([0x80, protocol]) + b"K\0" + b"\x85" * 160 + b"."
    back = pickle.loads(deep_blob)
    for _ in range(160):
        assert type(back) is tuple and len(back) == 1
        back = back[0]
    assert back == 0


class Marker:
    pass


def check_later_cycles():
    # Each container is nested in an immutable owner so registering only
    # the top-level result cannot satisfy this check.
    back = pickle.loads(b"\x80\x05]}\x86.")
    parts, mapping = back
    assert gc.is_tracked(parts)
    marker = Marker()
    ref = weakref.ref(marker)
    parts.extend([mapping, marker])
    mapping["back"] = parts
    assert gc.is_tracked(mapping)
    return ref


was_enabled = gc.isenabled()
gc.disable()
try:
    for _ in range(5):
        ref = check_later_cycles()
        gc.collect()
        assert ref() is None
finally:
    if was_enabled:
        gc.enable()

events = []


def rebuild(value):
    events.append(value)
    return ["rebuilt", value]


class Custom:
    def __reduce__(self):
        return rebuild, (42,)


blob = pickle.dumps([1, Custom(), 3], protocol=5)
assert events == []
assert pickle.loads(blob) == [1, ["rebuilt", 42], 3]
assert events == [42]

for value in (None, [1, 2], {"a": "b"}):
    for protocol in range(4):
        same(pickle.loads(pickle.dumps(value, protocol=protocol)), value)

print("pickle built-in data: ok")
