"""Callbacks that call an instance with a native `__call__`.

`map(itemgetter(1), xs)`, `sorted(xs, key=attrgetter("n"))` and their kin
find the class's native `__call__` and its leaf half once per class
version: a subclass with its own `__call__`, or a class whose `__call__`
changes, is called as Python calls it.
"""

import operator
import unittest


class P:
    def __init__(self, n):
        self.n = n


class NativeCallCallbackTests(unittest.TestCase):
    def test_map_and_keys(self):
        pairs = [(i, -i) for i in range(50)]
        ps = [P(3), P(1), P(2)]
        for _ in range(100):
            self.assertEqual(list(map(operator.itemgetter(1), pairs))[:3], [0, -1, -2])
            self.assertEqual([p.n for p in sorted(ps, key=operator.attrgetter("n"))], [1, 2, 3])
            self.assertEqual(max(pairs, key=operator.itemgetter(1)), (0, 0))
            self.assertEqual(min(ps, key=operator.attrgetter("n")).n, 1)

    def test_subclass_and_changed_call(self):
        try:
            class G(operator.itemgetter):
                def __call__(self, obj):
                    return ("own", obj[0])
        except TypeError:
            self.skipTest("an immutable C type")
        g = G(0)
        for _ in range(100):
            self.assertEqual(list(map(g, [(1,), (2,)])), [("own", 1), ("own", 2)])

        class H(operator.itemgetter):
            pass

        h = H(0)
        self.assertEqual(list(map(h, [(5,)] * 50)), [5] * 50)
        H.__call__ = lambda self, obj: "patched"
        self.assertEqual(list(map(h, [(5,)] * 3)), ["patched"] * 3)
        del H.__call__
        self.assertEqual(list(map(h, [(6,)])), [6])

    def test_errors(self):
        with self.assertRaises(IndexError):
            list(map(operator.itemgetter(5), [(1,)]))
        with self.assertRaises(AttributeError):
            sorted([P(1), object()], key=operator.attrgetter("n"))


if __name__ == "__main__":
    unittest.main()
