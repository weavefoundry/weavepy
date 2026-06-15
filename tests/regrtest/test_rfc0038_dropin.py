"""RFC 0038 regression guard - CPython Lib/test conformance sweep, wave 3.

Locks in the binary/codec (WS-A), filesystem/OS (WS-B) and CLI/text (WS-C)
behaviour measured while running CPython 3.13's own `Lib/test/` files under
WeavePy, plus the concrete bug fixes landed this wave (CPython-faithful
`open()` mode validation, the `io.FileIO(fd, ..., closefd=False)` constructor,
and `os.scandir` populating `OSError.filename`). Every section maps to a
workstream in the RFC. Plain `assert`s only - the file exits 0 iff every
behaviour matches CPython.
"""

import os
import tempfile

# ===========================================================================
# WS-A - binary / hashing / compression / codecs
# ===========================================================================
import base64
import binascii
import hashlib
import hmac
import zlib
import gzip
import bz2
import lzma

# base64: Ascii85 + Base85 round-trips and the urlsafe vs standard alphabets.
assert base64.a85decode(base64.a85encode(b"hello world!")) == b"hello world!"
assert base64.b85decode(base64.b85encode(b"hello world!")) == b"hello world!"
assert base64.urlsafe_b64encode(b"\xfb\xef\xff") == b"--__"
assert base64.standard_b64encode(b"\xfb\xef\xff") == b"++//"

# binascii: hexlify with a separator (3.8+) and the round-trip a2b/b2a.
assert binascii.hexlify(b"\x01\x02\x03", b"-") == b"01-02-03"
assert binascii.hexlify(b"\xde\xad\xbe\xef", b" ", 2) == b"dead beef"
assert binascii.unhexlify(binascii.hexlify(b"\x00\xff")) == b"\x00\xff"

# hashlib: known digests, variable-length SHA-3, blake2 params, new(), and the
# usedforsecurity= flag plumbed through the constructor.
assert hashlib.sha256(b"abc").hexdigest() == (
    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
assert hashlib.new("sha1", b"abc").hexdigest() == (
    "a9993e364706816aba3e25717850c26c9cd0d89d")
assert len(hashlib.shake_128(b"abc").hexdigest(16)) == 32
assert hashlib.blake2b(b"", digest_size=16).digest_size == 16
assert hashlib.md5(b"abc", usedforsecurity=False).hexdigest() == (
    "900150983cd24fb0d6963f7d28e17f72")

# hmac: name-based digestmod matches the callable form; compare_digest is
# constant-time over both bytes and ASCII str.
assert (hmac.new(b"key", b"msg", "sha256").hexdigest()
        == hmac.new(b"key", b"msg", hashlib.sha256).hexdigest())
assert hmac.compare_digest(b"abc", b"abc")
assert hmac.compare_digest("abc", "abc")
assert not hmac.compare_digest(b"abc", b"abd")

# zlib: compressobj/decompressobj reuse, flush, and the eof/unused_data state.
_c = zlib.compressobj()
_payload = b"x" * 4096 + b"y" * 4096
_blob = _c.compress(_payload) + _c.flush()
_d = zlib.decompressobj()
assert _d.decompress(_blob) + _d.flush() == _payload
assert _d.eof is True
assert _d.unused_data == b""

# gzip / bz2 / lzma: one-shot round-trips through the real backing codecs.
assert gzip.decompress(gzip.compress(_payload)) == _payload
assert bz2.decompress(bz2.compress(_payload)) == _payload
assert lzma.decompress(lzma.compress(_payload)) == _payload

# ===========================================================================
# WS-B - filesystem / OS surface
# ===========================================================================
import io
import glob
import fnmatch
import stat

# open() mode validation now matches CPython: a ValueError is raised for an
# invalid/ambiguous mode *before* the path is touched (landed this wave).
for _bad in ("rw", "rx", "b", "", "bt", "rtb"):
    try:
        open("/definitely/missing/path", _bad)
    except ValueError:
        pass
    except FileNotFoundError:
        raise AssertionError("open(mode=%r) reached the filesystem; "
                             "mode validation should fire first" % (_bad,))
    else:
        raise AssertionError("open(mode=%r) should raise ValueError" % (_bad,))

# Valid modes still construct the right wrapper types.
with tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False) as _tf:
    _tf.write("data")
    _tmp_path = _tf.name
