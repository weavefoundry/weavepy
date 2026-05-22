x = 2 ** 100
print("2**100:", x)
print("2**100 + 1:", x + 1)
print("type:", type(x).__name__)
print("hex:", hex(x))
print("oct:", oct(x))
print("bin:", bin(x))
print("bit_length:", x.bit_length())

# Round-trip via to_bytes / from_bytes.
b = x.to_bytes(32, "big")
y = int.from_bytes(b, "big")
print("roundtrip int:", x == y)

# Complex arithmetic.
c = complex(1, 2)
d = complex(3, 4)
print("c+d:", c + d)
print("c*d:", c * d)
print("c.conjugate():", c.conjugate())
print("abs(c):", abs(c))
print("complex literal:", 1 + 2j)
