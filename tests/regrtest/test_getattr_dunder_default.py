"""getattr with a default answers a missing dunder without a lookup.

`copy.deepcopy` probes `getattr(x, "__deepcopy__", None)` on every plain
instance. When no class in the MRO, no builtin base slot and no
instance-level special supplies the name, the default comes back
directly; anything that does supply it must still be found.
"""

import unittest


class Plain:
    def __init__(self):
        self.x = 1


class WithDeepcopy:
    def __deepcopy__(self, memo):
        return "copied"


class WithGetattr:
    def __getattr__(self, name):
        return "dynamic:" + name


class FromList(list):
    pass


class DunderDefaultTest(unittest.TestCase):
    def test_missing_dunder_returns_default(self):
        p = Plain()
        for _ in range(500):
            self.assertIsNone(getattr(p, "__deepcopy__", None))
            self.assertEqual(getattr(p, "__nope__", 7), 7)

    def test_supplied_dunders_are_found(self):
        p = Plain()
        for _ in range(500):
            self.assertIs(getattr(p, "__class__", None), Plain)
            self.assertEqual(getattr(p, "__dict__", None), {"x": 1})
            self.assertIsNotNone(getattr(p, "__repr__", None))
            self.assertIsNotNone(getattr(p, "__reduce_ex__", None))
            self.assertEqual(getattr(p, "__module__", None), __name__)
            self.assertIsNotNone(getattr(WithDeepcopy(), "__deepcopy__", None))
            self.assertEqual(getattr(WithGetattr(), "__deepcopy__", None), "dynamic:__deepcopy__")
            self.assertIsNotNone(getattr(FromList(), "__iadd__", None))

    def test_instance_and_class_assignment(self):
        p = Plain()
        for _ in range(300):
            getattr(p, "__deepcopy__", None)
        p.__deepcopy__ = "mine"
        self.assertEqual(getattr(p, "__deepcopy__", None), "mine")
        q = Plain()
        Plain.__deepcopy__ = lambda self, memo: "cls"
        try:
            self.assertIsNotNone(getattr(q, "__deepcopy__", None))
        finally:
            del Plain.__deepcopy__
        self.assertIsNone(getattr(q, "__deepcopy__", None))

    def test_missing_without_default_raises(self):
        with self.assertRaises(AttributeError):
            getattr(Plain(), "__deepcopy__")


if __name__ == "__main__":
    unittest.main()
