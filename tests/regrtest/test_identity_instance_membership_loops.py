"""`obj in container` for instances hashed by identity, and loops over lists
of objects, past the compile thresholds.

Membership settles by identity for an instance whose class keeps
`object`'s `__hash__`; a stored key of another kind with the instance's
very hash (an `int` equal to `hash(obj)`) still compares through the full
lookup, a class that later gains `__hash__` stops being probed by
identity, and an `int` subclass instance compares by value. Loops over
lists of instances, strings and tuples keep their results when the list
changes mid-loop.
"""

import unittest


class Plain:
    pass


class EqOnly:
    __hash__ = object.__hash__

    def __eq__(self, other):
        return isinstance(other, EqOnly) and other.tag == self.tag

    def __init__(self, tag):
        self.tag = tag


class Late:
    pass


class IntSub(int):
    pass


class MembershipTests(unittest.TestCase):
    def test_set_dict_frozenset(self):
        keys = [Plain() for _ in range(64)]
        others = [Plain() for _ in range(64)]
        s, d, fs = set(keys), dict.fromkeys(keys, 1), frozenset(keys)
        for _ in range(300):
            hits = misses = 0
            for k in keys:
                hits += (k in s) + (k in d) + (k in fs)
            for k in others:
                misses += (k in s) + (k in d) + (k in fs)
            self.assertEqual((hits, misses), (192, 0))

    def test_stored_int_with_the_same_hash(self):
        p = Plain()
        s = {hash(p), "x"}
        d = {hash(p): 1}
        for _ in range(300):
            self.assertNotIn(p, s)
            self.assertNotIn(p, d)
        s.add(p)
        for _ in range(300):
            self.assertIn(p, s)
            self.assertIn(hash(p), s)

    def test_python_eq_with_object_hash(self):
        a, b = EqOnly(1), EqOnly(1)
        s = {a}
        for _ in range(300):
            self.assertIn(a, s)
            # (Different identities hash apart, so `__eq__` never runs.)
            self.assertNotIn(b, s)

    def test_class_gains_hash(self):
        x, y = Late(), Late()
        s = {x}
        for _ in range(300):
            self.assertIn(x, s)
            self.assertNotIn(y, s)
        Late.__hash__ = lambda self: 7
        Late.__eq__ = lambda self, other: isinstance(other, Late)
        try:
            t = {x}
            for _ in range(300):
                self.assertIn(y, t)
        finally:
            del Late.__hash__, Late.__eq__

    def test_int_subclass_by_value(self):
        s = {5, 6}
        for _ in range(300):
            self.assertIn(IntSub(5), s)
            self.assertNotIn(IntSub(7), s)


class ObjectLoopTests(unittest.TestCase):
    def test_loops_over_objects(self):
        pts = [Plain() for _ in range(512)]
        for i, p in enumerate(pts):
            p.x = i
        total = 0
        for _ in range(200):
            for p in pts:
                total += p.x
        self.assertEqual(total, 200 * sum(range(512)))
        words = [str(i) * 2 for i in range(512)]
        n = 0
        for _ in range(200):
            for w in words:
                if w.startswith("1"):
                    n += len(w)
        self.assertEqual(n, 200 * sum(len(w) for w in words if w.startswith("1")))

    def test_list_changes_mid_loop(self):
        for _ in range(100):
            items = [Plain() for _ in range(50)]
            seen = []
            for p in items:
                seen.append(p)
                if len(seen) == 10:
                    del items[20:]
                    items.append(Plain())
            self.assertEqual(len(seen), 21)
            self.assertIs(seen[-1], items[-1])

    def test_previous_element_kept(self):
        pts = [Plain() for _ in range(300)]
        for _ in range(100):
            prev = None
            pairs = 0
            for p in pts:
                if prev is not None and prev is not p:
                    pairs += 1
                prev = p
            self.assertEqual(pairs, 299)
            self.assertIs(prev, pts[-1])


if __name__ == "__main__":
    unittest.main()
