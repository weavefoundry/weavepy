"""`set.discard` and `set.remove` by the kinds of key programs use.

A `str`, an `int` or a `float` that only meets stored keys of its own kind
is found and removed with one probe; anything else (a `bool` beside an
`int`, an `int` beside an equal `float`, a key with its own `__eq__`, a
set being changed from inside a comparison) takes the general path. Both
must leave the same set and raise the same errors, which a model kept in
a dict checks through a long mixed run.
"""

import unittest

ROUNDS = 40_000


class Rng:
    def __init__(self, seed):
        self.state = seed

    def below(self, n):
        self.state = (self.state * 6364136223846793005 + 1442695040888963407) % (1 << 64)
        return (self.state >> 33) % n


class Key:
    """Equal to the int it wraps, through Python."""

    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return hash(self.v)

    def __eq__(self, other):
        return (other.v if isinstance(other, Key) else other) == self.v


def keys_for(i):
    return (i & 127, str(i & 63), (i & 31) + 0.5, float(i & 15), bool(i & 1), (i & 7, "t"))


class SetRemovalTests(unittest.TestCase):
    def test_mixed_keys_against_a_model(self):
        rng = Rng(11)
        s = set()
        model = {}
        misses = 0
        for i in range(ROUNDS):
            pool = keys_for(rng.below(1 << 20))
            k = pool[rng.below(len(pool))]
            roll = rng.below(4)
            if roll < 2:
                s.add(k)
                model.setdefault(k, None)
            elif roll == 2:
                s.discard(k)
                model.pop(k, None)
            else:
                try:
                    s.remove(k)
                    removed = True
                except KeyError as e:
                    removed = False
                    self.assertIs(e.args[0], k)
                    misses += 1
                self.assertEqual(removed, k in model)
                model.pop(k, None)
            if i % 997 == 0:
                self.assertEqual(len(s), len(model))
                self.assertEqual(s, set(model))
        self.assertEqual(s, set(model))
        self.assertGreater(misses, 100)

    def test_equal_keys_of_other_kinds(self):
        for stored, probe in ((1, True), (True, 1), (1, 1.0), (2.0, 2), (0, False), (0.0, 0), (3, Key(3))):
            s = {stored, "other"}
            s.discard(probe)
            self.assertEqual(s, {"other"}, (stored, probe))
            s = {stored, "other"}
            s.remove(probe)
            self.assertEqual(s, {"other"}, (stored, probe))
        s = {Key(5), Key(6), 7}
        for i in range(ROUNDS // 100):
            s.discard(5)
            self.assertEqual(len(s), 2)
            s.add(Key(5))
        s.discard(7)
        self.assertEqual(len(s), 2)

    def test_hot_loop_of_ints_and_strs(self):
        s = set(range(64))
        names = {"n%d" % i for i in range(64)}
        for i in range(ROUNDS):
            s.discard(i & 127)
            s.add((i * 7) & 127)
            names.discard("n%d" % (i & 127))
            names.add("n%d" % ((i * 5) & 63))
        want = set(range(64))
        want_names = {"n%d" % i for i in range(64)}
        for i in range(ROUNDS):
            want -= {i & 127}
            want |= {(i * 7) & 127}
            want_names -= {"n%d" % (i & 127)}
            want_names |= {"n%d" % ((i * 5) & 63)}
        self.assertEqual(s, want)
        self.assertEqual(names, want_names)

    def test_errors(self):
        s = {1, 2}
        with self.assertRaises(TypeError):
            s.discard([1])
        with self.assertRaises(TypeError):
            s.remove({})
        with self.assertRaises(KeyError):
            s.remove(3)
        with self.assertRaises(TypeError):
            s.discard()
        self.assertEqual(s, {1, 2})
        f = frozenset(s)
        self.assertFalse(hasattr(f, "discard"))

    def test_discard_during_iteration_is_noticed(self):
        s = set(range(100))
        with self.assertRaises(RuntimeError):
            for x in s:
                s.discard(x)
        s = set(range(100))
        for x in s:
            s.discard(1000 + x)
        self.assertEqual(len(s), 100)

    def test_change_from_inside_a_comparison(self):
        class Meddler:
            def __init__(self, target):
                self.target = target

            def __hash__(self):
                return hash(1)

            def __eq__(self, other):
                self.target.discard(2)
                self.target.add(99)
                return False

        s = {1, 2, 3}
        s.discard(Meddler(s))
        self.assertEqual(s, {1, 3, 99})


if __name__ == "__main__":
    unittest.main()
