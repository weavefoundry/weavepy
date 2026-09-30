"""Generators whose bodies take fast steps (see `gen_fast` in the VM).

Each case checks a behavior at the boundary between the fast steps and
the general loop: overflow into big integers, raises, returns, sends,
throws and closes after partial iteration, nesting, and folding
consumers.
"""

import sys
import unittest


def counter(n):
    i = 0
    while i < n:
        yield i
        i += 1


def squares(it):
    for x in it:
        yield x * x


def evens(it):
    for x in it:
        if x & 1 == 0:
            yield x


def over_range(n):
    for i in range(n):
        yield i * 3


def over_list(xs):
    for x in xs:
        yield x + 1


def doubling(start, n):
    x = start
    for _ in range(n):
        yield x
        x = x * 2


def floats(n):
    x = 0.5
    i = 0
    while i < n:
        yield x
        x = x * 1.5
        i += 1


def divider(xs):
    for x in xs:
        yield 10 // x


def returning(n):
    i = 0
    while i < n:
        yield i
        i += 1
    return "done"


class FastStepTests(unittest.TestCase):
    def test_counter_and_pipeline(self):
        self.assertEqual(list(counter(5)), [0, 1, 2, 3, 4])
        self.assertEqual(list(evens(squares(counter(10)))), [0, 4, 16, 36, 64])
        self.assertEqual(sum(evens(squares(counter(1000)))), sum(
            x * x for x in range(1000) if (x * x) % 2 == 0))
        total = 0
        for x in squares(counter(100)):
            total += x
        self.assertEqual(total, sum(x * x for x in range(100)))

    def test_iterables_inside(self):
        self.assertEqual(list(over_range(5)), [0, 3, 6, 9, 12])
        self.assertEqual(list(over_list([1, 2, 3])), [2, 3, 4])
        self.assertEqual(list(over_list((1.5, 2.5))), [2.5, 3.5])
        self.assertEqual(list(over_list([])), [])

    def test_overflow_to_big_integers(self):
        values = list(doubling(1 << 60, 8))
        self.assertEqual(values, [(1 << 60) << k for k in range(8)])
        self.assertEqual(list(squares(doubling(3 << 30, 4))),
                         [(3 << 30 << k) ** 2 for k in range(4)])

    def test_floats(self):
        self.assertEqual(list(floats(4)), [0.5, 0.75, 1.125, 1.6875])

    def test_raise_midway(self):
        g = divider([5, 2, 0, 1])
        self.assertEqual(next(g), 2)
        self.assertEqual(next(g), 5)
        with self.assertRaises(ZeroDivisionError):
            next(g)
        with self.assertRaises(StopIteration):
            next(g)

    def test_return_value(self):
        g = returning(2)
        self.assertEqual(next(g), 0)
        self.assertEqual(next(g), 1)
        with self.assertRaises(StopIteration) as cm:
            next(g)
        self.assertEqual(cm.exception.value, "done")

    def test_send_and_throw_after_fast_steps(self):
        g = counter(10)
        self.assertEqual(next(g), 0)
        self.assertEqual(next(g), 1)
        self.assertEqual(g.send(None), 2)
        with self.assertRaises(KeyError):
            g.throw(KeyError("k"))
        self.assertIsNone(g.gi_frame)

    def test_close_after_fast_steps(self):
        g = squares(counter(10))
        self.assertEqual([next(g), next(g), next(g)], [0, 1, 4])
        g.close()
        with self.assertRaises(StopIteration):
            next(g)

    def test_frame_inspection(self):
        g = counter(10)
        next(g)
        next(g)
        frame = g.gi_frame
        self.assertEqual(frame.f_locals["i"], 1)
        self.assertEqual(next(g), 2)
        self.assertEqual(frame.f_locals["i"], 2)
        self.assertFalse(g.gi_running)

    def test_nested_inner_partial(self):
        # The inner generator overflows partway through a fast step of
        # the outer one.
        def outer(it):
            for x in it:
                yield x + 1

        self.assertEqual(list(outer(doubling(1 << 61, 4))),
                         [(1 << 61 << k) + 1 for k in range(4)])
        g = outer(returning(3))
        self.assertEqual(list(g), [1, 2, 3])

    def test_yield_from_fast_inner(self):
        def delegator(n):
            r = yield from returning(n)
            yield r

        self.assertEqual(list(delegator(3)), [0, 1, 2, "done"])

    def test_already_executing(self):
        def selfish():
            for x in g:
                yield x

        g = selfish()
        with self.assertRaises(ValueError):
            next(g)

    def test_folding_consumers(self):
        self.assertEqual(sum(counter(100)), 4950)
        self.assertEqual(list(squares(counter(5))), [0, 1, 4, 9, 16])
        self.assertEqual(tuple(evens(counter(7))), (0, 2, 4, 6))
        self.assertEqual(sum(floats(3)), 0.5 + 0.75 + 1.125)
        self.assertEqual(sum(doubling(1 << 62, 3)), (1 << 62) * 7)

    def test_tracing_sees_lines(self):
        seen = []

        def tracer(frame, event, arg):
            if frame.f_code is counter.__code__ and event == "line":
                seen.append(frame.f_lineno)
            return tracer

        sys.settrace(tracer)
        try:
            list(counter(2))
        finally:
            sys.settrace(None)
        self.assertTrue(seen)

    def test_recursion_depth(self):
        def chain(depth):
            if depth == 0:
                yield from counter(3)
                return
            for x in chain(depth - 1):
                yield x

        self.assertEqual(list(chain(30)), [0, 1, 2])


if __name__ == "__main__":
    unittest.main()
