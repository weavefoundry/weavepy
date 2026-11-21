"""`memoryview.tobytes`, strided gathers, and `bytearray.extend` of ints.

A contiguous view's bytes are copied once into the result; a strided
one-dimensional view is gathered in one read of the buffer; a list or
tuple of ints in `range(256)` extends a bytearray natively, with every
other item still converted or refused as the full method does.
"""

import unittest


class ToBytesTests(unittest.TestCase):
    def test_contiguous_and_strided(self):
        b = bytearray(range(256)) * 4
        mv = memoryview(b)
        for _ in range(50):
            self.assertEqual(mv.tobytes(), bytes(b))
            self.assertEqual(mv[::2].tobytes(), bytes(b[::2]))
            self.assertEqual(mv[::-1].tobytes(), bytes(b[::-1]))
            self.assertEqual(mv[10:100:7].tobytes(), bytes(b[10:100:7]))
            self.assertEqual(mv[5:5].tobytes(), b"")
        ints = mv.cast("I")
        self.assertEqual(ints[::2].tobytes(), b"".join(ints[i:i + 1].tobytes() for i in range(0, len(ints), 2)))
        self.assertEqual(mv.cast("H")[::-1].tolist()[:2], [0xFFFE, 0xFDFC])

    def test_orders(self):
        mv = memoryview(bytes(range(12))).cast("B", shape=[3, 4])
        self.assertEqual(mv.tobytes(), bytes(range(12)))
        self.assertEqual(mv.tobytes("F"), bytes([0, 4, 8, 1, 5, 9, 2, 6, 10, 3, 7, 11]))
        with self.assertRaises(ValueError):
            mv.tobytes("X")


class ExtendTests(unittest.TestCase):
    def test_values(self):
        cases = [([1, 2, 3], b"ab\x01\x02\x03"), ((4, 5), b"ab\x04\x05"), ([], b"ab"), ([255, 0], b"ab\xff\x00"),
                 ([True, 2], b"ab\x01\x02"), (range(3), b"ab\x00\x01\x02"), (b"xy", b"abxy")]
        for _ in range(100):
            for data, want in cases:
                b = bytearray(b"ab")
                b.extend(data)
                self.assertEqual(b, want)
            b = bytearray(b"ab")
            b.extend(iter([7, 8]))
            self.assertEqual(b, b"ab\x07\x08")

    def test_errors_leave_it_unchanged(self):
        for data, exc in (([256], ValueError), ([-1], ValueError), ([1, "x"], TypeError), ([1.0], TypeError)):
            b = bytearray(b"ab")
            with self.assertRaises(exc):
                b.extend(data)
            self.assertEqual(b, b"ab")

    def test_exported(self):
        b = bytearray(b"q")
        v = memoryview(b)
        with self.assertRaises(BufferError):
            b.extend([1])
        v.release()
        b.extend([1])
        self.assertEqual(b, b"q\x01")


if __name__ == "__main__":
    unittest.main()
