"""Constructing exception classes that keep `BaseException`'s `__init__`.

A call of such a class allocates the instance and stores `args` (and a
`StopIteration`'s `value`) directly. Classes that override `__init__` or
`__new__`, or inherit another builtin exception's `__init__`, must keep
their own behavior, and the fresh instance must look like CPython's.
"""

import copy
import pickle
import unittest

N = 2000


class E(Exception):
    pass


class Deep(E):
    pass


class Stop(StopIteration):
    pass


class Key(KeyError):
    pass


class OS(OSError):
    pass


class Attr(AttributeError):
    pass


class WithInit(Exception):
    def __init__(self, a, b=2):
        super().__init__(a)
        self.b = b


class WithNew(Exception):
    def __new__(cls, *args):
        inst = super().__new__(cls, *args)
        inst.tag = "new"
        return inst


def build(cls, *args):
    out = None
    n = len(args)
    for _ in range(N):
        if n == 0:
            out = cls()
        elif n == 1:
            out = cls(args[0])
        else:
            out = cls(args[0], args[1])
    return out


class ExceptionNativeInitTest(unittest.TestCase):
    def test_args(self):
        self.assertEqual(build(E).args, ())
        self.assertEqual(build(E, 1).args, (1,))
        self.assertEqual(build(Deep, 1, "a").args, (1, "a"))
        e = build(E, "boom")
        self.assertEqual(str(e), "boom")
        self.assertEqual(repr(e), "E('boom')")

    def test_fresh_state(self):
        e = build(E, 1)
        self.assertIsNone(e.__traceback__)
        self.assertIsNone(e.__cause__)
        self.assertIsNone(e.__context__)
        self.assertFalse(e.__suppress_context__)
        self.assertFalse(hasattr(e, "__notes__"))
        e.extra = 5
        self.assertEqual(e.__dict__, {"extra": 5})

    def test_subclasses_of_builtins(self):
        s = build(Stop, 7)
        self.assertEqual((s.value, s.args), (7, (7,)))
        self.assertIsNone(build(Stop).value)
        self.assertEqual(str(build(Key, "k")), "'k'")
        o = build(OS, 2, "nope")
        self.assertEqual((o.errno, o.strerror, type(o)), (2, "nope", OS))
        a = build(Attr, "x")
        self.assertEqual(a.args, ("x",))

    def test_overrides(self):
        w = build(WithInit, 1)
        self.assertEqual((w.args, w.b), ((1,), 2))
        n = build(WithNew, 3)
        self.assertEqual((n.args, n.tag), ((3,), "new"))

    def test_raise_and_copy(self):
        for _ in range(N):
            try:
                raise E(1, 2)
            except E as caught:
                exc = caught
        self.assertEqual(exc.args, (1, 2))
        self.assertEqual(copy.copy(exc).args, (1, 2))
        self.assertEqual(pickle.loads(pickle.dumps(build(E, "p"))).args, ("p",))

    def test_keyword_arguments_rejected(self):
        with self.assertRaises(TypeError):
            E(x=1)


if __name__ == "__main__":
    unittest.main()
