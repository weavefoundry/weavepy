"""Calls compiled code runs in place: math.prod and math.fsum of lists
and tuples of plain numbers (int overflow, floats mixed in, bools, a
start value), unicodedata lookups and their argument errors,
csv.reader construction with default and given dialects, and stores
into a SimpleNamespace (a dunder name, attributes read back), each
matching CPython."""

import csv
import math
import types
import unicodedata
import unittest


def run(n, f, *args, **kw):
    out = None
    for i in range(n):
        out = f(*args, **kw)
    return out


class LeafCallTests(unittest.TestCase):
    def test_prod_fsum(self):
        for n in (1, 300):
            self.assertEqual(run(n, math.prod, [2, 3, 4]), 24)
            self.assertEqual(run(n, math.prod, (2, 3.5, 4)), 28.0)
            self.assertEqual(run(n, math.prod, [10**10] * 3), 10**30)
            self.assertEqual(run(n, math.prod, [2**62, 4]), 2**64)
            self.assertEqual(run(n, math.prod, []), 1)
            self.assertEqual(run(n, math.prod, [2, True]), 2)
            self.assertEqual(run(n, math.prod, [1.5], start=2), 3.0)
            self.assertEqual(run(n, math.fsum, [0.1] * 10), 1.0)
            self.assertEqual(run(n, math.fsum, (1, 2.5)), 3.5)
        with self.assertRaises(TypeError):
            run(300, math.prod, [[1], 2.5])

    def test_unicodedata(self):
        for n in (1, 300):
            self.assertEqual(run(n, unicodedata.category, "A"), "Lu")
            self.assertEqual(run(n, unicodedata.name, "a"), "LATIN SMALL LETTER A")
            self.assertEqual(run(n, unicodedata.decimal, "x", None), None)
        for f, arg in ((unicodedata.category, "argument"), (unicodedata.name, "argument 1")):
            with self.assertRaises(TypeError) as cm:
                f("ab")
            self.assertEqual(
                str(cm.exception),
                f"{f.__name__}(): {arg} must be a unicode character, not a string of length 2",
            )
            with self.assertRaises(TypeError) as cm:
                f(5)
            self.assertEqual(
                str(cm.exception), f"{f.__name__}() {arg} must be a unicode character, not int"
            )

    def test_csv(self):
        lines = ["a,b", '"c,d",e']
        for n in (1, 300):
            self.assertEqual(run(n, lambda: list(csv.reader(lines))), [["a", "b"], ["c,d", "e"]])
            self.assertEqual(run(n, lambda: list(csv.reader(["a;b"], delimiter=";"))), [["a", "b"]])
            r = run(n, csv.reader, lines, "excel-tab")
            self.assertEqual((r.dialect.delimiter, r.dialect.quotechar), ("\t", '"'))
        d = csv.reader([]).dialect
        self.assertEqual((d.delimiter, d.lineterminator, d.quoting, d.strict), (",", "\r\n", 0, False))

    def test_namespace_store(self):
        def store(n):
            s = types.SimpleNamespace(a=0)
            for i in range(n):
                s.b = i
                s.a += 1
            return s

        for n in (1, 400):
            s = store(n)
            self.assertEqual((s.a, s.b), (n, n - 1))
            self.assertEqual(vars(s), {"a": n, "b": n - 1})
        s = types.SimpleNamespace()
        for i in range(300):
            s.__doc__ = "d"
        self.assertEqual(s.__doc__, "d")


if __name__ == "__main__":
    unittest.main()
