"""Per-class verdicts memoized in the type cache follow class changes.

`getattr(obj, "__deepcopy__", None)` decides a missing dunder from the
class once per class version, and `object.__reduce_ex__` decides the
plain `copyreg.__newobj__` reduction the same way. Adding the attribute
to the class or a base, a `__getattr__`, a `__getstate__`, or
`__getnewargs__` after warm-up must change the answer, and an instance
attribute must still win per instance.
"""

import copy
import copyreg
import unittest


class VerdictMemoTest(unittest.TestCase):
    def test_missing_dunder_follows_class_changes(self):
        class Base:
            pass

        class Leaf(Base):
            pass

        obj = Leaf()
        for _ in range(500):
            self.assertIsNone(getattr(obj, "__deepcopy__", None))
            self.assertFalse(hasattr(obj, "__deepcopy__"))
        Base.__deepcopy__ = lambda self, memo: "base"
        for _ in range(500):
            self.assertEqual(obj.__deepcopy__({}), "base")
            self.assertIsNotNone(getattr(obj, "__deepcopy__", None))
        del Base.__deepcopy__
        for _ in range(500):
            self.assertIsNone(getattr(obj, "__deepcopy__", None))
        obj.__dict__["__deepcopy__"] = "own"
        self.assertEqual(getattr(obj, "__deepcopy__", None), "own")
        self.assertIsNone(getattr(Leaf(), "__deepcopy__", None))
        Base.__getattr__ = lambda self, name: "dynamic"
        self.assertEqual(getattr(Leaf(), "__deepcopy__", None), "dynamic")

    def test_plain_reduction_follows_class_changes(self):
        class Base:
            pass

        class Point(Base):
            def __init__(self, x):
                self.x = x

        p = Point(3)
        for _ in range(500):
            rv = p.__reduce_ex__(4)
            self.assertIs(rv[0], copyreg.__newobj__)
            self.assertEqual(rv[1], (Point,))
            self.assertEqual(rv[2], {"x": 3})
            self.assertEqual(copy.deepcopy(p).x, 3)
        Base.__getstate__ = lambda self: {"x": 42}
        for _ in range(500):
            self.assertEqual(p.__reduce_ex__(4)[2], {"x": 42})
            self.assertEqual(copy.deepcopy(p).x, 42)
        del Base.__getstate__
        Base.__getnewargs__ = lambda self: (7,)
        rv = p.__reduce_ex__(4)
        self.assertEqual(rv[1], (Point, 7))
        del Base.__getnewargs__
        self.assertEqual(p.__reduce_ex__(4)[1], (Point,))

    def test_builtin_function_reduction(self):
        self.assertEqual(len.__reduce_ex__(4), "len")
        self.assertEqual(copy.deepcopy(len), len)


if __name__ == "__main__":
    unittest.main()
