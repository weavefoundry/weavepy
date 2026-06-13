"""RFC 0027 — Group 3: Numeric / string / format edges.

CPython 3.13 behavioural sweep for ``int``, ``float``, ``complex``,
``Fraction``, ``str``/``bytes``/``format``, deep f-strings, and the
%-formatting tail.
"""

# ---------- int methods ----------
assert (0).bit_count() == 0
assert (1).bit_count() == 1
assert (255).bit_count() == 8
assert (256).bit_count() == 1
assert (-7).bit_count() == 3   # abs(-7) = 7 = 0b111
assert (0).bit_length() == 0
assert (1).bit_length() == 1
assert (255).bit_length() == 8
assert (-256).bit_length() == 9

assert (5).is_integer() is True
assert (-3).is_integer() is True

# int.as_integer_ratio
assert (5).as_integer_ratio() == (5, 1)
assert (-7).as_integer_ratio() == (-7, 1)

# int.to_bytes / from_bytes
assert (1024).to_bytes(2, "big") == b"\x04\x00"
assert (1024).to_bytes(2, "little") == b"\x00\x04"
assert int.from_bytes(b"\x04\x00", "big") == 1024
assert int.from_bytes(b"\x00\x04", "little") == 1024

# int(s, base) with non-ASCII handled via underscore separators
# (CPython 3.6+ accepts ``_`` between digits)
assert int("1_000") == 1000
assert int("0x1_0", 16) == 16


# ---------- float ----------
assert (1.0).is_integer() is True
assert (1.5).is_integer() is False

# float.hex / fromhex roundtrip
import math

for v in [0.0, -0.0, 1.0, -1.0, 1.5, 0.1, 1e-300, 1e300, math.pi]:
    h = v.hex()
    parsed = float.fromhex(h)
    if math.isfinite(v):
        assert parsed == v or (math.isnan(parsed) and math.isnan(v)), (v, h, parsed)

# inf / nan
assert float.fromhex("inf") == float("inf")
assert float.fromhex("-inf") == float("-inf")
assert math.isnan(float.fromhex("nan"))

# float.as_integer_ratio
assert (0.5).as_integer_ratio() == (1, 2)
assert (-0.25).as_integer_ratio() == (-1, 4)

# float repr — shortest round-trip + CPython's exponential thresholds
# (exponential when decpt <= -4 or decpt > 16).
assert repr(0.0) == "0.0"
assert repr(-0.0) == "-0.0"
assert repr(1.0) == "1.0"
assert repr(0.1) == "0.1"
assert repr(1234.5678) == "1234.5678"
assert repr(1e15) == "1000000000000000.0"
assert repr(1e16) == "1e+16"
assert repr(1e17) == "1e+17"
assert repr(1e100) == "1e+100"
assert repr(0.0001) == "0.0001"
assert repr(0.00001) == "1e-05"
assert repr(1e-100) == "1e-100"
assert repr(1234567890123456.0) == "1234567890123456.0"
assert repr(12345678901234567.0) == "1.2345678901234568e+16"
assert repr(5e-324) == "5e-324"             # smallest subnormal
assert repr(1.7976931348623157e308) == "1.7976931348623157e+308"  # max
assert repr(float("inf")) == "inf"
assert repr(float("-inf")) == "-inf"
assert repr(float("nan")) == "nan"
# str(float) == repr(float) in Python 3
assert str(1e16) == "1e+16"
assert str(0.1) == "0.1"
# complex parts reuse the float rules but drop a trailing ``.0``
assert repr(complex(4, 5)) == "(4+5j)"
assert repr(complex(1.5, 2)) == "(1.5+2j)"
assert repr(complex(1e100, 0)) == "(1e+100+0j)"
assert repr(complex(0, 1)) == "1j"
assert repr(2.0 + 0j) == "(2+0j)"


# ---------- complex ----------
assert complex(1, 2) == 1 + 2j
assert complex("1+2j") == 1 + 2j
assert (1 + 2j).real == 1.0
assert (1 + 2j).imag == 2.0
assert abs(3 + 4j) == 5.0
assert (1 + 2j).conjugate() == 1 - 2j


# ---------- Fraction ----------
from fractions import Fraction

assert Fraction(1, 2) + Fraction(1, 3) == Fraction(5, 6)
assert Fraction(1, 2) * Fraction(2, 3) == Fraction(1, 3)
# Integer exponent — stays rational.
assert Fraction(1, 2) ** 3 == Fraction(1, 8)
assert Fraction(2, 3) ** 2 == Fraction(4, 9)
# Negative integer exponent — still rational.
assert Fraction(2, 3) ** -1 == Fraction(3, 2)
assert Fraction(2, 3) ** -2 == Fraction(9, 4)


