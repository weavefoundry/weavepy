"""array.array(typecode) and array.array(typecode, list) from compiled
loops, for every numeric typecode: typecode, itemsize, contents, bounds
checks, and the arrays behaving as ones built by the general path
(appends, slices, equality, pickling), with what the native constructor
declines (an out-of-range or non-number item, another initializer, 'u'
and 'w', a bad typecode, a subclass, keywords) still taking the general
path, each matching CPython."""

import array
import pickle
import unittest
import warnings


class Sub(array.array):
    pass


def build(n, code, items=None):
    out = None
    for i in range(n):
        out = array.array(code) if items is None else array.array(code, items)
    return out


class ArrayConstructionTests(unittest.TestCase):
    def test_typecodes(self):
        for code in "bBhHiIlLqQfd":
            for n in (1, 400):
                a = build(n, code)
                self.assertEqual((a.typecode, len(a)), (code, 0))
                self.assertEqual(a.itemsize, array.array(code).itemsize)
                items = [1.5, 2.0] if code in "fd" else [1, 2, 3]
                b = build(n, code, items)
                self.assertEqual(b.tolist(), items)
                b.append(items[0])
                self.assertEqual(b[-1], items[0])
                self.assertEqual(b[1:], array.array(code, items[1:] + items[:1]))
                self.assertEqual(pickle.loads(pickle.dumps(b)), b)

    def test_bounds(self):
        for n in (1, 400):
            self.assertEqual(build(n, "b", [-128, 127]).tolist(), [-128, 127])
            self.assertEqual(build(n, "Q", [2**64 - 1]).tolist(), [2**64 - 1])
            self.assertEqual(build(n, "q", [-(2**63)]).tolist(), [-(2**63)])
            a = build(n, "B")
            with self.assertRaises(OverflowError):
                a.append(256)
            with self.assertRaises(OverflowError):
                build(n, "H").append(-1)
            with self.assertRaises(OverflowError):
                build(n, "Q").append(2**64)
        with self.assertRaises(OverflowError):
            build(300, "b", [128])
        with self.assertRaises(TypeError):
            build(300, "i", [1, "x"])
        with self.assertRaises(ValueError):
            build(300, "z")
        with self.assertRaises(TypeError):
            build(300, "ii")

    def test_other_initializers(self):
        for n in (1, 400):
            self.assertEqual(build(n, "i", (1, 2)).tolist(), [1, 2])
            self.assertEqual(build(n, "i", range(3)).tolist(), [0, 1, 2])
            self.assertEqual(build(n, "B", b"ab").tolist(), [97, 98])
            self.assertEqual(build(n, "d", [1, 2]).tolist(), [1.0, 2.0])
            self.assertEqual(build(n, "w", "hi").tolist(), ["h", "i"])
        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always")
            build(3, "u")
        self.assertEqual(len(caught), 3)

    def test_subclass_and_keywords(self):
        def subs(n):
            out = None
            for i in range(n):
                out = Sub("i", [1])
            return out

        for n in (1, 400):
            s = subs(n)
            self.assertIs(type(s), Sub)
            self.assertEqual(s.tolist(), [1])
        with self.assertRaises(TypeError):
            for i in range(300):
                array.array("i", initializer=[1])


if __name__ == "__main__":
    unittest.main()
