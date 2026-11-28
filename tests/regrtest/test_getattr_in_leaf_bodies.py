"""Functions whose bodies call getattr(), run often enough to evaluate
frameless: an instance attribute by a computed name, a default for a
miss, a builtin value's bound method, a property (plain and raising),
and a miss with no default (AttributeError), each matching CPython."""

import unittest


class Obj:
    def __init__(self):
        self.a = 1

    @property
    def p(self):
        return self.a + 1

    @property
    def bad(self):
        raise RuntimeError("bad")


def get(o, name):
    return getattr(o, name)


def get_d(o, name):
    return getattr(o, name, "dflt")


def run(n, f, *args):
    out = None
    for i in range(n):
        out = f(*args)
    return out


class GetattrLeafTests(unittest.TestCase):
    def test_values(self):
        o = Obj()
        for n in (1, 400):
            self.assertEqual(run(n, get, o, "a"), 1)
            self.assertEqual(run(n, get, o, "p"), 2)
            self.assertEqual(run(n, get_d, o, "zz"), "dflt")
            self.assertEqual(run(n, get, [1, 1], "count")(1), 2)
            self.assertEqual(run(n, get, "ab", "upper")(), "AB")

    def test_errors(self):
        o = Obj()
        with self.assertRaises(AttributeError):
            run(400, get, o, "zz")
        with self.assertRaises(RuntimeError):
            run(400, get, o, "bad")
        with self.assertRaises(RuntimeError):
            run(400, get_d, o, "bad")


if __name__ == "__main__":
    unittest.main()
