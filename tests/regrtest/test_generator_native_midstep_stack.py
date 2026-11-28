"""A generator whose native code arrives while a consumer drains it.

`sum(gen)`, `list(gen)` and the like can run a generator's whole body in
one step. The body gets hot and compiles during that step, and its native
code needs more value-stack room than the step started with: the stack
grows then, and the rest of the step runs natively with the same results
as the interpreted start. Run again with compiles made synchronous, so
the native code lands at the same back edge every time.
"""

import os
import subprocess
import sys
import unittest

N = 30000


def naturals(limit):
    i = 0
    while i < limit:
        yield i
        i += 1


def squared(it):
    for x in it:
        yield x * x


def odds_only(it):
    for x in it:
        if x & 1:
            yield x


def wide(it):
    # A body whose deepest stack is well past the loop's own.
    for x in it:
        yield (x, x + 1, x + 2, x + 3, x + 4, x + 5, x + 6, x + 7, x + 8, x + 9)


def wide_calls(it):
    for x in it:
        yield max(x, x + 1, x + 2, x + 3, x + 4, x + 5, x + 6, x + 7) + min(x, 1, 2, 3, 4, 5, 6, 7)


class Box:
    def __init__(self, v):
        self.v = v

    def __del__(self):
        Box.dropped += 1


Box.dropped = 0


def boxes(n):
    i = 0
    while i < n:
        yield Box(i)
        i += 1


def values(it):
    for b in it:
        yield b.v


class DrainTests(unittest.TestCase):
    def test_sum_of_a_pipeline(self):
        want = sum(x * x for x in range(N) if (x * x) & 1)
        self.assertEqual(sum(odds_only(squared(naturals(N)))), want)
        self.assertEqual(sum(x + 1 for x in naturals(N)), N * (N + 1) // 2)

    def test_list_and_tuple(self):
        self.assertEqual(list(squared(naturals(N))), [x * x for x in range(N)])
        self.assertEqual(tuple(odds_only(naturals(N))), tuple(range(1, N, 2)))

    def test_wide_bodies(self):
        got = list(wide(naturals(N)))
        self.assertEqual(len(got), N)
        self.assertEqual(got[0], tuple(range(10)))
        self.assertEqual(got[-1], tuple(range(N - 1, N + 9)))
        self.assertEqual(sum(t[9] for t in got), sum(range(9, N + 9)))
        self.assertEqual(
            sum(wide_calls(naturals(N))),
            sum(x + 7 + min(x, 1) for x in range(N)),
        )

    def test_min_max_any_all(self):
        self.assertEqual(max(squared(naturals(N))), (N - 1) ** 2)
        self.assertEqual(min(odds_only(naturals(N))), 1)
        self.assertTrue(all(x >= 0 for x in squared(naturals(N))))
        self.assertFalse(any(x < 0 for x in squared(naturals(N))))

    def test_objects_are_released_on_time(self):
        Box.dropped = 0
        self.assertEqual(sum(values(boxes(N))), N * (N - 1) // 2)
        self.assertEqual(Box.dropped, N)

    def test_early_exit_and_reuse(self):
        g = odds_only(squared(naturals(N)))
        head = [next(g) for _ in range(5)]
        self.assertEqual(head, [1, 9, 25, 49, 81])
        self.assertEqual(sum(g), sum(x * x for x in range(11, N) if x & 1))
        self.assertEqual(sum(g), 0)


class SynchronousCompileTests(unittest.TestCase):
    @unittest.skipIf(os.environ.get("WEAVEPY_FRAME_JIT_SYNC"), "the synchronous run itself")
    def test_synchronous_compiles(self):
        env = dict(os.environ, WEAVEPY_FRAME_JIT_SYNC="1")
        r = subprocess.run(
            [sys.executable, __file__, "DrainTests"],
            env=env,
            capture_output=True,
            text=True,
            timeout=240,
        )
        self.assertEqual(r.returncode, 0, r.stderr[-2000:])
        self.assertIn("OK", r.stderr)


if __name__ == "__main__":
    unittest.main()
