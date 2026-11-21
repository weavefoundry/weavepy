"""Iterating a plain byte memoryview.

A one-dimensional `'B'` view of a `bytes` object steps as the bytes do (no
element objects made first); one over a whole `bytearray` steps the
bytearray live, seeing a store made mid-loop, as CPython's `memoryiter`
does. Other formats, strides and windows keep the element path, and a
released view raises CPython's error.
"""

import unittest


class MemoryviewIterationTests(unittest.TestCase):
    def test_bytes_views(self):
        data = bytes(range(256)) * 4
        for _ in range(20):
            self.assertEqual(sum(memoryview(data)), sum(data))
            self.assertEqual(list(memoryview(data)[10:20]), list(range(10, 20)))
            self.assertEqual(list(memoryview(data)[::3])[:4], [0, 3, 6, 9])
        self.assertEqual(list(memoryview(b"")), [])

    def test_other_formats(self):
        data = bytes(range(256))
        self.assertEqual(sum(memoryview(data).cast("b")), -128)
        self.assertEqual(list(memoryview(b"\x01\x00\x02\x00").cast("H")), [1, 2])
        self.assertEqual(list(memoryview(b"\x00" * 8).cast("d")), [0.0])

    def test_live_bytearray(self):
        ba = bytearray(b"abc")
        it = iter(memoryview(ba))
        self.assertEqual(next(it), 97)
        ba[1] = 0x7A
        self.assertEqual(list(it), [0x7A, 99])
        window = memoryview(bytearray(b"abcdef"))[1:4]
        self.assertEqual(list(window), [98, 99, 100])

    def test_released(self):
        m = memoryview(b"x")
        m.release()
        for f in (iter, lambda v: v.tobytes(), lambda v: v.tolist()):
            with self.assertRaisesRegex(ValueError, "operation forbidden on released memoryview object"):
                f(m)

    def test_loop_counts(self):
        data = bytes(range(256)) * 100
        total = 0
        for b in memoryview(data):
            total += b
        self.assertEqual(total, sum(data))


if __name__ == "__main__":
    unittest.main()