# ---------- f-strings ----------
x = 42
assert f"{x}" == "42"
assert f"{x:5}" == "   42"
assert f"{x:05}" == "00042"
assert f"{x:>5}" == "   42"
assert f"{x:<5}" == "42   "
assert f"{x:^5}" == " 42  "

# PEP 701 — nested format specs
width = 5
assert f"{x:>{width}}" == "   42"

# Deep nesting
n = 3
assert f"{x:>{n + 2}}" == "   42"

# Quotes inside expression — PEP 701
d = {"key": 42}
assert f"{d['key']}" == "42"


# Multi-line f-strings (PEP 701)
def _multi():
    return f"""value={
    x
}"""

assert _multi() == "value=42"


# ---------- % formatting ----------
assert "%d" % 42 == "42"
assert "%5d" % 42 == "   42"
assert "%-5d" % 42 == "42   "
assert "%05d" % 42 == "00042"
assert "%x" % 255 == "ff"
assert "%X" % 255 == "FF"
assert "%o" % 8 == "10"
assert "%.2f" % 1.5 == "1.50"
assert "%10.2f" % 1.5 == "      1.50"
assert "%s,%s" % ("a", "b") == "a,b"

# % formatting on bytes
assert b"%d" % 42 == b"42"
assert b"%s" % b"hello" == b"hello"
assert b"%x" % 255 == b"ff"


# ---------- str.format / format_map ----------
assert "{} {}".format("hello", "world") == "hello world"
assert "{1} {0}".format("a", "b") == "b a"
assert "{name}".format(name="Alice") == "Alice"
assert "{:>10}".format("hi") == "        hi"

# format_map with regular dict
assert "{name}".format_map({"name": "Bob"}) == "Bob"


# ---------- struct ----------
import struct

assert struct.pack("<I", 1024) == b"\x00\x04\x00\x00"
assert struct.unpack("<I", b"\x00\x04\x00\x00") == (1024,)
assert struct.pack(">H", 256) == b"\x01\x00"
assert struct.calcsize("<I") == 4
assert struct.calcsize(">Q") == 8

# struct.pack_into / unpack_from
buf = bytearray(8)
struct.pack_into("<I", buf, 0, 0xDEADBEEF)
assert bytes(buf[:4]) == b"\xef\xbe\xad\xde"
assert struct.unpack_from("<I", buf, 0) == (0xDEADBEEF,)


# ---------- bytes/bytearray ----------
b = bytes([1, 2, 3, 4])
assert b == b"\x01\x02\x03\x04"
assert b.hex() == "01020304"
assert bytes.fromhex("01020304") == b
assert b.hex(":") == "01:02:03:04"
assert b.hex(":", 2) == "0102:0304"


# ---------- bytes/str translate ----------
assert "abc".translate(str.maketrans("abc", "xyz")) == "xyz"
# translate with deletion via None mapping
assert "abcde".translate(str.maketrans("", "", "ace")) == "bd"

# ---------- str splitlines / encode ----------
assert "a\nb\rc\r\nd".splitlines() == ["a", "b", "c", "d"]
assert "abc".encode("utf-8") == b"abc"
assert b"abc".decode("utf-8") == "abc"

# Different encodings
assert "abc".encode("latin-1") == b"abc"
assert "abc".encode("ascii", "replace") == b"abc"


# ---------- textwrap ----------
import textwrap

wrapped = textwrap.wrap("hello world this is a test", width=10)
assert all(len(line) <= 10 for line in wrapped)
assert " ".join(wrapped) == "hello world this is a test"


# ---------- math ----------
assert math.floor(1.5) == 1
assert math.ceil(1.5) == 2
assert math.trunc(1.5) == 1
assert math.gcd(12, 18) == 6
assert math.gcd(12, 18, 24) == 6
assert math.lcm(4, 6) == 12
assert math.lcm(4, 6, 8) == 24
assert math.isfinite(1.0)
assert not math.isfinite(float("inf"))
assert not math.isfinite(float("nan"))
assert math.isclose(0.1 + 0.2, 0.3)


# ---------- decimal ----------
from decimal import Decimal

assert Decimal("0.1") + Decimal("0.2") == Decimal("0.3")
assert Decimal("1.5") * 2 == Decimal("3.0")
assert Decimal("10") / Decimal("3") != Decimal(0)


print("test_numeric_string_format: OK")
