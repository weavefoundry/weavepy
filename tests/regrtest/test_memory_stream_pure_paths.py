"""`BytesIO.write` at the stream's end and `getvalue()` run in place.

The in-place halves append the bytes and copy the contents as the full
methods do; a write anywhere else, into an exported buffer, or on a
closed stream keeps the full method and its errors.
"""

import io
import unittest

N = 3000


class MemoryStreamPurePathTests(unittest.TestCase):
    def test_appends_and_values(self):
        for _ in range(N):
            b = io.BytesIO()
            n = b.write(b"abc")
            b.write(b"de")
            v = b.getvalue()
        self.assertEqual((n, v, b.tell()), (3, b"abcde", 5))

    def test_overwrite_after_seek(self):
        for _ in range(N):
            b = io.BytesIO(b"hello")
            b.seek(1)
            b.write(b"EY")
            b.seek(0, 2)
            b.write(b"!")
        self.assertEqual(b.getvalue(), b"hEYlo!")

    def test_overseek_fills_gap(self):
        b = io.BytesIO(b"ab")
        b.seek(4)
        for _ in range(N):
            pass
        b.write(b"z")
        self.assertEqual(b.getvalue(), b"ab\x00\x00z")

    def test_export_refuses_write(self):
        b = io.BytesIO(b"ab")
        b.seek(0, 2)
        view = b.getbuffer()
        for _ in range(100):
            with self.assertRaises(BufferError):
                b.write(b"x")
        view.release()
        b.write(b"x")
        self.assertEqual(b.getvalue(), b"abx")

    def test_closed_streams_raise(self):
        b = io.BytesIO()
        s = io.StringIO("q")
        b.close()
        s.close()
        for _ in range(100):
            with self.assertRaises(ValueError):
                b.write(b"x")
            with self.assertRaises(ValueError):
                b.getvalue()
            with self.assertRaises(ValueError):
                s.getvalue()

    def test_string_io_value(self):
        for _ in range(N):
            s = io.StringIO()
            s.write("ab")
            v = s.getvalue()
        self.assertEqual(v, "ab")

    def test_subclass_write_override(self):
        class B(io.BytesIO):
            def write(self, data):
                return super().write(data.upper())

        for _ in range(N):
            b = B()
            b.write(b"ab")
        self.assertEqual(b.getvalue(), b"AB")


if __name__ == "__main__":
    unittest.main()
