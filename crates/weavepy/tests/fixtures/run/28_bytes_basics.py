b = b"hello"
print(b)
print(len(b))
print(b[0])
print(b + b" world")

print(b.upper())
print(b.replace(b"l", b"L"))
print(b.split(b"l"))

ba = bytearray(b)
ba.append(33)
print(ba)
print(bytes(ba))

print(b"abc".hex())
print(b"a,b,c".decode("utf-8").split(","))
print(",".join(["a", "b", "c"]).encode("utf-8"))
