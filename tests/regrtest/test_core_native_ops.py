"""Operations on native objects that hot code runs without leaving the core loop.

Truth tests of streams and other native objects, method loads on streams
(cached per stream kind), `str % dict` over plain values, and the clock
and process builtins must behave as the full handlers do: a stream's own
attribute shadows its native method, a dict value with a `__str__` still
formats through it, and errors are unchanged.
"""

import io
import os
import subprocess
import sys
import time
import unittest

FORCED = "--forced-frame-jit"
N = 3000


def truths(objs, n):
    t = 0
    for i in range(n):
        if objs[i % len(objs)]:
            t += 1
    return t


def writes(stream, n):
    for i in range(n):
        stream.write("x")
        stream.flush()
    return stream


def formats(fmt, d, n):
    out = None
    for i in range(n):
        out = fmt % d
    return out


class Shown:
    def __str__(self):
        return "shown"


def clocks(n):
    pid = 0
    stamp = None
    for i in range(n):
        pid = os.getpid()
        tm = time.localtime(1_000_000 + i)
        stamp = time.strftime("%Y-%m-%d", tm)
        time.gmtime(i)
    return pid, stamp


class CoreNativeOpsTest(unittest.TestCase):
    def test_truth(self):
        objs = [io.StringIO(), io.BytesIO(), b"", b"x", frozenset(), {1}, len, range(0)]
        self.assertEqual(truths(objs, N), sum(bool(o) for o in objs) * N // len(objs))

    def test_stream_methods(self):
        s = writes(io.StringIO(), N)
        self.assertEqual(len(s.getvalue()), N)
        b = io.BytesIO()
        for _ in range(N):
            b.write(b"y")
        self.assertEqual(len(b.getvalue()), N)

    def test_stream_own_attribute_shadows_method(self):
        s = io.StringIO()
        writes(s, N)
        seen = []
        s.write = seen.append
        writes(s, 5)
        self.assertEqual(seen, ["x"] * 5)
        del s.write
        writes(s, 5)
        self.assertEqual(len(s.getvalue()), N + 5)

    def test_closed_stream(self):
        s = io.StringIO()
        writes(s, N)
        s.close()
        with self.assertRaises(ValueError):
            writes(s, 1)

    def test_percent_dict(self):
        d = {"a": 1, "b": "two", "c": 2.5, "d": None, "e": True}
        self.assertEqual(formats("%(a)d %(b)s %(c).1f %(d)s %(e)s %%", d, N), "1 two 2.5 None True %")
        self.assertEqual(formats("%(b)r %(a)5d", d, N), "'two'     1")
        self.assertEqual(formats("%(x)s", {"x": Shown()}, N), "shown")
        self.assertEqual(formats("%s", {"k": 1}, N), "{'k': 1}")
        with self.assertRaises(KeyError):
            formats("%(missing)s", d, N)
        with self.assertRaises(TypeError):
            formats("%(b)d", d, N)

    def test_clocks(self):
        pid, stamp = clocks(N)
        self.assertEqual(pid, os.getpid())
        self.assertEqual(stamp, time.strftime("%Y-%m-%d", time.localtime(1_000_000 + N - 1)))
        with self.assertRaises(TypeError):
            for _ in range(N):
                time.localtime("x")

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
