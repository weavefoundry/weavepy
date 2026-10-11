"""`next()` and native consumers stepping simple generators in place.

A simple generator body may run to its next yield without an activation
of its own, whether the consumer is a `FOR_ITER`, a `next()` call from
compiled or interpreted code, or a native consumer (`enumerate`, `zip`,
`map`, `iter` with a sentinel). Values, exhaustion (`StopIteration` and
its value, a default), the generator's state and the release of what it
yields must not depend on which consumer stepped it.
"""

import inspect
import unittest

ROUNDS = 300


def counting(n):
    for i in range(n):
        yield i
    return n * 10


class Noted:
    log = []

    def __init__(self, i):
        self.i = i

    def __del__(self):
        Noted.log.append(self.i)


def noted(n):
    for i in range(n):
        yield Noted(i)


class NextFastStepsTest(unittest.TestCase):
    def test_next_until_exhausted(self):
        def drain(g):
            out = []
            while True:
                try:
                    out.append(next(g))
                except StopIteration as e:
                    return out, e.value

        for _ in range(ROUNDS):
            out, value = drain(counting(5))
            self.assertEqual(out, [0, 1, 2, 3, 4])
            self.assertEqual(value, 50)

    def test_next_with_default(self):
        marker = object()

        def drain(g):
            out = []
            while (x := next(g, marker)) is not marker:
                out.append(x)
            return out

        for _ in range(ROUNDS):
            g = counting(4)
            self.assertEqual(drain(g), [0, 1, 2, 3])
            self.assertIs(next(g, marker), marker)
            self.assertEqual(inspect.getgeneratorstate(g), inspect.GEN_CLOSED)

    def test_next_state_between_items(self):
        def step(g, n):
            for _ in range(n):
                next(g)

        for _ in range(ROUNDS):
            g = counting(10)
            self.assertEqual(inspect.getgeneratorstate(g), inspect.GEN_CREATED)
            step(g, 3)
            self.assertEqual(inspect.getgeneratorstate(g), inspect.GEN_SUSPENDED)
            self.assertEqual(g.gi_frame.f_locals["i"], 2)
            self.assertEqual(next(g), 3)
            self.assertEqual(list(g), [4, 5, 6, 7, 8, 9])

    def test_release_of_yielded_values(self):
        def take(g, n):
            for _ in range(n):
                x = next(g)
                del x

        for _ in range(ROUNDS // 10):
            Noted.log = []
            g = noted(5)
            take(g, 3)
            self.assertEqual(Noted.log, [0, 1, 2])
            del g
            self.assertEqual(Noted.log, [0, 1, 2])

    def test_release_of_the_default(self):
        def last(g, default):
            x = next(g, default)
            return x

        for _ in range(ROUNDS // 10):
            Noted.log = []
            g = counting(0)
            self.assertIsInstance(last(g, Noted(-1)), Noted)
            self.assertEqual(Noted.log, [-1])

    def test_native_consumers(self):
        for _ in range(ROUNDS):
            self.assertEqual(list(enumerate(counting(3))), [(0, 0), (1, 1), (2, 2)])
            self.assertEqual(list(zip(counting(3), counting(4))), [(0, 0), (1, 1), (2, 2)])
            self.assertEqual(list(map(next, [counting(2)] * 2)), [0, 1])
            g = counting(3)
            self.assertEqual(list(map(next, [g, g, g])), [0, 1, 2])
            self.assertEqual(list(iter(counting(9).__next__, 4)), [0, 1, 2, 3])

    def test_native_consumer_exhaustion_value(self):
        for _ in range(ROUNDS):
            g = counting(2)
            self.assertEqual(list(enumerate(g)), [(0, 0), (1, 1)])
            with self.assertRaises(StopIteration) as cm:
                g.__next__()
            self.assertIsNone(cm.exception.value)
            g = counting(1)
            self.assertEqual(g.__next__(), 0)
            with self.assertRaises(StopIteration) as cm:
                g.__next__()
            self.assertEqual(cm.exception.value, 10)

    def test_running_inside(self):
        seen = []

        def peeking():
            for i in range(3):
                seen.append(me.gi_running)
                yield i

        for _ in range(ROUNDS // 10):
            seen.clear()
            me = peeking()
            self.assertEqual([next(me), next(me), next(me)], [0, 1, 2])
            self.assertEqual(seen, [True, True, True])

    def test_reentrant_next_raises(self):
        def selfish():
            yield next(me)

        for _ in range(ROUNDS // 10):
            me = selfish()
            with self.assertRaisesRegex(ValueError, "already executing"):
                next(me)


if __name__ == "__main__":
    unittest.main()
