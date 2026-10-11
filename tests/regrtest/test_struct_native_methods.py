"""`struct.Struct.pack`/`unpack` served natively keep the class's rules.

The exact class's methods pack plain values and unpack `bytes` without
running the Python methods, which remain the fallback: subclasses,
`__index__` values, non-`bytes` buffers, an uninitialized or
re-initialized `Struct`, and every error must answer as they do.
"""

import struct
import unittest


class StructNativeTest(unittest.TestCase):
    def test_round_trip(self):
        s = struct.Struct("<IhdB3s")
        for i in range(2000):
            packed = s.pack(i, -i & 0x7FFF, i * 0.5, i & 255, b"abc")
            self.assertEqual(packed, struct.pack("<IhdB3s", i, -i & 0x7FFF, i * 0.5, i & 255, b"abc"))
            self.assertEqual(s.unpack(packed), (i, -i & 0x7FFF, i * 0.5, i & 255, b"abc"))
        self.assertEqual(s.unpack(bytearray(packed)), s.unpack(packed))
        self.assertEqual(s.unpack(memoryview(packed)), s.unpack(packed))

    def test_errors(self):
        s = struct.Struct("<hB")
        with self.assertRaises(struct.error):
            s.pack(1)
        with self.assertRaises(struct.error):
            s.pack(1, 256)
        with self.assertRaises(struct.error):
            s.pack("x", 1)
        with self.assertRaises(struct.error):
            s.unpack(b"\x00")
        with self.assertRaises(TypeError):
            s.unpack("abc")

    def test_fallback_shapes(self):
        class Index:
            def __index__(self):
                return 7

        class Sub(struct.Struct):
            pass

        self.assertEqual(struct.Struct("<h").pack(Index()), b"\x07\x00")
        self.assertEqual(Sub("<h").pack(5), b"\x05\x00")
        self.assertEqual(Sub("<h").unpack(b"\x05\x00"), (5,))
        blank = struct.Struct.__new__(struct.Struct)
        with self.assertRaises(RuntimeError):
            blank.pack(1)
        s = struct.Struct("<h")
        self.assertEqual(s.pack(1), b"\x01\x00")
        s.__init__(">h")
        self.assertEqual(s.pack(1), b"\x00\x01")
        self.assertEqual(s.unpack(b"\x00\x01"), (1,))
        self.assertEqual(s.format, ">h")


if __name__ == "__main__":
    unittest.main()
