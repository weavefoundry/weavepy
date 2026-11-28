"""Set algebra between a large operand and a small one, and between kinds.

Every operator, comparison and method of `set` and `frozenset`, and the
set operators of a dict's keys view, must give what a plain model gives
(lists searched with `==`), keep the elements CPython keeps where equal
keys differ (`1` and `1.0`), give the result its type, change the
receiver itself for the in-place forms, and take time that depends on the
operand it has to walk, not on the larger one: `small <= big`,
`big -= small`, `big.intersection(small)` and `small & d.keys()` cost the
same against a set of a thousand keys as against one of a hundred
thousand.
"""

import time
import unittest


def model(keys):
    out = []
    for k in keys:
        if not any(k == x for x in out):
            out.append(k)
    return out


def same(test, got, want, kind=None):
    want = model(want)
    test.assertEqual(len(got), len(want), (got, want))
    for k in want:
        test.assertIn(k, got)
    if kind is not None:
        test.assertIs(type(got), kind)


POOLS = (
    [1, 2, 3, 4, 5, 6, 7, 8],
    [2.0, 4.0, 9.5, "a"],
    [True, 0, "a", "b", (1, 2)],
    [],
    list(range(40)),
    ["k%d" % i for i in range(30)] + [3, 4.0],
    [(i, str(i)) for i in range(12)] + [None],
)


class Odd:
    """A key that compares through Python (equal to others of its value)."""

    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return hash(self.v) & 7

    def __eq__(self, other):
        return isinstance(other, Odd) and self.v == other.v

    def __repr__(self):
        return "Odd(%r)" % (self.v,)


ODD_POOLS = ([Odd(i) for i in range(12)], [Odd(i) for i in range(6, 20)] + [1, "a"], [Odd(3)])


