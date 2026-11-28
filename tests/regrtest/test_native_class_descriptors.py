"""Native class and static methods read through a class.

`itertools.chain.from_iterable`, `dict.fromkeys`, and `str.maketrans` bind
(or don't) as `classmethod.__get__` and `staticmethod.__get__` do, from a
site's memo once it's warm, while Python class methods keep their own
lane. `dict.fromkeys` of a string, a mapping, or a set builds in place.
"""

import itertools
import unittest

N = 3000


class Plain:
    @classmethod
    def cm(cls, x):
        return (cls, x)

    @staticmethod
    def sm(x):
        return x


class Sub(Plain):
    pass


class D(dict):
    pass


class NativeClassDescriptorTests(unittest.TestCase):
    def test_chain_from_iterable_binds_each_read(self):
        out = []
        for _ in range(N):
            f = itertools.chain.from_iterable
            out.append(f)
        self.assertTrue(all(f.__self__ is itertools.chain for f in out))
        self.assertIsNot(out[0], out[1])
        self.assertEqual(list(out[-1]([[1, 2], [3]])), [1, 2, 3])

    def test_fromkeys_through_classes(self):
        for _ in range(N):
            f = dict.fromkeys
        self.assertIs(f.__self__, dict)
        for _ in range(N):
            g = D.fromkeys
        self.assertIs(g.__self__, D)
        self.assertIs(type(g("ab")), D)

    def test_maketrans_is_unbound(self):
        for _ in range(N):
            m = str.maketrans
        self.assertFalse(hasattr(m, "__self__") and m.__self__ is str)
        self.assertEqual("abc".translate(m("ab", "xy")), "xyc")
        for _ in range(N):
            b = bytes.maketrans
        self.assertEqual(b"abc".translate(b(b"a", b"z")), b"zbc")

    def test_python_class_methods_unchanged(self):
        for i in range(N):
            r = Plain.cm(i)
        self.assertEqual(r, (Plain, N - 1))
        for i in range(N):
            r = Sub.cm(i)
        self.assertEqual(r, (Sub, N - 1))
        for i in range(N):
            r = Plain.sm(i)
        self.assertEqual(r, N - 1)

    def test_reassigned_class_attribute(self):
        class K:
            @classmethod
            def f(cls):
                return 1

        got = []
        for i in range(N):
            if i == N // 2:
                K.f = staticmethod(len)
            got.append(K.f)
        self.assertEqual(got[0](), 1)
        self.assertIs(got[-1], len)

    def test_fromkeys_sources(self):
        for _ in range(N):
            a = dict.fromkeys("abca")
            b = dict.fromkeys({"x": 1, "y": 2}, 0)
            c = dict.fromkeys(frozenset([3]), "v")
            d = dict.fromkeys("hé", [])
        self.assertEqual(list(a.items()), [("a", None), ("b", None), ("c", None)])
        self.assertEqual(b, {"x": 0, "y": 0})
        self.assertEqual(c, {3: "v"})
        self.assertEqual(list(d), ["h", "é"])
        self.assertIs(d["h"], d["é"])
        s = {1, 2, 3}
        self.assertEqual(list(dict.fromkeys(s)), list(s))
        e = {}
        self.assertEqual(dict.fromkeys(e), {})
        self.assertEqual(dict.fromkeys(""), {})
        with self.assertRaises(TypeError):
            dict.fromkeys([[1]])


if __name__ == "__main__":
    unittest.main()
