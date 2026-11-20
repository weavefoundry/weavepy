"""`io.BytesIO` reads, `readinto` and writes, at any position.

A read copies its bytes once from the stream, `readinto` copies them
straight into the caller's buffer, and a write after a seek writes over
the bytes there, growing the stream if it runs past the end. Positions,
return values, closed streams, exported buffers and a stream read into
its own buffer behave as the documented methods do.
"""

import io
import unittest


class ReadTests(unittest.TestCase):
    def test_reads(self):
        data = bytes(range(256)) * 4
        f = io.BytesIO(data)
        self.assertEqual(f.read(10), data[:10])
        self.assertEqual(f.tell(), 10)
        self.assertEqual(f.read(0), b"")
        self.assertEqual(f.tell(), 10)
        self.assertEqual(f.read(-1)[:5], data[10:15])
        self.assertEqual(f.tell(), len(data))
        self.assertEqual(f.read(5), b"")
        f.seek(1000)
        self.assertEqual(f.read(100), data[1000:])
        f.seek(5000)
        self.assertEqual(f.read(4), b"")
        self.assertEqual(f.tell(), 5000)
        f.seek(3)
        self.assertEqual(f.read(None), data[3:])
        f.seek(0)
        self.assertEqual(f.read(), data)
        self.assertIs(type(f.read()), bytes)
        f.seek(-4, io.SEEK_END)
        self.assertEqual(f.read(-7), data[-4:])

    def test_a_read_is_a_copy(self):
        f = io.BytesIO(b"abcdef")
        first = f.read(3)
        f.seek(0)
        f.write(b"XYZ")
        self.assertEqual(first, b"abc")
        f.seek(0)
        self.assertEqual(f.read(), b"XYZdef")

    def test_readinto(self):
        data = bytes(range(100))
        f = io.BytesIO(data)
        buf = bytearray(30)
        self.assertEqual(f.readinto(buf), 30)
        self.assertEqual(buf, data[:30])
        view = memoryview(bytearray(50))
        self.assertEqual(f.readinto(view), 50)
        self.assertEqual(view.tobytes(), data[30:80])
        self.assertEqual(f.readinto(buf), 20)
        self.assertEqual(buf[:20], data[80:])
        self.assertEqual(buf[20:], data[20:30])
        self.assertEqual(f.readinto(buf), 0)
        self.assertEqual(f.tell(), 100)
        f.seek(0)
        self.assertEqual(f.readinto(bytearray()), 0)
        self.assertEqual(f.tell(), 0)
        with self.assertRaises(TypeError):
            f.readinto(b"read-only")
        with self.assertRaises(TypeError):
            f.readinto(memoryview(b"read-only"))
        exported = bytearray(5)
        held = memoryview(exported)
        self.assertEqual(f.readinto(exported), 5)
        self.assertEqual(held.tobytes(), data[:5])
        held.release()

    def test_readinto_a_strided_or_released_view(self):
        f = io.BytesIO(bytes(range(20)))
        with self.assertRaises(TypeError):
            f.readinto(memoryview(bytearray(10))[::2])
        released = memoryview(bytearray(4))
        released.release()
        with self.assertRaises((TypeError, ValueError)):
            f.readinto(released)
        self.assertEqual(f.tell(), 0)

    def test_reading_into_the_streams_own_buffer(self):
        f = io.BytesIO(b"0123456789")
        view = f.getbuffer()
        with self.assertRaises(BufferError):
            f.write(b"x")
        n = f.readinto(view[5:])
        self.assertEqual(n, 5)
        self.assertEqual(bytes(view), b"0123401234")
        view.release()
        self.assertEqual(f.getvalue(), b"0123401234")

    def test_closed_streams(self):
        f = io.BytesIO(b"abc")
        f.close()
        for op in (lambda: f.read(), lambda: f.read(1), lambda: f.readinto(bytearray(2)), lambda: f.write(b"x")):
            with self.assertRaises(ValueError):
                op()


class WriteTests(unittest.TestCase):
    def test_appends(self):
        f = io.BytesIO()
        for i in range(500):
            self.assertEqual(f.write(bytes([i & 0xFF]) * 3), 3)
        self.assertEqual(f.tell(), 1500)
        self.assertEqual(f.getvalue(), b"".join(bytes([i & 0xFF]) * 3 for i in range(500)))
        self.assertEqual(f.write(b""), 0)
        self.assertEqual(f.write(bytearray(b"ba")), 2)
        self.assertEqual(f.write(memoryview(b"mv")), 2)
        self.assertEqual(f.getvalue()[-4:], b"bamv")

    def test_writes_after_a_seek(self):
        for start, data in ((0, b"XY"), (3, b"XY"), (8, b"XY"), (9, b"XY"), (10, b"XY"), (4, b"LONGER-THAN-REST")):
            with self.subTest(start=start, data=data):
                f = io.BytesIO(b"0123456789")
                f.seek(start)
                self.assertEqual(f.write(data), len(data))
                want = bytearray(b"0123456789")
                want[start:start + len(data)] = data
                self.assertEqual(f.getvalue(), bytes(want))
                self.assertEqual(f.tell(), start + len(data))
        f = io.BytesIO(b"abc")
        f.seek(6)
        f.write(b"Z")
        self.assertEqual(f.getvalue(), b"abc\x00\x00\x00Z")
        f = io.BytesIO(b"abcdef")
        f.seek(2)
        f.write(bytearray(b"--"))
        self.assertEqual(f.getvalue(), b"ab--ef")

    def test_a_stream_written_into_itself(self):
        f = io.BytesIO(b"abc")
        f.seek(0, io.SEEK_END)
        f.write(f.getvalue())
        self.assertEqual(f.getvalue(), b"abcabc")
        f.seek(1)
        f.write(f.getbuffer().tobytes())
        self.assertEqual(f.getvalue(), b"aabcabc")

    def test_an_exported_stream_refuses_writes(self):
        f = io.BytesIO(b"abc")
        view = f.getbuffer()
        for data in (b"x", bytearray(b"x")):
            with self.assertRaises(BufferError):
                f.write(data)
        f.seek(0)
        with self.assertRaises(BufferError):
            f.write(b"Z")
        view.release()
        f.write(b"Z")
        self.assertEqual(f.getvalue(), b"Zbc")

    def test_wrong_types(self):
        f = io.BytesIO()
        with self.assertRaises(TypeError):
            f.write("text")
        with self.assertRaises(TypeError):
            f.write(5)
        self.assertEqual(f.getvalue(), b"")

    def test_loops(self):
        f = io.BytesIO()
        chunk = bytearray(b"0123456789abcdef")
        for i in range(3000):
            f.write(chunk)
            if i % 500 == 499:
                f.seek(0)
        f.seek(0)
        total = 0
        buf = bytearray(16)
        while True:
            n = f.readinto(buf)
            if not n:
                break
            total += n
        self.assertEqual(total, 500 * 16)
        f.seek(0)
        parts = []
        while True:
            part = f.read(7)
            if not part:
                break
            parts.append(part)
        self.assertEqual(b"".join(parts), bytes(chunk) * 500)


if __name__ == "__main__":
    unittest.main()
