"""Ecosystem probe: numpy (PyPI wheel) — dtype arithmetic, broadcasting,
linalg, and a buffer round-trip through memoryview."""

import numpy as np

# dtype arithmetic
a = np.array([1, 2, 3], dtype=np.int64)
b = np.array([0.5, 1.5, 2.5], dtype=np.float64)
c = a + b
assert c.dtype == np.float64, c.dtype
assert c.tolist() == [1.5, 3.5, 5.5], c.tolist()

# broadcasting
m = np.arange(6).reshape(2, 3)
col = np.array([[10], [20]])
out = m + col
assert out.shape == (2, 3)
assert out.tolist() == [[10, 11, 12], [23, 24, 25]], out.tolist()

# reductions + linalg
assert m.sum() == 15 and m.sum(axis=0).tolist() == [3, 5, 7]
v = np.array([3.0, 4.0])
assert abs(np.linalg.norm(v) - 5.0) < 1e-12
eye = np.eye(3)
assert np.allclose(eye @ eye, eye)

# buffer round-trip via memoryview
buf = memoryview(a)
assert buf.format in ("l", "q"), buf.format
assert buf.itemsize == 8 and list(buf) == [1, 2, 3]
back = np.frombuffer(bytes(buf), dtype=np.int64)
assert np.array_equal(back, a)

# fancy indexing + boolean masks
x = np.arange(10)
assert x[x % 2 == 0].tolist() == [0, 2, 4, 6, 8]
assert x[[9, 0, 3]].tolist() == [9, 0, 3]

print("numpy ok", np.__version__)
