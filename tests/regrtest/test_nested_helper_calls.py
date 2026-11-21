"""Hot loops calling short helpers that call helpers of their own.

The frame JIT lowers a short callee's body into its caller, and a callee's
callee in turn once the frameless evaluator has run it; tier 2 leaves a
loop whose guarded calls run frameless to the frame JIT. These loops check
results, rebinding of the helpers mid-loop, exceptions raised from the
innermost helper, and helpers with defaults and closures.
"""

import unittest


def g_leaf(x):
    return x + 1


def g_mid(x):
    y = g_leaf(x)
    return y


def g_two(x):
    a = x * 2
    if a > 1000000:
        a = 0
    return a + 1


def g_raise(x):
    if x == 7777:
        raise ValueError(x)
    return x


def g_mid_raise(x):
    return g_raise(x) + 1


def g_default(x, k=3):
    return g_leaf(x) * k


class NestedHelperTests(unittest.TestCase):
    N = 20000

    def test_global_chain(self):
        for _ in range(3):
            t = 0
            for i in range(self.N):
                t = g_mid(t)
            self.assertEqual(t, self.N)

    def test_closure_chain(self):
        def leaf(x):
            return x + 2

        def mid(x):
            y = leaf(x)
            return y

        for _ in range(3):
            t = 0
            for i in range(self.N):
                t = mid(t)
            self.assertEqual(t, 2 * self.N)

    def test_branchy_helper(self):
        expect = 0
        for i in range(self.N):
            a = expect * 2
            if a > 1000000:
                a = 0
            expect = a + 1
        for _ in range(3):
            t = 0
            for i in range(self.N):
                t = g_two(t)
            self.assertEqual(t, expect)

    def test_float_loop_with_helper(self):
        expect = 0.0
        for i in range(self.N):
            expect = expect * 0.5 + (i * 2 % 1000001 + 1) * 1.5 - 0.25 * expect
        for _ in range(3):
            t = 0.0
            for i in range(self.N):
                t = t * 0.5 + g_two(i) * 1.5 - 0.25 * t
            self.assertAlmostEqual(t, expect)

    def test_rebinding_inner_helper(self):
        global g_leaf
        saved = g_leaf
        try:
            for _ in range(3):
                t = 0
                for i in range(self.N):
                    if i == self.N // 2:
                        g_leaf = lambda x: x + 10  # noqa: E731
                    t = g_mid(t)
                self.assertEqual(t, self.N // 2 + 10 * (self.N - self.N // 2))
                g_leaf = saved
        finally:
            g_leaf = saved

    def test_raise_from_inner_helper(self):
        for _ in range(3):
            t = 0
            with self.assertRaises(ValueError) as cm:
                for i in range(self.N):
                    t += g_mid_raise(i)
            self.assertEqual(cm.exception.args, (7777,))
            self.assertEqual(t, sum(range(7777)) + 7777)

    def test_defaults(self):
        for _ in range(3):
            t = 0
            for i in range(self.N):
                t += g_default(i)
            self.assertEqual(t, 3 * sum(i + 1 for i in range(self.N)))
            t = 0
            for i in range(self.N):
                t += g_default(i, 2)
            self.assertEqual(t, 2 * sum(i + 1 for i in range(self.N)))

    def test_short_comprehension_helper(self):
        # A helper whose only loop is a short comprehension, called often.
        def values(xs):
            return [g_leaf(x) for x in xs]

        for _ in range(3):
            out = 0
            for i in range(self.N // 4):
                out += sum(values((i, i + 1)))
            self.assertEqual(out, sum(2 * i + 3 for i in range(self.N // 4)))


if __name__ == "__main__":
    unittest.main()
