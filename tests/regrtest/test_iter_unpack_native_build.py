"""`struct.iter_unpack` built natively over a bytes or bytearray buffer, and
`list()` of it: the iterator is the module's `unpack_iterator`, counts its
records in `__length_hint__`, drains to the records `unpack_from` reads,
and stays finished; a buffer the size doesn't divide, a zero-size format,
a bad format, a view, a bytes subclass and an array take the Python path
with its errors and results."""

import array
import struct
import unittest

DATA = bytes(range(64))


class B(bytes):
    pass


def make_and_drain(n, fmt, buf):
    out = None
    for i in range(n):
        out = list(struct.iter_unpack(fmt, buf))
    return out


class IterUnpackNativeBuildTests(unittest.TestCase):
    def test_drain(self):
        for fmt in ("<I", ">H", "<hh", "4s", "<d", "B"):
            size = struct.calcsize(fmt)
            buf = DATA[: len(DATA) // size * size]
            want = [struct.unpack_from(fmt, buf, at) for at in range(0, len(buf), size)]
            for b in (buf, bytearray(buf), memoryview(buf), B(buf)):
                for n in (1, 300):
                    self.assertEqual(make_and_drain(n, fmt, b), want, (fmt, type(b)))

    def test_iterator(self):
        for _ in range(300):
            it = struct.iter_unpack("<H", DATA[:10])
            self.assertEqual(type(it).__name__, "unpack_iterator")
            self.assertIs(iter(it), it)
            self.assertEqual(it.__length_hint__(), 5)
            self.assertEqual(next(it), (0x0100,))
            self.assertEqual(it.__length_hint__(), 4)
            self.assertEqual(len(list(it)), 4)
            self.assertEqual(it.__length_hint__(), 0)
            self.assertEqual(list(it), [])
            with self.assertRaises(StopIteration):
                next(it)

    def test_errors(self):
        for _ in range(300):
            with self.assertRaises(struct.error):
                struct.iter_unpack("<I", b"abc")
            with self.assertRaises(struct.error):
                struct.iter_unpack("", b"")
            with self.assertRaises(struct.error):
                struct.iter_unpack("<Q!", b"12345678")
            with self.assertRaises(TypeError):
                struct.iter_unpack("<I", "abcd")

    def test_array(self):
        a = array.array("I", [1, 2, 3])
        for _ in range(300):
            self.assertEqual(list(struct.iter_unpack("=I", a)), [(1,), (2,), (3,)])


if __name__ == "__main__":
    unittest.main()
