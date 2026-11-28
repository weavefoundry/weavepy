"""A dict subclass's __missing__ called from compiled loops: Counter
increments of new and old keys (d[k] += 1), a user class whose
__missing__ stores or raises, a subclass that overrides __getitem__ (no
__missing__ call), keys whose hash runs Python, and a class whose
__missing__ changes partway, each matching CPython's results, with
(k, v) in d.items(), v in d.values() and k in d.keys() answered as CPython
answers them."""

import collections
import unittest

WORDS = "the quick brown fox jumps over the lazy dog".split()


class Default(dict):
    def __missing__(self, key):
        return -1


class Storing(dict):
    def __missing__(self, key):
        self[key] = v = len(key)
        return v


class Raising(dict):
    def __missing__(self, key):
        raise LookupError(key)


class Own(dict):
    def __getitem__(self, key):
        return "own"

    def __missing__(self, key):
        return "missing"


class H:
    def __init__(self, k):
        self.k = k

    def __hash__(self):
        return hash(self.k)

    def __eq__(self, other):
        return isinstance(other, H) and self.k == other.k


def counts(n):
    c = None
    for i in range(n):
        c = collections.Counter()
        for w in WORDS:
            c[w] += 1
    return c


def reads(n, d, keys):
    out = None
    for i in range(n):
        out = [d[k] for k in keys]
    return out


class MissingTests(unittest.TestCase):
    def test_counter(self):
        for n in (1, 400):
            c = counts(n)
            self.assertEqual(c["the"], 2)
            self.assertEqual(sum(c.values()), 9)
            self.assertEqual(c["absent"], 0)
            self.assertNotIn("absent", c)

    def test_default(self):
        for n in (1, 400):
            self.assertEqual(reads(n, Default(a=1), ["a", "b", 3]), [1, -1, -1])
            d = Storing()
            self.assertEqual(reads(n, d, ["ab", "abc"]), [2, 3])
            self.assertEqual(d, {"ab": 2, "abc": 3})
            self.assertEqual(reads(n, Own(a=1), ["a", "b"]), ["own", "own"])
            self.assertEqual(reads(n, Default({H(1): 5}), [H(1), H(2)]), [5, -1])

    def test_raising(self):
        def run(n):
            caught = 0
            d = Raising(a=1)
            for i in range(n):
                try:
                    d["b"]
                except LookupError as e:
                    caught += e.args == ("b",)
            return caught

        self.assertEqual(run(400), 400)
        with self.assertRaises(KeyError):
            reads(300, dict(a=1), ["a", "b"])

    def test_changed_missing(self):
        class C(dict):
            def __missing__(self, key):
                return 1

        d = C()
        self.assertEqual(reads(400, d, ["x"]), [1])
        C.__missing__ = lambda self, key: 2
        self.assertEqual(reads(300, d, ["x"]), [2])
        del C.__missing__
        with self.assertRaises(KeyError):
            reads(300, d, ["x"])

    def test_view_membership(self):
        d = {"a": 1, "b": [2], 3: "c"}

        def run(n):
            out = None
            for i in range(n):
                out = (
                    ("a", 1) in d.items(),
                    ("a", 2) in d.items(),
                    ("b", [2]) in d.items(),
                    ("z", 1) in d.items(),
                    "a" in d.items(),
                    ("a",) in d.items(),
                    ("a", 1, 2) in d.items(),
                    (3, "c") in d.items(),
                    1 in d.values(),
                    [2] in d.values(),
                    "z" in d.values(),
                    "a" in d.keys(),
                    4 in d.keys(),
                )
            return out

        want = (True, False, True, False, False, False, False, True, True, True, False, True, False)
        for n in (1, 400):
            self.assertEqual(run(n), want)
        with self.assertRaises(TypeError):
            ([], 1) in d.items()
        self.assertTrue((H(1), 5) in {H(1): 5}.items())


if __name__ == "__main__":
    unittest.main()
