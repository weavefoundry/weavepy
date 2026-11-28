"""dict.popitem from compiled loops (a leaf half that pops the last entry
in place), set &= set (the replaced table released whole) and set |= set
(in place in compiled code), with set and str/int key lookups: popped
pairs in LIFO order, KeyError on an empty dict, a dict subclass, an
OrderedDict, a dict changed while iterated over, and intersections that
keep the left operand's elements, each matching CPython."""

import collections
import unittest


def pops(n):
    out = []
    for i in range(n):
        d = {"a": 1, "b": 2, i: i}
        out.append(d.popitem())
        out.append(d.popitem())
    return out


class Key:
    def __init__(self, v, tag):
        self.v = v
        self.tag = tag

    def __hash__(self):
        return hash(self.v)

    def __eq__(self, other):
        return isinstance(other, Key) and self.v == other.v


class PopitemTests(unittest.TestCase):
    def test_lifo(self):
        for n in (1, 300):
            r = pops(n)
            self.assertEqual(r[-2:], [(n - 1, n - 1), ("b", 2)])
            self.assertEqual(len(r), 2 * n)

    def test_empty_and_args(self):
        d = {}
        for i in range(300):
            d[i] = i
            self.assertEqual(d.popitem(), (i, i))
        with self.assertRaises(KeyError):
            d.popitem()
        with self.assertRaises(TypeError):
            {1: 2}.popitem(1)

    def test_subclasses(self):
        class D(dict):
            pass

        d = D(a=1, b=2)
        self.assertEqual(d.popitem(), ("b", 2))
        o = collections.OrderedDict(a=1, b=2, c=3)
        self.assertEqual(o.popitem(last=False), ("a", 1))
        self.assertEqual(o.popitem(), ("c", 3))

    def test_changed_while_iterated(self):
        d = {i: i for i in range(10)}
        with self.assertRaises(RuntimeError):
            for k in d:
                d.popitem()

    def test_set_iand(self):
        a = {Key(1, "a"), Key(2, "a"), Key(3, "a")}
        b = {Key(2, "b"), Key(3, "b"), Key(4, "b")}
        for i in range(300):
            s = set(a)
            s &= b
        self.assertEqual(sorted(k.v for k in s), [2, 3])
        ints = set(range(10))
        for i in range(300):
            t = set(ints)
            t &= set(range(5, 15))
            t &= frozenset({5, 6, 99})
        self.assertEqual(t, {5, 6})
        t &= set()
        self.assertEqual(t, set())

    def test_set_ior(self):
        s = set(range(5))
        alias = s
        for i in range(300):
            s |= {i % 7, 100}
            s |= frozenset({i % 3})
        self.assertIs(s, alias)
        self.assertEqual(s, set(range(7)) | {100})
        k = {Key(1, "a")}
        for i in range(300):
            k |= {Key(1, "b"), Key(i % 4, "c")}
        self.assertEqual(sorted(x.v for x in k), [0, 1, 2, 3])
        self.assertEqual([x.tag for x in k if x.v == 1], ["a"])
        s |= s
        self.assertEqual(len(s), 8)
        f = frozenset({1})
        g = f
        f |= {2}
        self.assertIsNot(f, g)
        self.assertEqual(f, {1, 2})

    def test_key_equality(self):
        a = "".join(["ab", "c"])
        d = {"abc": 1, 5: 2, 2**40: 3}
        for i in range(300):
            r = (d[a], d[5], d[2**40], a in {"abc"}, 5.0 in {5}, True in {1})
        self.assertEqual(r, (1, 2, 3, True, True, True))


if __name__ == "__main__":
    unittest.main()
