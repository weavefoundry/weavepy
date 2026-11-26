"""BytesIO writes and getvalue() from compiled loops (read through views
of the buffer): values after writes and seeks, the cached value reused
until a write, an exported getbuffer() view (the full path), a closed
stream, and BytesIO(initial) returning its bytes, each matching
CPython."""

import io
import unittest


def churn(n):
    out = None
    for i in range(n):
        b = io.BytesIO()
        b.write(b"ab")
        b.write(b"cd")
        out = (b.getvalue(), b.getvalue(), b.tell())
    return out


class BytesIOTests(unittest.TestCase):
    def test_writes_and_values(self):
        for n in (1, 300):
            self.assertEqual(churn(n), (b"abcd", b"abcd", 4))
        b = io.BytesIO()
        for i in range(300):
            b.write(b"x")
            v = b.getvalue()
        self.assertEqual(v, b"x" * 300)
        b.seek(1)
        b.write(b"yy")
        self.assertEqual(b.getvalue()[:4], b"xyyx")
        b.seek(305)
        b.write(b"z")
        self.assertEqual(b.getvalue()[-7:], b"x\0\0\0\0\0z")

    def test_cached_and_initial(self):
        init = b"seed"
        b = io.BytesIO(init)
        self.assertIs(b.getvalue(), init)
        v1 = b.getvalue()
        self.assertIs(b.getvalue(), v1)
        b.seek(0, 2)
        b.write(b"!")
        self.assertEqual(b.getvalue(), b"seed!")

    def test_exported_and_closed(self):
        b = io.BytesIO(b"abc")
        m = b.getbuffer()
        m[0] = ord("z")
        self.assertEqual(b.getvalue(), b"zbc")
        with self.assertRaises(BufferError):
            b.write(b"q")
        del m
        b.write(b"q")
        self.assertEqual(b.getvalue(), b"qbc")
        b.close()
        with self.assertRaises(ValueError):
            b.getvalue()
        with self.assertRaises(ValueError):
            b.write(b"x")


if __name__ == "__main__":
    unittest.main()
