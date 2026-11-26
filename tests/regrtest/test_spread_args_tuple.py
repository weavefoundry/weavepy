"""A call's spread arguments (`f(a, *rest)`) and a starred tuple display
(`(a, *rest)`) build their tuple straight from the leading items and an
exact tuple or list; other sources take the general steps."""

import gc
import unittest

G = (7, 8)


def args(*a):
    return a


class Seq:
    def __iter__(self):
        return iter([5, 6])


class SpreadTests(unittest.TestCase):
    def test_sources(self):
        t, l, s = (1, 2), [3, 4], Seq()
        for _ in range(300):
            self.assertEqual(args(0, *t), (0, 1, 2))
            self.assertEqual(args(0, 9, *l), (0, 9, 3, 4))
            self.assertEqual(args(0, *s), (0, 5, 6))
            self.assertEqual(args(0, *"ab"), (0, "a", "b"))
            self.assertEqual(args(0, *(x for x in (1, 2))), (0, 1, 2))
            self.assertEqual(args(0, *G), (0, 7, 8))
            self.assertEqual(args(0, *()), (0,))
            self.assertEqual(args(0, *[]), (0,))
            self.assertEqual((0, *t), (0, 1, 2))
            self.assertEqual((*l,), (3, 4))

    def test_source_unchanged_and_items_shared(self):
        l = [object(), object()]
        for _ in range(200):
            r = args(1, *l)
        self.assertEqual(len(l), 2)
        self.assertIs(r[1], l[0])
        self.assertIs(r[2], l[1])
        self.assertIsInstance(r, tuple)

    def test_errors(self):
        def bad(x):
            return args(1, *x)
        for _ in range(200):
            with self.assertRaises(TypeError):
                bad(5)
            self.assertEqual(bad([2]), (1, 2))

    def test_cycles_still_collected(self):
        class Node:
            pass
        for _ in range(300):
            n = Node()
            n.me = args(n, *[n])
        del n
        gc.collect()
        self.assertFalse([o for o in gc.get_objects() if type(o) is Node])


if __name__ == "__main__":
    unittest.main()
