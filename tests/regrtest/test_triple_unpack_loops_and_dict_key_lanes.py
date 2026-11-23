"""Loops that unpack three or four items, and dict loops over int keys.

`for a, b, c in zip(xs, ys, zs)` and `for a, b, c in rows` step their
items straight onto the stack in compiled code, no tuple built; a loop
over a dict's int keys or values steps them in place in tier 2. Each must
agree with the full paths: zips of lists and tuples, of unequal lengths,
strict ones, with a generator or the same iterator twice among the
sources; rows that aren't all exact triples (a list, a short or long
tuple, a tuple subclass); dicts that change size or keys mid-loop, with
keys or values outside the int lane.
"""

import unittest


class T3(tuple):
    pass


def zip_sum(xs, ys, zs):
    t = 0
    for a, b, c in zip(xs, ys, zs):
        t += a * 100 + b * 10 + c
    return t


def zip4_list(ws, xs, ys, zs):
    out = []
    for a, b, c, d in zip(ws, xs, ys, zs):
        out.append((a, b, c, d))
    return out


def rows_sum(rows):
    t = 0
    for a, b, c in rows:
        t += a - b + c
    return t


def keys_sum(d):
    t = 0
    for k in d:
        t += k
    return t


def values_sum(d):
    t = 0
    for v in d.values():
        t += v
    return t


class TripleUnpackTests(unittest.TestCase):
    def test_zip_shapes(self):
        xs, ys, zs = list(range(50)), tuple(range(1, 51)), list(range(2, 52))
        want = sum(a * 100 + b * 10 + c for a, b, c in zip(xs, ys, zs))
        for _ in range(100):
            self.assertEqual(zip_sum(xs, ys, zs), want)
            self.assertEqual(zip_sum(xs, ys[:7], zs), zip_sum(xs[:7], ys[:7], zs[:7]))
        self.assertEqual(zip_sum(xs, (y for y in ys), zs), want)
        it = iter(range(60))
        self.assertEqual(
            [t for t in zip(it, it, it)][:2], [(0, 1, 2), (3, 4, 5)]
        )
        it = iter(xs)
        got = 0
        for a, b, c in zip(it, it, it):
            got += a + b + c
        self.assertEqual(got, sum(xs[:48]))

    def test_zip_four_and_strict(self):
        ws = list("abcd")
        for _ in range(100):
            self.assertEqual(
                zip4_list(ws, [1, 2, 3, 4], (5, 6, 7, 8), "wxyz"),
                [("a", 1, 5, "w"), ("b", 2, 6, "x"), ("c", 3, 7, "y"), ("d", 4, 8, "z")],
            )
        with self.assertRaises(ValueError):
            for _ in zip([1, 2], [1, 2], [1], strict=True):
                pass
        seen = []
        with self.assertRaises(ValueError):
            for a, b, c in zip([1, 2], [3, 4], [5], strict=True):
                seen.append((a, b, c))
        self.assertEqual(seen, [(1, 3, 5)])

    def test_rows(self):
        rows = [(i, i + 1, i + 2) for i in range(40)]
        want = sum(a - b + c for a, b, c in rows)
        for _ in range(100):
            self.assertEqual(rows_sum(rows), want)
            self.assertEqual(rows_sum(tuple(rows)), want)
        mixed = rows[:5] + [[1, 2, 3], T3((4, 5, 6)), (7, 8, 9)]
        self.assertEqual(rows_sum(mixed), sum(a - b + c for a, b, c in mixed))
        for bad in ((1, 2), (1, 2, 3, 4)):
            with self.assertRaises(ValueError):
                rows_sum(rows[:3] + [bad])

    def test_rows_hold_their_items(self):
        class Box:
            pass

        rows = [(Box(), Box(), Box()) for _ in range(30)]
        for _ in range(100):
            seen = []
            for a, b, c in rows:
                seen.append(c)
            self.assertEqual(seen, [r[2] for r in rows])


class DictKeyLaneTests(unittest.TestCase):
    def test_int_keys_and_values(self):
        d = {i: i * 2 for i in range(200)}
        for _ in range(100):
            self.assertEqual(keys_sum(d), sum(range(200)))
            self.assertEqual(values_sum(d), 2 * sum(range(200)))

    def test_mixed_lanes(self):
        d = {i: i for i in range(100)}
        d[1.5] = 2.5
        d[None] = None
        with self.assertRaises(TypeError):
            keys_sum(d)
        d2 = {i: i for i in range(100)}
        d2[5] = "x"
        with self.assertRaises(TypeError):
            values_sum(d2)
        e = {i: i for i in range(100)}
        e[100] = 1.5
        self.assertEqual(values_sum(e), sum(range(100)) + 1.5)

    def test_change_during_iteration(self):
        for _ in range(50):
            d = {i: i for i in range(100)}
            with self.assertRaises(RuntimeError):
                for k in d:
                    if k == 50:
                        d[1000] = 1
            d = {i: i for i in range(100)}
            with self.assertRaises(RuntimeError):
                for k in d:
                    if k == 50:
                        del d[0]
            d = {i: i for i in range(100)}
            got = 0
            for k in d:
                if k == 10:
                    d[0] = -1
                got += k
            self.assertEqual(got, sum(range(100)))


if __name__ == "__main__":
    unittest.main()
