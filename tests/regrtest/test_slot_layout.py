"""Instances of `__slots__` classes lay their member slots out per class.

An instance starts with every member slot unset in a layout its class
shares, and a store fills its slot in place. Unset slots must stay
invisible everywhere: reads raise `AttributeError`, and `copy`,
`pickle`, `__getstate__` and `vars`-free introspection see only the set
ones, whatever order the slots were set or deleted in.
"""

import copy
import gc
import pickle
import unittest


class Point:
    __slots__ = ("x", "y", "z")

    def __init__(self, x, y, z):
        self.x = x
        self.y = y
        self.z = z


class Sparse:
    __slots__ = ("a", "b", "c")


class Point4(Point):
    __slots__ = ("w",)


class Mixed:
    __slots__ = ("s", "__dict__")


class Other:
    __slots__ = ("x", "y", "z")


class SlotLayoutTest(unittest.TestCase):
    def test_unset_slots_raise(self):
        s = Sparse()
        for name in "abc":
            with self.assertRaises(AttributeError):
                getattr(s, name)
            self.assertFalse(hasattr(s, name))
        s.b = 2
        self.assertEqual(s.b, 2)
        with self.assertRaises(AttributeError):
            s.a
        with self.assertRaises(AttributeError):
            s.c

    def test_out_of_order_stores(self):
        for _ in range(200):
            s = Sparse()
            s.c = 3
            s.a = 1
            self.assertEqual((s.a, s.c), (1, 3))
            self.assertFalse(hasattr(s, "b"))
            s.b = 2
            self.assertEqual((s.a, s.b, s.c), (1, 2, 3))

    def test_delete_and_restore(self):
        p = Point(1, 2, 3)
        del p.y
        with self.assertRaises(AttributeError):
            p.y
        with self.assertRaises(AttributeError):
            del p.y
        self.assertEqual((p.x, p.z), (1, 3))
        p.y = 20
        self.assertEqual((p.x, p.y, p.z), (1, 20, 3))

    def test_construction_in_a_loop(self):
        total = 0.0
        for i in range(5000):
            p = Point(i, 2.0, 3.0)
            total += p.x + p.y + p.z
        self.assertEqual(total, sum(range(5000)) + 5000 * 5.0)

    def test_subclass_slots(self):
        q = Point4(1, 2, 3)
        self.assertFalse(hasattr(q, "w"))
        q.w = 4
        self.assertEqual((q.x, q.y, q.z, q.w), (1, 2, 3, 4))
        del q.x
        self.assertFalse(hasattr(q, "x"))

    def test_slots_with_dict(self):
        m = Mixed()
        self.assertFalse(hasattr(m, "s"))
        m.s = 1
        m.other = 2
        self.assertEqual((m.s, m.other), (1, 2))
        self.assertEqual(vars(m), {"other": 2})

    def test_copy_and_pickle_skip_unset(self):
        s = Sparse()
        s.a = 1
        s.c = 3
        for clone in (copy.copy(s), copy.deepcopy(s), pickle.loads(pickle.dumps(s))):
            self.assertEqual((clone.a, clone.c), (1, 3))
            self.assertFalse(hasattr(clone, "b"))
        state = s.__getstate__()
        self.assertEqual(state, (None, {"a": 1, "c": 3}))

    def test_class_assignment(self):
        p = Point(1, 2, 3)
        p.__class__ = Other
        self.assertIsInstance(p, Other)
        self.assertEqual((p.x, p.y, p.z), (1, 2, 3))
        del p.z
        self.assertFalse(hasattr(p, "z"))

    def test_cycle_through_slots_collects(self):
        class Node:
            __slots__ = ("other", "__weakref__")

        import weakref

        a, b = Node(), Node()
        a.other = b
        b.other = a
        r = weakref.ref(a)
        del a, b
        gc.collect()
        self.assertIsNone(r())


if __name__ == "__main__":
    unittest.main()
