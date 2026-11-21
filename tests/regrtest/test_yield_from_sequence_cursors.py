"""`yield from` over builtin sequences in compiled generator bodies.

Compiled code takes a list's, tuple's, string's or range's iterator for
`yield from` in place and steps a list's or tuple's cursor without
leaving: values, the `None` result, a list growing mid-iteration,
`send()` of another value, `throw()`, and `__iter__` generators behave as
before.
"""

import unittest

N = 3000


def yf(xs):
    r = yield from xs
    return r


def collect(xs):
    return list(yf(xs))


class Gen:
    def __init__(self, d):
        self.d = d

    def __iter__(self):
        yield from self.d


class YieldFromCursorTests(unittest.TestCase):
    def test_sources(self):
        for _ in range(N):
            r = (collect([1, 2, 3]), collect((4, 5)), collect("ab"), collect(range(3)),
                 collect(iter([6])), collect([]))
        self.assertEqual(r, ([1, 2, 3], [4, 5], ["a", "b"], [0, 1, 2], [6], []))
        total = 0
        for _ in range(N):
            for x in Gen([1, 2, 3]):
                total += x
        self.assertEqual(total, 6 * N)

    def test_result_is_none(self):
        def outer(xs):
            r = yield from xs
            yield ("result", r)

        for _ in range(N):
            r = list(outer([1, 2]))
        self.assertEqual(r, [1, 2, ("result", None)])

    def test_growing_list(self):
        for _ in range(N // 10):
            xs = [1, 2]
            out = []
            for x in yf(xs):
                out.append(x)
                if x < 4:
                    xs.append(x + 2)
        self.assertEqual(out, [1, 2, 3, 4, 5])

    def test_send_and_throw(self):
        for _ in range(N // 10):
            g = yf([1, 2])
            self.assertEqual(next(g), 1)
            with self.assertRaises(AttributeError):
                g.send(5)
        g = yf([1, 2])
        next(g)
        with self.assertRaises(KeyError):
            g.throw(KeyError("k"))
        g = yf((1, 2))
        self.assertEqual(g.send(None), 1)
        self.assertEqual(g.send(None), 2)
        with self.assertRaises(StopIteration) as cm:
            g.send(None)
        self.assertIsNone(cm.exception.value)


if __name__ == "__main__":
    unittest.main()
