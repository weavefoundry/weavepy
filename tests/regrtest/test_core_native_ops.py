"""Operations that hot code runs without leaving the core loop.

Truth tests of streams and other native objects, method loads on streams
(cached per stream kind), `str % dict` over plain values, the clock and
process builtins, property setters, `del` of list items and dict keys,
`float()` and `int()` of plain values, `min`/`max` over mixed numbers, and
namedtuple `_replace` must behave as the full handlers do: a stream's own
attribute shadows its native method, a dict value with a `__str__` still
formats through it, a class's own `__setattr__` still runs, and errors
are unchanged.
"""

import collections
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


class Box:
    def __init__(self):
        self._w = 0
        self.log = []

    @property
    def w(self):
        return self._w

    @w.setter
    def w(self, value):
        if value < 0:
            raise ValueError("negative")
        self._w = value
        return "ignored"

    @property
    def ro(self):
        return 1


class Logged(Box):
    def __setattr__(self, name, value):
        if name == "w":
            self.log.append(value)
        object.__setattr__(self, name, value)


def set_widths(b, n):
    for i in range(n):
        b.w = b.w + 1
    return b.w


def deletes(n):
    xs = list(range(n + 10))
    d = {i: i for i in range(n)}
    for i in range(n):
        del xs[-1]
        del d[i]
    return len(xs), len(d)


def conversions(texts, n):
    out = []
    for i in range(n):
        for t in texts:
            out.append((float(t[0]), int(t[1])))
    return out[-len(texts):]


def extremes(n):
    r = None
    for i in range(n):
        r = (max(1.5, 2, 1.0), max(2, 2.0), min(3, 2.5, 7), max(2**60, 1.0), min(-1, -1.0))
    return r


P = collections.namedtuple("P", "x y z")


def replaces(n):
    p = P(1, 2, 3)
    for i in range(n):
        p = p._replace(x=i, z=-i)
    return p


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

    def test_property_setter(self):
        b = Box()
        self.assertEqual(set_widths(b, N), N)
        with self.assertRaisesRegex(ValueError, "negative"):
            for _ in range(N):
                b.w = -1
        with self.assertRaises(AttributeError):
            for _ in range(N):
                b.ro = 2
        lg = Logged()
        self.assertEqual(set_widths(lg, N), N)
        self.assertEqual(lg.log, list(range(1, N + 1)))

    def test_deletes(self):
        self.assertEqual(deletes(N), (10, 0))
        with self.assertRaises(KeyError):
            d = {}
            for i in range(N):
                del d[i]
        with self.assertRaises(IndexError):
            xs = [1]
            for i in range(N):
                del xs[0]

    def test_conversions(self):
        texts = [("1e3", "12"), (" 2.5 ", " -7 "), ("-inf", "1_000"), ("0", "+0")]
        self.assertEqual(conversions(texts, N), [(1000.0, 12), (2.5, -7), (float("-inf"), 1000), (0.0, 0)])
        for _ in range(N):
            self.assertEqual((int(2.9), int(-2.9), int(True), float(True), float(3)), (2, -2, 1, 1.0, 3.0))
        with self.assertRaises(ValueError):
            conversions([("bad", "1")], N)
        with self.assertRaises(ValueError):
            conversions([("1", "bad")], N)

    def test_extremes(self):
        r = extremes(N)
        self.assertEqual(r, (2, 2, 2.5, 2**60, -1))
        self.assertIs(type(r[1]), int)
        self.assertIs(type(r[4]), int)

    def test_namedtuple_replace(self):
        self.assertEqual(replaces(N), P(N - 1, 2, -(N - 1)))
        with self.assertRaisesRegex(TypeError, "unexpected field"):
            for _ in range(N):
                P(1, 2, 3)._replace(w=1)

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
