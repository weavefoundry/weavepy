"""list(enumerate(gen)) drains the generator as list(gen) does, then
numbers its items: start values, a partly consumed generator, an
exhausted one, a generator that raises partway (the enumerate's count
moving past the items taken), StopIteration inside the body (PEP 479),
a generator that reads its own enumerate, and tuple()/sorted() of the
same, each matching CPython."""

import unittest


def gen(n, fail_at=None):
    for i in range(n):
        if i == fail_at:
            raise KeyError(i)
        yield i * 10


def drain(n, start):
    out = None
    for i in range(n):
        out = list(enumerate((x for x in "abc"), start))
    return out


class EnumerateGenTests(unittest.TestCase):
    def test_values(self):
        for n in (1, 300):
            self.assertEqual(drain(n, 0), [(0, "a"), (1, "b"), (2, "c")])
            self.assertEqual(drain(n, -2), [(-2, "a"), (-1, "b"), (0, "c")])
        self.assertEqual(list(enumerate(gen(0))), [])
        self.assertEqual(tuple(enumerate(gen(2), 5)), ((5, 0), (6, 10)))
        self.assertEqual(sorted(enumerate(gen(3)), reverse=True)[0], (2, 20))
        big = 2**62
        self.assertEqual(list(enumerate(gen(2), big)), [(big, 0), (big + 1, 10)])

    def test_partly_consumed(self):
        g = gen(5)
        next(g)
        e = enumerate(g)
        next(e)
        self.assertEqual(list(e), [(1, 20), (2, 30), (3, 40)])
        self.assertEqual(list(e), [])
        self.assertEqual(list(enumerate(g)), [])

    def test_raise_partway(self):
        e = enumerate(gen(5, fail_at=3))
        with self.assertRaises(KeyError):
            list(e)
        with self.assertRaises(StopIteration):
            next(e)
        e = enumerate(gen(5, fail_at=3), 7)
        try:
            list(e)
        except KeyError:
            pass
        self.assertEqual(e.__reduce__()[1][1], 10)
        e = enumerate(gen(4), 2)
        list(e)
        self.assertEqual(e.__reduce__()[1][1], 6)

    def test_stop_iteration_in_body(self):
        def bad():
            yield 1
            raise StopIteration
        with self.assertRaises(RuntimeError):
            list(enumerate(bad()))

    def test_self_reference(self):
        holder = []

        def g():
            yield 1
            try:
                next(holder[0])
            except ValueError as exc:
                yield str(exc)
        e = enumerate(g())
        holder.append(e)
        self.assertEqual(list(e), [(0, 1), (1, "generator already executing")])


if __name__ == "__main__":
    unittest.main()
