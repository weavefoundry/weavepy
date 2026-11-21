"""Building containers from instances and iterators in place.

`list(obj)` of an instance whose `__iter__` and `__len__` evaluate as pure
leaves skips the full protocol, `iter()` runs inside frameless
evaluations, a list or tuple iterator drains at once, and an `__iter__`
returning a non-iterator raises CPython's `TypeError` on every path.
"""

import unittest

N = 3000


class Seq:
    def __init__(self):
        self.d = [1, 2, 3]

    def __len__(self):
        return len(self.d)

    def __iter__(self):
        return iter(self.d)


class BadLen:
    def __len__(self):
        raise RuntimeError("len")

    def __iter__(self):
        return iter([1])


class TypeErrorLen:
    def __len__(self):
        raise TypeError("t")

    def __iter__(self):
        return iter([2])


class Hint:
    def __init__(self):
        self.calls = 0

    def __length_hint__(self):
        self.calls += 1
        return 1

    def __iter__(self):
        return iter([3])


class NotIter:
    def __iter__(self):
        return [1]


class GenIter:
    def __iter__(self):
        yield 5


class DictIter:
    def __init__(self):
        self.m = {"a": 1}

    def __iter__(self):
        return iter(self.m)


class IterProtocolTests(unittest.TestCase):
    def test_collect(self):
        for i in range(N):
            r = (list(Seq()), tuple(Seq()), list(TypeErrorLen()), list(GenIter()),
                 list(DictIter()), set(Seq()))
        self.assertEqual(r, ([1, 2, 3], (1, 2, 3), [2], [5], ["a"], {1, 2, 3}))
        h = Hint()
        self.assertEqual(list(h), [3])
        self.assertEqual(h.calls, 1)

    def test_errors(self):
        for i in range(N // 10):
            with self.assertRaises(RuntimeError):
                list(BadLen())
            with self.assertRaises(TypeError) as cm:
                list(NotIter())
        self.assertEqual(str(cm.exception), "iter() returned non-iterator of type 'list'")

        def loop():
            for i in range(N):
                try:
                    for x in NotIter():
                        pass
                except TypeError as e:
                    msg = str(e)
            return msg

        self.assertEqual(loop(), "iter() returned non-iterator of type 'list'")

    def test_partial_iterators(self):
        for i in range(N):
            xs = [1, 2, 3]
            it = iter(xs)
            next(it)
            first = list(it)
            xs.append(4)
            r = (first, list(it), next(it, "end"))
            t = iter((1, 2))
            r2 = (tuple(t), list(t))
        self.assertEqual(r, ([2, 3], [], "end"))
        self.assertEqual(r2, ((1, 2), []))


class ZipDrainTests(unittest.TestCase):
    def test_end_semantics(self):
        for i in range(N):
            a = [1, 2, 3, 4]
            b = (5, 6, 7)
            c = [8, 9]
            ia, ib = iter(a), iter(b)
            r1 = list(zip(ia, ib, c))
            rest = (list(ia), list(ib))
            it = iter([1, 2, 3, 4, 5])
            pairs = list(zip(it, it))
            left = list(it)
            ja, jb = iter([1, 2, 3]), iter((4, 5, 6))
            full = list(zip(ja, jb))
            after = (next(ja, "end"), next(jb, "end"))
            z = zip([1, 2], [3, 4, 5], [6, 7, 8])
            w = (list(z), list(z))
        self.assertEqual(r1, [(1, 5, 8), (2, 6, 9)])
        self.assertEqual(rest, ([4], []))
        self.assertEqual((pairs, left), ([(1, 2), (3, 4)], []))
        self.assertEqual((full, after), ([(1, 4), (2, 5), (3, 6)], ("end", "end")))
        self.assertEqual(w, ([(1, 3, 6), (2, 4, 7)], []))

    def test_wide_and_mixed(self):
        for i in range(N // 10):
            big = list(zip(range(3), [1, 2, 3, 4], (5, 6, 7, 8, 9), [0] * 5, "abcdef",
                           [1] * 9, (2,) * 9, [3] * 9))
            strict_err = None
            try:
                list(zip([1, 2], (3,), strict=True))
            except ValueError as e:
                strict_err = str(e)
        self.assertEqual(big[2], (2, 3, 7, 0, "c", 1, 2, 3))
        self.assertEqual(strict_err, "zip() argument 2 is shorter than argument 1")


class PythonNextTests(unittest.TestCase):
    def test_python_iterator_classes(self):
        import sys

        class It:
            def __init__(self, n):
                self.i = 0
                self.n = n

            def __iter__(self):
                return self

            def __next__(self):
                if self.i >= self.n:
                    raise StopIteration
                self.i += 1
                return self.i

        class Tracked:
            def __init__(self):
                self.log = []
                self.k = 0

            def __iter__(self):
                return self

            def __next__(self):
                self.k += 1
                if self.k > 3:
                    raise StopIteration("done")
                self.log.append(self.k)
                return self.k * 10

        class Bad:
            def __iter__(self):
                return self

            def __next__(self):
                raise KeyError("x")

        class Framed:
            def __iter__(self):
                return self

            def __next__(self):
                return sys._getframe().f_code.co_name

        for i in range(N):
            a = [x for x in It(5)]
            t = Tracked()
            b = list(t)
            c = (t.k, t.log)
            totals = (sum(It(4)), max(It(6)), tuple(It(3)))
            it = It(3)
            next(it)
            d = list(it)
            try:
                for x in Bad():
                    pass
            except KeyError as e:
                k = e.args
            g = next(Framed())
        self.assertEqual(a, [1, 2, 3, 4, 5])
        self.assertEqual((b, c), ([10, 20, 30], (4, [1, 2, 3])))
        self.assertEqual(totals, (10, 6, (1, 2, 3)))
        self.assertEqual((d, k, g), ([2, 3], ("x",), "__next__"))


if __name__ == "__main__":
    unittest.main()
