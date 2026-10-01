"""Branches on locals and class attributes read through instances.

The core loop decides `if x is None`, `if x is not None` and `if x:` on
a local without loading it, and serves a class's scalar attribute read
through an instance from a site cache. These cases check each shape's
edges: every truth kind, unbound locals, and an instance attribute or
class change that shadows the cached value.
"""

import unittest


class Holder:
    LIMIT = 3
    RATE = 0.5
    FLAG = True
    NOTHING = None


def branches(values):
    out = []
    for v in values:
        if v is None:
            out.append("none")
        if v is not None:
            out.append("some")
        if v:
            out.append("truthy")
        else:
            out.append("falsy")
    return out


class BranchFusionTests(unittest.TestCase):
    def test_truth_kinds(self):
        values = [None, 0, 1, -1, 0.0, 0.5, "", "x", (), (1,), [], [1],
                  {}, {1: 2}, True, False, object()]
        for _ in range(3):
            self.assertEqual(branches(values), [
                x for v in values for x in (
                    (["none"] if v is None else ["some"])
                    + (["truthy"] if v else ["falsy"]))
            ])

    def test_custom_truth(self):
        class Falsy:
            def __bool__(self):
                return False

        class Empty:
            def __len__(self):
                return 0

        for _ in range(3):
            self.assertEqual(branches([Falsy(), Empty()]),
                             ["some", "falsy", "some", "falsy"])

    def test_unbound_local(self):
        def f(flag):
            if flag:
                x = None
            if x is None:
                return "none"
            return "other"

        for _ in range(3):
            self.assertEqual(f(True), "none")
            with self.assertRaises(UnboundLocalError):
                f(False)

    def test_class_scalars_through_instances(self):
        h = Holder()
        for _ in range(50):
            self.assertEqual((h.LIMIT, h.RATE, h.FLAG, h.NOTHING),
                             (3, 0.5, True, None))
        h.LIMIT = 7
        self.assertEqual(h.LIMIT, 7)
        del h.LIMIT
        self.assertEqual(h.LIMIT, 3)
        Holder.LIMIT = 9
        try:
            self.assertEqual([h.LIMIT for _ in range(5)], [9] * 5)
        finally:
            Holder.LIMIT = 3

    def test_shadowing_through_dict_and_subclass(self):
        class Sub(Holder):
            pass

        s = Sub()
        for _ in range(20):
            self.assertEqual(s.LIMIT, 3)
        Sub.LIMIT = 4
        self.assertEqual(s.LIMIT, 4)
        s.__dict__["LIMIT"] = 5
        self.assertEqual(s.LIMIT, 5)


if __name__ == "__main__":
    unittest.main()
