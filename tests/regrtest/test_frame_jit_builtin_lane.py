"""Leaf builtins whose arguments call back into Python, from compiled code.

`len` of an instance with a Python `__len__`, `isinstance` against an ABC,
three-argument `getattr` of an instance, and an instance's bound
`__reduce_ex__` run through the core loop's lane for them when compiled
code calls them, and the compiled caller goes on afterwards. Results,
raises inside the callbacks, and bad callback results must all match the
interpreter's.
"""

import abc
import copy
import os
import subprocess
import sys
import unittest

FORCED = "--forced-frame-jit"
N = 3000


class Sized:
    def __init__(self, n):
        self.n = n

    def __len__(self):
        if self.n < 0:
            raise ValueError("negative")
        return self.n


class Bad:
    def __len__(self):
        return "nope"


class Base(abc.ABC):
    @abc.abstractmethod
    def f(self): ...


class Impl(Base):
    def f(self):
        return 1


class Point:
    def __init__(self, x):
        self.x = x


def lengths(objs, n):
    t = 0
    for i in range(n):
        t += len(objs[i % len(objs)])
    return t


def abc_checks(objs, n):
    t = 0
    for i in range(n):
        t += isinstance(objs[i % len(objs)], Base)
    return t


def getattrs(objs, n):
    t = 0
    for i in range(n):
        o = objs[i % len(objs)]
        t += getattr(o, "x", 0) + getattr(o, "missing", 1)
    return t


def reductions(objs, n):
    out = None
    for i in range(n):
        reductor = getattr(objs[i % len(objs)], "__reduce_ex__", None)
        out = reductor(4)
    return out


class BuiltinLaneTest(unittest.TestCase):
    def test_len(self):
        objs = [Sized(1), Sized(2), [1, 2, 3]]
        self.assertEqual(lengths(objs, N), N * 2)
        with self.assertRaisesRegex(ValueError, "negative"):
            lengths([Sized(1), Sized(-1)], N)
        with self.assertRaises(TypeError):
            lengths([Sized(1), Bad()], N)

    def test_isinstance_abc(self):
        objs = [Impl(), object(), Impl()]
        self.assertEqual(abc_checks(objs, N), 2 * N // 3)

    def test_getattr(self):
        objs = [Point(2), Point(3), object()]
        self.assertEqual(getattrs(objs, N), N // 3 * (2 + 1) + N // 3 * (3 + 1) + N // 3 * (0 + 1))

    def test_reduce_ex(self):
        r = reductions([Point(1), Point(2)], N)
        self.assertEqual(r[0].__name__, "__newobj__")
        self.assertEqual(copy.deepcopy([Point(5)] * 3)[0].x, 5)

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