class AlgebraTests(unittest.TestCase):
    def check_pair(self, xs, ys):
        for make_a in (set, frozenset):
            for make_b in (set, frozenset):
                a, b = make_a(xs), make_b(ys)
                in_b = lambda k: any(k == y for y in ys)
                in_a = lambda k: any(k == x for x in xs)
                same(self, a & b, [k for k in xs if in_b(k)], make_a)
                same(self, a | b, list(xs) + list(ys), make_a)
                same(self, a - b, [k for k in xs if not in_b(k)], make_a)
                same(self, a ^ b, [k for k in xs if not in_b(k)] + [k for k in ys if not in_a(k)], make_a)
                same(self, a.intersection(b), [k for k in xs if in_b(k)], make_a)
                same(self, a.union(b), list(xs) + list(ys), make_a)
                same(self, a.difference(b), [k for k in xs if not in_b(k)], make_a)
                same(self, a.symmetric_difference(b), (a - b) | (b - a), make_a)
                same(self, a.intersection(list(ys)), [k for k in xs if in_b(k)], make_a)
                same(self, a.difference(tuple(ys)), [k for k in xs if not in_b(k)], make_a)
                sub = all(in_b(k) for k in model(xs))
                sup = all(in_a(k) for k in model(ys))
                self.assertEqual(a <= b, sub)
                self.assertEqual(a >= b, sup)
                self.assertEqual(a < b, sub and len(a) < len(b))
                self.assertEqual(a > b, sup and len(a) > len(b))
                self.assertEqual(a == b, sub and sup)
                self.assertEqual(a != b, not (sub and sup))
                self.assertEqual(a.issubset(b), sub)
                self.assertEqual(a.issuperset(b), sup)
                self.assertEqual(a.issubset(list(ys)), sub)
                self.assertEqual(a.issuperset(list(ys)), sup)
                self.assertEqual(a.isdisjoint(b), not any(in_b(k) for k in xs))
                self.assertEqual(len(a), len(model(xs)))
                self.assertEqual(len(b), len(model(ys)))

    def test_every_pair_of_pools(self):
        for xs in POOLS:
            for ys in POOLS:
                self.check_pair(xs, ys)

    def test_keys_compared_through_python(self):
        for xs in ODD_POOLS:
            for ys in ODD_POOLS:
                self.check_pair(xs, ys)

    def test_in_place_forms_change_the_receiver(self):
        for xs in POOLS + ODD_POOLS:
            for ys in POOLS + ODD_POOLS:
                for make_b in (set, frozenset, list, tuple):
                    ops = (
                        ("difference_update", lambda a, b: a - set(b)),
                        ("intersection_update", lambda a, b: a & set(b)),
                        ("symmetric_difference_update", lambda a, b: a ^ set(b)),
                        ("update", lambda a, b: a | set(b)),
                    )
                    for name, ref in ops:
                        a = set(xs)
                        alias = a
                        want = ref(set(xs), make_b(ys))
                        self.assertIsNone(getattr(a, name)(make_b(ys)))
                        self.assertIs(a, alias)
                        self.assertEqual(a, want, (name, xs, ys))
                for make_b in (set, frozenset):
                    a = set(xs)
                    alias = a
                    a -= make_b(ys)
                    self.assertIs(a, alias)
                    self.assertEqual(a, set(xs) - set(ys))
                    a = set(xs)
                    alias = a
                    a &= make_b(ys)
                    self.assertIs(a, alias)
                    self.assertEqual(a, set(xs) & set(ys))
                    a = set(xs)
                    alias = a
                    a ^= make_b(ys)
                    self.assertIs(a, alias)
                    self.assertEqual(a, set(xs) ^ set(ys))
                    a = set(xs)
                    alias = a
                    a |= make_b(ys)
                    self.assertIs(a, alias)
                    self.assertEqual(a, set(xs) | set(ys))

    def test_a_set_with_itself(self):
        for xs in POOLS + ODD_POOLS:
            a = set(xs)
            self.assertEqual(a & a, a)
            self.assertEqual(a | a, a)
            self.assertEqual(a - a, set())
            self.assertEqual(a ^ a, set())
            self.assertTrue(a <= a and a >= a and a == a and not a < a)
            b = set(xs)
            b |= b
            b &= b
            self.assertEqual(b, a)
            b.intersection_update(b)
            b.update(b)
            self.assertEqual(b, a)
            b ^= b
            self.assertEqual(b, set())
            b = set(xs)
            b -= b
            self.assertEqual(b, set())
            b = set(xs)
            b.difference_update(b)
            self.assertEqual(b, set())
            b = set(xs)
            b.symmetric_difference_update(b)
            self.assertEqual(b, set())

    def test_which_equal_element_is_kept(self):
        kinds = lambda s: sorted(type(k).__name__ for k in s)
        self.assertEqual(kinds({1, 2} & {1.0, 2.0, 3}), ["int", "int"])
        self.assertEqual(kinds({1.0, 2.0, 3} & {1, 2}), ["int", "int"])
        self.assertEqual(kinds({1, 2} & {1.0, 2.0}), ["float", "float"])
        self.assertEqual(kinds({1, 2}.intersection({1.0, 2.0})), ["float", "float"])
        self.assertEqual(kinds({1.0, 2.0}.intersection({1, 2})), ["int", "int"])
        self.assertEqual(kinds({1, 2}.intersection([1.0, 2.0, 2.0])), ["float", "float"])
        self.assertEqual(kinds({1, 2} | {1.0, 5}), ["int", "int", "int"])
        self.assertEqual(kinds({1, 2, 3} - {2.0}), ["int", "int"])
        self.assertEqual(kinds({1, 2} ^ {2.0, 3.5}), ["float", "int"])
        s = {1, 2}
        s |= {1.0, 2.0}
        self.assertEqual(kinds(s), ["int", "int"])
        s &= {1.0, 2.0}
        self.assertEqual(kinds(s), ["float", "float"])
        s = {1, 2, 3}
        s.intersection_update({1.0, 2.0})
        self.assertEqual(kinds(s), ["float", "float"])
        s = {1, 2}
        s.intersection_update({1.0, 2.0, 5})
        self.assertEqual(kinds(s), ["int", "int"])
        s = {1, 2}
        s ^= {2.0, 7}
        self.assertEqual(kinds(s), ["int", "int"])
        s -= {1.0}
        self.assertEqual(kinds(s), ["int"])

    def test_changes_from_inside_a_comparison(self):
        class Meddler:
            def __init__(self, target):
                self.target = target

            def __hash__(self):
                return hash(1)

            def __eq__(self, other):
                self.target.clear()
                return False

        for op in (
            lambda a, b: a & b,
            lambda a, b: a | b,
            lambda a, b: a - b,
            lambda a, b: a ^ b,
            lambda a, b: a <= b,
            lambda a, b: a == b,
            lambda a, b: a.isdisjoint(b),
            lambda a, b: a.issubset(b),
            lambda a, b: a.intersection(b),
        ):
            a = {1, 2, 3}
            b = {Meddler(a), 5}
            try:
                op(a, b)
            except RuntimeError:
                pass
            a = {1, 2, 3}
            b = {Meddler(a), 5}
            try:
                op(b, a)
            except RuntimeError:
                pass

    def test_errors(self):
        with self.assertRaises(TypeError):
            {1} & [1]
        with self.assertRaises(TypeError):
            {1} <= [1]
        with self.assertRaises(TypeError):
            {1}.issubset(5)
        with self.assertRaises(TypeError):
            {1}.intersection([[]])
        with self.assertRaises(AttributeError):
            frozenset([1]).difference_update({1})
        f = frozenset([1, 2])
        g = f
        g -= {1}
        self.assertEqual(f, frozenset([1, 2]))
        self.assertEqual(g, frozenset([2]))


