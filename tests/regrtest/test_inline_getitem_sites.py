"""`obj[key]` of a Python `__getitem__` lowered in line by the frame JIT.

A subscript site that has called a plain instance's Python `__getitem__`
runs its body in line, guarded by the class's version. It must agree with
the full call: any key type, a `NotImplemented` or `None` result taken as
a value, a raise inside the method (with its traceback through the
method), a method rebound or a class swapped mid-loop, a second class at
the same site, a subclass, deep recursion through `__getitem__`, a body
that leaves the compiled code (a generator, a call it can't lower), and a
metaclass `__getitem__`.
"""

import sys
import traceback
import unittest


class Seq:
    def __init__(self, d):
        self.d = d

    def __getitem__(self, i):
        return self.d[i]


class Odd:
    def __getitem__(self, key):
        if key == "ni":
            return NotImplemented
        if key == "none":
            return None
        if key == "boom":
            raise KeyError("boom")
        return (key, key)


class Sub(Seq):
    pass


class Other:
    def __getitem__(self, i):
        return -i


class Deep:
    def __getitem__(self, n):
        return 0 if n == 0 else self[n - 1] + 1


class Leaves:
    def __getitem__(self, n):
        return sum(x for x in range(n))


class Meta(type):
    def __getitem__(cls, key):
        return f"{cls.__name__}[{key}]"


class WithMeta(metaclass=Meta):
    pass


def total(obj, n):
    t = 0
    for i in range(n):
        t += obj[i % 3]
    return t


def items(obj, keys):
    return [obj[k] for k in keys]


class InlineGetitemTests(unittest.TestCase):
    def test_values_and_keys(self):
        s = Seq([10, 20, 30])
        for _ in range(50):
            self.assertEqual(total(s, 300), 6000)
        o = Odd()
        for _ in range(300):
            self.assertIs(o["ni"], NotImplemented)
            self.assertIsNone(o["none"])
            self.assertEqual(o[1.5], (1.5, 1.5))
            self.assertEqual(o[(1, 2)], ((1, 2), (1, 2)))

    def test_raise_inside(self):
        o = Odd()
        for _ in range(300):
            with self.assertRaises(KeyError):
                o["boom"]
        try:
            o["boom"]
        except KeyError as e:
            names = [f.name for f in traceback.extract_tb(e.__traceback__)]
        self.assertEqual(names[-1], "__getitem__")

    def test_index_error_in_body(self):
        s = Seq([1])
        for _ in range(300):
            with self.assertRaises(IndexError):
                s[5]

    def test_rebound_and_swapped(self):
        class R:
            def __getitem__(self, i):
                return i * 2

        r = R()
        self.assertEqual(sum(items(r, range(100))), 9900)
        for _ in range(30):
            items(r, range(100))
        R.__getitem__ = lambda self, i: i * 3
        self.assertEqual(sum(items(r, range(100))), 14850)
        r.__class__ = Other
        self.assertEqual(sum(items(r, range(100))), -4950)

    def test_polymorphic_site(self):
        objs = [Seq([1, 2, 3]), Other(), Sub([4, 5, 6]), Odd()]
        for _ in range(100):
            got = [o[1] for o in objs]
            self.assertEqual(got, [2, -1, 5, (1, 1)])

    def test_recursion_and_leaving(self):
        d = Deep()
        for _ in range(100):
            self.assertEqual(d[20], 20)
        limit = sys.getrecursionlimit()
        with self.assertRaises(RecursionError):
            d[limit + 100]
        lv = Leaves()
        for _ in range(200):
            self.assertEqual(lv[10], 45)

    def test_metaclass_getitem(self):
        for _ in range(300):
            self.assertEqual(WithMeta[3], "WithMeta[3]")


if __name__ == "__main__":
    unittest.main()
