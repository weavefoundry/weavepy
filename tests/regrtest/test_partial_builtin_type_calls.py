"""functools.partial over a builtin type from compiled loops: int with a
stored base (as keyword or position), str with an encoding, enumerate
with a start, keywords the call repeats or the type doesn't take, and a
bad value's error, each matching CPython."""

import functools
import unittest


def run(n, f, *args, **kw):
    out = None
    for i in range(n):
        out = f(*args, **kw)
    return out


class PartialTypeTests(unittest.TestCase):
    def test_int(self):
        hexint = functools.partial(int, base=16)
        for n in (1, 400):
            self.assertEqual(run(n, hexint, "ff"), 255)
            self.assertEqual(run(n, functools.partial(int, "11"), base=2), 3)
            self.assertEqual(run(n, functools.partial(int, "11", 8)), 9)
            self.assertEqual(run(n, functools.partial(int), "7"), 7)
            self.assertEqual(run(n, hexint, "10", base=8), 8)
        with self.assertRaises(ValueError):
            run(300, hexint, "zz")
        with self.assertRaises(TypeError):
            run(300, hexint, "ff", 10)
        with self.assertRaises(TypeError):
            run(300, functools.partial(int, x="1"))

    def test_str_and_enumerate(self):
        dec = functools.partial(str, encoding="utf-8")
        en = functools.partial(enumerate, start=5)
        for n in (1, 400):
            self.assertEqual(run(n, dec, b"ab"), "ab")
            self.assertEqual(list(run(n, en, "ab")), [(5, "a"), (6, "b")])
            self.assertEqual(run(n, functools.partial(str, b"x", "ascii")), "x")
        with self.assertRaises(TypeError):
            run(300, functools.partial(str, spam=1), "x")


if __name__ == "__main__":
    unittest.main()
