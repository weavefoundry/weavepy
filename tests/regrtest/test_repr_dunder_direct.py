"""Compiled `repr(obj)` and f-string `!r` and `!s` conversions call a
class's Python `__repr__` or `__str__` directly; the text, a `str`
subclass result (kept as it is), a non-string result's TypeError, raises
(caught in the frame or past it), recursion guarded by
`reprlib.recursive_repr`, enum members and Decimals (answered natively),
objects without their own `__repr__`, `repr` shadowed, and methods
replaced later match the general call."""

import builtins
import enum
import reprlib
import unittest
from decimal import Decimal


class K:
    def __init__(self, a):
        self.a = a
        self.calls = 0

    def fmt(self):
        return "K(%r)" % (self.a,)

    def __repr__(self):
        self.calls += 1
        return self.fmt()


class Leaf:
    def __repr__(self):
        return "Leaf()"


class S(str):
    pass


class SubResult:
    def __repr__(self):
        return S("sub")


class Bad:
    def __repr__(self):
        return 5


class Raiser:
    def __repr__(self):
        raise KeyError("repr")


class Node:
    def __init__(self):
        self.kids = []

    @reprlib.recursive_repr()
    def __repr__(self):
        return "Node(%s)" % ", ".join(repr(k) for k in self.kids)


class Color(enum.Enum):
    RED = 1


class Num(int):
    pass


class Both:
    def __str__(self):
        return "str"

    def __repr__(self):
        return "repr"


class ReprDunderDirect(unittest.TestCase):
    def test_text(self):
        k = K("x")
        for i in range(3000):
            self.assertEqual(repr(k), "K('x')")
        self.assertEqual(k.calls, 3000)
        o = Leaf()
        for i in range(3000):
            self.assertEqual(repr(o), "Leaf()")

    def test_subclass_result(self):
        o = SubResult()
        for i in range(1000):
            r = repr(o)
            self.assertEqual(r, "sub")
            self.assertIs(type(r), S)

    def test_bad_result(self):
        b = Bad()
        for i in range(500):
            with self.assertRaises(TypeError) as cm:
                repr(b)
            self.assertEqual(str(cm.exception), "__repr__ returned non-string (type int)")

    def test_raises(self):
        r = Raiser()
        n = 0
        for i in range(1000):
            try:
                repr(r)
            except KeyError:
                n += 1
        self.assertEqual(n, 1000)

        def show(o):
            return repr(o) + "!"

        for i in range(1000):
            with self.assertRaises(KeyError):
                show(r)
            self.assertEqual(show(K(i)), "K(%d)!" % i)

    def test_recursive(self):
        a = Node()
        b = Node()
        a.kids.append(b)
        b.kids.append(a)
        for i in range(500):
            self.assertEqual(repr(a), "Node(Node(...))")

    def test_other_objects(self):
        d = Decimal("1.5")
        for i in range(1000):
            self.assertEqual(repr(i), "%d" % i)
            self.assertEqual(repr(S("s")), "'s'")
            self.assertEqual(repr(d), "Decimal('1.5')")
            self.assertEqual(repr(KeyError(1)), "KeyError(1)")
            self.assertTrue(repr(object()).startswith("<object object at"))

    def test_conversions(self):
        k = K("y")
        b = Both()
        for i in range(2000):
            self.assertEqual(f"{k!r}", "K('y')")
            self.assertEqual(f"[{k!s}]", "[K('y')]")
            self.assertEqual(f"{b!r} {b!s} {b}", "repr str str")
            self.assertEqual(f"{SubResult()!r:>4}", " sub")
        self.assertEqual(k.calls, 4000)
        for i in range(500):
            with self.assertRaises(TypeError) as cm:
                f"{Bad()!r}"
            self.assertEqual(str(cm.exception), "__repr__ returned non-string (type int)")
            with self.assertRaises(KeyError):
                f"{Raiser()!r}"

    def test_native_text(self):
        c = Color.RED
        d = Decimal("-2.50")
        n = Num(7)
        for i in range(2000):
            self.assertEqual(str(c), "Color.RED")
            self.assertEqual(f"{c!s}", "Color.RED")
            self.assertEqual(repr(c), "<Color.RED: 1>")
            self.assertEqual(str(d), "-2.50")
            self.assertEqual(repr(d), "Decimal('-2.50')")
            self.assertEqual(f"{d!r} {d!s}", "Decimal('-2.50') -2.50")
            self.assertEqual(str(n), "7")
            self.assertEqual(f"{n!r}", "7")

    def test_repr_shadowed(self):
        k = K(1)
        real = builtins.repr
        try:
            for i in range(500):
                self.assertEqual(repr(k), "K(1)")
            builtins.repr = lambda o: "shadow"
            got = repr(k)
        finally:
            builtins.repr = real
        self.assertEqual(got, "shadow")
        self.assertEqual(repr(k), "K(1)")

    def test_replaced(self):
        class C:
            def __repr__(self):
                return "a"

        c = C()
        out = []
        for i in range(1000):
            out.append(repr(c))
        C.__repr__ = lambda self: "b"
        for i in range(1000):
            out.append(repr(c))
        c.__repr__ = lambda: "instance"
        out.append(repr(c))
        del C.__repr__
        self.assertTrue(repr(c).startswith("<"))
        self.assertEqual((out.count("a"), out.count("b")), (1000, 1001))


if __name__ == "__main__":
    unittest.main()
