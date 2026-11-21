"""`zlib.crc32`/`adler32` and `hashlib` over buffers read in place.

A `bytes`, `bytearray` or contiguous `memoryview` is read where it is; a
released view raises `ValueError` and a strided one `BufferError`, as
CPython's `PyBUF_SIMPLE` consumers do. `adler32` reduces per block (and its
starting value even for no data).
"""

import hashlib
import unittest
import zlib


DATA = bytes(range(256)) * 300


class ChecksumTests(unittest.TestCase):
    def sources(self):
        return [DATA, bytearray(DATA), memoryview(DATA), memoryview(bytearray(DATA))[7:]]

    def test_crc32_and_adler32(self):
        expected = [(zlib.crc32(bytes(s)), zlib.adler32(bytes(s))) for s in self.sources()]
        for _ in range(20):
            got = [(zlib.crc32(s), zlib.adler32(s)) for s in self.sources()]
            self.assertEqual(got, expected)
        self.assertEqual(zlib.crc32(b"hello", 12345), zlib.crc32(b"lo", zlib.crc32(b"hel", 12345)))
        self.assertEqual(zlib.adler32(b"hello"), 0x062C0215)
        self.assertEqual(zlib.adler32(b"", 0xFFFFFFFF), 917518)
        self.assertEqual(zlib.adler32(b"", 65530), 9)
        self.assertEqual(zlib.adler32(b"\xff" * 100000, 0xFFF0FFF0), zlib.adler32(b"\xff" * 50000, zlib.adler32(b"\xff" * 50000, 0xFFF0FFF0)))

    def test_hashes(self):
        want = hashlib.sha256(DATA).hexdigest()
        for s in self.sources()[:3]:
            self.assertEqual(hashlib.sha256(s).hexdigest(), want)
            h = hashlib.sha256()
            h.update(s[:1000])
            h.update(s[1000:])
            self.assertEqual(h.hexdigest(), want)
        self.assertEqual(hashlib.md5(bytearray(b"abc")).hexdigest(), "900150983cd24fb0d6963f7d28e17f72")

    def test_buffer_errors(self):
        released = memoryview(b"x")
        released.release()
        strided = memoryview(bytearray(b"abcdef"))[::2]
        for f in (zlib.crc32, zlib.adler32, hashlib.md5, lambda v: hashlib.sha1().update(v), zlib.compress):
            with self.assertRaises(ValueError):
                f(released)
            with self.assertRaises(BufferError):
                f(strided)
        with self.assertRaises(TypeError):
            zlib.crc32("text")


if __name__ == "__main__":
    unittest.main()
