"""Generators a compiled generator body iterates, stepped in place.

When a generator's body runs natively in a fast step, its `for x in inner`
over another simple generator steps the inner one directly instead of
leaving native code. Results, exceptions, early exhaustion, finalizer
timing and deep nesting must match the interpreter.
"""

import os
import subprocess
import sys
import unittest

FORCED = "--forced-frame-jit"
N = 20000


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


def failing(limit):
    i = 0
    while i < limit:
        if i == limit - 3:
            raise ValueError("inner")
        yield i
        i += 1


def returns(limit):
    i = 0
    while i < limit:
        yield i
        i += 1
    return "done"


class Tracked:
    deaths = 0

    def __del__(self):
        Tracked.deaths += 1


def objects(limit):
    i = 0
    while i < limit:
        yield Tracked()
        i += 1


def counted(it):
    n = 0
    for _ in it:
        n += 1
        yield n


def chain(depth, limit):
    g = naturals(limit)
    for _ in range(depth):
        g = squared_identity(g)
    return g


def squared_identity(it):
    for x in it:
        yield x


class NestedGeneratorNativeStepsTest(unittest.TestCase):
    def test_pipeline(self):
        expected = sum(x * x for x in range(N) if (x * x) & 1)
        for _ in range(3):
            self.assertEqual(sum(odds_only(squared(naturals(N)))), expected)
            self.assertEqual(sum(x + 1 for x in naturals(N)), N * (N + 1) // 2)

    def test_inner_raises(self):
        for _ in range(3):
            with self.assertRaisesRegex(ValueError, "inner"):
                sum(squared(failing(N)))

    def test_inner_return_value_ignored(self):
        for _ in range(3):
            self.assertEqual(list(squared(returns(5))), [0, 1, 4, 9, 16])
            self.assertEqual(sum(squared(returns(N))), sum(x * x for x in range(N)))

    def test_finalizers_run_promptly(self):
        Tracked.deaths = 0
        self.assertEqual(sum(counted(objects(N))), N * (N + 1) // 2)
        self.assertEqual(Tracked.deaths, N)

    def test_deep_chains(self):
        for depth in (3, 8, 12, 40):
            self.assertEqual(sum(chain(depth, 2000)), sum(range(2000)), depth)

    def test_forced_frame_jit(self):
        if FORCED in sys.argv:
            self.skipTest("already forced")
        env = dict(os.environ)
        env.update(
            WEAVEPY_JIT="0",
            WEAVEPY_FRAME_JIT_TUNE="8,1,3,0",
            WEAVEPY_FRAME_JIT_HOT="50",
        )
        r = subprocess.run(
            [sys.executable, __file__, FORCED],
            env=env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 0, r.stderr)


if __name__ == "__main__":
    unittest.main(argv=[a for a in sys.argv if a != FORCED])
