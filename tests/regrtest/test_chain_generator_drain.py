"""`list(chain(...))` drains a generator source whole, as `list(gen)`
does, then moves on; results, the chain's state after a raise, partly
consumed sources, and other sources match CPython."""

import itertools
import unittest


def gen(n, start=0):
    for i in range(start, start + n):
        yield i


class Boom(Exception):
    pass


def raising():
    yield 1
    yield 2
    raise Boom


class ChainGeneratorDrain(unittest.TestCase):
    def test_mixed_sources(self):
        for _ in range(200):
            self.assertEqual(list(itertools.chain(gen(3), gen(2, 10))), [0, 1, 2, 10, 11])
            self.assertEqual(list(itertools.chain([7], gen(2), (8, 9), gen(1, 5), "ab")),
                             [7, 0, 1, 8, 9, 5, "a", "b"])
            self.assertEqual(list(itertools.chain.from_iterable(gen(2, k) for k in (0, 10))),
                             [0, 1, 10, 11])
            self.assertEqual(list(itertools.chain(gen(0), gen(0), [1])), [1])
            self.assertEqual(list(itertools.chain({"k": 1}, gen(1))), ["k", 0])

    def test_raise_then_resume(self):
        c = itertools.chain(raising(), gen(2, 5))
        with self.assertRaises(Boom):
            list(c)
        self.assertEqual(list(c), [5, 6])

    def test_partly_consumed(self):
        g = gen(5)
        next(g)
        c = itertools.chain(g, [9])
        self.assertEqual(next(c), 1)
        self.assertEqual(list(c), [2, 3, 4, 9])
        self.assertEqual(list(g), [])

    def test_finally_runs(self):
        log = []

        def g():
            try:
                yield 1
                yield 2
            finally:
                log.append("done")
        for _ in range(50):
            log.clear()
            self.assertEqual(list(itertools.chain(g(), [3])), [1, 2, 3])
            self.assertEqual(log, ["done"])


if __name__ == "__main__":
    unittest.main()
