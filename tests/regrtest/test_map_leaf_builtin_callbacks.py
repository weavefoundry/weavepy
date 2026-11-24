"""list(map(f, xs)) where f is a builtin with a leaf body, from compiled
loops: unbound str methods, abs and ord over lists and tuples, an item
the body rejects (the full call raises CPython's error), a receiver of
the wrong type for an unbound method, a str subclass item, and a list
that grows while mapped, each matching CPython."""

import unittest

WORDS = ["hello", "World", "fOO"]


class S(str):
    pass


def run(n, f, src):
    out = None
    for i in range(n):
        out = list(map(f, src))
    return out


class MapLeafTests(unittest.TestCase):
    def test_methods(self):
        for n in (1, 300):
            self.assertEqual(run(n, str.upper, WORDS), ["HELLO", "WORLD", "FOO"])
            self.assertEqual(run(n, str.capitalize, tuple(WORDS)), ["Hello", "World", "Foo"])
            self.assertEqual(run(n, str.lower, [S("AbC")]), ["abc"])
            self.assertEqual(run(n, abs, [-1, 2.5, -3]), [1, 2.5, 3])
            self.assertEqual(run(n, ord, "ab"), [97, 98])
            self.assertEqual(run(n, len, [[1], "ab", ()]), [1, 2, 0])

    def test_errors(self):
        with self.assertRaises(TypeError) as cm:
            run(300, str.upper, ["a", 1])
        self.assertIn("descriptor 'upper'", str(cm.exception))
        with self.assertRaises(TypeError):
            run(300, ord, ["a", "bc"])
        with self.assertRaises(TypeError):
            run(300, abs, [1, "x"])

    def test_growing_source(self):
        def grow(n):
            out = None
            for i in range(n):
                xs = ["a", "b"]
                m = map(str.upper, xs)
                first = next(m)
                xs.append("c")
                out = [first] + list(m)
            return out

        self.assertEqual(grow(300), ["A", "B", "C"])


if __name__ == "__main__":
    unittest.main()
