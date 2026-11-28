"""`%r` of plain values, and native reprs of instances with `__repr__`.

`"%r" % x` renders an `int`, `bool`, `None` or `str` in place; widths,
flags, mapping keys, subclasses and other values take the general path.
A container's repr calls its items' Python `__repr__` directly; a raising
or non-string `__repr__`, a class whose `__repr__` changes, and
`object`'s default repr behave as before.
"""

import unittest

N = 3000


class P:
    def __init__(self, x):
        self.x = x

    def __repr__(self):
        return "P(%r)" % self.x


class Bad:
    def __repr__(self):
        raise ValueError("bad repr")


class Late:
    pass


class S(str):
    pass


class PercentReprTests(unittest.TestCase):
    def test_plain_values(self):
        for i in range(N):
            r = "%r|%r|%r|%r|%r" % (i, "a'b", True, None, "x\n")
        self.assertEqual(r, "%d|\"a'b\"|True|None|'x\\n'" % (N - 1))

    def test_general_path(self):
        for _ in range(N):
            r = ("%5r|%-6r|" % (7, "x"), "%(a)r %(b)r" % {"a": -3, "b": "q"},
                 "%r" % (S("z"),), "%r %r" % (2.5, [1]), "%r" % ((1, 2),))
        self.assertEqual(r, ("    7|'x'   |", "-3 'q'", "'z'", "2.5 [1]", "(1, 2)"))


class InstanceReprTests(unittest.TestCase):
    def test_container_reprs(self):
        ps = [P(i) for i in range(3)]
        for _ in range(N):
            r = (repr(ps), repr({"k": ps[0]}), repr((ps[1],)), str(ps), f"{ps!r}")
        self.assertEqual(r[0], "[P(0), P(1), P(2)]")
        self.assertEqual(r[1], "{'k': P(0)}")
        self.assertEqual(r[2], "(P(1),)")
        self.assertEqual(r[3], r[0])
        self.assertEqual(r[4], r[0])

    def test_raising_repr(self):
        for _ in range(200):
            with self.assertRaises(ValueError):
                repr([Bad()])
            with self.assertRaises(ValueError):
                repr({1: Bad()})

    def test_repr_changes(self):
        objs = [Late(), Late()]
        out = []
        for i in range(N):
            if i == N // 2:
                Late.__repr__ = lambda self: "late"
            out.append(repr(objs))
        self.assertTrue(out[0].startswith("[<"))
        self.assertEqual(out[-1], "[late, late]")
        del Late.__repr__
        self.assertTrue(repr(objs).startswith("[<"))


if __name__ == "__main__":
    unittest.main()
