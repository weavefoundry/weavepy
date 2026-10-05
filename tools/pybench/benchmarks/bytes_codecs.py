"""Binary data: struct pack/unpack, bytes/bytearray ops, encode/decode,
base64, hashlib, zlib, and int.to_bytes."""

import base64
import hashlib
import struct
import zlib

WORK = 30000

REC = struct.Struct("<IhdB3s")


def bench(n):
    total = 0
    buf = bytearray()
    for i in range(n):
        packed = REC.pack(i, -i & 0x7FFF, i * 0.5, i & 255, b"abc")
        buf += packed
        a, b, c, d, e = REC.unpack(packed)
        total += a + b + d + len(e)
        s = "café-%d" % i
        total += len(s.encode("utf-8")) + len(s.encode("latin-1").decode("latin-1"))
        total += int.from_bytes(i.to_bytes(4, "big"), "little") & 0xFF
        if i % 50 == 0:
            chunk = bytes(buf[-1000:])
            total += len(base64.b64encode(chunk)) + len(base64.b64decode(base64.b64encode(chunk)))
            total += hashlib.sha256(chunk).digest()[0] + len(hashlib.md5(chunk).hexdigest())
            total += len(zlib.decompress(zlib.compress(chunk)))
            total += chunk.count(b"a") + chunk.find(b"bc") + len(chunk.split(b"\x00"))
    return total + len(buf)
