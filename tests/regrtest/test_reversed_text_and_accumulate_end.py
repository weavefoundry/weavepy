"""`reversed()` of text and bytes, builtin type calls, and `accumulate`'s end.

`reversed(s)` of a `str` or `bytes` builds its iterator without the
general type call; builtin type constructors are reused across calls; and
a function-less `accumulate` or a `pairwise` over a list or tuple ends in
place; and compiled `list(x)`, `tuple(x)`, `set(x)`, `dict(x)` and
`str(x)` calls of adapters go straight to the container lane. Results,
errors, and the iterators' later behavior are unchanged.
"""

import itertools
import unittest

N = 3000


class ReversedTextTests(unittest.TestCase):
    def test_str_and_bytes(self):
        for _ in range(N):
            r = "".join(reversed("hello world"))
        self.assertEqual(r, "dlrow olleh")
        for _ in range(N):
            r = list(reversed("héllo✓"))
        self.assertEqual(r, ["✓", "o", "l", "l", "é", "h"])
        for _ in range(N):
            r = bytes(reversed(b"abc"))
        self.assertEqual(r, b"cba")
        self.assertEqual(list(reversed("")), [])
        it = reversed("ab")
        self.assertEqual(it.__length_hint__(), 2)
        self.assertEqual(next(it), "b")
        self.assertEqual(list(it), ["a"])
        self.assertEqual(list(it), [])

    def test_type_calls(self):
        for _ in range(N):
            r = (int("ff", 16), str(5), complex(1, 2), bool(3), frozenset([1]))
        self.assertEqual(r, (255, "5", 1 + 2j, True, frozenset([1])))
        with self.assertRaises(TypeError):
            reversed(3)
        with self.assertRaises(TypeError):
            reversed("ab", "c")
        with self.assertRaises(ValueError):
            int("zz", 10)

    def test_accumulate_ends(self):
        xs = [5, 1, 4, 2, 3]
        for _ in range(N):
            total = 0
            for x in itertools.accumulate(xs):
                total += x
            r = list(itertools.accumulate(tuple(xs)))
        self.assertEqual(total, 5 + 6 + 10 + 12 + 15)
        self.assertEqual(r, [5, 6, 10, 12, 15])
        a = itertools.accumulate([1, 2])
        self.assertEqual(list(a), [1, 3])
        self.assertEqual(list(a), [])
        self.assertRaises(StopIteration, next, a)
        b = itertools.accumulate([1.5, 2.5])
        for _ in b:
            pass
        self.assertEqual(next(b, "end"), "end")
        self.assertEqual(list(itertools.accumulate([1, 2], initial=10)), [10, 11, 13])
        self.assertEqual(list(itertools.accumulate([2, 3], lambda a, b: a * b)), [2, 6])
        self.assertEqual(list(itertools.accumulate(["a", "b"])), ["a", "ab"])
        self.assertEqual(list(itertools.accumulate([])), [])


    def test_pairwise_ends(self):
        for _ in range(N):
            r = list(itertools.pairwise([1, 2, 3]))
            for a, b in itertools.pairwise((1, 2)):
                pass
        self.assertEqual(r, [(1, 2), (2, 3)])
        p = itertools.pairwise("abc")
        self.assertEqual(list(p), [("a", "b"), ("b", "c")])
        self.assertEqual(list(p), [])
        self.assertEqual(next(p, "end"), "end")
        objs = [object(), object()]
        self.assertEqual(list(itertools.pairwise(objs)), [(objs[0], objs[1])])
        self.assertEqual(list(itertools.pairwise([1])), [])


    def test_combinatorics_and_keywords(self):
        L = [1, 2, 3]
        for _ in range(N):
            r = (
                list(itertools.product(L, repeat=2))[-1],
                list(itertools.product("ab", "c")),
                list(itertools.permutations(L))[3],
                list(itertools.combinations(L, 2)),
                list(itertools.combinations_with_replacement("ab", 2)),
                list(itertools.accumulate(L, initial=10)),
                list(itertools.zip_longest("ab", "c", fillvalue="-")),
                list(itertools.islice(itertools.count(start=5, step=2), 3)),
            )
        self.assertEqual(r, (
            (3, 3),
            [("a", "c"), ("b", "c")],
            (2, 3, 1),
            [(1, 2), (1, 3), (2, 3)],
            [("a", "a"), ("a", "b"), ("b", "b")],
            [10, 11, 13, 16],
            [("a", "c"), ("b", "-")],
            [5, 7, 9],
        ))
        self.assertEqual(list(itertools.combinations(L, 0)), [()])
        self.assertEqual(list(itertools.product(L, repeat=0)), [()])
        for _ in range(N // 10):
            with self.assertRaises(ValueError):
                itertools.product(L, repeat=-1)
            with self.assertRaises(TypeError):
                itertools.product(L, bogus=2)
            with self.assertRaises(TypeError):
                itertools.accumulate(L, initial=1, extra=2)


class ContainerLaneTests(unittest.TestCase):
    def test_lane_calls(self):
        xs = [3, -1, 2]
        for _ in range(N):
            r = (
                list(map(abs, xs)),
                tuple(map(abs, xs)),
                set(map(abs, xs)),
                frozenset(map(abs, xs)),
                dict(zip("abc", xs)),
                str(map),
                list(x * 2 for x in xs),
            )
        self.assertEqual(r[0], [3, 1, 2])
        self.assertEqual(r[1], (3, 1, 2))
        self.assertEqual(r[2], {1, 2, 3})
        self.assertEqual(r[3], frozenset({1, 2, 3}))
        self.assertEqual(r[4], {"a": 3, "b": -1, "c": 2})
        self.assertEqual(r[5], "<class 'map'>")
        self.assertEqual(r[6], [6, -2, 4])

    def test_lane_errors(self):
        def bad(x):
            raise KeyError(x)

        for _ in range(N // 10):
            with self.assertRaises(KeyError):
                list(map(bad, [1]))
            with self.assertRaises(TypeError):
                set(map(list, [[1]]))
            with self.assertRaises(ValueError):
                dict(map(tuple, ["abc"]))
        try:
            tuple(map(bad, [7]))
        except KeyError as e:
            tb = e.__traceback__
        names = []
        while tb is not None:
            names.append(tb.tb_frame.f_code.co_name)
            tb = tb.tb_next
        self.assertEqual(names, ["test_lane_errors", "bad"])


if __name__ == "__main__":
    unittest.main()