try:
    with open(_tmp_path, "r") as _f:
        assert isinstance(_f.read(), str)
    with open(_tmp_path, "rb") as _f:
        assert isinstance(_f.read(), bytes)
finally:
    os.unlink(_tmp_path)

# io.FileIO(fd, ..., closefd=False) writes through a borrowed fd without
# closing it on .close() (the constructor + closefd handling landed this wave).
_r, _w = os.pipe()
try:
    _raw = io.FileIO(_w, "w", closefd=False)
    _raw.write(b"hi")
    _raw.close()
    os.write(_w, b"!")          # raises if closefd=False failed to keep _w open
    assert os.read(_r, 8) == b"hi!"
finally:
    os.close(_r)
    os.close(_w)

# os.scandir yields DirEntry objects and an OSError on a missing dir carries
# the offending path in .filename (the fix that cleared the tempfile/shutil
# cleanup cluster this wave).
with tempfile.TemporaryDirectory() as _d:
    open(os.path.join(_d, "a.txt"), "w").close()
    _names = sorted(e.name for e in os.scandir(_d))
    assert _names == ["a.txt"]
_missing = os.path.join(tempfile.gettempdir(), "weavepy_rfc0038_nope_xyz")
try:
    list(os.scandir(_missing))
except FileNotFoundError as e:
    assert e.filename == _missing
else:
    raise AssertionError("os.scandir(missing) should raise FileNotFoundError")

# glob / fnmatch: case-sensitive matching and a real recursive walk.
assert fnmatch.fnmatchcase("file.txt", "*.txt")
assert not fnmatch.fnmatchcase("FILE.TXT", "*.txt")
with tempfile.TemporaryDirectory() as _d:
    os.mkdir(os.path.join(_d, "sub"))
    open(os.path.join(_d, "top.py"), "w").close()
    open(os.path.join(_d, "sub", "deep.py"), "w").close()
    _hits = glob.glob(os.path.join(_d, "**", "*.py"), recursive=True)
    assert len(_hits) == 2

# stat: filemode + the S_IS* predicates.
assert stat.S_ISDIR(stat.S_IFDIR)
assert stat.filemode(0o040755) == "drwxr-xr-x"
assert stat.filemode(0o100644) == "-rw-r--r--"

# ===========================================================================
# WS-C - CLI / text tooling
# ===========================================================================
import getopt
import optparse
import pprint

# getopt: unambiguous long-option abbreviation + GetoptError on unknown opts.
_opts, _args = getopt.getopt(["--verb", "rest"], "", ["verbose"])
assert _opts == [("--verbose", "")] and _args == ["rest"]
try:
    getopt.getopt(["--bogus"], "", ["verbose"])
except getopt.GetoptError:
    pass
else:
    raise AssertionError("expected GetoptError for an unknown long option")

# optparse: basic option/argument split.
_p = optparse.OptionParser()
_p.add_option("-f", "--file", dest="file")
_o, _a = _p.parse_args(["-f", "x.txt", "leftover"])
assert _o.file == "x.txt" and _a == ["leftover"]

# pprint: sort_dicts ordering, width-driven wrapping, and the depth `...` guard.
assert pprint.pformat({"b": 1, "a": 2}, sort_dicts=True).startswith("{'a'")
assert pprint.pformat({"a": {"b": {"c": 1}}}, depth=1) == "{'a': {...}}"
assert "\n" in pprint.pformat(list(range(10)), width=10)

print("ok")
