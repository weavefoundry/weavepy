"""Methods, staticmethods and classmethods read as values through
instances from compiled loops (the class entry memoized per class
version, bound or unwrapped on each read): identity and binding of the
results, a site that sees several classes with different kinds of entry,
an instance attribute that shadows later, the class entry replaced,
subclasses, and calls of the values, each matching CPython."""

import unittest


class A:
    def m(self):
        return ("m", type(self).__name__)

    @staticmethod
    def s(x=0):
        return ("s", x)

    @classmethod
    def c(cls):
        return ("c", cls.__name__)


class B(A):
    pass


class C(A):
    m = staticmethod(lambda: ("sm",))
    s = classmethod(lambda cls: ("cs", cls.__name__))

    def c(self):
        return ("cm", type(self).__name__)


def read(n, o, name):
    out = None
    for i in range(n):
        out = getattr(o, "__class__") and (o.m, o.s, o.c)
    return out


def site(n, objs):
    seen = []
    for i in range(n):
        for o in objs:
            m = o.m
            s = o.s
            c = o.c
            seen = (m(), s(), c())
    return seen


class MethodReadTests(unittest.TestCase):
    def test_binding(self):
        a, b = A(), B()
        for i in range(300):
            m, s, c = a.m, a.s, a.c
        self.assertEqual(m.__self__, a)
        self.assertIs(m.__func__, A.__dict__["m"])
        self.assertIs(s, A.__dict__["s"].__func__)
        self.assertIs(c.__self__, A)
        self.assertEqual((m(), s(3), c()), (("m", "A"), ("s", 3), ("c", "A")))
        for i in range(300):
            bc = b.c
            bm = b.m
        self.assertIs(bc.__self__, B)
        self.assertEqual(bm(), ("m", "B"))
        self.assertIsNot(a.m, a.m)
        self.assertEqual(a.m, a.m)

    def test_polymorphic_site(self):
        r = site(300, [A(), B(), C()])
        self.assertEqual(r, (("sm",), ("cs", "C"), ("cm", "C")))

    def test_shadow_and_replace(self):
        a = A()
        for i in range(300):
            m = a.m
        a.m = lambda: "inst"
        self.assertEqual(a.m(), "inst")
        del a.m
        self.assertEqual(a.m(), ("m", "A"))
        old = A.__dict__["s"]
        A.s = staticmethod(lambda x=0: ("s2", x))
        try:
            self.assertEqual(a.s(1), ("s2", 1))
        finally:
            A.s = old
        for i in range(300):
            v = a.s
        self.assertEqual(v(4), ("s", 4))


if __name__ == "__main__":
    unittest.main()
