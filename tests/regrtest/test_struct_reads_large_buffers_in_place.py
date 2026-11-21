"""`struct.unpack_from` and `struct.pack_into` on a large buffer.

Unpacking a few bytes at an offset of a `bytes`, `bytearray` or
`memoryview` reads those bytes where they are, and packing writes them
where they go: neither copies the buffer, so a parser that walks a large
buffer field by field costs its fields, not its fields times the buffer.
Offsets, keyword arguments, other buffer kinds and every error behave as
the documented functions do.
"""

import array
import struct
import time
import unittest


class UnpackFromTests(unittest.TestCase):
    def test_every_buffer_kind_and_offset(self):
        data = bytes(range(256)) * 4
        buffers = {
            "bytes": data,
            "bytearray": bytearray(data),
            "memoryview": memoryview(data),
            "memoryview of bytearray": memoryview(bytearray(data)),
            "sub-view": memoryview(b"\xff" * 5 + data)[5:],
            "array": array.array("B", data),
        }
        for name, buf in buffers.items():
            for fmt in ("<I", ">H", "<q", "4s", "<BHI", "3x2B", "=Q", ">2h3s"):
                size = struct.calcsize(fmt)
                for offset in (0, 1, 7, 100, len(data) - size, -size, -len(data)):
                    with self.subTest(buf=name, fmt=fmt, offset=offset):
                        start = offset if offset >= 0 else len(data) + offset
                        want = struct.unpack(fmt, data[start:start + size])
                        self.assertEqual(struct.unpack_from(fmt, buf, offset), want)
                        self.assertEqual(struct.unpack_from(fmt, buffer=buf, offset=offset), want)
                        self.assertEqual(struct.Struct(fmt).unpack_from(buf, offset), want)
            self.assertEqual(struct.unpack_from("<I", buf), (0x03020100,))

    def test_errors(self):
        data = bytearray(16)
        for fmt, offset in (("<I", 13), ("<I", 16), ("<I", 100), ("<q", 9)):
            with self.subTest(fmt=fmt, offset=offset):
                with self.assertRaises(struct.error) as cm:
                    struct.unpack_from(fmt, data, offset)
                self.assertIn("requires a buffer of at least", str(cm.exception))
        with self.assertRaises(struct.error):
            struct.unpack_from("<I", data, -3)
        with self.assertRaises(struct.error):
            struct.unpack_from("<I", data, -17)
        with self.assertRaises(struct.error):
            struct.unpack_from("<Z", data)
        with self.assertRaises(TypeError):
            struct.unpack_from("<I", "not bytes")
        with self.assertRaises(TypeError):
            struct.unpack_from("<I", 5)
        with self.assertRaises(TypeError):
            struct.unpack_from("<I")
        with self.assertRaises((TypeError, struct.error)):
            struct.unpack_from("<I", data, "1")

    def test_views_that_are_not_plain(self):
        data = bytes(range(32))
        # A strided view isn't a contiguous buffer: refused, not gathered.
        for view in (memoryview(data)[::2], memoryview(data)[::-1]):
            with self.assertRaises(BufferError):
                struct.unpack_from("<2B", view)
            with self.assertRaises(BufferError):
                struct.unpack("<16B", view[:16])
        self.assertEqual(struct.unpack_from("<2B", memoryview(data)[4:8], 1), (5, 6))
        released = memoryview(data)
        released.release()
        with self.assertRaises(ValueError):
            struct.unpack_from("<I", released)
        words = memoryview(struct.pack("<4I", 1, 2, 3, 4)).cast("I")
        self.assertEqual(struct.unpack_from("<I", words, 4), (2,))

    def test_index_objects_and_bools_as_offsets(self):
        class Index:
            def __index__(self):
                return 4

        data = bytes(range(16))
        self.assertEqual(struct.unpack_from("<I", data, Index()), (0x07060504,))
        self.assertEqual(struct.unpack_from("<B", data, True), (1,))

    def test_a_buffer_changed_between_reads(self):
        buf = bytearray(12)
        self.assertEqual(struct.unpack_from("<I", buf, 4), (0,))
        buf[4:8] = b"\x01\x02\x03\x04"
        self.assertEqual(struct.unpack_from("<I", buf, 4), (0x04030201,))
        del buf[:4]
        self.assertEqual(struct.unpack_from("<I", buf, 0), (0x04030201,))
        buf.extend(b"\xff" * 8)
        self.assertEqual(struct.unpack_from("<I", buf, -4), (0xFFFFFFFF,))


