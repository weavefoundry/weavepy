"""`list(g)` of a `groupby` group over a plain list or tuple takes the
group's items in one loop; groups, the parent's state, and the general
cases match `itertools.groupby`."""

import itertools
import unittest


class GroupDrain(unittest.TestCase):
    def test_groups(self):
        for xs in ([], [1], [1, 1, 2, 2, 2, 3], (5, 5, 5), list("aabbbc"),
                   [1, 1.0, True, 2], [None, None, 0]):
            for _ in range(50):
                got = [(k, list(g)) for k, g in itertools.groupby(xs)]
                want = []
                for x in xs:
                    if want and want[-1][0] == x:
                        want[-1][1].append(x)
                    else:
                        want.append((x, [x]))
                self.assertEqual(got, want)

    def test_partial_then_drain(self):
        it = itertools.groupby([1, 1, 1, 2, 2, 3])
        k, g = next(it)
        self.assertEqual(next(g), 1)
        self.assertEqual(list(g), [1, 1])
        self.assertEqual(list(g), [])
        k2, g2 = next(it)
        self.assertEqual((k2, list(g2)), (2, [2, 2]))
        self.assertEqual([(k, list(g)) for k, g in it], [(3, [3])])

    def test_skipped_group_and_stale_grouper(self):
        it = itertools.groupby("aaabbc")
        _, ga = next(it)
        _, gb = next(it)
        self.assertEqual(list(ga), [])
        self.assertEqual(list(gb), ["b", "b"])
        self.assertEqual(next(it)[0], "c")

    def test_keyfunc_and_objects(self):
        class K:
            def __init__(self, v):
                self.v = v

            def __eq__(self, o):
                return self.v == o.v
        ks = [K(1), K(1), K(2)]
        self.assertEqual([len(list(g)) for _, g in itertools.groupby(ks)], [2, 1])
        self.assertEqual([(k, list(g)) for k, g in itertools.groupby(range(6), key=lambda x: x // 3)],
                         [(0, [0, 1, 2]), (1, [3, 4, 5])])


if __name__ == "__main__":
    unittest.main()
