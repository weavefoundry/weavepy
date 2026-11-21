"""Frameless getters reading instances whose `__dict__` was materialized.

`cached_property`, `obj.__dict__[...] = v`, `vars(obj).update(...)` and
unpickling leave an instance with a published dictionary. Compiled leaf
reads cache a field's position in it and check the name there on every
read; the value must follow every change to the dictionary and to the
class.
"""

import copy
import functools
import pickle
import unittest


class Rec:
    label = "class-label"

    def __init__(self, x):
        self.x = x
        self.y = x + 1

    @functools.cached_property
    def doubled(self):
        return self.x * 2

    def get_x(self):
        return self.x

    def get_label(self):
        return self.label

    def __hash__(self):
        return hash(self.x)


class MaterializedReadTests(unittest.TestCase):
    ROUNDS = 4000

    def test_reads_follow_dict_writes(self):
        r = Rec(1)
        r.doubled
        for i in range(self.ROUNDS):
            r.__dict__["x"] = i
            self.assertEqual(r.get_x(), i)
            self.assertEqual(hash(r), hash(i))

    def test_delete_and_reinsert(self):
        r = Rec(1)
        vars(r).update(z=0)
        for i in range(self.ROUNDS):
            self.assertEqual(r.get_x(), 1 if i % 2 == 0 else "again")
            if i % 2 == 0:
                del r.__dict__["x"]
                r.__dict__["x"] = "again"
            else:
                del r.__dict__["x"]
                r.__dict__["x"] = 1
        del r.__dict__["x"]
        with self.assertRaises(AttributeError):
            r.get_x()

    def test_instance_shadow_comes_and_goes(self):
        r = Rec(1)
        r.doubled
        for i in range(self.ROUNDS):
            self.assertEqual(r.get_label(), "class-label")
            r.__dict__["label"] = i
            self.assertEqual(r.get_label(), i)
            del r.__dict__["label"]
        Rec.label = "changed"
        try:
            self.assertEqual(r.get_label(), "changed")
        finally:
            Rec.label = "class-label"

    def test_mixed_layouts_share_sites(self):
        plain = [Rec(i) for i in range(20)]
        mat = [Rec(i) for i in range(20)]
        for m in mat:
            m.doubled
        for _ in range(200):
            self.assertEqual(sum(r.get_x() for r in plain), 190)
            self.assertEqual(sum(r.get_x() for r in mat), 190)
            self.assertEqual(
                [r.get_x() for pair in zip(plain, mat) for r in pair][:4], [0, 0, 1, 1]
            )

    def test_copies_and_unpickled(self):
        base = Rec(5)
        items = [copy.copy(base), copy.deepcopy(base), pickle.loads(pickle.dumps(base))]
        for _ in range(self.ROUNDS):
            self.assertEqual([r.get_x() for r in items], [5, 5, 5])
        items[1].x = 9
        self.assertEqual([r.get_x() for r in items], [5, 9, 5])

    def test_cached_property_value(self):
        for i in range(self.ROUNDS):
            r = Rec(i)
            self.assertEqual(r.doubled, i * 2)
            self.assertEqual(r.doubled, i * 2)
            del r.doubled
            r.x = 0
            self.assertEqual(r.doubled, 0)


if __name__ == "__main__":
    unittest.main()
