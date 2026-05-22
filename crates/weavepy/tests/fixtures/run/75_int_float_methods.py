# int helpers.
print((42).bit_length())
print((255).bit_count())
print((42).to_bytes(2, "big"))
print(int.from_bytes(b"\x00\x2a", "big"))
print((42).is_integer())
print((-7).as_integer_ratio())
print((0).conjugate())

# float helpers.
print((3.14).is_integer())
print((4.0).is_integer())
print((1.5).hex())
print(float.fromhex("0x1.8p+0"))
print((3.5).as_integer_ratio())

# bytes helpers.
print(bytes.fromhex("DEADBEEF"))
print(b"\xde\xad\xbe\xef".hex())
