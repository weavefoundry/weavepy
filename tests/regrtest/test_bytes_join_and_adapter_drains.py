"""bytes.join from compiled loops (a leaf half over lists and tuples of
bytes and bytearray parts) and list() drains of native adapters (each
bulk path tried only for its own kind): joined bytes, a bytearray
separator, a generator of parts, wrong part types, and chain, accumulate,
zip, filter and map drains, each matching CPython."""

import itertools
import operator
import unittest


def joins(n, sep, parts):
    out = None
    for i in range(n):
        out = sep.join(parts)
    return out


def drains(n, xs):
    out = None
    for i in range(n):
        out = (
            list(itertools.chain(xs, xs)),
            list(itertools.accumulate(xs)),
            list(zip(xs, xs)),
            list(filter(None, xs)),
            list(map(abs, xs)),
            list(itertools.accumulate(xs, operator.mul, initial=2)),
        )
    return out


class BytesJoinTests(unittest.TestCase):
    def test_parts(self):
        for n in (1, 300):
            self.assertEqual(joins(n, b"-", [b"a", b"b", b"c"]), b"a-b-c")
            self.assertEqual(joins(n, b"", (b"x", bytearray(b"y"))), b"xy")
            self.assertEqual(joins(n, b", ", []), b"")
            self.assertEqual(joins(n, b"+", [b"only"]), b"only")
            self.assertEqual(joins(n, bytearray(b"/"), [b"a", b"b"]), bytearray(b"a/b"))
            self.assertIs(type(joins(n, bytearray(b"/"), [b"a"])), bytearray)

    def test_other_iterables_and_errors(self):
        self.assertEqual(joins(300, b".", (bytes([c]) for c in b"abc")), b"")
        self.assertEqual(b".".join(bytes([c]) for c in b"abc"), b"a.b.c")
        self.assertEqual(joins(300, b".", [memoryview(b"m"), b"n"]), b"m.n")
        with self.assertRaises(TypeError):
            joins(300, b".", [b"a", "b"])
        with self.assertRaises(TypeError):
            joins(300, b".", [b"a", 1])


class DrainTests(unittest.TestCase):
    def test_drains(self):
        xs = [0, -1, 2, 0, -3]
        r = drains(300, xs)
        self.assertEqual(r[0], xs + xs)
        self.assertEqual(r[1], [0, -1, 1, 1, -2])
        self.assertEqual(r[2], [(x, x) for x in xs])
        self.assertEqual(r[3], [-1, 2, -3])
        self.assertEqual(r[4], [0, 1, 2, 0, 3])
        self.assertEqual(r[5], [2, 0, 0, 0, 0, 0])


if __name__ == "__main__":
    unittest.main()
