"""Iterating a class with only `__getitem__` steps the sequence's
`__getitem__` frameless; the items, the end at IndexError or
StopIteration, the iterator's state, and every case the step declines
match the general protocol."""

import unittest


class Seq:
    def __init__(self, n):
        self.n = n

    def __getitem__(self, i):
        if i >= self.n:
            raise IndexError
        return i * 2


class StopSeq:
    def __getitem__(self, i):
        if i >= 3:
            raise StopIteration
        return i


class Other(Exception):
    pass


class BadSeq:
    def __getitem__(self, i):
        if i >= 2:
            raise Other
        return i


class SubErr(IndexError):
    pass


class SubSeq:
    def __getitem__(self, i):
        if i >= 2:
            raise SubErr
        return i


class Grow:
    def __init__(self):
        self.items = [1, 2]

    def __getitem__(self, i):
        return self.items[i]


class SeqIterStep(unittest.TestCase):
    def test_items_and_end(self):
        for _ in range(300):
            self.assertEqual(list(Seq(4)), [0, 2, 4, 6])
            self.assertEqual([x for x in Seq(0)], [])
            self.assertEqual(sum(x for x in Seq(10)), 90)
            self.assertEqual(list(StopSeq()), [0, 1, 2])
            self.assertEqual(list(SubSeq()), [0, 1])

    def test_other_exception_propagates(self):
        for _ in range(100):
            out = []
            with self.assertRaises(Other):
                for x in BadSeq():
                    out.append(x)
            self.assertEqual(out, [0, 1])

    def test_iterator_state(self):
        s = Seq(5)
        for _ in range(100):
            it = iter(s)
            self.assertEqual(next(it), 0)
            for x in it:
                if x == 4:
                    break
            self.assertEqual(next(it), 6)
            self.assertEqual(list(it), [8])
            self.assertEqual(list(it), [])
            # Exhausted for good, even if the sequence grows.
            s.n = 10
            self.assertEqual(list(it), [])
            s.n = 5

    def test_list_backed_sequence(self):
        g = Grow()
        for _ in range(100):
            self.assertEqual(list(g), [1, 2])

    def test_patched_getitem(self):
        class P:
            def __getitem__(self, i):
                if i >= 2:
                    raise IndexError
                return i
        for _ in range(200):
            self.assertEqual(list(P()), [0, 1])
        P.__getitem__ = lambda self, i: [9, 8][i]
        self.assertEqual(list(P()), [9, 8])

    def test_next_inside_getitem(self):
        class Inner:
            def __init__(self):
                self.it = iter([1, 2])

            def __getitem__(self, i):
                return next(self.it)
        for _ in range(100):
            self.assertEqual(list(Inner()), [1, 2])


if __name__ == "__main__":
    unittest.main()
