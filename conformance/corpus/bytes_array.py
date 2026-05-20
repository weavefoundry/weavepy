ba = bytearray(b"abc")
ba.append(100)
ba.extend(b"ef")
print(bytes(ba))
print(len(ba))
ba[0] = 65
print(bytes(ba))
