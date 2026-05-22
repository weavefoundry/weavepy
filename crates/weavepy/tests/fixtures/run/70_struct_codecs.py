import struct, codecs

# struct: pack/unpack round-trip.
buf = struct.pack(">Ihf", 0x12345678, -1, 3.14)
print("struct len:", len(buf))
print("unpack:", struct.unpack(">Ihf", buf))
print("calcsize:", struct.calcsize(">Ihf"))

# struct.Struct pre-compiled.
s = struct.Struct("<3sBI")
print("s.size:", s.size)
print("packed:", s.pack(b"abc", 1, 65535).hex())

# codecs: ascii / latin-1.
print("ascii:", codecs.encode("hello", "ascii"))
print("latin-1:", codecs.encode("\xe9", "latin-1"))

# UTF-8 with BOM constants.
print("BOM_UTF8:", codecs.BOM_UTF8.hex())

# UTF-16 round-trip.
b = "WeavePy".encode("utf-16-le")
print("utf-16-le bytes:", len(b))
print("decoded:", b.decode("utf-16-le"))
