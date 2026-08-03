"""Ecosystem probe: msgpack — pack/unpack round-trip over the type
palette, streaming unpacker, and extension types."""

import msgpack

# basic round-trip
obj = {
    "int": 42,
    "big": 2**40,
    "neg": -7,
    "float": 3.5,
    "str": "héllo",
    "bytes": b"\x00\x01",
    "list": [1, [2, 3]],
    "bool": True,
    "none": None,
}
packed = msgpack.packb(obj)
assert msgpack.unpackb(packed, strict_map_key=False) == obj

# streaming unpacker sees each packed object in order
unpacker = msgpack.Unpacker()
unpacker.feed(msgpack.packb([1, 2]) + msgpack.packb({"k": "v"}))
assert list(unpacker) == [[1, 2], {"k": "v"}]

# ExtType round-trip
ext = msgpack.ExtType(4, b"payload")
out = msgpack.unpackb(msgpack.packb(ext))
assert out == ext, out

print("msgpack ok", msgpack.version)
