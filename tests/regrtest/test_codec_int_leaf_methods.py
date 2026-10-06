"""str.encode, bytes.decode, int.to_bytes, int.from_bytes and bit_length in place.

The common shapes (UTF-8, Latin-1 and ASCII without an errors handler,
machine ints, the exact int class) run without leaving the dispatch
loop; everything else (an unencodable character, another codec, an
errors handler, a subclass, keyword arguments) takes the full method,
whose results and errors must be unchanged.
"""

import unittest


class MyInt(int):
    pass


class CodecIntLeafTest(unittest.TestCase):
    def test_encode_decode(self):
        for _ in range(300):
            self.assertEqual("café".encode(), b"caf\xc3\xa9")
            self.assertEqual("café".encode("UTF_8"), b"caf\xc3\xa9")
            self.assertEqual("café".encode("latin-1"), b"caf\xe9")
            self.assertEqual(b"caf\xe9".decode("latin-1"), "café")
            self.assertEqual(b"abc".decode("ascii"), "abc")
            self.assertEqual("abc".encode("ascii"), b"abc")
        with self.assertRaises(UnicodeEncodeError):
            "café".encode("ascii")
        with self.assertRaises(UnicodeEncodeError):
            "€".encode("latin-1")
        with self.assertRaises(UnicodeDecodeError):
            b"\xff".decode("utf-8")
        with self.assertRaises(UnicodeDecodeError):
            b"\xe9".decode("ascii")
        self.assertEqual("café".encode("ascii", "replace"), b"caf?")
        self.assertEqual("café".encode("utf-16-le"), "café".encode("utf_16_le"))
        with self.assertRaises(LookupError):
            "x".encode("no-such-codec")

    def test_to_bytes_from_bytes(self):
        for _ in range(300):
            self.assertEqual((1024).to_bytes(4, "big"), b"\x00\x00\x04\x00")
            self.assertEqual((1024).to_bytes(4, "little"), b"\x00\x04\x00\x00")
            self.assertEqual((5).to_bytes(), b"\x05")
            self.assertEqual(int.from_bytes(b"\x00\x04", "big"), 4)
            self.assertEqual(int.from_bytes(b"\x00\x04", "little"), 1024)
            self.assertEqual((1023).bit_length(), 10)
            self.assertEqual((-1023).bit_length(), 10)
        self.assertEqual((-1).to_bytes(2, "big", signed=True), b"\xff\xff")
        with self.assertRaises(OverflowError):
            (256).to_bytes(1, "big")
        with self.assertRaises(OverflowError):
            (-1).to_bytes(2, "big")
        self.assertEqual(int.from_bytes(b"\xff\xff", "big", signed=True), -1)
        self.assertEqual(int.from_bytes(b"\xff" * 9, "big"), 2**72 - 1)
        v = MyInt.from_bytes(b"\x01", "big")
        self.assertIs(type(v), MyInt)
        self.assertEqual(v, 1)
        self.assertEqual((2**70).to_bytes(9, "big"), b"\x40" + b"\x00" * 8)
        self.assertEqual((0).bit_length(), 0)


if __name__ == "__main__":
    unittest.main()
