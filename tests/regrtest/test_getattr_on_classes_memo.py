"""`getattr(cls, name[, default])` answers that are remembered per class.

The answer for a class under plain `type` is kept by the class's version
and the name: a missing name with a default, a value found on the MRO,
the class's own `__name__`. Every change that could alter the answer
must be seen at once: an attribute set, replaced or deleted on the class
or a base, a base swapped, a metaclass with its own answers, and a value
the class stops holding must die when it does.
"""

import gc
import unittest
import weakref

ROUNDS = 2000


def read(cls, name, *default):
    return getattr(cls, name, *default)


def has(cls, name):
    return hasattr(cls, name)


class Base:
    shared = "base"

    def method(self):
        return "base method"

    @staticmethod
    def static():
        return "static"


class Child(Base):
    count = 3
    flag = True
    ratio = 1.5
    data = b"bytes"
    nothing = None
    items = (1, 2)
    table = {"k": 1}

    def own(self):
        return "own"


class Token:
    pass


class GetattrOnClassesTests(unittest.TestCase):
    def test_missing_with_default_then_defined(self):
        class C(Base):
            pass

        marker = object()
        for _ in range(ROUNDS):
            self.assertIs(read(C, "__wrapped__", marker), marker)
            self.assertIsNone(read(C, "later", None))
            self.assertFalse(has(C, "later"))
        C.later = 7
        self.assertEqual(read(C, "later", None), 7)
        self.assertTrue(has(C, "later"))
        C.__wrapped__ = len
        self.assertIs(read(C, "__wrapped__", marker), len)
        del C.later
        self.assertIsNone(read(C, "later", None))
        with self.assertRaises(AttributeError):
            read(C, "later")

    def test_missing_then_defined_on_a_base(self):
        class A:
            pass

        class B(A):
            pass

        for _ in range(ROUNDS):
            self.assertEqual(read(B, "color", "none"), "none")
        A.color = "red"
        self.assertEqual(read(B, "color", "none"), "red")
        B.color = "blue"
        self.assertEqual(read(B, "color", "none"), "blue")
        self.assertEqual(read(A, "color", "none"), "red")
        del B.color
        del A.color
        self.assertEqual(read(B, "color", "none"), "none")

    def test_values_of_every_kind(self):
        for _ in range(ROUNDS):
            self.assertEqual(read(Child, "count"), 3)
            self.assertIs(read(Child, "flag"), True)
            self.assertEqual(read(Child, "ratio"), 1.5)
            self.assertEqual(read(Child, "data"), b"bytes")
            self.assertIsNone(read(Child, "nothing", 1))
            self.assertIs(read(Child, "items"), Child.__dict__["items"])
            self.assertIs(read(Child, "table"), Child.__dict__["table"])
            self.assertEqual(read(Child, "shared"), "base")
            self.assertIs(read(Child, "method"), Base.__dict__["method"])
            self.assertIs(read(Child, "own"), Child.__dict__["own"])
            self.assertIs(read(Child, "static"), Base.__dict__["static"].__func__)
            self.assertIs(read(Child, "__init__"), object.__init__)

    def test_replaced_values(self):
        class C:
            x = 1
            f = len

            def m(self):
                return 1

        first = C.__dict__["m"]
        for _ in range(ROUNDS):
            self.assertEqual(read(C, "x"), 1)
            self.assertIs(read(C, "m"), first)
            self.assertIs(read(C, "f"), len)
        C.x = 2
        C.f = repr

        def second(self):
            return 2

        C.m = second
        for _ in range(50):
            self.assertEqual(read(C, "x"), 2)
            self.assertIs(read(C, "m"), second)
            self.assertIs(read(C, "f"), repr)

    def test_a_dropped_value_dies_at_once(self):
        class C:
            pass

        for kind in (Token, dict, list):
            value = kind()
            C.held = value
            ref = weakref.ref(value) if kind is Token else None
            for _ in range(ROUNDS):
                self.assertIs(read(C, "held"), value)
            del value
            if ref is not None:
                self.assertIsNotNone(ref())
                del C.held
                self.assertIsNone(ref())
            else:
                del C.held
            self.assertEqual(read(C, "held", "gone"), "gone")

        def make():
            def f(self):
                return 1

            return f

        C.fn = make()
        ref = weakref.ref(C.fn)
        for _ in range(ROUNDS):
            self.assertIs(read(C, "fn"), ref())
        C.fn = None
        gc.collect()
        self.assertIsNone(ref())
        self.assertIsNone(read(C, "fn", 1))

    def test_a_class_that_dies(self):
        refs = []
        for i in range(200):

            class Temp:
                tag = (i, i)

            for _ in range(20):
                self.assertEqual(read(Temp, "tag"), (i, i))
                self.assertEqual(read(Temp, "missing", i), i)
            refs.append(weakref.ref(Temp))
            del Temp
        gc.collect()
        self.assertTrue(all(r() is None for r in refs))

    def test_names(self):
        class Outer:
            class Inner:
                pass

        for _ in range(ROUNDS):
            self.assertEqual(read(Outer.Inner, "__name__"), "Inner")
            self.assertTrue(read(Outer.Inner, "__qualname__").endswith("Outer.Inner"))
            self.assertEqual(read(int, "__name__"), "int")
            self.assertEqual(read(dict, "__qualname__"), "dict")
        Outer.Inner.__name__ = "Renamed"
        self.assertEqual(read(Outer.Inner, "__name__"), "Renamed")
        Outer.Inner.__qualname__ = "Q.Renamed"
        self.assertEqual(read(Outer.Inner, "__qualname__"), "Q.Renamed")
        self.assertIs(read(Outer.Inner, "__name__"), read(Outer.Inner, "__name__"))
        dynamic = type("Dyn", (Outer.Inner,), {})
        self.assertEqual(read(dynamic, "__name__"), "Dyn")
        self.assertEqual(read(dynamic, "__qualname__"), "Dyn")

    def test_metaclass_answers_first(self):
        class Meta(type):
            @property
            def shared(cls):
                return "from the metaclass"

            def __getattr__(cls, name):
                if name == "dynamic":
                    return "made up"
                raise AttributeError(name)

        class M(Base, metaclass=Meta):
            pass

        for _ in range(ROUNDS):
            self.assertEqual(read(M, "shared"), "from the metaclass")
            self.assertEqual(read(M, "dynamic", None), "made up")
            self.assertIsNone(read(M, "absent", None))
            self.assertEqual(read(M, "__name__"), "M")

    def test_bases_swapped(self):
        class A:
            who = "a"

        class B:
            who = "b"

        class C(A):
            pass

        for _ in range(ROUNDS):
            self.assertEqual(read(C, "who"), "a")
            self.assertEqual(read(C, "only_b", 0), 0)
        B.only_b = 9
        C.__bases__ = (B,)
        self.assertEqual(read(C, "who"), "b")
        self.assertEqual(read(C, "only_b", 0), 9)

    def test_same_text_different_name_objects(self):
        class C:
            value = 1

        for i in range(ROUNDS):
            name = "".join(["va", "lue"])
            self.assertEqual(read(C, name), 1)
            other = "".join(["val", "ue%d" % (i % 3)])
            self.assertEqual(read(C, other, -1), -1)


if __name__ == "__main__":
    unittest.main()
