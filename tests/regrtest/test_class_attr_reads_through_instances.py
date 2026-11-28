"""Class attributes read through instances from compiled loops (memoized
per class version, now ahead of the slow lanes, and for module values
too): ints, strings, a module, a range, a bytearray, an instance
attribute that shadows later, the class attribute reassigned, a
subclass overriding it, slotted classes, several classes at one site,
and a class with __getattribute__, each matching CPython."""

import posixpath
import unittest


class A:
    LIMIT = 10
    NAME = "a"
    mod = posixpath
    span = range(3)
    buf = bytearray(b"x")

    def limit(self):
        return self.LIMIT

    def mod_sep(self):
        return self.mod.sep


class B(A):
    LIMIT = 20


class S:
    __slots__ = ("v",)
    mod = posixpath
    LIMIT = 5

    def get(self):
        return self.mod, self.LIMIT


class G:
    LIMIT = 1

    def __getattribute__(self, name):
        if name == "LIMIT":
            return 99
        return object.__getattribute__(self, name)


def reads(n, objs):
    out = []
    for i in range(n):
        for o in objs:
            out.append((o.LIMIT, o.mod is posixpath))
    return out[-len(objs):]


class ClassAttrTests(unittest.TestCase):
    def test_values(self):
        a, b, s = A(), B(), S()
        for i in range(300):
            r = (a.limit(), a.NAME, a.mod_sep(), a.span, a.buf, b.limit(), s.get())
        self.assertEqual(r[:5], (10, "a", "/", range(3), bytearray(b"x")))
        self.assertEqual(r[5], 20)
        self.assertEqual(r[6], (posixpath, 5))
        self.assertEqual(reads(300, [a, b, s]), [(10, True), (20, True), (5, True)])

    def test_shadow_and_reassign(self):
        a = A()
        for i in range(300):
            v = a.limit()
        a.LIMIT = 7
        self.assertEqual(a.limit(), 7)
        del a.LIMIT
        self.assertEqual(a.limit(), 10)
        A.LIMIT = 11
        try:
            self.assertEqual(a.limit(), 11)
            self.assertEqual(B().limit(), 20)
        finally:
            A.LIMIT = 10
        a.mod = None
        with self.assertRaises(AttributeError):
            a.mod_sep()

    def test_getattribute(self):
        g = G()
        for i in range(300):
            v = g.LIMIT
        self.assertEqual(v, 99)


if __name__ == "__main__":
    unittest.main()
