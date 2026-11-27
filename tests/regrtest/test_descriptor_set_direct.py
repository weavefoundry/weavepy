"""Compiled attribute stores through a data descriptor's Python `__set__`
call it directly; the values stored, validation raises (caught in the
frame or past it), stores from `self.x = v`, descriptors shadowing the
instance dict, non-data descriptors, and a descriptor replaced later
match the general store."""

import unittest
import weakref


class Typed:
    def __set_name__(self, owner, name):
        self.name = "_" + name

    def __get__(self, obj, cls):
        if obj is None:
            return self
        return getattr(obj, self.name)

    def __set__(self, obj, v):
        if not isinstance(v, int):
            raise TypeError(f"{self.name[1:]} must be int")
        setattr(obj, self.name, v)


class Counted:
    def __init__(self):
        self.sets = 0

    def __get__(self, obj, cls):
        return obj.__dict__.get("c")

    def __set__(self, obj, v):
        self.sets += 1
        obj.__dict__["c"] = v
        return "ignored"


class NonData:
    def __get__(self, obj, cls):
        return "from descriptor"


class P:
    x = Typed()
    c = Counted()
    nd = NonData()

    def __init__(self, x):
        self.x = x

    def bump(self):
        for i in range(20):
            self.x = self.x + 1


class Tracked:
    deleted = 0

    def __del__(self):
        Tracked.deleted += 1


class Holder:
    def __set__(self, obj, v):
        obj.__dict__["h"] = v

    def __get__(self, obj, cls):
        return obj.__dict__["h"]


class Q:
    h = Holder()


class DescriptorSetDirect(unittest.TestCase):
    def test_typed(self):
        p = P(0)
        for i in range(3000):
            p.x = i
        self.assertEqual(p.x, 2999)
        self.assertEqual(p._x, 2999)
        self.assertNotIn("x", p.__dict__)

    def test_counted_and_dict_shadow(self):
        p = P(0)
        for i in range(2000):
            p.c = i
        self.assertEqual(P.__dict__["c"].sets, 2000)
        self.assertEqual(p.__dict__["c"], 1999)
        p.__dict__["x"] = "shadow"
        p.x = 5
        self.assertEqual(p._x, 5)
        self.assertEqual(p.x, 5)

    def test_validation_raises(self):
        p = P(0)
        n = 0
        for i in range(1000):
            try:
                p.x = "bad"
            except TypeError as e:
                n += 1
                self.assertEqual(str(e), "x must be int")
        self.assertEqual(n, 1000)

        def store(o, v):
            o.x = v
            return "after"

        for i in range(1000):
            with self.assertRaises(TypeError):
                store(p, "no")
            self.assertEqual(store(p, i), "after")
        self.assertEqual(p.x, 999)

    def test_self_stores(self):
        for i in range(200):
            p = P(i)
            p.bump()
            self.assertEqual(p.x, i + 20)

    def test_non_data_descriptor(self):
        p = P(0)
        for i in range(1000):
            p.nd = i
        self.assertEqual(p.nd, 999)
        self.assertEqual(p.__dict__["nd"], 999)

    def test_lifetimes(self):
        q = Q()
        for i in range(500):
            q.h = Tracked()
        before = Tracked.deleted
        t = Tracked()
        r = weakref.ref(t)
        q.h = t
        del t
        self.assertIsNotNone(r())
        q.h = None
        self.assertIsNone(r())
        self.assertEqual(Tracked.deleted, before + 2)

    def test_replaced(self):
        class D:
            def __set__(self, obj, v):
                obj.__dict__["d"] = v + 1

            def __get__(self, obj, cls):
                return obj.__dict__["d"]

        class C:
            d = D()

        c = C()
        for i in range(1000):
            c.d = i
        self.assertEqual(c.d, 1000)
        D.__set__ = lambda self, obj, v: obj.__dict__.__setitem__("d", -v)
        for i in range(1000):
            c.d = i
        self.assertEqual(c.d, -999)
        C.d = 5
        for i in range(1000):
            c.d = i
        self.assertEqual(c.__dict__["d"], 999)
        self.assertEqual(c.d, 999)


if __name__ == "__main__":
    unittest.main()
