"""Stepped slices, `bytearray` repeats and `translate` of bytes-likes.

Slices with steps above one, from any start and stop, of bytes,
bytearrays, lists and tuples give the items a Python loop picks; a
`bytearray` times a count is a new `bytearray` (the operand unchanged);
`translate` with a table, a deletion set, or both, of `bytes` and
`bytearray` receivers, gives what a byte-by-byte loop gives, and reads
the receiver only after the table and deletion set are taken. A
memoryview's slices keep the view's format.
"""

import unittest


def stepped(seq, start, stop, step):
    idx = range(*slice(start, stop, step).indices(len(seq)))
    return [seq[i] for i in idx]


class StrideTests(unittest.TestCase):
    def test_slices(self):
        data = bytes(range(256)) * 3
        for kind in (bytes, bytearray, list, tuple):
            seq = kind(data)
            for step in (2, 3, 7, 255, 1000, -2, -3):
                for start, stop in ((None, None), (1, None), (5, 700), (-10, None),
                                    (700, 5), (None, -1), (-1000, 1000), (3, 4)):
                    got = seq[start:stop:step]
                    self.assertIs(type(got), kind)
                    self.assertEqual(list(got), stepped(seq, start, stop, step),
                                     (kind, start, stop, step))

    def test_objects(self):
        items = [object() for _ in range(20)]
        got = items[1::3]
        self.assertEqual(len(got), 7)
        for a, b in zip(got, items[1::3]):
            self.assertIs(a, b)


class RepeatTests(unittest.TestCase):
    def test_bytearray_repeat(self):
        ba = bytearray(b"abc")
        for n in (0, 1, 2, 5, -1):
            r = ba * n
            self.assertIs(type(r), bytearray)
            self.assertEqual(r, bytearray(b"abc" * max(n, 0)))
            self.assertIsNot(r, ba)
            self.assertEqual(n * ba, r)
        self.assertEqual(ba, bytearray(b"abc"))
        big = bytearray(range(256)) * 100
        self.assertEqual(big * 3, bytearray(bytes(range(256)) * 300))
        with self.assertRaises((MemoryError, OverflowError)):
            bytearray(b"x") * (2**62)


class TranslateTests(unittest.TestCase):
    def test_table_and_delete(self):
        table = bytes(range(255, -1, -1))
        data = bytes(range(256)) * 4
        want = bytes(table[b] for b in data)
        for kind in (bytes, bytearray):
            recv = kind(data)
            got = recv.translate(table)
            self.assertIs(type(got), kind)
            self.assertEqual(got, want)
            self.assertEqual(recv.translate(None), data)
            self.assertEqual(recv.translate(None, b"\x00\x01"),
                             bytes(b for b in data if b > 1))
            self.assertEqual(recv.translate(table, delete=b"\xff"),
                             bytes(table[b] for b in data if b != 0xff))
            self.assertEqual(recv.translate(bytearray(table), memoryview(b"ab")),
                             bytes(table[b] for b in data if b not in b"ab"))
        with self.assertRaises(ValueError):
            b"x".translate(b"short")
        with self.assertRaises(TypeError):
            b"x".translate(None, nonsense=b"")

    def test_receiver_read_after_table(self):
        ba = bytearray(b"hello")

        class Table:
            def __buffer__(self, flags):
                ba.extend(b"!!")
                return memoryview(bytes(range(256)))

            def __release_buffer__(self, view):
                pass

        self.assertEqual(ba.translate(Table()), bytearray(b"hello!!"))


class MemoryviewSliceTests(unittest.TestCase):
    def test_formats_kept(self):
        import array
        a = array.array("i", range(10))
        mv = memoryview(a)
        sub = mv[2:5]
        self.assertEqual(sub.format, "i")
        self.assertEqual(sub.tolist(), [2, 3, 4])
        b = memoryview(b"abcdef")
        self.assertEqual(b[1:3].format, "B")
        self.assertEqual(bytes(b[1:5:2]), b"bd")
        c = b.cast("c")
        self.assertEqual(c[1:3].format, "c")
        self.assertEqual(c[1:3].tolist(), [b"b", b"c"])
        ba = bytearray(b"xyz")
        with memoryview(ba)[1:] as v:
            self.assertEqual(v.format, "B")
            v[0] = ord("Y")
        self.assertEqual(ba, bytearray(b"xYz"))
        ba.append(1)


if __name__ == "__main__":
    unittest.main()
