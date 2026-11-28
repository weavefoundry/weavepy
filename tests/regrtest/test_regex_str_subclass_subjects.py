"""`findall` and `split` of a str subclass's instance.

A compiled pattern's `findall` and `split` read a str subclass's instance
(`email`'s header tokens) as its string, in place in compiled code once
warm, and return exact strs, as CPython's slices of it are. A match keeps
the instance as its `string`.
"""

import re
import unittest

N = 3000


class T(str):
    pass


class RegexStrSubclassTests(unittest.TestCase):
    def test_findall(self):
        p = re.compile(r"[\x00-\x20\x7F]")
        g = re.compile(r"(\w)(\w)")
        cases = [
            ("hello", [], [("h", "e"), ("l", "l")]),
            ("a b\x01c", [" ", "\x01"], []),
            ("", [], []),
            ("\u00e9 xy", [" "], [("x", "y")]),
        ]
        for s, want, groups in cases:
            t = T(s)
            for _ in range(N):
                r = p.findall(t)
                q = g.findall(t)
            self.assertEqual(r, want)
            self.assertTrue(all(type(x) is str for x in r))
            self.assertEqual(q, groups)
            self.assertTrue(all(type(x) is str for pair in q for x in pair))

    def test_split(self):
        p = re.compile(r"\s+")
        k = re.compile(r"(-)")
        for _ in range(N):
            a = p.split(T("a b  c"))
            b = p.split(T("whole"))
            c = k.split(T("x-y"), 1)
        self.assertEqual(a, ["a", "b", "c"])
        self.assertEqual(b, ["whole"])
        self.assertIs(type(b[0]), str)
        self.assertEqual(c, ["x", "-", "y"])
        self.assertTrue(all(type(x) is str for x in a + c))

    def test_match_keeps_instance(self):
        t = T("a b")
        p = re.compile(r"\s")
        for _ in range(N):
            m = p.search(t)
        self.assertIs(m.string, t)
        self.assertIs(type(m.group()), str)

    def test_bytes_pattern_rejects_str_subclass(self):
        p = re.compile(rb"x")
        for _ in range(100):
            with self.assertRaises(TypeError):
                p.findall(T("x"))


if __name__ == "__main__":
    unittest.main()
