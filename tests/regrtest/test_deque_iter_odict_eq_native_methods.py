"""Deque iteration through its iterators' leaf halves, OrderedDict
equality over plain keys compared natively, and a native method of a
Python-level class (deque.extend) called in place from compiled loops:
items in order and reversed, a deque mutated during iteration
(RuntimeError), equality sensitive to order and length, keys with a
Python __eq__ (the full walk), bounded deques trimming as they extend,
and extend from a generator or the deque itself, each matching CPython."""

import collections
import unittest


class Key:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return hash(self.v)

    def __eq__(self, other):
        return isinstance(other, Key) and self.v == other.v


class DequeOdictTests(unittest.TestCase):
    def test_deque_iteration(self):
        d = collections.deque(range(20))
        d.rotate(3)
        for i in range(300):
            a = list(d)
            r = list(reversed(d))
            s = sum(x for x in d)
        self.assertEqual(a, [17, 18, 19] + list(range(17)))
        self.assertEqual(r, a[::-1])
        self.assertEqual(s, 190)
        with self.assertRaises(RuntimeError):
            for x in d:
                d.append(1)

    def test_odict_eq(self):
        a = collections.OrderedDict(a=1, b=2, c=3)
        b = collections.OrderedDict(a=1, b=2, c=3)
        c = collections.OrderedDict(b=2, a=1, c=3)
        e = collections.OrderedDict([(1, "x"), ("1", "y")])
        f = collections.OrderedDict([(1, "x"), ("1", "y")])
        for i in range(300):
            r = (a == b, a == c, a != c, a == dict(c), e == f, a == e)
        self.assertEqual(r, (True, False, True, True, True, False))
        k1 = collections.OrderedDict([(Key(1), 1), (Key(2), 2)])
        k2 = collections.OrderedDict([(Key(1), 1), (Key(2), 2)])
        k3 = collections.OrderedDict([(Key(2), 2), (Key(1), 1)])
        self.assertTrue(k1 == k2)
        self.assertFalse(k1 == k3)

    def test_extend_in_place(self):
        d = collections.deque(maxlen=5)
        for i in range(300):
            d.extend((i, i + 1, i + 2))
        self.assertEqual(list(d), [299, 300, 299, 300, 301])
        e = collections.deque()
        for i in range(300):
            e.extend(x for x in range(2))
            e.extendleft([9])
        self.assertEqual(len(e), 900)
        self.assertEqual(list(e)[:3], [9, 9, 9])
        f = collections.deque([1, 2])
        f.extend(f)
        self.assertEqual(list(f), [1, 2, 1, 2])


if __name__ == "__main__":
    unittest.main()
