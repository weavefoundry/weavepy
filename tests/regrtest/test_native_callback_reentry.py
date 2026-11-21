"""Python callbacks that native functions call (`bisect`'s `key`).

A native function calling back into Python from a running frame reuses
the frame's interpreter state instead of publishing it again: results,
errors, tracebacks, `sys._getframe` from inside the callback, and
callbacks that reenter the native function behave as before.
"""

import bisect
import sys
import unittest

N = 3000


class NativeCallbackTests(unittest.TestCase):
    def test_bisect_key(self):
        xs = list(range(0, 300, 3))
        for i in range(N):
            r = (bisect.bisect_left(xs, i % 300, key=lambda x: x),
                 bisect.bisect_right(xs, 30, key=lambda x: x))
        self.assertEqual(r, (bisect.bisect_left(xs, (N - 1) % 300), 11))

    def test_raising_key(self):
        def bad(x):
            raise KeyError(x)

        for _ in range(300):
            try:
                bisect.bisect_left([1, 2, 3], 2, key=bad)
            except KeyError as e:
                caught = e
        tb = caught.__traceback__
        names = []
        while tb is not None:
            names.append(tb.tb_frame.f_code.co_name)
            tb = tb.tb_next
        self.assertEqual(names[0], "test_raising_key")
        self.assertEqual(names[-1], "bad")

    def test_frames_inside_key(self):
        seen = []

        def key(x):
            seen.append(sys._getframe(1).f_code.co_name)
            return x

        for _ in range(300):
            bisect.bisect_left([1, 2, 3, 4], 3, key=key)
        self.assertEqual(set(seen), {"test_frames_inside_key"})

    def test_reentrant_key(self):
        inner = list(range(10))

        def key(x):
            return bisect.bisect_left(inner, x, key=lambda y: y)

        for _ in range(N // 10):
            r = bisect.bisect_left(list(range(10)), 5, key=key)
        self.assertEqual(r, 5)


if __name__ == "__main__":
    unittest.main()
