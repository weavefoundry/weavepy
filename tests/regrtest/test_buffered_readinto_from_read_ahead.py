"""`readinto` of buffered binary files reads the bytes at the position.

Targets of `bytearray`, `memoryview` (of a `bytearray` and a slice of one)
and of several sizes (small, half the buffer, past it), files opened with
the default buffer, a small one and none, interleaved with `read`,
`readline`, `seek`, `tell` and writes in `r+b` mode, and the short count at
the end of the file, all agree with the file's bytes, past the compile
threshold.
"""

import os
import tempfile
import unittest

DATA = bytes((i * 131 + 7) % 256 for i in range(70000)).replace(b"\n", b"n") + b"\nlast line\n"


class ReadintoTests(unittest.TestCase):
    def setUp(self):
        fd, self.path = tempfile.mkstemp()
        with os.fdopen(fd, "wb") as f:
            f.write(DATA)

    def tearDown(self):
        os.unlink(self.path)

    def walk(self, size, buffering, make):
        with open(self.path, "rb", buffering=buffering) as f:
            pos = 0
            target = make(size)
            while True:
                n = f.readinto(target)
                want = DATA[pos:pos + size]
                self.assertEqual(n, len(want), (size, buffering, pos))
                self.assertEqual(bytes(target[:n]), want, (size, buffering, pos))
                pos += n
                self.assertEqual(f.tell(), pos)
                if n < size:
                    break
            self.assertEqual(f.readinto(target), 0)

    def test_sizes_and_targets(self):
        makes = [bytearray, lambda n: memoryview(bytearray(n)),
                 lambda n: memoryview(bytearray(n + 6))[3:-3]]
        for buffering in (-1, 1024, 0):
            for size in (7, 512, 4096, 5000, 70001):
                for make in makes:
                    self.walk(size, buffering, make)

    def test_interleaved(self):
        for _ in range(30):
            with open(self.path, "rb") as f:
                b = bytearray(100)
                self.assertEqual(f.readinto(b), 100)
                self.assertEqual(bytes(b), DATA[:100])
                self.assertEqual(f.read(50), DATA[100:150])
                self.assertEqual(f.readinto(b), 100)
                self.assertEqual(bytes(b), DATA[150:250])
                f.seek(69990)
                self.assertEqual(f.readinto(b), len(DATA) - 69990)
                self.assertEqual(bytes(b[:len(DATA) - 69990]), DATA[69990:])
                f.seek(len(DATA) - 10)
                line = f.readline()
                self.assertEqual(line, DATA[len(DATA) - 10:])
                f.seek(10)
                self.assertEqual(f.readinto(b), 100)
                self.assertEqual(bytes(b), DATA[10:110])
                self.assertEqual(f.tell(), 110)

    def test_after_write(self):
        with open(self.path, "r+b") as f:
            b = bytearray(64)
            f.readinto(b)
            f.write(b"WXYZ")
            f.seek(60)
            self.assertEqual(f.readinto(b), 64)
            self.assertEqual(bytes(b[:8]), DATA[60:64] + b"WXYZ")
            self.assertEqual(f.tell(), 124)

    def test_exported_target(self):
        with open(self.path, "rb") as f:
            b = bytearray(32)
            view = memoryview(b)
            self.assertEqual(f.readinto(b), 32)
            self.assertEqual(bytes(view), DATA[:32])
            view.release()


if __name__ == "__main__":
    unittest.main()
