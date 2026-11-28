"""getattr, hasattr and attrgetter read a `__slots__` member directly; an
unset member gives the default, a `__getattr__`'s answer, or the
AttributeError, as the full lookup does."""

import operator
import unittest


class S:
    __slots__ = ("a", "b")

    def __init__(self, a):
        self.a = a


class G(S):
    __slots__ = ()

    def __getattr__(self, name):
        return "fallback:" + name


class Sub(S):
    pass


class Prop:
    __slots__ = ("_x",)

    def __init__(self):
        self._x = 3

    @property
    def x(self):
        return self._x * 2


class SlotGetattr(unittest.TestCase):
    def test_reads(self):
        s, g, sub = S(1), G(2), Sub(3)
        sub.c = 9
        get_a = operator.attrgetter("a")
        for _ in range(500):
            self.assertEqual(getattr(s, "a"), 1)
            self.assertEqual(getattr(s, "a", None), 1)
            self.assertIsNone(getattr(s, "b", None))
            self.assertEqual(getattr(s, "b", 5), 5)
            self.assertTrue(hasattr(s, "a"))
            self.assertFalse(hasattr(s, "b"))
            self.assertEqual(getattr(g, "b", None), "fallback:b")
            self.assertTrue(hasattr(g, "b"))
            self.assertEqual(getattr(sub, "a"), 3)
            self.assertEqual(getattr(sub, "c"), 9)
            self.assertEqual(get_a(s), 1)
            self.assertEqual(list(map(get_a, [s, sub])), [1, 3])
            self.assertEqual(getattr(Prop(), "x"), 6)
        with self.assertRaises(AttributeError):
            getattr(s, "b")

    def test_set_and_delete(self):
        s = S(1)
        for i in range(300):
            s.b = i
            self.assertEqual(getattr(s, "b", None), i)
            del s.b
            self.assertIsNone(getattr(s, "b", None))
            self.assertFalse(hasattr(s, "b"))

    def test_class_change(self):
        s = S(1)
        for _ in range(300):
            getattr(s, "a")
        S.a_alias = 1
        self.assertEqual(getattr(s, "a"), 1)
        del S.a_alias

    def test_class_gains_attribute_after_misses(self):
        class P:
            pass

        class Q(P):
            pass

        q = Q()
        for _ in range(300):
            self.assertEqual(getattr(q, "z", 1), 1)
            self.assertFalse(hasattr(q, "z"))
        P.z = 7
        self.assertEqual(getattr(q, "z", 1), 7)
        P.z = property(lambda self: 8)
        self.assertEqual(getattr(q, "z", 1), 8)
        q.__dict__["z"] = 9
        self.assertEqual(getattr(q, "z", 1), 8)
        del P.z
        self.assertEqual(getattr(q, "z", 1), 9)
        P.__getattr__ = lambda self, name: "ga"
        self.assertEqual(getattr(q, "w", 1), "ga")

    def test_slot_replaced_on_class(self):
        class T:
            __slots__ = ("v",)

        t = T()
        t.v = 1
        for _ in range(300):
            self.assertEqual(getattr(t, "v", None), 1)
        T.v = 5
        self.assertEqual(getattr(t, "v", None), 5)


if __name__ == "__main__":
    unittest.main()
