"""Compiled `str(obj)` calls a class's Python `__str__` directly; the
text, a `str` subclass result (kept as it is), a non-string result's
TypeError, raises (caught in the frame or past it), objects without
their own `__str__`, `str` shadowed, and methods replaced later match the
general call."""

import builtins
import unittest


class K:
    def __init__(self, a):
        self.a = a
        self.calls = 0

    def fmt(self):
        return "K<%r>" % (self.a,)

    def __str__(self):
        self.calls += 1
        return self.fmt()


class S(str):
    pass


class SubResult:
    def __str__(self):
        return S("sub")


class Bad:
    def __str__(self):
        return 5


class Raiser:
    def __str__(self):
        raise KeyError("str")


class OnlyRepr:
    def __repr__(self):
        return "R"


class StrDunderDirect(unittest.TestCase):
    def test_text(self):
        k = K("x")
        for i in range(3000):
            self.assertEqual(str(k), "K<'x'>")
        self.assertEqual(k.calls, 3000)

    def test_subclass_result(self):
        o = SubResult()
        for i in range(1000):
            r = str(o)
            self.assertEqual(r, "sub")
            self.assertIs(type(r), S)

    def test_bad_result(self):
        b = Bad()
        for i in range(500):
            with self.assertRaises(TypeError) as cm:
                str(b)
            self.assertEqual(str(cm.exception), "__str__ returned non-string (type int)")

    def test_raises(self):
        r = Raiser()
        n = 0
        for i in range(1000):
            try:
                str(r)
            except KeyError:
                n += 1
        self.assertEqual(n, 1000)

        def show(o):
            return str(o) + "!"

        for i in range(1000):
            with self.assertRaises(KeyError):
                show(r)
            self.assertEqual(show(K(i)), "K<%d>!" % i)

    def test_other_objects(self):
        o = OnlyRepr()
        for i in range(1000):
            self.assertEqual(str(o), "R")
            self.assertEqual(str(i), "%d" % i)
            self.assertEqual(str(S("s")), "s")

    def test_str_shadowed(self):
        k = K(1)
        real = builtins.str
        try:
            for i in range(500):
                self.assertEqual(str(k), "K<1>")
            builtins.str = lambda o: "shadow"
            got = str(k)
        finally:
            builtins.str = real
        self.assertEqual(got, "shadow")
        self.assertEqual(str(k), "K<1>")

    def test_replaced(self):
        class C:
            def __str__(self):
                return "a"

        c = C()
        out = []
        for i in range(1000):
            out.append(str(c))
        C.__str__ = lambda self: "b"
        for i in range(1000):
            out.append(str(c))
        del C.__str__
        self.assertTrue(str(c).startswith("<"))
        self.assertEqual((out.count("a"), out.count("b")), (1000, 1000))


if __name__ == "__main__":
    unittest.main()
