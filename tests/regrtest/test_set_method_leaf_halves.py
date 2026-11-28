"""Set methods with set and frozenset arguments run in place from compiled
loops (issubset, issuperset, isdisjoint, union of several sets,
intersection, difference, symmetric_difference), and enumerate over a str
or bytes leaves compiled code only at the end: results, kinds and orders
matching CPython, keys whose __eq__ runs Python, subclass receivers,
non-set arguments, and an argument changed while the loop runs. Also
list(map(f, xs, ys)) of a builtin over two cursors, and min/max of two
ints or floats."""

import unittest

S1 = set(range(10))
S2 = set(range(5, 15))
F1 = frozenset(range(3))


def tests(n, a, b):
    out = None
    for i in range(n):
        out = (a.issubset(b), a.issuperset(b), a.isdisjoint(b))
    return out


def algebra(n, a, *others):
    out = None
    for i in range(n):
        out = (
            a.union(*others),
            a.intersection(others[0]),
            a.difference(others[0]),
            a.symmetric_difference(others[0]),
        )
    return out


class Key:
    calls = 0

    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return hash(self.v)

    def __eq__(self, other):
        Key.calls += 1
        return isinstance(other, Key) and self.v == other.v


def enum_pairs(n, s, start):
    out = []
    for i in range(n):
        out = [(j, c) for j, c in enumerate(s, start)]
        for j, c in enumerate(s, start):
            pass
    return out


class SetLeafTests(unittest.TestCase):
    def test_tests(self):
        for n in (1, 300):
            self.assertEqual(tests(n, {1, 2}, S1), (True, False, False))
            self.assertEqual(tests(n, S1, {1, 2}), (False, True, False))
            self.assertEqual(tests(n, F1, frozenset({7})), (False, False, True))
            self.assertEqual(tests(n, set(), S1), (True, False, True))
            self.assertEqual(tests(n, {1, 2}, [1, 2, 3]), (True, False, False))

    def test_algebra(self):
        for n in (1, 300):
            u, i, d, x = algebra(n, S1, S2, {99})
            self.assertEqual(u, set(range(15)) | {99})
            self.assertEqual(list(u)[:10], list(S1))
            self.assertEqual(i, set(range(5, 10)))
            self.assertEqual(d, set(range(5)))
            self.assertEqual(x, set(range(5)) | set(range(10, 15)))
            self.assertIs(type(u), set)
            fu, fi, fd, fx = algebra(n, F1, {2, 3}, frozenset({4}))
            self.assertEqual(fu, frozenset({0, 1, 2, 3, 4}))
            self.assertEqual((fi, fd, fx), ({2}, {0, 1}, {0, 1, 3}))
            for r in (fu, fi, fd, fx):
                self.assertIs(type(r), frozenset)
            lu = algebra(n, {1}, [2, 3], (4,))[0]
            self.assertEqual(lu, {1, 2, 3, 4})

    def test_python_keys(self):
        a = {Key(1), Key(2)}
        b = {Key(1), Key(2), Key(3)}
        Key.calls = 0
        self.assertEqual(tests(300, a, b), (True, False, False))
        self.assertGreater(Key.calls, 0)
        u = algebra(300, a, b, {Key(4)})[0]
        self.assertEqual(sorted(k.v for k in u), [1, 2, 3, 4])

    def test_subclass_receiver(self):
        class S(set):
            pass

        u = algebra(300, S({1, 2}), {3}, {4})[0]
        self.assertEqual(u, {1, 2, 3, 4})
        self.assertIs(type(u), set)
        self.assertEqual(tests(300, S({1}), {1, 2}), (True, False, False))

    def test_argument_changes(self):
        a = {1, 2}
        b = {1}
        seen = []
        for i in range(300):
            seen.append(a.issubset(b))
            if i == 150:
                b.add(2)
        self.assertEqual(seen, [False] * 151 + [True] * 149)

    def test_set_copies(self):
        f = frozenset({1, 2})
        k = {Key(1), Key(2)}
        for i in range(300):
            a = set(S1)
            b = frozenset(S1)
            c = frozenset(f)
            d = set(f)
            e = set(k)
        self.assertEqual(a, S1)
        self.assertIsNot(a, S1)
        self.assertEqual(list(a), list(S1))
        self.assertIs(type(b), frozenset)
        self.assertEqual(b, S1)
        self.assertIs(c, f)
        self.assertIs(type(d), set)
        self.assertEqual(d, {1, 2})
        self.assertEqual(len(e), 2)
        a.add(99)
        self.assertNotIn(99, S1)

    def test_map_two_sources(self):
        a = [1, 5, 3]
        b = [4, 2, 6, 9]
        for i in range(300):
            r = list(map(max, a, b))
            m = list(map(min, b, a))
        self.assertEqual(r, [4, 5, 6])
        self.assertEqual(m, [1, 2, 3])
        x, y = iter(b), iter(a)
        self.assertEqual(list(map(max, x, y)), [4, 5, 6])
        self.assertEqual(list(x), [])
        x, y = iter(a), iter(b)
        self.assertEqual(list(map(max, x, y)), [4, 5, 6])
        self.assertEqual(list(y), [9])
        with self.assertRaises(ValueError):
            list(map(max, a, b, strict=True))
        with self.assertRaises(TypeError):
            list(map(max, [1, "a"], [2, 3]))
        self.assertEqual(list(map(pow, [2, 3], (3, 2))), [8, 9])
        self.assertEqual(list(map(max, ["b", "a"], ["a", "c"])), ["b", "c"])

    def test_min_max_pairs(self):
        nan = float("nan")
        for i in range(300):
            r = (max(1, 2), min(1, 2), max(2.5, 1.5), min(-0.0, 0.0), max(0.0, -0.0))
            s = (max(nan, 1.0), max(1.0, nan), min(nan, 1.0), max(True, 1), max(1, True))
        self.assertEqual(r[:3], (2, 1, 2.5))
        self.assertEqual(str(r[3]), "-0.0")
        self.assertEqual(str(r[4]), "0.0")
        self.assertTrue(s[0] != s[0])
        self.assertEqual(s[1], 1.0)
        self.assertTrue(s[2] != s[2])
        self.assertIs(s[3], True)
        self.assertEqual(s[4], 1)
        self.assertIsNot(s[4], True)

    def test_enumerate_str_and_bytes(self):
        for n in (1, 300):
            self.assertEqual(enum_pairs(n, "abc", 1), [(1, "a"), (2, "b"), (3, "c")])
            self.assertEqual(enum_pairs(n, "", 0), [])
            self.assertEqual(enum_pairs(n, b"xy", 5), [(5, 120), (6, 121)])
            self.assertEqual(enum_pairs(n, "hé\U0001f600", 0), [(0, "h"), (1, "é"), (2, "\U0001f600")])
        it = iter("abcd")
        next(it)
        self.assertEqual(list(enumerate(it)), [(0, "b"), (1, "c"), (2, "d")])
        self.assertEqual(list(it), [])


if __name__ == "__main__":
    unittest.main()
