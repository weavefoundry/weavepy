"""`operator.methodcaller` called on native values.

A `methodcaller` with positional arguments only, called on a `str`,
`bytes`, `list`, `dict`, `set` or `int` whose method is a leaf builtin,
runs that method in place; anything else (an instance, a subclass, stored
keywords, a missing method, arguments the method refuses) takes the full
call, with CPython's result or error.
"""

import operator
import unittest


class MethodcallerTests(unittest.TestCase):
    def test_native_receivers(self):
        cases = [
            (operator.methodcaller("lower"), "AbC", "abc"),
            (operator.methodcaller("upper"), "abc", "ABC"),
            (operator.methodcaller("strip"), "  x ", "x"),
            (operator.methodcaller("split", ","), "a,b", ["a", "b"]),
            (operator.methodcaller("split"), "a b", ["a", "b"]),
            (operator.methodcaller("startswith", "ab"), "abc", True),
            (operator.methodcaller("replace", "a", "b", 1), "aaa", "baa"),
            (operator.methodcaller("get", "k", 0), {"k": 1}, 1),
            (operator.methodcaller("get", "z", 0), {"k": 1}, 0),
            (operator.methodcaller("get", "z"), {"k": 1}, None),
            (operator.methodcaller("bit_length"), 255, 8),
            (operator.methodcaller("count", 1), [1, 2, 1], 2),
            (operator.methodcaller("index", 2), (1, 2), 1),
            (operator.methodcaller("hex"), b"\x01\xff", "01ff"),
            (operator.methodcaller("issubset", {1, 2}), {1}, True),
        ]
        for _ in range(300):
            for m, obj, want in cases:
                self.assertEqual(m(obj), want)

    def test_mutating_methods(self):
        append = operator.methodcaller("append", 7)
        pop = operator.methodcaller("pop")
        add = operator.methodcaller("add", 3)
        xs, s = [], set()
        for _ in range(200):
            append(xs)
            add(s)
        self.assertEqual(len(xs), 200)
        self.assertEqual(s, {3})
        for _ in range(200):
            self.assertEqual(pop(xs), 7)
        self.assertEqual(xs, [])

    def test_errors_and_other_receivers(self):
        with self.assertRaises(IndexError):
            operator.methodcaller("pop")([])
        with self.assertRaises(AttributeError):
            operator.methodcaller("nope")("x")
        with self.assertRaises(TypeError):
            operator.methodcaller("split", 5)("a b")
        with self.assertRaises(KeyError):
            operator.methodcaller("pop", "z")({})

        class S(str):
            def lower(self):
                return "own"

        class C:
            def f(self, x):
                return x * 2

        lower = operator.methodcaller("lower")
        f = operator.methodcaller("f", 21)
        for _ in range(200):
            self.assertEqual(lower(S("ABC")), "own")
            self.assertEqual(f(C()), 42)
        kw = operator.methodcaller("split", sep=",")
        self.assertEqual(kw("a,b"), ["a", "b"])
        self.assertEqual(operator.methodcaller("fromkeys", "ab")(dict), {"a": None, "b": None})
        self.assertEqual(sorted(["b", "A", "c"], key=operator.methodcaller("lower")), ["A", "b", "c"])
        self.assertEqual(list(map(operator.methodcaller("strip"), [" a", "b "])), ["a", "b"])


if __name__ == "__main__":
    unittest.main()
