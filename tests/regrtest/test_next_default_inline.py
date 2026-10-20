"""`next(gen, default)` resumes the generator inline, as `next(gen)` does,
with the default waiting atop the caller's stack until the generator
yields (the value replaces it) or finishes (it is the result); see
`InlineResume::NextDefault` in the VM. Each case runs warm (loops long
enough to compile the caller) and compares with the general path.
"""

import sys
import unittest

WARM = 3000


def count(n):
    for i in range(n):
        yield i


def returns(n):
    yield n
    return "ignored"


def raises():
    yield 1
    raise KeyError("inner")


def leaky():
    yield 1
    raise StopIteration


class Tree:
    __slots__ = ("left", "value", "right")

    def __init__(self, left, value, right):
        self.left = left
        self.value = value
        self.right = right

    def __iter__(self):
        if self.left:
            yield from self.left
        yield self.value
        if self.right:
            yield from self.right


def build(lo, hi):
    if lo >= hi:
        return None
    mid = (lo + hi) // 2
    return Tree(build(lo, mid), mid, build(mid + 1, hi))


def pairs(it):
    it = iter(it)
    for a in it:
        b = next(it, -1)
        yield a, b


def relay(it, default):
    while True:
        v = yield next(it, default)
        if v is not None:
            return v


class Mortal:
    deaths = 0

    def __del__(self):
        Mortal.deaths += 1


class NextDefaultTests(unittest.TestCase):
    def test_values_then_default_warm(self):
        for n in range(WARM // 100):
            g = count(n)
            got = [next(g, None) for _ in range(n + 2)]
            self.assertEqual(got, list(range(n)) + [None, None])

    def test_loop_warm(self):
        total = 0
        g = count(WARM)
        while (v := next(g, -1)) != -1:
            total += v
        self.assertEqual(total, sum(range(WARM)))
        self.assertEqual(next(g, "done"), "done")

    def test_return_value_ignored(self):
        for _ in range(WARM):
            g = returns(3)
            self.assertEqual(next(g, 0), 3)
            self.assertEqual(next(g, 0), 0)
            self.assertEqual(next(g, 0), 0)

    def test_raise_propagates(self):
        for _ in range(WARM // 10):
            g = raises()
            self.assertEqual(next(g, 0), 1)
            with self.assertRaises(KeyError):
                next(g, 0)
            self.assertEqual(next(g, "after"), "after")

    def test_pep479(self):
        g = leaky()
        next(g, 0)
        with self.assertRaises(RuntimeError):
            next(g, 0)

    def test_tree_pairs(self):
        tree = build(0, 101)
        for _ in range(30):
            got = list(pairs(tree))
        expected = [(a, a + 1) for a in range(0, 100, 2)] + [(100, -1)]
        self.assertEqual(got, expected)

    def test_yield_of_next(self):
        for _ in range(WARM // 10):
            r = relay(count(3), "end")
            self.assertEqual([next(r) for _ in range(5)], [0, 1, 2, "end", "end"])
            with self.assertRaises(StopIteration) as cm:
                r.send(7)
            self.assertEqual(cm.exception.value, 7)

    def test_default_released(self):
        for _ in range(WARM // 10):
            g = count(1)
            next(g, Mortal())
        Mortal.deaths = 0
        g = count(1)
        self.assertEqual(next(g, Mortal()), 0)
        self.assertEqual(Mortal.deaths, 1)
        self.assertIsInstance(next(g, Mortal()), Mortal)
        self.assertEqual(Mortal.deaths, 2)

    def test_running_generator(self):
        def selfish():
            yield next(me, "default")

        me = selfish()
        with self.assertRaises(ValueError):
            next(me, None)

    def test_unstarted_and_closed(self):
        g = count(2)
        g.close()
        self.assertEqual(next(g, "closed"), "closed")
        g = count(2)
        self.assertEqual(next(g, "x"), 0)

    def test_frame_line_during_resume(self):
        def probe():
            yield sys._getframe(1).f_lineno

        for _ in range(WARM // 10):
            line = next(probe(), None); expected = sys._getframe().f_lineno
        self.assertEqual(line, expected)


if __name__ == "__main__":
    unittest.main()
