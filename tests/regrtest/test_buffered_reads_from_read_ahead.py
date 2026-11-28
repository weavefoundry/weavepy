"""Sized reads of buffered binary files served from the read-ahead.

`read(n)` of an `open(..., "rb")` or `"r+b"` file returns the file's next
`n` bytes whether or not the buffer already holds them; `tell()`, `seek()`,
`peek()`, `readinto()`, `readline()` and writes between reads see the
positions a buffered reader keeps; a closed file raises; a write-only file
refuses to read; and a text wrapper over the binary file reads the same
bytes.
"""

import io
import os
import tempfile
import unittest

DATA = bytes(range(256)) * 200


class ReadAheadTests(unittest.TestCase):
    def setUp(self):
        fd, self.path = tempfile.mkstemp()
        with os.fdopen(fd, "wb") as f:
            f.write(DATA)

    def tearDown(self):
        os.unlink(self.path)

    def test_sizes(self):
        for size in (1, 7, 100, 1000, 4096, 5000, 9000, 20000):
            with open(self.path, "rb") as f:
                got = []
                while True:
                    chunk = f.read(size)
                    if not chunk:
                        break
                    self.assertLessEqual(len(chunk), size)
                    got.append(chunk)
                self.assertEqual(b"".join(got), DATA)
                self.assertEqual(f.read(10), b"")

    def test_positions(self):
        with open(self.path, "rb") as f:
            self.assertEqual(f.read(10), DATA[:10])
            self.assertEqual(f.tell(), 10)
            self.assertEqual(f.read(100), DATA[10:110])
            self.assertEqual(f.tell(), 110)
            f.seek(5)
            self.assertEqual(f.read(3), DATA[5:8])
            f.seek(-6, os.SEEK_END)
            self.assertEqual(f.read(10), DATA[-6:])
            f.seek(1000)
            self.assertEqual(f.peek(1)[:1], DATA[1000:1001])
            self.assertEqual(f.read(4), DATA[1000:1004])
            b = bytearray(8)
            self.assertEqual(f.readinto(b), 8)
            self.assertEqual(bytes(b), DATA[1004:1012])
            self.assertEqual(f.read(0), b"")
            self.assertEqual(f.tell(), 1012)
            for _ in range(500):
                pos = f.tell()
                self.assertEqual(f.read(16), DATA[pos:pos + 16])

    def test_read_write_mode(self):
        with open(self.path, "r+b") as f:
            self.assertEqual(f.read(4), DATA[:4])
            f.write(b"XYZ")
            self.assertEqual(f.tell(), 7)
            self.assertEqual(f.read(3), DATA[7:10])
            f.seek(0)
            self.assertEqual(f.read(10), DATA[:4] + b"XYZ" + DATA[7:10])
        with open(self.path, "rb") as f:
            self.assertEqual(f.read(10), DATA[:4] + b"XYZ" + DATA[7:10])

    def test_errors(self):
        f = open(self.path, "rb")
        f.read(5)
        f.close()
        with self.assertRaises(ValueError):
            f.read(5)
        with open(self.path, "ab") as f:
            with self.assertRaises(io.UnsupportedOperation):
                f.read(5)
        with open(self.path, "rb") as f:
            f.read(1)
            with self.assertRaises(ValueError):
                f.read(-2)
            self.assertEqual(len(f.read(-1)), len(DATA) - 1)

    def test_text_over_binary(self):
        with open(self.path, "rb") as f:
            t = io.TextIOWrapper(f, encoding="latin-1")
            self.assertEqual(t.read(5), DATA[:5].decode("latin-1"))
            t.detach()


if __name__ == "__main__":
    unittest.main()
