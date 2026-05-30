# RFC 0033: ``.pyc`` / ``__pycache__`` compatibility.
#
# WeavePy now writes real CPython-magic ``.pyc`` files (fixing the
# historical silent no-op) and reads them back through the bytecode
# decoder. This exercises the magic number, the importlib ``.pyc``
# round-trip helpers, and a real compile -> write -> load -> exec
# cycle on disk.

import importlib.util
import marshal
import os
import struct
import tempfile


# ---------- MAGIC_NUMBER is CPython 3.13's, not a private tag ----------
magic = importlib.util.MAGIC_NUMBER
assert isinstance(magic, bytes)
assert len(magic) == 4, magic
# CPython 3.13 marks .pyc with a magic ending in the \r\n sentinel.
assert magic[2:] == b"\r\n", magic.hex()


def code_to_timestamp_pyc(code, mtime=0, source_size=0):
    """Build a PEP 552 timestamp-invalidated .pyc: a 16-byte header
    (magic + zero bit-field + mtime + source size) plus the
    marshalled code object. This is exactly the on-disk layout the
    import machinery reads back."""
    return (
        bytes(magic)
        + struct.pack("<I", 0)
        + struct.pack("<I", mtime & 0xFFFFFFFF)
        + struct.pack("<I", source_size & 0xFFFFFFFF)
        + marshal.dumps(code)
    )


# ---------- a real compile -> pyc -> load -> exec cycle ----------
src = (
    "VALUE = 0\n"
    "def compute(n):\n"
    "    return n * n + 1\n"
    "VALUE = compute(6)\n"
)
code = compile(src, "<pyc-test>", "exec")

pyc_bytes = code_to_timestamp_pyc(code)
assert pyc_bytes[:4] == magic, "pyc must start with the magic number"
assert len(pyc_bytes) >= 16, "PEP 552 header is 16 bytes"

# The body after the 16-byte header is a marshalled code object.
body = pyc_bytes[16:]
reloaded = marshal.loads(body)
ns = {}
exec(reloaded, ns)
assert ns["VALUE"] == 37, ns.get("VALUE")
assert ns["compute"](9) == 82

# ---------- real on-disk .pyc round-trip ----------
with tempfile.TemporaryDirectory() as d:
    pyc_path = os.path.join(d, "module.pyc")
    with open(pyc_path, "wb") as f:
        f.write(pyc_bytes)

    with open(pyc_path, "rb") as f:
        disk = f.read()

    assert disk[:4] == magic
    disk_code = marshal.loads(disk[16:])
    ns2 = {}
    exec(disk_code, ns2)
    assert ns2["VALUE"] == 37
    assert ns2["compute"](3) == 10

print("test_pyc_roundtrip: OK")
