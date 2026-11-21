"""Container operators, `len(range)` and `dict(mapping, **kw)` in hot loops.

Loops that tier 2 compiles run `list + list`, `tuple + tuple`,
`dict | dict`, set algebra and `bytes + bytes` without the general
operator dispatch when no Python code is involved; operands that need it
(a key with a Python `__eq__`, a subclass, a reflected operator) still
take it. `len()` of a range and `dict(mapping, **kw)` have direct paths.
"""

import unittest

N = 4000


class Key:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return hash(self.v)

    def __eq__(self, other):
        return isinstance(other, Key) and self.v == other.v


class MyList(list):
    def __add__(self, other):
        return "mine"


class Radd:
    def __radd__(self, other):
        return "radd"


class ContainerBinopTests(unittest.TestCase):
    def test_plain_operands(self):
        L = [1, 2, 3]
        D = {"a": 1, "b": 2}
        for i in range(N):
            a = L + [i]
            b = (1, 2) + (i,)
            c = D | {"q": i}
            d = {1, 2} | {i}
            e = {1, 2, 3} & {2, i}
            f = {1, 2, 3} - {2}
            g = {1, 2} ^ {2, 3}
            h = b"ab" + b"cd"
        self.assertEqual(a, [1, 2, 3, N - 1])
        self.assertEqual(b, (1, 2, N - 1))
        self.assertEqual(c, {"a": 1, "b": 2, "q": N - 1})
        self.assertEqual(list(c), ["a", "b", "q"])
        self.assertEqual(d, {1, 2, N - 1})
        self.assertEqual(e, {2})
        self.assertEqual((f, g, h), ({1, 3}, {1, 3}, b"abcd"))
        self.assertEqual(L, [1, 2, 3])
        self.assertEqual(D, {"a": 1, "b": 2})

    def test_python_keys_and_subclasses(self):
        k1, k2 = Key(1), Key(1)
        ml = MyList([1])
        r = Radd()
        for i in range(N):
            a = {k1: "x"} | {k2: "y"}
            b = {k1} | {k2}
            c = ml + [2]
            d = [1] + r
        self.assertEqual(len(a), 1)
        self.assertEqual(a[k1], "y")
        self.assertEqual(len(b), 1)
        self.assertEqual((c, d), ("mine", "radd"))

    def test_errors(self):
        for i in range(N // 10):
            with self.assertRaises(TypeError):
                [1] + (2,)
            with self.assertRaises(TypeError):
                {"a": 1} | [1]
            with self.assertRaises(TypeError):
                {1} | [1]

    def test_range_len_and_dict_kw(self):
        base = {"a": 1}
        for i in range(N):
            n = (len(range(10, 100, 3)), len(range(i)), len(range(5, 0, -2)), len(range(3, 3)))
            d = dict(base, z=i)
            e = dict({"k": 1}.items(), a=2)
            f = dict(z=i)
        self.assertEqual(n, (30, N - 1, 3, 0))
        self.assertEqual(d, {"a": 1, "z": N - 1})
        self.assertEqual(list(d), ["a", "z"])
        self.assertEqual(e, {"k": 1, "a": 2})
        self.assertEqual(f, {"z": N - 1})
        self.assertEqual(base, {"a": 1})
        with self.assertRaises(OverflowError):
            len(range(10 ** 30))
        self.assertEqual(dict(base, a=5), {"a": 5})
        self.assertEqual(dict({1: "x"}, a=5), {1: "x", "a": 5})


if __name__ == "__main__":
    unittest.main()
