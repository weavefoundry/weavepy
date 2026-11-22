"""`BytesIO.tell()` and `seek()` keep the positions CPython keeps.

Absolute seeks inside and past the end (a later write zero-fills the gap),
relative and end-relative seeks, `True`/`False` offsets, a negative
absolute offset (`ValueError`), an unknown whence, a closed stream, a live
`getbuffer()` view, subclasses, and `tell()` after reads, writes,
`readline` and `truncate`, run past the compile threshold.
"""

import io
import unittest


class TellSeekTests(unittest.TestCase):
    def test_positions(self):
        for _ in range(300):
            f = io.BytesIO(b"0123456789")
            self.assertEqual(f.tell(), 0)
            self.assertEqual(f.seek(4), 4)
            self.assertEqual(f.tell(), 4)
            self.assertEqual(f.read(2), b"45")
            self.assertEqual(f.tell(), 6)
            self.assertEqual(f.seek(0, 0), 0)
            self.assertEqual(f.seek(3, 1), 3)
            self.assertEqual(f.seek(-2, 2), 8)
            self.assertEqual(f.seek(-100, 1), 0)
            self.assertEqual(f.seek(True), 1)
            self.assertEqual(f.seek(False), 0)
            self.assertEqual(f.seek(20), 20)
            self.assertEqual(f.read(), b"")
            f.write(b"!")
            self.assertEqual(f.getvalue(), b"0123456789" + b"\0" * 10 + b"!")
            self.assertEqual(f.tell(), 21)

    def test_errors(self):
        f = io.BytesIO(b"abc")
        with self.assertRaises(ValueError):
            f.seek(-1)
        with self.assertRaises(ValueError):
            f.seek(0, 3)
        with self.assertRaises(TypeError):
            f.seek(1.5)
        with self.assertRaises(TypeError):
            f.seek()
        with self.assertRaises(OverflowError):
            f.seek(2**70)
        f.close()
        with self.assertRaises(ValueError):
            f.tell()
        with self.assertRaises(ValueError):
            f.seek(0)

    def test_with_view_and_lines(self):
        f = io.BytesIO(b"one\ntwo\nthree\n")
        view = f.getbuffer()
        self.assertEqual(f.seek(4), 4)
        self.assertEqual(f.readline(), b"two\n")
        self.assertEqual(f.tell(), 8)
        view.release()
        f.seek(0)
        self.assertEqual(f.readlines(), [b"one\n", b"two\n", b"three\n"])
        self.assertEqual(f.tell(), 14)
        f.seek(3)
        f.truncate()
        self.assertEqual(f.tell(), 3)
        self.assertEqual(f.getvalue(), b"one")

    def test_subclass(self):
        class B(io.BytesIO):
            seeks = 0

            def seek(self, *a):
                B.seeks += 1
                return super().seek(*a)

        f = B(b"abcdef")
        for _ in range(100):
            self.assertEqual(f.seek(2), 2)
            self.assertEqual(f.tell(), 2)
        self.assertEqual(B.seeks, 100)

    def test_text_layer(self):
        raw = io.BytesIO("héllo\nwörld\n".encode())
        t = io.TextIOWrapper(raw, encoding="utf-8")
        self.assertEqual(t.readline(), "héllo\n")
        pos = t.tell()
        self.assertEqual(t.readline(), "wörld\n")
        t.seek(pos)
        self.assertEqual(t.readline(), "wörld\n")
        t.detach()


if __name__ == "__main__":
    unittest.main()
