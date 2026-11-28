"""`iter(obj)` of a Python `__iter__`, and the native `struct.unpack`.

`iter()` calls a class's Python `__iter__` without a bound method; the
public `struct.unpack` reads `bytes` and `bytearray` buffers natively and
sends everything else through the Python wrapper. Results, errors and
signatures behave as before.
"""

import array
import inspect
import struct
import unittest

N = 3000


class Seq:
    def __init__(self):
        self.d = [1, 2, 3]

    def __iter__(self):
        return iter(self.d)


class Bad:
    def __iter__(self):
        return 5


class NoNext:
    def __iter__(self):
        return self


class Raises:
    def __iter__(self):
        raise KeyError("it")


class IterCallTests(unittest.TestCase):
    def test_iter(self):
        s = Seq()
        for _ in range(N):
            it = iter(s)
        self.assertEqual(list(it), [1, 2, 3])
        self.assertEqual(list(s), [1, 2, 3])
        self.assertEqual(tuple(s), (1, 2, 3))

    def test_errors(self):
        with self.assertRaises(TypeError):
            iter(Bad())
        with self.assertRaisesRegex(TypeError, "non-iterator"):
            iter(NoNext())
        with self.assertRaises(KeyError):
            iter(Raises())

    def test_instance_dict_ignored(self):
        s = Seq()
        s.__iter__ = lambda: iter([9])
        self.assertEqual(list(iter(s)), [1, 2, 3])


class NativeUnpackTests(unittest.TestCase):
    def test_buffers(self):
        pk = struct.pack("<ih", 7, -2)
        for _ in range(N):
            r = (struct.unpack("<ih", pk), struct.unpack("<ih", bytearray(pk)))
        self.assertEqual(r, ((7, -2), (7, -2)))
        self.assertEqual(struct.unpack("<ih", memoryview(pk)), (7, -2))
        self.assertEqual(struct.unpack("b", array.array("b", [3])), (3,))
        self.assertEqual(struct.unpack(b"<i", b"1234"), (875770417,))

    def test_errors(self):
        with self.assertRaises(struct.error):
            struct.unpack("<i", b"12")
        with self.assertRaises(struct.error):
            struct.unpack("<z", b"12")
        with self.assertRaises(TypeError):
            struct.unpack("<i", 5)
        with self.assertRaises(TypeError):
            struct.unpack("<i")

    def test_signature(self):
        self.assertEqual(str(inspect.signature(struct.unpack)), "(format, buffer, /)")


if __name__ == "__main__":
    unittest.main()
