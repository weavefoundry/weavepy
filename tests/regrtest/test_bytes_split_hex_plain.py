"""`bytes.split()`, `bytes.split(sep)` and `bytes.hex()` without options.

These build each piece, or the digits, directly from the bytes; empty
pieces are the shared empty bytes, and a bytearray, a subclass, a
`maxsplit` or a separator for `hex` takes the general path.
"""

assert b"  a  b\tc\n".split() == [b"a", b"b", b"c"]
assert b"".split() == [] and b" \t".split() == [] and b"x".split(None) == [b"x"]
assert b"a,b,,c".split(b",") == [b"a", b"b", b"", b"c"]
assert b",".split(b",") == [b"", b""] and b"".split(b",") == [b""]
assert b"a,,b,,".split(b",,") == [b"a", b"b", b""]
assert b"a,b,,c".split(b",")[2] is bytes()
assert b"a b c".split(maxsplit=1) == [b"a", b"b c"]
assert b"a,b,c".split(b",", 1) == [b"a", b"b,c"]
assert b"a b".split(sep=None) == [b"a", b"b"] and b"a,b".split(sep=b",") == [b"a", b"b"]
assert bytearray(b"a b").split() == [bytearray(b"a"), bytearray(b"b")]
assert type(bytearray(b"a,b").split(b",")[0]) is bytearray


class B(bytes):
    pass


assert B(b"a b").split() == [b"a", b"b"] and type(B(b"a b").split()[0]) is bytes
try:
    b"a".split(b"")
except ValueError:
    pass
else:
    raise AssertionError("empty separator")
try:
    b"a".split("x")
except TypeError:
    pass
else:
    raise AssertionError("str separator")

assert b"".hex() == "" and b"\x00\xff\x10".hex() == "00ff10"
assert bytes(range(256)).hex() == "".join(f"{i:02x}" for i in range(256))
assert b"\x01\x02".hex(":") == "01:02" and bytearray(b"\xab").hex() == "ab"
assert B(b"\x0f").hex() == "0f" and len(b"\x7f\x80".hex()) == 4
print("ok")
