"""Python descriptors and stored callables read from hot code.

The core loop and the frame JIT run a Python data descriptor's `__get__`
as an inline activation, remembering the descriptor in its owner class's
attribute cache, and call a function stored on an instance without the
method machinery. Changing the descriptor's class or the owner class,
shadowing by the instance dict, and `__getattr__` must all behave as the
full attribute protocol does.
"""

import enum
import os
import subprocess
import sys
import unittest

FORCED = "--forced-frame-jit"
N = 3000


class Data:
    def __get__(self, inst, owner=None):
        if inst is None:
            return self
        return inst._v * 2

    def __set__(self, inst, value):
        inst._v = value


class NonData:
    def __get__(self, inst, owner=None):
        return "class"


class Holder:
    d = Data()
    n = NonData()

    def __init__(self, v):
        self._v = v


def read_d(obj, n):
    out = None
    for _ in range(n):
        out = obj.d
    return out


def read_n(obj, n):
    out = None
    for _ in range(n):
        out = obj.n
    return out


class Raising:
    def __get__(self, inst, owner=None):
        raise AttributeError("from __get__")

    def __set__(self, inst, value):
        pass


class Fallback:
    r = Raising()

    def __getattr__(self, name):
        return "fallback " + name


def read_r(obj, n):
    out = None
    for _ in range(n):
        out = obj.r
    return out


class Calls:
    def __init__(self, fn):
        self.fn = fn

    def method(self, x):
        return ("method", x)


def call_fn(obj, n):
    out = None
    for i in range(n):
        out = obj.fn(i)
    return out


def call_method(obj, n):
    out = None
    for i in range(n):
        out = obj.method(i)
    return out


class Color(enum.Enum):
    RED = 1
    GREEN = 2


def enum_values(n):
    t = 0
    for _ in range(n):
        t += Color.RED.value + Color.GREEN.value
    return t, Color.RED.name


class PythonDescriptorsTest(unittest.TestCase):
    def test_data_descriptor(self):
        h = Holder(3)
        self.assertEqual(read_d(h, N), 6)
        h.__dict__["d"] = "shadow"  # a data descriptor wins over the dict
        self.assertEqual(read_d(h, N), 6)
        h.d = 5
        self.assertEqual(read_d(h, N), 10)

    def test_descriptor_class_changes(self):
        class D2:
            def __get__(self, inst, owner=None):
                return 1

            def __set__(self, inst, value):
                pass

        class H:
            x = D2()

        def read_x(obj, n):
            out = None
            for _ in range(n):
                out = obj.x
            return out

        h = H()
        self.assertEqual(read_x(h, N), 1)
        D2.__get__ = lambda self, inst, owner=None: 2
        self.assertEqual(read_x(h, N), 2)
        del D2.__set__  # now a non-data descriptor: the dict wins
        h.__dict__["x"] = "own"
        self.assertEqual(read_x(h, N), "own")
        H.x = 7
        del h.__dict__["x"]
        self.assertEqual(read_x(h, N), 7)

    def test_non_data_descriptor_yields_to_instance(self):
        h = Holder(1)
        self.assertEqual(read_n(h, N), "class")
        h.__dict__["n"] = "instance"
        self.assertEqual(read_n(h, N), "instance")

    def test_getattr_fallback(self):
        self.assertEqual(read_r(Fallback(), N), "fallback r")

    def test_stored_callable(self):
        c = Calls(lambda x: x + 1)
        self.assertEqual(call_fn(c, N), N)
        c.fn = len
        with self.assertRaises(TypeError):
            call_fn(c, 1)
        c.fn = str
        self.assertEqual(call_fn(c, N), str(N - 1))

    def test_stored_callable_shadows_method(self):
        c = Calls(None)
        self.assertEqual(call_method(c, N), ("method", N - 1))
        c.method = lambda x: ("own", x)
        self.assertEqual(call_method(c, N), ("own", N - 1))
        del c.method
        self.assertEqual(call_method(c, N), ("method", N - 1))

    def test_enum_values(self):
        self.assertEqual(enum_values(N), (3 * N, "RED"))

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
