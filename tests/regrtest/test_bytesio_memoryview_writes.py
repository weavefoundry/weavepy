"""`BytesIO.write` of memoryviews.

Views of bytes, bytearrays and arrays, whole or sliced, of any format,
write their bytes and return the byte count; a stepped view raises
`BufferError` and a released one `ValueError`; a view of the stream's own buffer
blocks the write with `BufferError` while it lives; writes after a seek
overwrite in place.
"""

import array
import io
import unittest


class MemoryviewWriteTests(unittest.TestCase):
    def test_plain_views(self):
        data = bytes(range(256))
        f = io.BytesIO()
        mv = memoryview(data)
        total = 0
        for i in range(0, 256, 16):
            total += f.write(mv[i:i + 16])
        self.assertEqual(total, 256)
        self.assertEqual(f.getvalue(), data)
        self.assertEqual(f.write(memoryview(bytearray(b"xyz"))), 3)
        self.assertEqual(f.write(memoryview(b"")), 0)
        self.assertEqual(f.getvalue(), data + b"xyz")

    def test_other_layouts(self):
        f = io.BytesIO()
        a = array.array("i", [1, 2, 3])
        self.assertEqual(f.write(memoryview(a)), a.itemsize * 3)
        self.assertEqual(f.getvalue(), a.tobytes())
        f = io.BytesIO()
        with self.assertRaises(BufferError):
            f.write(memoryview(b"abcdef")[::2])
        self.assertEqual(f.getvalue(), b"")
        f = io.BytesIO()
        self.assertEqual(f.write(memoryview(b"abcd").cast("c")), 4)
        self.assertEqual(f.write(memoryview(b"abcd").cast("b")), 4)
        self.assertEqual(f.getvalue(), b"abcdabcd")

    def test_seek_and_overwrite(self):
        f = io.BytesIO(b"0123456789")
        f.seek(2)
        self.assertEqual(f.write(memoryview(b"ab")), 2)
        self.assertEqual(f.getvalue(), b"01ab456789")
        f.seek(12)
        f.write(memoryview(b"Z"))
        self.assertEqual(f.getvalue(), b"01ab456789\x00\x00Z")

    def test_errors(self):
        f = io.BytesIO()
        mv = memoryview(b"abc")
        mv.release()
        with self.assertRaises(ValueError):
            f.write(mv)
        g = io.BytesIO(b"hello")
        view = g.getbuffer()
        with self.assertRaises(BufferError):
            g.write(view[:2])
        with self.assertRaises(BufferError):
            g.write(b"x")
        view.release()
        g.seek(0, 2)
        self.assertEqual(g.write(memoryview(b"!")), 1)
        self.assertEqual(g.getvalue(), b"hello!")
        f.close()
        with self.assertRaises(ValueError):
            f.write(memoryview(b"x"))

    def test_view_of_another_stream(self):
        src = io.BytesIO(b"source")
        dst = io.BytesIO()
        with src.getbuffer() as view:
            self.assertEqual(dst.write(view[1:4]), 3)
        self.assertEqual(dst.getvalue(), b"our")
        src.write(b"more")


if __name__ == "__main__":
    unittest.main()
