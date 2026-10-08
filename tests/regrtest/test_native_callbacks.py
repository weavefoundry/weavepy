"""Python functions that native code calls back from hot code.

`sorted(key=...)`, `functools.partial`, `lru_cache`, `map`, and `f(*gen)`
call Python functions from native code while the calling activation runs
in the core loop. The callbacks must see the whole frame stack, raise
through it with complete tracebacks, and run in order.
"""

import functools
import os
import subprocess
import sys
import traceback
import unittest

FORCED = "--forced-frame-jit"
N = 2000


def keyed(n):
    out = None
    for i in range(n):
        out = sorted((i % 7, i % 3, i % 5), key=lambda v: -v)
    return out


def partials(n):
    double = functools.partial(lambda a, b: a * b, 2)
    return sum(double(i) for i in range(n))


@functools.lru_cache(maxsize=64)
def cached(k):
    return k * 3


def caches(n):
    return sum(cached(i % 100) for i in range(n))


def spread(n):
    def three(a, b, c):
        return a + b + c

    t = 0
    for i in range(n):
        t += three(*(x * 2 for x in (i, 1, 2)))
    return t


def caller_names():
    names = []
    f = sys._getframe(1)
    while f is not None:
        names.append(f.f_code.co_name)
        f = f.f_back
    return names


def via_sorted(n):
    seen = []
    for i in range(n):
        if i == n - 1:
            sorted([1, 2], key=lambda v: seen.append(caller_names()) or v)
    return seen


def failing_key(v):
    if v == 3:
        raise ValueError("bad key")
    return v


def raising(n):
    for i in range(n):
        sorted([1, 2, 3], key=failing_key)


class NativeCallbacksTest(unittest.TestCase):
    def test_results(self):
        self.assertEqual(keyed(N), sorted(((N - 1) % 7, (N - 1) % 3, (N - 1) % 5), reverse=True))
        self.assertEqual(partials(N), 2 * sum(range(N)))
        self.assertEqual(caches(N), sum(3 * (i % 100) for i in range(N)))
        self.assertEqual(spread(N), sum(2 * i + 6 for i in range(N)))

    def test_frames_seen_from_callback(self):
        seen = via_sorted(N)
        self.assertEqual(len(seen), 2)
        self.assertEqual(seen[0][:3], ["<lambda>", "via_sorted", "test_frames_seen_from_callback"])

    def test_traceback_through_native(self):
        try:
            raising(N)
        except ValueError as e:
            names = [fs.name for fs in traceback.extract_tb(e.__traceback__)]
        else:
            self.fail("no ValueError")
        self.assertEqual(names, ["test_traceback_through_native", "raising", "failing_key"])

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
