"""Slicing a memoryview gives a view of the same buffer.

`view[i:j]` of a one-dimensional view is a view of the same bytes: it
reads and writes through to the buffer, slices again, keeps its format
and item size, and counts as an export of a bytearray until it's
released. The same holds whether the slice runs once or in a loop long
enough to be compiled, and for a view only the expression holds.
"""

import array
import struct
import unittest


def window(view, lo, hi):
    return view[lo:hi]


def windows(view, width, count):
    out = []
    for i in range(count):
        out.append(bytes(view[i:i + width]))
    return out


def window_sum(view, width, count):
    total = 0
    for i in range(count):
        part = view[i:i + width]
        total += part[0] + part[-1] + len(part)
    return total


class SubviewTests(unittest.TestCase):
    def test_a_subview_reads_the_same_bytes(self):
        data = bytes(range(200))
        view = memoryview(data)
        for lo, hi in [(0, 200), (10, 20), (190, 300), (-5, None), (None, -195), (50, 10), (7, 7)]:
            with self.subTest(lo=lo, hi=hi):
                sub = view[lo:hi]
                self.assertEqual(bytes(sub), data[lo:hi])
                self.assertEqual(len(sub), len(data[lo:hi]))
                self.assertEqual(sub.nbytes, len(data[lo:hi]))
                self.assertEqual(sub.format, "B")
                self.assertEqual(sub.itemsize, 1)
                self.assertEqual(sub.shape, (len(data[lo:hi]),))
                self.assertEqual(sub.strides, (1,))
                self.assertEqual(sub.ndim, 1)
                self.assertTrue(sub.readonly)
                self.assertTrue(sub.c_contiguous)
                self.assertTrue(sub.contiguous)
                self.assertIs(sub.obj, data)

    def test_subviews_of_subviews(self):
        data = bytes(range(256))
        view = memoryview(data)
        a = view[10:200]
        b = a[5:100]
        c = b[-10:]
        self.assertEqual(bytes(c), data[100:110])
        self.assertEqual(c[0], 100)
        self.assertEqual(c[-1], 109)
        self.assertEqual(bytes(c[::-1]), data[100:110][::-1])
        self.assertEqual(bytes(b[::3][1:4]), data[15:110][::3][1:4])
        self.assertEqual(bytes(view[::2][10:20]), data[::2][10:20])
        self.assertEqual(bytes(view[::-1][10:20]), data[::-1][10:20])

    def test_wider_items(self):
        values = array.array("i", range(100))
        view = memoryview(values)
        sub = view[10:20]
        self.assertEqual(sub.tolist(), list(range(10, 20)))
        self.assertEqual(sub.itemsize, values.itemsize)
        self.assertEqual(sub.format, "i")
        self.assertEqual(sub.nbytes, 10 * values.itemsize)
        self.assertEqual(sub.shape, (10,))
        self.assertEqual(sub.strides, (values.itemsize,))
        self.assertEqual(sub[3:5].tolist(), [13, 14])
        self.assertEqual(sub[-1], 19)
        sub[0] = -7
        self.assertEqual(values[10], -7)
        self.assertEqual(view[5:15][::2].tolist(), [5, 7, 9, 11, 13])
        cast = memoryview(struct.pack("8H", *range(8))).cast("H")
        self.assertEqual(cast[2:6].tolist(), [2, 3, 4, 5])
        self.assertEqual(cast[2:6][1:3].tolist(), [3, 4])

    def test_a_cast_shape_keeps_its_trailing_dimensions(self):
        view = memoryview(bytes(range(24))).cast("B", (4, 6))
        sub = view[1:3]
        self.assertEqual(sub.shape, (2, 6))
        self.assertEqual(sub.tolist(), [list(range(6, 12)), list(range(12, 18))])

    def test_writes_go_through(self):
        buf = bytearray(b"0123456789")
        view = memoryview(buf)
        sub = view[2:6]
        self.assertFalse(sub.readonly)
        sub[0] = ord("a")
        sub[1:3] = b"bc"
        self.assertEqual(buf, b"01abc56789")
        sub[:] = b"WXYZ"
        self.assertEqual(buf, b"01WXYZ6789")
        self.assertEqual(bytes(view[1:7]), b"1WXYZ6")
        ro = view.toreadonly()[2:6]
        self.assertTrue(ro.readonly)
        with self.assertRaises(TypeError):
            ro[0] = 1

    def test_a_subview_is_an_export(self):
        buf = bytearray(b"0123456789")
        view = memoryview(buf)
        sub = view[2:6]
        view.release()
        with self.assertRaises(BufferError):
            buf.append(1)
        with self.assertRaises(BufferError):
            del buf[:1]
        self.assertEqual(bytes(sub), b"2345")
        sub.release()
        buf.append(65)
        self.assertEqual(buf, b"0123456789A")
        with self.assertRaises(ValueError):
            sub[0:1]
        with self.assertRaises(ValueError):
            view[0:1]

    def test_temporary_views(self):
        buf = bytearray(b"0123456789")
        for _ in range(200):
            self.assertEqual(bytes(memoryview(buf)[3:5]), b"34")
            self.assertEqual(memoryview(buf)[3:5][1], ord("4"))
        # Every temporary is gone: the buffer resizes again.
        buf.extend(b"ab")
        self.assertEqual(len(buf), 12)

    def test_hash_and_equality(self):
        data = b"abcdefabcdef"
        view = memoryview(data)
        self.assertEqual(view[0:6], view[6:12])
        self.assertEqual(hash(view[0:6]), hash(b"abcdef"))
        self.assertEqual(view[0:6], b"abcdef")
        self.assertNotEqual(view[0:6], view[1:7])
        self.assertEqual({view[0:3]: 1}[b"abc"], 1)

    def test_zero_dimensional_views_refuse_slices(self):
        scalar = memoryview(b"\x01\x00\x00\x00").cast("i", ())
        with self.assertRaises(TypeError):
            scalar[0:1]


class HotLoopTests(unittest.TestCase):
    def test_slices_in_a_long_loop(self):
        data = bytes(i & 0xFF for i in range(5000))
        view = memoryview(data)
        got = windows(view, 16, 4000)
        self.assertEqual(got[0], data[0:16])
        self.assertEqual(got[3999], data[3999:4015])
        self.assertTrue(all(got[i] == data[i:i + 16] for i in range(0, 4000, 97)))
        want = sum(data[i] + data[i + 15] + 16 for i in range(4000))
        for _ in range(4):
            self.assertEqual(window_sum(view, 16, 4000), want)

    def test_bounds_from_variables(self):
        data = bytes(range(100))
        view = memoryview(data)
        total = 0
        for i in range(3000):
            lo = i % 90
            total += len(window(view, lo, lo + 10)) + window(view, lo, None)[0]
        self.assertEqual(total, sum(10 + (i % 90) for i in range(3000)))

    def test_a_loop_over_a_bytearray_view_releases_every_subview(self):
        buf = bytearray(range(256)) * 4
        view = memoryview(buf)
        total = 0
        for i in range(3000):
            part = view[i % 1000:i % 1000 + 8]
            total += part[0]
            part.release()
        self.assertEqual(total, sum((i % 1000) % 256 for i in range(3000)))
        del part
        view.release()
        buf.append(0)
        self.assertEqual(len(buf), 1025)

    def test_unreleased_subviews_die_with_their_names(self):
        buf = bytearray(64)
        view = memoryview(buf)
        for i in range(3000):
            part = view[i % 32:i % 32 + 8]
        del part
        view.release()
        buf.append(0)
        self.assertEqual(len(buf), 65)


if __name__ == "__main__":
    unittest.main()
