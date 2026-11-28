"""`str(obj)`, `f"{obj}"`, `f"{obj!s}"` and `"%s" % obj` of an instance
whose class defines only a Python `__repr__` (keeping `object.__str__`
and `object.__format__`) call the `__repr__` directly; the text, a
non-string result's TypeError (worded as `__str__`'s), raises, a format
spec (still `object.__format__`'s TypeError), and classes that define
`__str__` or `__format__` later match the general call."""

import unittest


class Q:
    def __init__(self, a):
        self.a = a
        self.calls = 0

    def __repr__(self):
        self.calls += 1
        return "Q(%r)" % (self.a,)


class Leaf:
    def __repr__(self):
        return "Leaf"


class Bad:
    def __repr__(self):
        return 7


class Raiser:
    def __repr__(self):
        raise KeyError("r")


class Child(Q):
    def __str__(self):
        return "child"


class StrThroughRepr(unittest.TestCase):
    def test_text(self):
        q = Q("x")
        leaf = Leaf()
        for i in range(2000):
            self.assertEqual(str(q), "Q('x')")
            self.assertEqual(f"{q}", "Q('x')")
            self.assertEqual(f"[{q!s}]", "[Q('x')]")
            self.assertEqual("%s" % q, "Q('x')")
            self.assertEqual(str(leaf), "Leaf")
            self.assertEqual(f"{leaf}|{leaf!s}", "Leaf|Leaf")
        self.assertEqual(q.calls, 8000)

    def test_bad_and_raises(self):
        for i in range(300):
            for make in (lambda b: str(b), lambda b: f"{b}", lambda b: f"{b!s}", lambda b: "%s" % b):
                with self.assertRaises(TypeError) as cm:
                    make(Bad())
                self.assertEqual(str(cm.exception), "__str__ returned non-string (type int)")
                with self.assertRaises(KeyError):
                    make(Raiser())

    def test_spec_and_overrides(self):
        q = Q(1)
        for i in range(300):
            with self.assertRaises(TypeError):
                f"{q:>5}"
            self.assertEqual(str(Child(2)), "child")
            self.assertEqual(f"{Child(2)}", "child")
            self.assertEqual(repr(Child(2)), "Q(2)")

    def test_replaced(self):
        class C:
            def __repr__(self):
                return "r"

        c = C()
        out = []
        for i in range(1000):
            out.append(str(c) + f"{c}")
        C.__str__ = lambda self: "s"
        for i in range(1000):
            out.append(str(c) + f"{c}")
        C.__format__ = lambda self, spec: "f"
        for i in range(1000):
            out.append(str(c) + f"{c}")
        self.assertEqual(
            (out.count("rr"), out.count("ss"), out.count("sf")), (1000, 1000, 1000)
        )


if __name__ == "__main__":
    unittest.main()
