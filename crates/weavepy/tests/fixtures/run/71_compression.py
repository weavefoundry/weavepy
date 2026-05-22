import gzip, bz2, lzma

data = b"hello world! " * 50

g = gzip.compress(data)
print("gzip ok:", gzip.decompress(g) == data)
print("gzip smaller:", len(g) < len(data))

b = bz2.compress(data)
print("bz2 ok:", bz2.decompress(b) == data)

l = lzma.compress(data)
print("lzma ok:", lzma.decompress(l) == data)
