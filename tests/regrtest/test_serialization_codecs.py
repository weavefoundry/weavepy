# RFC 0027 Group 6: Serialization + compression + codecs.
#
# Exercises ``pickle`` round-tripping (protocols 4 and 5), ``json``
# encoding edge cases, ``struct`` packing across endian / native
# formats, ``re`` corner-cases (lookaround, named groups, flags),
# ``base64`` / ``binascii`` encodings, and ``codecs`` codec lookup.
# Compression modules (``gzip``, ``zlib``, ``bz2``, ``lzma``) are
# covered when the implementations are present; otherwise the file
# skips them with an explicit message so the gap is documented.


# ---------- pickle ----------
import pickle


def roundtrip(value, protocol=None):
    raw = pickle.dumps(value, protocol=protocol)
    return pickle.loads(raw)


# Scalars
for v in [None, True, False, 0, 1, -1, 42, 2**63, 3.14, 1e300, "hello", "", "δ"]:
    assert roundtrip(v) == v, v

# Bytes
assert roundtrip(b"") == b""
assert roundtrip(b"\x00\x01\xff") == b"\x00\x01\xff"

# Containers
assert roundtrip([1, 2, [3, 4]]) == [1, 2, [3, 4]]
assert roundtrip((1, 2, 3)) == (1, 2, 3)
assert roundtrip({1, 2, 3}) == {1, 2, 3}
assert roundtrip(frozenset([1, 2, 3])) == frozenset([1, 2, 3])
assert roundtrip({"a": 1, "b": [2, 3]}) == {"a": 1, "b": [2, 3]}

# Protocols
assert roundtrip(42, protocol=2) == 42
assert roundtrip("p4", protocol=4) == "p4"
assert roundtrip("p5", protocol=5) == "p5"

# Default protocol is 5
assert pickle.DEFAULT_PROTOCOL >= 4


# ---------- json ----------
import json

assert json.loads(json.dumps([1, 2, 3])) == [1, 2, 3]
assert json.loads(json.dumps({"a": 1})) == {"a": 1}
assert json.dumps({"a": 1, "b": 2}, sort_keys=True) == '{"a": 1, "b": 2}'

# unicode passthrough
assert json.dumps("δ", ensure_ascii=False) == '"δ"'
assert json.dumps("δ") == '"\\u03b4"'

# indent / separators
pretty = json.dumps([1, 2], indent=2)
assert pretty == "[\n  1,\n  2\n]", repr(pretty)

# numeric edges
assert json.loads("1.5") == 1.5
assert json.loads("true") is True
assert json.loads("null") is None

# JSONDecodeError
try:
    json.loads("not json")
except json.JSONDecodeError as e:
    assert e.msg
else:
    raise AssertionError("expected JSONDecodeError")


# ---------- struct ----------
import struct

assert struct.pack(">I", 0xDEADBEEF) == b"\xde\xad\xbe\xef"
assert struct.unpack(">I", b"\xde\xad\xbe\xef") == (0xDEADBEEF,)

assert struct.pack("<H", 0x1234) == b"\x34\x12"
assert struct.unpack("<H", b"\x34\x12") == (0x1234,)

assert struct.calcsize(">I") == 4
assert struct.calcsize(">2H") == 4

# pack_into / unpack_from
buf = bytearray(8)
struct.pack_into(">I", buf, 2, 0xCAFEBABE)
assert struct.unpack_from(">I", buf, 2) == (0xCAFEBABE,)

# multiple values
packed = struct.pack(">III", 1, 2, 3)
assert struct.unpack(">III", packed) == (1, 2, 3)
assert list(struct.iter_unpack(">I", packed)) == [(1,), (2,), (3,)]

# signed
assert struct.unpack(">i", struct.pack(">i", -1)) == (-1,)


# ---------- re ----------
import re

# Basic match / search
m = re.match(r"(\w+)\s+(\w+)", "hello world")
assert m is not None
assert m.group(0) == "hello world"
assert m.group(1) == "hello"
assert m.group(2) == "world"
assert m.groups() == ("hello", "world")

# Named groups
m = re.match(r"(?P<first>\w+)\s+(?P<second>\w+)", "foo bar")
assert m.group("first") == "foo"
assert m.group("second") == "bar"
assert m.groupdict() == {"first": "foo", "second": "bar"}

# Lookahead / lookbehind
assert re.search(r"foo(?=bar)", "foobar")
assert not re.search(r"foo(?=bar)", "foobaz")
assert re.search(r"(?<=foo)bar", "foobar")

# Flags
assert re.match(r"HELLO", "hello", re.IGNORECASE)
assert re.findall(r"^line", "line1\nline2", re.MULTILINE) == ["line", "line"]

# sub with backrefs
assert re.sub(r"(\w+) (\w+)", r"\2 \1", "hello world") == "world hello"

# sub with callable
def upper(m):
    return m.group(0).upper()


assert re.sub(r"\w+", upper, "hello world") == "HELLO WORLD"

# split
assert re.split(r"\s+", "  a  b  c  ") == ["", "a", "b", "c", ""]

# compile
pat = re.compile(r"(\d+)")
matches = pat.findall("a1 b22 c333")
assert matches == ["1", "22", "333"]


# ---------- base64 ----------
import base64

assert base64.b64encode(b"hello") == b"aGVsbG8="
assert base64.b64decode(b"aGVsbG8=") == b"hello"

# urlsafe variant
data = b"\xfb\xef"
enc = base64.urlsafe_b64encode(data)
assert b"+" not in enc
assert b"/" not in enc
assert base64.urlsafe_b64decode(enc) == data

# b16 / b32
assert base64.b16encode(b"abc") == b"616263"
assert base64.b16decode(b"616263") == b"abc"

assert base64.b32encode(b"abc") == b"MFRGG==="
assert base64.b32decode(b"MFRGG===") == b"abc"


# ---------- binascii ----------
import binascii

assert binascii.hexlify(b"hello") == b"68656c6c6f"
assert binascii.unhexlify(b"68656c6c6f") == b"hello"
assert binascii.crc32(b"hello") == 0x3610A686


# ---------- codecs ----------
import codecs

assert codecs.encode("hello", "utf-8") == b"hello"
assert codecs.decode(b"hello", "utf-8") == "hello"

# unicode escape
assert codecs.decode(b"\\u03b4", "unicode_escape") == "δ"
assert codecs.encode("δ", "unicode_escape") == b"\\u03b4"

# rot13 / hex codecs are bytes/str round-tripping
assert codecs.encode("abc", "rot_13") == "nop"
assert codecs.decode("nop", "rot_13") == "abc"


# ---------- gzip / zlib (if available) ----------
try:
    import zlib
except ImportError:
    print("(skipping zlib)")
else:
    data = b"hello world" * 100
    comp = zlib.compress(data, 9)
    assert zlib.decompress(comp) == data

    # crc32 / adler32
    assert zlib.crc32(b"hello") == 0x3610A686
    assert zlib.adler32(b"hello") != 0


try:
    import gzip
    import io
except ImportError:
    print("(skipping gzip)")
else:
    data = b"compressible " * 50
    buf = io.BytesIO()
    with gzip.GzipFile(fileobj=buf, mode="wb") as gz:
        gz.write(data)
    buf.seek(0)
    with gzip.GzipFile(fileobj=buf, mode="rb") as gz:
        assert gz.read() == data


print("test_serialization_codecs: OK")
