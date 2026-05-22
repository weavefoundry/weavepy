import pickle, marshal

# pickle round-trips for the universal subset.
cases = [
    None, True, False, 0, 42, -7, 2 ** 100, -(2 ** 100),
    1.5, "hello", b"\x00\x01\x02",
    [], [1, 2, 3], {"a": 1}, (1, 2, 3),
    {1, 2, 3}, frozenset({1, 2}),
]
for c in cases:
    blob = pickle.dumps(c)
    back = pickle.loads(blob)
    print(repr(c), "==", back == c)

# marshal round-trips.
m = marshal.dumps([1, "x", (3.14, b"y"), 2 ** 64])
print("marshal ok:", marshal.loads(m) == [1, "x", (3.14, b"y"), 2 ** 64])
