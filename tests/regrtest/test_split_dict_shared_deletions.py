"""Deleting an attribute from an instance with a split `__dict__` lays the
remaining values out over a table of their names. Instances that delete
the same attributes in the same order share those tables (see
`SharedKeys::without` in the VM), so each case checks that one instance's
later stores and deletions never show through another's `__dict__`,
attribute reads, or iteration order, in loops long enough to compile.
"""

import unittest

WARM = 2000


class Box:
    def __init__(self, a, b, c, d):
        self.a = a
        self.b = b
        self.c = c
        self.d = d


class SharedDeletionTests(unittest.TestCase):
    def test_same_deletions_warm(self):
        for i in range(WARM):
            o = Box(i, 2, 3, 4)
            del o.b, o.c
            self.assertEqual(o.__dict__, {"a": i, "d": 4})
            self.assertEqual(o.a + o.d, i + 4)
            self.assertFalse(hasattr(o, "b"))
            self.assertFalse(hasattr(o, "c"))

    def test_store_after_shared_deletion(self):
        first = Box(1, 2, 3, 4)
        del first.b
        first.z = 26
        second = Box(5, 6, 7, 8)
        del second.b
        self.assertEqual(list(second.__dict__), ["a", "c", "d"])
        self.assertFalse(hasattr(second, "z"))
        second.w = 23
        self.assertEqual(list(second.__dict__), ["a", "c", "d", "w"])
        self.assertEqual(list(first.__dict__), ["a", "c", "d", "z"])
        self.assertEqual(second.w, 23)
        self.assertFalse(hasattr(first, "w"))
        third = Box(9, 10, 11, 12)
        del third.b
        third.z = 0
        self.assertEqual(third.__dict__, {"a": 9, "c": 11, "d": 12, "z": 0})
        self.assertEqual(first.z, 26)

    def test_different_orders(self):
        for i in range(WARM):
            o = Box(1, 2, 3, 4)
            p = Box(5, 6, 7, 8)
            del o.a, o.c
            del p.c, p.a
            self.assertEqual(o.__dict__, {"b": 2, "d": 4})
            self.assertEqual(p.__dict__, {"b": 6, "d": 8})
            del o.d
            self.assertEqual(o.__dict__, {"b": 2})
            self.assertEqual(p.__dict__, {"b": 6, "d": 8})
            p.a = i
            self.assertEqual(list(p.__dict__), ["b", "d", "a"])
            self.assertFalse(hasattr(o, "a"))

    def test_delete_then_readd(self):
        for i in range(WARM):
            o = Box(1, 2, 3, 4)
            del o.a
            o.a = i
            self.assertEqual(list(o.__dict__), ["b", "c", "d", "a"])
            self.assertEqual(o.a, i)
            del o.c
            self.assertEqual(list(o.__dict__), ["b", "d", "a"])

    def test_many_patterns(self):
        names = "abcd"
        for i in range(WARM):
            o = Box(1, 2, 3, 4)
            k = names[i % 3]
            delattr(o, k)
            expected = {n: v for n, v in zip(names, (1, 2, 3, 4)) if n != k}
            self.assertEqual(o.__dict__, expected)
            vars(o)["e"] = 5
            self.assertEqual(o.e, 5)

    def test_missing_attribute(self):
        o = Box(1, 2, 3, 4)
        del o.b
        with self.assertRaises(AttributeError):
            del o.b
        self.assertEqual(o.__dict__, {"a": 1, "c": 3, "d": 4})


if __name__ == "__main__":
    unittest.main()
