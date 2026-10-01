"""Constructors whose `__init__` runs without a frame.

A class's `__init__` that only stores into `self` runs as a leaf: its
body is evaluated in place on the fresh instance. These cases check the
shapes around that path: trailing default parameters (including ones
rebound through `__defaults__`), empty list and dict literals, and
bodies that stop partway and fall back to the ordinary call.
"""

import gc
import unittest


class WithDefaults:
    def __init__(self, name, value=0, mark=None):
        self.name = name
        self.value = value
        self.mark = mark


class WithContainers:
    def __init__(self, name):
        self.name = name
        self.items = []
        self.index = {}
        self.more = []


class ManyLists:
    def __init__(self):
        self.a = []
        self.b = []
        self.c = []
        self.d = []
        self.e = []
        self.f = []


class Declines:
    def __init__(self, x, extra=1):
        self.items = []
        self.x = x
        self.y = x + extra


def build(cls, n, *args):
    return [cls(*args) for _ in range(n)]


class LeafConstructorTests(unittest.TestCase):
    def test_trailing_defaults(self):
        objs = build(WithDefaults, 50, "v")
        self.assertTrue(all(o.value == 0 and o.mark is None for o in objs))
        objs = build(WithDefaults, 50, "v", 5)
        self.assertTrue(all(o.value == 5 and o.mark is None for o in objs))
        objs = build(WithDefaults, 50, "v", 5, "m")
        self.assertTrue(all((o.value, o.mark) == (5, "m") for o in objs))
        self.assertEqual(vars(objs[0]), {"name": "v", "value": 5, "mark": "m"})

    def test_arity_errors(self):
        for _ in range(50):
            with self.assertRaises(TypeError):
                WithDefaults()
            with self.assertRaises(TypeError):
                WithDefaults(1, 2, 3, 4)

    def test_rebound_defaults(self):
        class C:
            def __init__(self, a, b=1):
                self.a = a
                self.b = b

        self.assertEqual([C(0).b for _ in range(50)], [1] * 50)
        C.__init__.__defaults__ = (2,)
        self.assertEqual([C(0).b for _ in range(50)], [2] * 50)
        C.__init__.__defaults__ = None
        for _ in range(3):
            with self.assertRaises(TypeError):
                C(0)

    def test_fresh_containers(self):
        objs = build(WithContainers, 50, "n")
        for o in objs:
            self.assertEqual((o.items, o.index, o.more), ([], {}, []))
        # Each instance gets its own containers.
        objs[0].items.append(1)
        objs[0].index["k"] = 2
        self.assertEqual(objs[1].items, [])
        self.assertEqual(objs[1].index, {})
        self.assertIsNot(objs[0].items, objs[0].more)
        self.assertEqual(len({id(o.items) for o in objs}), len(objs))
        self.assertTrue(gc.is_tracked(objs[1].items))
        self.assertTrue(gc.is_tracked(objs[1].index))

    def test_more_containers_than_scratch(self):
        for o in build(ManyLists, 50):
            self.assertEqual([o.a, o.b, o.c, o.d, o.e, o.f], [[]] * 6)
            self.assertEqual(len({id(v) for v in vars(o).values()}), 6)

    def test_fallback_after_partial_body(self):
        objs = build(Declines, 50, 1)
        self.assertTrue(all((o.x, o.y, o.items) == (1, 2, []) for o in objs))
        big = 1 << 62
        objs = build(Declines, 50, big, big)
        self.assertTrue(all(o.y == big * 2 for o in objs))
        objs = build(Declines, 50, 1.5)
        self.assertTrue(all(o.y == 2.5 for o in objs))
        with self.assertRaises(TypeError):
            Declines("a")

    def test_cycles_are_collected(self):
        class Node:
            def __init__(self, prev=None):
                self.prev = prev
                self.next = []

        gc.collect()
        for _ in range(20):
            prev = None
            for _ in range(50):
                node = Node(prev)
                if prev is not None:
                    prev.next.append(node)
                prev = node
        del prev, node
        gc.collect()
        self.assertFalse(any(type(o) is Node for o in gc.get_objects()))

    def test_leaf_functions_return_fresh_containers(self):
        def empty_list():
            return []

        def empty_dict():
            return {}

        lists = [empty_list() for _ in range(50)]
        dicts = [empty_dict() for _ in range(50)]
        self.assertEqual(len({id(x) for x in lists}), 50)
        self.assertEqual(len({id(x) for x in dicts}), 50)
        lists[0].append(1)
        self.assertEqual(lists[1], [])


if __name__ == "__main__":
    unittest.main()
