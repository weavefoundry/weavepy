"""`__next__` of the native-backed `itertools` classes.

The classes whose Python `__next__` is `next(self._core)` get a native
`__next__` that steps the adapter in place: bound and unbound calls,
subclasses (with and without their own `__next__`), errors raised while
stepping, and exhaustion behave as before.
"""

import itertools
import operator
import unittest


class NativeNextTests(unittest.TestCase):
    def test_bound_calls(self):
        c = itertools.count(5)
        nx = c.__next__
        self.assertEqual([nx() for _ in range(3)], [5, 6, 7])
        self.assertEqual(next(c), 8)
        self.assertEqual(type(c).__next__(c), 9)

    def test_each_class(self):
        cases = [
            (itertools.cycle("ab"), ["a", "b", "a"]),
            (itertools.repeat(1, 3), [1, 1, 1]),
            (itertools.accumulate([1, 2, 3]), [1, 3, 6]),
            (itertools.chain([1], [2, 3]), [1, 2, 3]),
            (itertools.compress("abc", [1, 0, 1]), ["a", "c"]),
            (itertools.dropwhile(lambda x: x < 2, [1, 2, 3]), [2, 3]),
            (itertools.takewhile(lambda x: x < 3, [1, 2, 3]), [1, 2]),
            (itertools.filterfalse(None, [0, 1, 0, 2]), [0, 0]),
            (itertools.starmap(operator.add, [(1, 2), (3, 4)]), [3, 7]),
            (itertools.islice(range(10), 2, 8, 3), [2, 5]),
            (itertools.pairwise("abc"), [("a", "b"), ("b", "c")]),
            (itertools.zip_longest("ab", "x"), [("a", "x"), ("b", None)]),
            (itertools.product("ab", repeat=1), [("a",), ("b",)]),
            (itertools.permutations("ab"), [("a", "b"), ("b", "a")]),
            (itertools.combinations("abc", 2), [("a", "b"), ("a", "c"), ("b", "c")]),
            (itertools.combinations_with_replacement("ab", 2),
             [("a", "a"), ("a", "b"), ("b", "b")]),
            (itertools.batched("abcde", 2), [("a", "b"), ("c", "d"), ("e",)]),
        ]
        for it, want in cases:
            with self.subTest(type(it).__name__):
                nx = it.__next__
                got = [nx() for _ in range(len(want))]
                self.assertEqual(got, want)
                if type(it) is not itertools.cycle:
                    self.assertRaises(StopIteration, nx)

    def test_tee_and_groupby(self):
        a, b = itertools.tee(iter([1, 2, 3]))
        self.assertEqual(a.__next__(), 1)
        self.assertEqual(list(b), [1, 2, 3])
        self.assertEqual(list(a), [2, 3])
        g = itertools.groupby("aabbb")
        k, grp = g.__next__()
        self.assertEqual((k, grp.__next__(), list(grp)), ("a", "a", ["a"]))
        k, grp = next(g)
        self.assertEqual((k, list(grp)), ("b", ["b", "b", "b"]))
        self.assertRaises(StopIteration, g.__next__)

    def test_subclasses(self):
        class Plain(itertools.count):
            pass

        class Doubled(itertools.count):
            def __next__(self):
                return 2 * super().__next__()

        p = Plain(3)
        self.assertEqual([p.__next__(), next(p)], [3, 4])
        d = Doubled(3)
        self.assertEqual([d.__next__(), next(d), next(d)], [6, 8, 10])
        self.assertEqual(list(itertools.islice(Doubled(), 3)), [0, 2, 4])

    def test_errors_while_stepping(self):
        def bad(x):
            raise KeyError(x)

        it = itertools.takewhile(bad, [7])
        with self.assertRaises(KeyError):
            it.__next__()
        nx = itertools.starmap(operator.truediv, [(1, 0)]).__next__
        self.assertRaises(ZeroDivisionError, nx)
        self.assertRaises(TypeError, itertools.count().__next__, 1)

    def test_source_stop_value(self):
        def gen():
            return 42
            yield

        with self.assertRaises(StopIteration) as cm:
            itertools.islice(gen(), 1).__next__()
        self.assertEqual(cm.exception.value, 42)

    def test_many_steps(self):
        nx = itertools.count().__next__
        for _ in range(20000):
            v = nx()
        self.assertEqual(v, 19999)


if __name__ == "__main__":
    unittest.main()
