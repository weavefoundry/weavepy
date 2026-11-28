"""binascii's encoders and decoders called positionally from compiled
loops: bytes, bytearray and str inputs, a memoryview (the full call), bad
input's errors, crc32 with and without a start value, and base64's
wrappers, each matching CPython."""

import base64
import binascii
import unittest


def run(n, f, *args):
    out = None
    for i in range(n):
        out = f(*args)
    return out


class BinasciiLeafTests(unittest.TestCase):
    def test_round_trips(self):
        for n in (1, 300):
            self.assertEqual(run(n, binascii.b2a_base64, b"hello"), b"aGVsbG8=\n")
            self.assertEqual(run(n, binascii.b2a_base64, bytearray(b"hi")), b"aGk=\n")
            self.assertEqual(run(n, binascii.b2a_base64, memoryview(b"hi")), b"aGk=\n")
            self.assertEqual(run(n, binascii.a2b_base64, b"aGVsbG8="), b"hello")
            self.assertEqual(run(n, binascii.a2b_base64, "aGk="), b"hi")
            self.assertEqual(run(n, binascii.unhexlify, "6869"), b"hi")
            self.assertEqual(run(n, binascii.a2b_hex, b"6869"), b"hi")
            self.assertEqual(run(n, binascii.crc32, b"abc"), 891568578)
            self.assertEqual(run(n, binascii.crc32, b"abc", 1), binascii.crc32(b"abc", 1))
            self.assertEqual(run(n, base64.b64encode, b"hello"), b"aGVsbG8=")
            self.assertEqual(run(n, base64.b64decode, b"aGVsbG8="), b"hello")

    def test_errors(self):
        with self.assertRaises(binascii.Error):
            run(300, binascii.unhexlify, "zz")
        with self.assertRaises(binascii.Error):
            run(300, binascii.a2b_base64, b"a")
        with self.assertRaises(TypeError):
            run(300, binascii.b2a_base64, "text")
        with self.assertRaises(ValueError):
            run(300, binascii.a2b_base64, "\\u00e9")


if __name__ == "__main__":
    unittest.main()
