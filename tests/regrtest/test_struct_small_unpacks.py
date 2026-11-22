"""One- and two-value `struct` formats unpack to the tuples CPython gives.

`unpack`, `unpack_from`, `Struct.unpack` and `iter_unpack` of every integer
code, `?`, `e`, `f` and `d`, in each byte order, at offsets, from bytes,
bytearrays and memoryviews; and the errors for a wrong buffer size, an
offset past the end and a buffer that isn't a whole number of records.
"""

import struct
import unittest

CODES = "bBhHiIlLqQ?efd"
ORDERS = "@=<>!"


class SmallUnpackTests(unittest.TestCase):
    def check(self, fmt, data):
        size = struct.calcsize(fmt)
        chunk = data[:size]
        expected = struct.Struct(fmt).unpack_from(bytes(chunk))
        for buf in (bytes(chunk), bytearray(chunk), memoryview(bytes(chunk))):
            self.assertEqual(struct.unpack(fmt, buf), expected, fmt)
            self.assertEqual(struct.Struct(fmt).unpack(buf), expected, fmt)
        padded = b"\xaa" * 3 + bytes(chunk)
        self.assertEqual(struct.unpack_from(fmt, padded, 3), expected, fmt)
        self.assertEqual(struct.unpack_from(fmt, bytearray(padded), offset=3), expected, fmt)
        many = bytes(chunk) * 5
        self.assertEqual(list(struct.iter_unpack(fmt, many)), [expected] * 5, fmt)
        self.assertEqual(type(expected), tuple)
        return expected

    def test_codes(self):
        data = bytes(range(1, 40))
        for order in ORDERS:
            for code in CODES:
                for count in ("", "2"):
                    fmt = order + count + code
                    got = self.check(fmt, data)
                    self.assertEqual(len(got), 2 if count else 1)

    def test_values(self):
        self.assertEqual(struct.unpack("<I", b"\x01\x00\x00\x00"), (1,))
        self.assertEqual(struct.unpack(">h", b"\xff\xfe"), (-2,))
        self.assertEqual(struct.unpack("<2H", b"\x01\x00\x02\x00"), (1, 2))
        self.assertEqual(struct.unpack("<Q", b"\xff" * 8), (2**64 - 1,))
        self.assertEqual(struct.unpack("<q", b"\xff" * 8), (-1,))
        self.assertEqual(struct.unpack("?", b"\x02"), (True,))
        self.assertEqual(struct.unpack("<d", struct.pack("<d", 1.5)), (1.5,))
        self.assertEqual(struct.unpack("<2f", struct.pack("<2f", 0.5, -2.0)), (0.5, -2.0))
        self.assertEqual(struct.unpack("<xI", b"\x00\x05\x00\x00\x00"), (5,))
        self.assertEqual(struct.unpack("3s", b"abc"), (b"abc",))

    def test_errors(self):
        with self.assertRaises(struct.error):
            struct.unpack("<I", b"\x00\x00\x00")
        with self.assertRaises(struct.error):
            struct.unpack("<I", b"\x00" * 5)
        with self.assertRaises(struct.error):
            struct.unpack("<2H", b"\x00" * 3)
        with self.assertRaises(struct.error):
            struct.unpack_from("<I", b"\x00" * 4, 1)
        with self.assertRaises(struct.error):
            struct.iter_unpack("<I", b"\x00" * 6)
        with self.assertRaises(struct.error):
            struct.Struct("<H").unpack(b"\x00")

    def test_iterator(self):
        it = struct.iter_unpack("<H", bytes(range(8)))
        self.assertEqual(it.__length_hint__(), 4)
        self.assertEqual(next(it), (256,))
        self.assertEqual(it.__length_hint__(), 3)
        self.assertEqual(list(it), [(770,), (1284,), (1798,)])
        with self.assertRaises(StopIteration):
            next(it)
        total = 0
        for _ in range(100):
            for (v,) in struct.iter_unpack("<I", bytes(range(64))):
                total += v
        self.assertEqual(total, 100 * sum(struct.unpack("<16I", bytes(range(64)))))


if __name__ == "__main__":
    unittest.main()
