"""`list(zip(...))` over four to eight list or tuple sources builds each
row straight into its tuple; rows, the shortest source's end, and the
sources' positions match CPython."""

import unittest


class ZipWideRows(unittest.TestCase):
    def test_rows(self):
        for width in range(4, 9):
            srcs = [list(range(k, k + 10 - k)) for k in range(width)]
            want = [tuple(s[i] for s in srcs) for i in range(min(map(len, srcs)))]
            for _ in range(50):
                self.assertEqual(list(zip(*srcs)), want)
        m = [[i * j for j in range(8)] for i in range(8)]
        self.assertEqual(list(zip(*m))[3], tuple(row[3] for row in m))

    def test_positions_after_drain(self):
        a, b, c, d = iter([1, 2, 3]), iter((4, 5)), iter([6, 7, 8]), iter([9, 10, 11])
        self.assertEqual(list(zip(a, b, c, d)), [(1, 4, 6, 9), (2, 5, 7, 10)])
        # (The first source's third item was read and dropped at the end.)
        self.assertEqual((next(a, None), next(c), next(d)), (None, 8, 11))

    def test_objects_kept(self):
        objs = [object() for _ in range(5)]
        rows = list(zip(*[[o] for o in objs]))
        self.assertTrue(all(x is y for x, y in zip(rows[0], objs)))


if __name__ == "__main__":
    unittest.main()