class PackIntoTests(unittest.TestCase):
    def test_writes_land_where_asked(self):
        for name, make in (
            ("bytearray", lambda: bytearray(32)),
            ("memoryview", lambda: memoryview(bytearray(32))),
            ("array", lambda: array.array("B", bytes(32))),
        ):
            for fmt, values in (
                ("<I", (0xDEADBEEF,)),
                (">H", (0x1234,)),
                ("<d", (1.5,)),
                ("<BHI", (1, 2, 3)),
                ("<?", (True,)),
                ("3s", (b"abc",)),
                ("<q", (-(2**62),)),
            ):
                size = struct.calcsize(fmt)
                for offset in (0, 3, 32 - size, -size, -32):
                    with self.subTest(buf=name, fmt=fmt, offset=offset):
                        buf = make()
                        self.assertIsNone(struct.pack_into(fmt, buf, offset, *values))
                        start = offset if offset >= 0 else 32 + offset
                        want = bytearray(32)
                        want[start:start + size] = struct.pack(fmt, *values)
                        self.assertEqual(bytes(buf), bytes(want))
                        st = struct.Struct(fmt)
                        other = make()
                        st.pack_into(other, offset, *values)
                        self.assertEqual(bytes(other), bytes(want))

    def test_a_view_writes_through(self):
        buf = bytearray(16)
        struct.pack_into("<I", memoryview(buf)[4:], 4, 0x01020304)
        self.assertEqual(buf[8:12], b"\x04\x03\x02\x01")
        self.assertEqual(sum(buf), 10)

    def test_errors_leave_the_buffer_alone(self):
        buf = bytearray(8)
        with self.assertRaises(struct.error):
            struct.pack_into("<I", buf, 5, 1)
        with self.assertRaises(struct.error):
            struct.pack_into("<I", buf, -9, 1)
        with self.assertRaises(struct.error):
            struct.pack_into("<B", buf, 0, 256)
        with self.assertRaises(struct.error):
            struct.pack_into("<I", buf, 0)
        with self.assertRaises(struct.error):
            struct.pack_into("<I", buf, 0, 1, 2)
        with self.assertRaises(struct.error):
            struct.pack_into("<I", buf, 0, "1")
        with self.assertRaises(TypeError):
            struct.pack_into("<I", bytes(8), 0, 1)
        with self.assertRaises(TypeError):
            struct.pack_into("<I", memoryview(bytes(8)), 0, 1)
        with self.assertRaises(TypeError):
            struct.pack_into("<I", "text", 0, 1)
        self.assertEqual(buf, bytearray(8))

    def test_values_that_convert(self):
        class Index:
            def __index__(self):
                return 9

        buf = bytearray(8)
        struct.pack_into("<I", buf, 0, Index())
        struct.pack_into("<f", buf, 4, 2)
        self.assertEqual(struct.unpack("<If", buf), (9, 2.0))


class CostTests(unittest.TestCase):
    """A field costs the same in a large buffer as in a small one."""

    @staticmethod
    def best(fn, repeat=5):
        best = float("inf")
        for _ in range(repeat):
            start = time.perf_counter()
            fn()
            best = min(best, time.perf_counter() - start)
        return best

    def check(self, make, work):
        small = make(4_000)
        large = make(4_000_000)
        t_small = self.best(lambda: work(small))
        t_large = self.best(lambda: work(large))
        self.assertLess(t_large, t_small * 25 + 0.01)

    def test_unpack_from(self):
        def work(buf):
            total = 0
            for offset in range(0, 2400, 4):
                total += struct.unpack_from("<I", buf, offset)[0]
            return total

        for make in (bytes, bytearray, lambda n: memoryview(bytes(n))):
            self.check(make, work)

    def test_pack_into(self):
        def work(buf):
            for offset in range(0, 2400, 4):
                struct.pack_into("<I", buf, offset, offset)

        for make in (bytearray, lambda n: memoryview(bytearray(n))):
            self.check(make, work)

    def test_struct_methods(self):
        record = struct.Struct("<HHI")

        def work(buf):
            total = 0
            for offset in range(0, 2400, 8):
                a, b, c = record.unpack_from(buf, offset)
                total += a + b + c
                record.pack_into(buf, offset, 1, 2, offset)
            return total

        self.check(bytearray, work)


if __name__ == "__main__":
    unittest.main()
