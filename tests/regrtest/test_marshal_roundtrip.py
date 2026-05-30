# RFC 0033: ``marshal`` round-tripping, including code objects.
#
# ``marshal`` is what ``.pyc`` files and ``importlib`` use to persist
# compiled bytecode. This exercises value round-tripping across the
# core types plus the headline RFC 0033 feature: serialising and
# reloading a ``code`` object and executing it.

import marshal


def roundtrip(value):
    return marshal.loads(marshal.dumps(value))


# ---------- scalars ----------
for v in [None, True, False, 0, 1, -1, 255, 256, -256,
          3.14, -0.0, 1e308, "", "hello", "δ-unicode-ζ"]:
    assert roundtrip(v) == v, v

# bools survive as bools, not ints
assert roundtrip(True) is True
assert roundtrip(False) is False
assert roundtrip(None) is None

# ---------- big integers (exact 15-bit digit packing) ----------
for v in [2 ** 15, 2 ** 30 - 1, 2 ** 64, 2 ** 128 + 7,
          -(2 ** 200), 12345678901234567890, -98765432109876543210]:
    assert roundtrip(v) == v, v

# ---------- bytes ----------
assert roundtrip(b"") == b""
assert roundtrip(b"\x00\x01\xfe\xff") == b"\x00\x01\xfe\xff"

# ---------- containers ----------
assert roundtrip([1, 2, [3, 4]]) == [1, 2, [3, 4]]
assert roundtrip((1, "x", 3.5, (4, 5))) == (1, "x", 3.5, (4, 5))
assert roundtrip({"a": 1, "b": [2, 3]}) == {"a": 1, "b": [2, 3]}
assert roundtrip(frozenset([1, 2, 3])) == frozenset([1, 2, 3])

# ---------- shared references survive (FLAG_REF) ----------
shared = ("shared-string-value",)
pair = roundtrip((shared, shared))
assert pair[0] == pair[1]

# ---------- code objects ----------
src = (
    "def add(a, b):\n"
    "    return a + b\n"
    "\n"
    "result = add(3, 4) * 10\n"
)
code = compile(src, "<marshal-test>", "exec")
blob = marshal.dumps(code)
assert isinstance(blob, bytes)
assert len(blob) > 0

code2 = marshal.loads(blob)
ns = {}
exec(code2, ns)
assert ns["result"] == 70, ns.get("result")
assert ns["add"](10, 20) == 30

# The reconstructed code object keeps its identity-bearing fields.
assert code2.co_filename == "<marshal-test>"
assert code2.co_argcount == code.co_argcount

# ---------- marshal.version ----------
assert marshal.version >= 4, marshal.version

print("test_marshal_roundtrip: OK")
