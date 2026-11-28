"""Array slices, sums and copies made from their source's fields.

A slice, `a + b` and `copy.copy(a)` build the new array from the source's
type code fields directly; each is a plain `array` of the same type code
(a subclass's too, as in CPython), independent of its source, and
a `'u'` array's slices don't warn again. Compiled code reads items, takes
slices and concatenates through the native methods' leaf halves; a bad
index, key or operand still raises CPython's error.
"""

import array
import copy
import unittest
import warnings


class ArraySliceTests(unittest.TestCase):
    def test_slices(self):
        for tc in "bBhHiIlLqQfd":
            a = array.array(tc, range(40))
            for _ in range(50):
                for sl in (slice(3, 7), slice(None), slice(-5, None), slice(7, 3), slice(100, 200),
                           slice(None, None, 3), slice(None, None, -1), slice(-1, -10, -2)):
                    got = a[sl]
                    self.assertIs(type(got), array.array)
                    self.assertEqual(got.typecode, tc)
                    self.assertEqual(got.tolist(), a.tolist()[sl])
        self.assertEqual(array.array("i", [1, 2, 3])[1:].itemsize, array.array("i").itemsize)

    def test_independent(self):
        a = array.array("i", [1, 2, 3, 4])
        b = a[1:3]
        b[0] = 99
        b.append(7)
        self.assertEqual(a.tolist(), [1, 2, 3, 4])
        self.assertEqual(b.tolist(), [99, 3, 7])
        c = a[:]
        c.extend([5])
        self.assertEqual(len(a), 4)
        e = a[3:1]
        e.append(8)
        self.assertEqual(e.tolist(), [8])

    def test_ranges_carry_over(self):
        b = array.array("b", [1, 2])[0:1]
        with self.assertRaises(OverflowError):
            b.append(200)
        h = array.array("H", [1])[:]
        with self.assertRaises(OverflowError):
            h.append(-1)

    def test_subclass_and_sum_and_copy(self):
        class A(array.array):
            pass

        a = A("d", [1.5, 2.5, 3.5])
        self.assertIs(type(a[0:2]), array.array)
        s = a + array.array("d", [4.5])
        self.assertIs(type(s), array.array)
        self.assertEqual(s.tolist(), [1.5, 2.5, 3.5, 4.5])
        c = copy.copy(a)
        self.assertIs(type(c), array.array)
        self.assertEqual(c.tolist(), a.tolist())
        plain = array.array("q", [5, 6])
        d = copy.copy(plain)
        d.append(7)
        self.assertEqual(plain.tolist(), [5, 6])
        self.assertEqual(copy.deepcopy(plain).tolist(), [5, 6])
        with self.assertRaises(TypeError):
            plain + array.array("i", [1])

    def test_concatenation_in_loops(self):
        class A(array.array):
            pass

        a = array.array("h", [1, -2])
        b = array.array("h", [3])
        sub = A("h", [9])
        for _ in range(500):
            self.assertEqual((a + b).tolist(), [1, -2, 3])
            self.assertEqual((a + a).tolist(), [1, -2, 1, -2])
            self.assertEqual((a + sub).tolist(), [1, -2, 9])
            self.assertIs(type(sub + a), array.array)
        for other in (array.array("i", [1]), [1], b"x", 5):
            with self.assertRaises(TypeError):
                a + other
        acc = array.array("d")
        for i in range(300):
            acc += array.array("d", [i])
        self.assertEqual(acc[-1], 299.0)
        self.assertEqual(len(acc), 300)
        u = array.array("w", "ab")
        self.assertEqual((u + u).tounicode(), "abab")

    def test_items_in_loops(self):
        a = array.array("l", range(-50, 50))
        total = 0
        for _ in range(100):
            for i in range(len(a)):
                total += a[i] + a[-1 - i]
        self.assertEqual(total, -100 * 100)
        f = array.array("f", [0.5, 1.5])
        self.assertEqual([f[i & 1] for i in range(1000)][-2:], [0.5, 1.5])
        caught = 0
        for i in range(3000):
            try:
                a[i % 101]
            except IndexError:
                caught += 1
        self.assertEqual(caught, sum(1 for i in range(3000) if i % 101 == 100))
        with self.assertRaises(TypeError):
            a["x"]
        for i in range(2000):
            part = a[i % 90:i % 90 + 3]
        self.assertEqual(part.tolist(), [a[(1999 % 90) + k] for k in range(3)])

    def test_unicode_slices_warn_once(self):
        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always")
            u = array.array("u", "hello")
            first = len(caught)
            part = u[1:4]
            self.assertEqual(part.tounicode(), "ell")
            self.assertEqual(len(caught), first)


if __name__ == "__main__":
    unittest.main()
