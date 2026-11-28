"""Calls of TypedDict classes (whose metaclass sets __call__ = dict) from
compiled loops, with keywords, a mapping, both, and none, giving plain
dicts as CPython does; and a metaclass whose __call__ is a Python
function or another builtin type still calls it."""

import typing
import unittest


class TD(typing.TypedDict):
    a: int
    b: str


class ListMeta(type):
    __call__ = list


class AsList(metaclass=ListMeta):
    pass


class Counting(type):
    calls = 0

    def __call__(cls, *a, **k):
        Counting.calls += 1
        return super().__call__(*a, **k)


class Counted(metaclass=Counting):
    def __init__(self, x=0):
        self.x = x


def make(n, f, *args, **kw):
    out = None
    for i in range(n):
        out = f(*args, **kw)
    return out


class TypedDictCallTests(unittest.TestCase):
    def test_typeddict(self):
        for n in (1, 400):
            d = make(n, TD, a=1, b="x")
            self.assertIs(type(d), dict)
            self.assertEqual(d, {"a": 1, "b": "x"})
            self.assertEqual(make(n, TD, {"a": 2}), {"a": 2})
            self.assertEqual(make(n, TD, {"a": 2}, b="y"), {"a": 2, "b": "y"})
            self.assertEqual(make(n, TD), {})

    def test_other_metaclasses(self):
        self.assertEqual(make(400, AsList, "ab"), ["a", "b"])
        Counting.calls = 0
        self.assertEqual(make(400, Counted, x=3).x, 3)
        self.assertEqual(Counting.calls, 400)


if __name__ == "__main__":
    unittest.main()
