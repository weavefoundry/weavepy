"""list.extend() and set.update() over a generator or an iterator class
add each item as it comes, as CPython does: a generator that reads the
receiver sees it grow, the items before an error stay added, and the
same from compiled loops; lists, tuples and other sets still extend at
once (a list extended by itself doubles)."""

import unittest


class Count:
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


def boom(xs):
    yield 1
    yield 2
    raise ValueError(len(xs))


def grow(n):
    out = None
    for i in range(n):
        xs = []
        xs.extend(len(xs) for _ in range(3))
        s = set()
        s.update(len(s) for _ in range(3))
        out = (xs, s)
    return out


class LiveExtendTests(unittest.TestCase):
    def test_sees_growth(self):
        for n in (1, 400):
            self.assertEqual(grow(n), ([0, 1, 2], {0, 1, 2}))

    def test_error_keeps_items(self):
        xs = [0]
        with self.assertRaises(ValueError) as cm:
            xs.extend(boom(xs))
        self.assertEqual(xs, [0, 1, 2])
        self.assertEqual(cm.exception.args, (3,))
        s = {0}
        with self.assertRaises(ValueError):
            s.update(boom(s))
        self.assertEqual(s, {0, 1, 2})

    def test_iterator_class(self):
        xs = []
        xs.extend(Count(3))
        self.assertEqual(xs, [1, 2, 3])
        s = set()
        s.update(Count(2))
        self.assertEqual(s, {1, 2})

    def test_mixed_items(self):
        class H:
            def __init__(self, k):
                self.k = k

            def __hash__(self):
                return hash(self.k)

            def __eq__(self, other):
                return self.k == (other.k if isinstance(other, H) else other)

        def run(n):
            out = None
            for i in range(n):
                s = {1}
                s.update(x for x in (1, "a", (2, "b"), H(1), H("z"), "a", 2.5))
                xs = [0]
                xs.extend(y for y in [1, 2])
                xs.extend(self._delegate([3, 4]))
                out = (s, xs)
            return out

        for n in (1, 300):
            s, xs = run(n)
            self.assertEqual(len(s), 5)
            self.assertIn((2, "b"), s)
            self.assertIn("z", s)
            self.assertEqual(xs, [0, 1, 2, 3, 4, 5])

    @staticmethod
    def _delegate(items):
        yield from items
        yield 5

    def test_length_hint(self):
        class BadLen:
            def __iter__(self):
                return iter(range(3))

            def __len__(self):
                raise RuntimeError("hello")

        class TypeLen(BadLen):
            def __len__(self):
                raise TypeError

        with self.assertRaises(RuntimeError):
            [].extend(BadLen())
        xs = []
        xs.extend(TypeLen())
        self.assertEqual(xs, [0, 1, 2])
        s = set()
        s.update(BadLen())
        self.assertEqual(s, {0, 1, 2})

    def test_plain_sources(self):
        xs = [1, 2]
        xs.extend(xs)
        self.assertEqual(xs, [1, 2, 1, 2])
        xs.extend((3,))
        s = {1}
        s.update({2}, [3], (4,))
        self.assertEqual((xs[-1], s), (3, {1, 2, 3, 4}))
        with self.assertRaises(TypeError):
            s.update(x for x in [[1]])


if __name__ == "__main__":
    unittest.main()
