"""The ordered hash table behind `dict`: order, growth, removal, and keys.

Covers the shapes the table handles differently: small tables (no slot
index), the switch to an indexed table and its growth, removal from the
front, middle and end (the later entries renumber), keys whose hashes
collide (a key's `__eq__` runs only against keys of equal hash, as in
CPython), a `__eq__` that mutates the dict mid-probe, and the positional
users of the table (iteration, `popitem`, `OrderedDict.move_to_end`, the
`del`-then-reinsert iteration trip-wire, `copy`, and pickling order).
"""

import pickle
import unittest
from collections import OrderedDict


class Collide:
    """A key with a fixed hash that counts its `__eq__` calls."""

    calls = 0

    def __init__(self, name, h=42):
        self.name = name
        self.h = h

    def __hash__(self):
        return self.h

    def __eq__(self, other):
        Collide.calls += 1
        return isinstance(other, Collide) and self.name == other.name

    def __repr__(self):
        return f"Collide({self.name!r})"


class TableTest(unittest.TestCase):
    def test_growth_keeps_order_and_lookups(self):
        d = {}
        for i in range(2000):
            d[i * 1024] = i
            if i in (7, 8, 9, 100, 1000):
                self.assertEqual(list(d), [k * 1024 for k in range(i + 1)])
        for i in range(2000):
            self.assertEqual(d[i * 1024], i)
        self.assertNotIn(5, d)
        self.assertEqual(len(d), 2000)

    def test_removal_renumbers(self):
        for n in (5, 9, 40, 500):
            d = {f"k{i}": i for i in range(n)}
            keys = list(d)
            # Front, middle and end.
            for k in (keys[0], keys[n // 2], keys[-1]):
                del d[k]
                keys.remove(k)
                self.assertEqual(list(d), keys)
                for kk in keys:
                    self.assertEqual(d[kk], int(kk[1:]))
            # Every other one.
            for k in keys[::2]:
                self.assertEqual(d.pop(k), int(k[1:]))
            keys = keys[1::2]
            self.assertEqual(list(d), keys)
            d["new"] = -1
            self.assertEqual(list(d)[-1], "new")
            self.assertEqual(d.popitem(), ("new", -1))

    def test_reinsert_goes_last(self):
        d = dict.fromkeys(range(20))
        del d[3]
        d[3] = "x"
        self.assertEqual(list(d)[-1], 3)
        self.assertEqual(list(d)[:3], [0, 1, 2])

    def test_collisions_compare_only_equal_hashes(self):
        a, b, c = Collide("a"), Collide("b"), Collide("c", h=7)
        d = {a: 1, c: 3}
        Collide.calls = 0
        d[b] = 2
        # `b` meets `a` (same hash) once; `c` (other hash) never.
        self.assertEqual(Collide.calls, 1)
        Collide.calls = 0
        self.assertEqual(d[Collide("c", h=7)], 3)
        self.assertEqual(Collide.calls, 1)
        self.assertEqual(list(d), [a, c, b])
        many = {Collide(str(i)): i for i in range(50)}
        for i in range(50):
            self.assertEqual(many[Collide(str(i))], i)
        del many[Collide("10")]
        self.assertNotIn(Collide("10"), many)
        self.assertEqual(len(many), 49)

    def test_eq_mutating_dict(self):
        class Evil:
            def __init__(self, d):
                self.d = d

            def __hash__(self):
                return 1

            def __eq__(self, other):
                self.d.clear()
                return False

        d = {}
        d[Evil(d)] = 1
        # The probe meets the stored key (equal hash), whose `__eq__`
        # empties the dict; CPython restarts and finds nothing.
        self.assertNotIn(Evil(d), d)
        self.assertEqual(len(d), 0)

    def test_unhashable_and_mixed_numeric_keys(self):
        d = {1: "int", 2.5: "float", "s": "str", (1, 2): "tuple"}
        self.assertEqual(d[1.0], "int")
        self.assertEqual(d[True], "int")
        self.assertEqual(d[(1, 2)], "tuple")
        with self.assertRaises(TypeError):
            d[[1]] = 1
        nan = float("nan")
        d[nan] = "nan"
        self.assertEqual(d[nan], "nan")

    def test_iteration_tripwires(self):
        d = {i: i for i in range(12)}
        with self.assertRaises(RuntimeError):
            for k in d:
                d[k + 100] = 0
        d = {i: i for i in range(12)}
        with self.assertRaises(RuntimeError):
            for k in d:
                del d[k]
                d[k + 100] = 0

    def test_copy_update_clear(self):
        src = {f"k{i}": i for i in range(30)}
        c = src.copy()
        c["k3"] = "changed"
        self.assertEqual(src["k3"], 3)
        self.assertEqual(list(c), list(src))
        c.clear()
        self.assertEqual(c, {})
        c.update(src)
        self.assertEqual(c, src)
        c["extra"] = 1
        self.assertEqual(list(c)[-1], "extra")

    def test_ordered_dict_moves(self):
        od = OrderedDict((i, str(i)) for i in range(30))
        od.move_to_end(5)
        od.move_to_end(20, last=False)
        keys = list(od)
        self.assertEqual(keys[-1], 5)
        self.assertEqual(keys[0], 20)
        self.assertEqual(od.popitem(last=False), (20, "20"))
        self.assertEqual(od.popitem(), (5, "5"))
        for k in range(30):
            if k not in (5, 20):
                self.assertEqual(od[k], str(k))
        lru = OrderedDict()
        for i in range(500):
            k = (i * 7) % 37
            lru[k] = i
            lru.move_to_end(k)
            if len(lru) > 20:
                lru.popitem(last=False)
        self.assertEqual(len(lru), 20)
        self.assertEqual(list(lru)[-1], (499 * 7) % 37)

    def test_pickle_order(self):
        d = {f"k{i}": i for i in range(40)}
        for k in list(d)[::3]:
            del d[k]
        r = pickle.loads(pickle.dumps(d))
        self.assertEqual(list(r.items()), list(d.items()))

    def test_setdefault_and_get(self):
        d = {}
        for i in range(100):
            self.assertEqual(d.setdefault(i % 10, i), i % 10)
        self.assertEqual(len(d), 10)
        self.assertEqual(d.get(99), None)
        self.assertEqual(d.get(99, "x"), "x")


if __name__ == "__main__":
    unittest.main()
