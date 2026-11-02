"""Keyword calls of a function whose parameters or locals are cells.

`urllib.parse.parse_qs` calls `parse_qsl` by keyword, and `parse_qsl`
defines a closure over its `encoding` and `errors` parameters. Such a call
binds its keywords, defaults and keyword-only parameters, then gives each
cell variable a fresh cell holding the bound value, whichever path runs it.
"""

import unittest

N = 300


def outer(qs, encoding="utf-8", errors="replace", *, _stacklevel=1):
    def inner(s):
        return (s, encoding, errors, _stacklevel)

    return inner(qs)


def counter(start, *, step=1):
    total = start

    def bump():
        nonlocal total
        total += step
        return total

    bump()
    bump()
    return total, bump


def make_adder(base):
    def adder(x, *, scale=1, extra=0):
        def inner():
            return base + x * scale + extra

        return inner()

    return adder


def varargs(*args, sep="-", **kwargs):
    def join():
        return sep.join(map(str, args)) + str(sorted(kwargs.items()))

    return join()


class KeywordCellCallTests(unittest.TestCase):
    def test_parameters_captured(self):
        for i in range(N):
            self.assertEqual(
                outer("q%d" % i, encoding="latin-1", errors="strict", _stacklevel=2),
                ("q%d" % i, "latin-1", "strict", 2),
            )
            self.assertEqual(outer("q", errors="x"), ("q", "utf-8", "x", 1))
            self.assertEqual(outer(qs="q"), ("q", "utf-8", "replace", 1))

    def test_nonlocal_rebinding(self):
        for i in range(N):
            total, bump = counter(i, step=3)
            self.assertEqual(total, i + 6)
            self.assertEqual(bump(), i + 9)
            self.assertEqual(bump(), i + 12)

    def test_closure_and_cells(self):
        adder = make_adder(100)
        for i in range(N):
            self.assertEqual(adder(i, scale=2, extra=1), 100 + 2 * i + 1)
            self.assertEqual(adder(x=i), 100 + i)

    def test_varargs(self):
        for i in range(N // 10):
            self.assertEqual(varargs(1, 2, i, sep="+", k=i), "1+2+%d[('k', %d)]" % (i, i))

    def test_errors(self):
        for _ in range(N // 10):
            with self.assertRaises(TypeError):
                outer("q", bogus=1)
            with self.assertRaises(TypeError):
                outer("q", qs="again")

    def test_parse_qs(self):
        import urllib.parse

        for i in range(N):
            self.assertEqual(
                urllib.parse.parse_qs("q=%d&r=x&r=y%%20z+w" % i),
                {"q": [str(i)], "r": ["x", "y z w"]},
            )


if __name__ == "__main__":
    unittest.main()