class KeysViewTests(unittest.TestCase):
    def test_against_sets_views_and_iterables(self):
        for xs in POOLS:
            for ys in POOLS:
                d = dict.fromkeys(xs)
                e = dict.fromkeys(ys)
                in_y = lambda k: any(k == y for y in ys)
                in_x = lambda k: any(k == x for x in xs)
                for other in (set(ys), frozenset(ys), e.keys(), list(ys)):
                    same(self, d.keys() & other, [k for k in xs if in_y(k)], set)
                    same(self, d.keys() | other, list(xs) + list(ys), set)
                    same(self, d.keys() - other, [k for k in xs if not in_y(k)], set)
                    same(self, d.keys() ^ other, [k for k in xs if not in_y(k)] + [k for k in ys if not in_x(k)], set)
                for other in (set(ys), frozenset(ys)):
                    same(self, other & d.keys(), [k for k in xs if in_y(k)], set)
                    same(self, other | d.keys(), list(xs) + list(ys), set)
                    same(self, other - d.keys(), [k for k in ys if not in_x(k)], set)
                    same(self, other ^ d.keys(), [k for k in xs if not in_y(k)] + [k for k in ys if not in_x(k)], set)
                self.assertEqual(len(d), len(model(xs)))

    def test_which_equal_key_is_kept(self):
        kinds = lambda s: sorted(type(k).__name__ for k in s)
        d = {1: 0, 2: 0, 3.0: 0}
        self.assertEqual(kinds(d.keys() & {1.0, 2.0, 3, 9}), ["float", "int", "int"])
        self.assertEqual(kinds(d.keys() & {1.0, 2.0}), ["float", "float"])
        self.assertEqual(kinds(d.keys() & frozenset([1.0, 2.0, 3, 9, 10])), ["float", "float", "int"])
        self.assertEqual(kinds({1.0, 2.0, 3, 9} & d.keys()), ["float", "int", "int"])
        self.assertEqual(kinds({1.0} & d.keys()), ["float"])
        self.assertEqual(kinds(d.keys() & {1.0: 0, 2.0: 0}.keys()), ["float", "float"])
        self.assertEqual(kinds({1.0: 0, 2.0: 0, 3: 0}.keys() & d.keys()), ["float", "int", "int"])
        self.assertEqual(kinds(d.keys() | {1.0, 4}), ["float", "int", "int", "int"])
        self.assertEqual(kinds({1.0, 4} | d.keys()), ["float", "float", "int", "int"])


class CostTests(unittest.TestCase):
    """What an operation costs follows the operand it must walk."""

    def per_call(self, fn, reps=400):
        best = None
        for _ in range(5):
            t0 = time.perf_counter()
            for _ in range(reps):
                fn()
            dt = (time.perf_counter() - t0) / reps
            best = dt if best is None else min(best, dt)
        return best

    def flat(self, make):
        # (A hundred times the keys cost a hundred times as much when the
        # large operand was copied or walked. Allow eight, far past noise.)
        for fn in make(1000):
            fn()
        small = [self.per_call(fn) for fn in make(1000)]
        large = [self.per_call(fn) for fn in make(100_000)]
        for i, (s, l) in enumerate(zip(small, large)):
            self.assertLess(l, 8 * s + 4e-6, (i, s, l))

    def test_small_against_large_sets(self):
        def make(n):
            big = set(range(n))
            froze = frozenset(big)
            small = {1, 2, n + 5}
            few = [1, 2, 3]

            def restore():
                big.update(small)
                big.discard(n + 5)

            def in_place():
                big.difference_update(small)
                big.symmetric_difference_update(small)
                big.symmetric_difference_update(small)
                big.update(small)
                big.discard(n + 5)
                x = big
                x -= small
                x |= small
                x.discard(n + 5)

            return (
                lambda: small <= big,
                lambda: big >= small,
                lambda: small < froze,
                lambda: small == big,
                lambda: big != small,
                lambda: big & small,
                lambda: small & big,
                lambda: small - big,
                lambda: big.intersection(small),
                lambda: small.intersection(big),
                lambda: small.issubset(big),
                lambda: big.issuperset(small),
                lambda: big.issuperset(few),
                lambda: big.isdisjoint(small),
                lambda: small.isdisjoint(froze),
                lambda: big.intersection(few),
                lambda: big.difference_update(few) or big.update(few),
                in_place,
                restore,
            )

        self.flat(make)

    def test_small_set_against_a_large_dicts_keys(self):
        def make(n):
            d = dict.fromkeys(range(n))
            small = {1, 2, n + 5}
            return (
                lambda: d.keys() & small,
                lambda: small & d.keys(),
                lambda: d.keys() >= small,
                lambda: small <= d.keys(),
                lambda: d.keys().isdisjoint(small),
            )

        self.flat(make)


if __name__ == "__main__":
    unittest.main()
