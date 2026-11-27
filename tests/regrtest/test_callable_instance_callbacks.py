"""A callable instance (its class's Python `__call__`) as a native
adapter's callback (`map`, `filter`, `sorted`'s key) is called as its
method; the results, `*args` signatures, raises, a `__call__` replaced or
removed, and an instance `__call__` attribute (ignored, as CPython's
lookup on the type ignores it) match the general call."""

import unittest


class Adder:
    def __init__(self, k):
        self.k = k
        self.calls = 0

    def __call__(self, x):
        self.calls += 1
        return x + self.k


class Var:
    def __call__(self, *a):
        return len(a)


class Raiser:
    def __call__(self, x):
        if x == 3:
            raise KeyError(x)
        return x


class CallableInstanceCallbacks(unittest.TestCase):
    def test_map(self):
        a = Adder(10)
        for _ in range(300):
            self.assertEqual(list(map(a, range(5))), [10, 11, 12, 13, 14])
        self.assertEqual(a.calls, 1500)

    def test_filter_and_sorted(self):
        class Odd:
            def __call__(self, x):
                return x % 2

        class Neg:
            def __call__(self, x):
                return -x

        for _ in range(300):
            self.assertEqual(list(filter(Odd(), range(7))), [1, 3, 5])
            self.assertEqual(sorted([3, 1, 2], key=Neg()), [3, 2, 1])

    def test_varargs(self):
        for _ in range(300):
            self.assertEqual(list(map(Var(), "ab")), [1, 1])
            self.assertEqual(list(map(Var(), "ab", "cd")), [2, 2])

    def test_raises(self):
        r = Raiser()
        for _ in range(300):
            out = []
            with self.assertRaises(KeyError):
                for v in map(r, range(10)):
                    out.append(v)
            self.assertEqual(out, [0, 1, 2])

    def test_replaced_and_instance_attribute(self):
        class C:
            def __call__(self, x):
                return "a"

        c = C()
        self.assertEqual(list(map(c, range(3))), ["a"] * 3)
        C.__call__ = lambda self, x: "b"
        self.assertEqual(list(map(c, range(3))), ["b"] * 3)
        c.__call__ = lambda x: "instance"
        self.assertEqual(list(map(c, range(3))), ["b"] * 3)
        del C.__call__
        with self.assertRaises(TypeError):
            list(map(c, range(3)))


if __name__ == "__main__":
    unittest.main()
