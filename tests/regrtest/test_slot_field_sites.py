"""Attribute sites that remember a `__slots__` member's layout position.

A warm `x.attr` or `x.attr = v` site on a slotted class reads or writes
the member by its position in the class's slot layout, and a store-only
`__init__` fills a fresh instance's members directly. Each shortcut must
step aside whenever the position stops naming the member: another class
at the site, `__class__` assignment between layouts that order the same
names differently, a class attribute replacing the member descriptor, an
unset (or deleted) member, `__getattr__`/`__getattribute__` overrides,
and a recycled instance taken over by another class. Releases must stay
prompt, so `__del__` runs exactly when a slot's old value goes.
"""

import gc
import unittest


class XY:
    __slots__ = ("x", "y")

    def __init__(self, x, y):
        self.x = x
        self.y = y


class YX:
    __slots__ = ("y", "x")

    def __init__(self, x, y):
        self.x = x
        self.y = y


class XYZ(XY):
    __slots__ = ("z",)

    def __init__(self, x, y, z=0):
        self.x = x
        self.y = y
        self.z = z


class WithDict:
    __slots__ = ("a", "__dict__")

    def __init__(self, a, b):
        self.a = a
        self.b = b


class Fallback:
    __slots__ = ("x",)

    def __getattr__(self, name):
        return ("fallback", name)


class Tracked:
    log = []

    def __init__(self, tag):
        self.tag = tag

    def __del__(self):
        Tracked.log.append(self.tag)


def read_xy(p):
    return p.x * 10 + p.y


def write_xy(p, x, y):
    p.x = x
    p.y = y


class SlotFieldSiteTest(unittest.TestCase):
    def test_reads_and_writes_warm(self):
        p = XY(1, 2)
        for i in range(3000):
            write_xy(p, i, i + 1)
            self.assertEqual(read_xy(p), i * 10 + i + 1)

    def test_polymorphic_site(self):
        objs = [XY(1, 2), YX(3, 4), XYZ(5, 6, 7)]
        for _ in range(2000):
            got = [read_xy(o) for o in objs]
            self.assertEqual(got, [12, 34, 56])
            for o in objs:
                write_xy(o, o.y, o.x)
                write_xy(o, o.y, o.x)

    def test_class_assignment_between_orders(self):
        for i in range(1500):
            p = XY(i, -i)
            self.assertEqual(read_xy(p), i * 10 - i)
            p.__class__ = YX
            self.assertEqual((p.x, p.y), (i, -i))
            self.assertEqual(read_xy(p), i * 10 - i)
            write_xy(p, -i, i)
            self.assertEqual((p.x, p.y), (-i, i))
            p.__class__ = XY
            self.assertEqual(read_xy(p), -i * 10 + i)

    def test_unset_and_deleted_members(self):
        p = XY(1, 2)
        for i in range(2000):
            self.assertEqual(read_xy(p), 12)
        del p.y
        with self.assertRaises(AttributeError):
            read_xy(p)
        q = XY.__new__(XY)
        with self.assertRaises(AttributeError):
            read_xy(q)
        q.y = 5
        with self.assertRaises(AttributeError):
            read_xy(q)
        q.x = 4
        self.assertEqual(read_xy(q), 45)

    def test_getattr_fallback_on_unset_member(self):
        f = Fallback()

        def get(o):
            return o.x

        for _ in range(2000):
            self.assertEqual(get(f), ("fallback", "x"))
        f.x = 1
        for _ in range(2000):
            self.assertEqual(get(f), 1)
        del f.x
        self.assertEqual(get(f), ("fallback", "x"))

    def test_member_replaced_by_class_attribute(self):
        class C:
            __slots__ = ("v",)

            def __init__(self, v):
                self.v = v

        def get(o):
            return o.v

        c = C(3)
        for _ in range(3000):
            self.assertEqual(get(c), 3)
        member = C.__dict__["v"]
        C.v = property(lambda self: "prop")
        self.assertEqual(get(c), "prop")
        C.v = member
        self.assertEqual(get(c), 3)

    def test_getattribute_override_in_subclass(self):
        class Loud(XY):
            __slots__ = ()

            def __getattribute__(self, name):
                if name == "x":
                    return 100
                return object.__getattribute__(self, name)

        p = XY(1, 2)
        for _ in range(2000):
            self.assertEqual(read_xy(p), 12)
        self.assertEqual(read_xy(Loud(1, 2)), 1002)

    def test_setattr_override(self):
        class Upper(XY):
            __slots__ = ()

            def __setattr__(self, name, value):
                object.__setattr__(self, name, value * 2)

        p = XY(0, 0)
        for i in range(2000):
            write_xy(p, i, i)
        u = Upper(1, 2)
        self.assertEqual((u.x, u.y), (2, 4))
        write_xy(u, 3, 4)
        self.assertEqual((u.x, u.y), (6, 8))
        object.__setattr__(u, "x", 7)
        self.assertEqual(u.x, 7)

    def test_store_only_init_shared_with_subclass(self):
        class Base:
            __slots__ = ("a", "b")

            def __init__(self, a, b):
                self.a = a
                self.b = b

        class Pre(Base):
            __slots__ = ("c",)

        class Plain(Base):
            pass

        for i in range(2000):
            b = Base(i, i + 1)
            p = Pre(i, i + 2)
            q = Plain(i, i + 3)
            self.assertEqual((b.a, b.b), (i, i + 1))
            self.assertEqual((p.a, p.b), (i, i + 2))
            self.assertEqual((q.a, q.b), (i, i + 3))
            self.assertFalse(hasattr(p, "c"))
            self.assertEqual(q.__dict__, {})
        w = WithDict(1, 2)
        for i in range(2000):
            w = WithDict(i, i * 2)
        self.assertEqual((w.a, w.b, w.__dict__), (1999, 3998, {"b": 3998}))

    def test_repeated_store_in_init(self):
        class Twice:
            __slots__ = ("v", "w")

            def __init__(self, a, b):
                self.v = a
                self.v = b

        for i in range(2000):
            t = Twice(i, -i)
            self.assertEqual(t.v, -i)
            self.assertFalse(hasattr(t, "w"))

    def test_recycled_instances_switch_classes(self):
        for i in range(3000):
            a = XY(i, 1)
            b = YX(2, i)
            c = XYZ(i, i, i)
            self.assertEqual((a.x, a.y, b.x, b.y), (i, 1, 2, i))
            self.assertEqual((c.x, c.y, c.z), (i, i, i))
            del a, b, c
            fresh = YX.__new__(YX)
            self.assertFalse(hasattr(fresh, "x"))
            self.assertFalse(hasattr(fresh, "y"))

    def test_release_timing(self):
        Tracked.log.clear()
        p = XY(None, None)
        for i in range(500):
            write_xy(p, Tracked(i), None)
            if i:
                self.assertEqual(Tracked.log[-1], i - 1)
        p.x = None
        self.assertEqual(Tracked.log[-1], 499)
        Tracked.log.clear()
        for i in range(500):
            q = XY(Tracked(i), 0)
            del q
            self.assertEqual(Tracked.log, [i])
            Tracked.log.clear()

    def test_self_reference_is_collected(self):
        class Node:
            __slots__ = ("me", "tag", "__weakref__")

            def __init__(self, tag):
                self.me = self
                self.tag = tag

        import weakref

        refs = []
        for i in range(500):
            refs.append(weakref.ref(Node(i)))
        gc.collect()
        self.assertTrue(all(r() is None for r in refs))


if __name__ == "__main__":
    unittest.main()
