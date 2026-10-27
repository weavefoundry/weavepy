"""Constructing a class whose `__new__` is Python code.

`pathlib.PurePath` and its subclasses define `__new__(cls, *args,
**kwargs)` returning `object.__new__(cls)`, then run `__init__(self,
*args)`. Such a construction runs `__new__`, then `__init__` on its
result when that is an instance of the class itself, as `type.__call__`
does: whatever `__new__` returns otherwise is the result untouched, and
errors, frames and tracebacks look as they do for any call.
"""

import pathlib
import sys
import traceback
import unittest

N = 300


class Path:
    __slots__ = ("args", "kw")

    def __new__(cls, *args, **kwargs):
        return object.__new__(cls)

    def __init__(self, *args):
        self.args = args


class SubPath(Path):
    __slots__ = ()


class Other:
    pass


class ReturnsOther:
    def __new__(cls, x):
        return x

    def __init__(self, x):
        raise AssertionError("__init__ must not run")


class ReturnsSubclass:
    def __new__(cls, x):
        if x:
            return object.__new__(Child)
        return object.__new__(cls)

    def __init__(self, x):
        self.ran = x


class Child(ReturnsSubclass):
    pass


class NewRaises:
    def __new__(cls, x):
        raise ValueError("new %s" % x)


class InitRaises:
    def __new__(cls, x):
        return object.__new__(cls)

    def __init__(self, x):
        raise KeyError(x)


class InitReturns:
    def __new__(cls):
        return object.__new__(cls)

    def __init__(self):
        return 1


class NoInit:
    def __new__(cls, *args):
        inst = object.__new__(cls)
        inst.args = args
        return inst


class Seen:
    frames = []

    def __new__(cls, depth):
        Seen.frames.append(sys._getframe(depth).f_code.co_name)
        return object.__new__(cls)

    def __init__(self, depth):
        Seen.frames.append(sys._getframe(depth).f_code.co_name)


class Defaults:
    def __new__(cls, a, b=2, *rest, c=3, **kw):
        inst = object.__new__(cls)
        inst.seen = (a, b, rest, c, kw)
        return inst

    def __init__(self, *args, **kw):
        self.init = (args, kw)


class Closure:
    def __new__(cls, x):
        def inner():
            return x * 2

        inst = object.__new__(cls)
        inst.v = inner()
        return inst

    def __init__(self, x):
        self.w = x


class PythonNewConstructionTests(unittest.TestCase):
    def test_new_then_init(self):
        for i in range(N):
            p = SubPath("a", i)
            self.assertIs(type(p), SubPath)
            self.assertEqual(p.args, ("a", i))
            self.assertEqual(type(p)("b").args, ("b",))

    def test_new_returns_other_object(self):
        o = Other()
        for i in range(N):
            self.assertIs(ReturnsOther(o), o)
            self.assertEqual(ReturnsOther(i), i)

    def test_new_returns_subclass_instance(self):
        for i in range(N):
            c = ReturnsSubclass(1)
            self.assertIs(type(c), Child)
            self.assertEqual(c.ran, 1)
            self.assertEqual(ReturnsSubclass(0).ran, 0)

    def test_errors(self):
        for i in range(N):
            with self.assertRaises(ValueError) as cm:
                NewRaises(i)
            self.assertEqual(str(cm.exception), "new %d" % i)
            with self.assertRaises(KeyError):
                InitRaises(i)
            with self.assertRaises(TypeError) as cm:
                InitReturns()
            self.assertIn("should return None", str(cm.exception))

    def test_traceback(self):
        try:
            NewRaises(1)
        except ValueError:
            names = [f.name for f in traceback.extract_tb(sys.exc_info()[2])]
        self.assertEqual(names[-1], "__new__")
        self.assertIn("test_traceback", names)

    def test_no_init(self):
        for i in range(N):
            self.assertEqual(NoInit(i, 2).args, (i, 2))

    def test_frames(self):
        Seen.frames.clear()
        for _ in range(N // 10):
            Seen(1)
        self.assertEqual(set(Seen.frames), {"test_frames"})

    def test_binding(self):
        for i in range(N):
            d = Defaults(i, 5, 6, c=7, z=8)
            self.assertEqual(d.seen, (i, 5, (6,), 7, {"z": 8}))
            self.assertEqual(d.init, ((i, 5, 6), {"c": 7, "z": 8}))
            self.assertEqual(Defaults(i).seen, (i, 2, (), 3, {}))
        with self.assertRaises(TypeError):
            Defaults()

    def test_closure(self):
        for i in range(N):
            c = Closure(i)
            self.assertEqual((c.v, c.w), (2 * i, i))

    def test_pathlib(self):
        base = pathlib.PurePosixPath("/srv/app")
        for i in range(N):
            p = base / ("d%d" % i) / "f.tar.gz"
            self.assertEqual(str(p), "/srv/app/d%d/f.tar.gz" % i)
            self.assertEqual(p.parent.name, "d%d" % i)
            self.assertEqual(p.suffixes, [".tar", ".gz"])


if __name__ == "__main__":
    unittest.main()
