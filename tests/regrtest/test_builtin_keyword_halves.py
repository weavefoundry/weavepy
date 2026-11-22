"""Keyword calls of builtins served in place.

`dict(a=1)`, `s.encode(encoding=...)` and `s.split(sep=..., maxsplit=...)`
take their keywords without the general keyword call when nothing they do
runs code. Every other shape, and every error, is the full call's, with
CPython's results and messages.
"""

import unittest


class BuiltinKeywordTests(unittest.TestCase):
    def test_dict_keywords(self):
        for _ in range(300):
            d = dict(a=1, b="x")
            self.assertEqual(d, {"a": 1, "b": "x"})
            self.assertIs(type(d), dict)
            self.assertEqual(list(d), ["a", "b"])
        self.assertEqual(dict({"z": 0}, a=1), {"z": 0, "a": 1})
        self.assertEqual(dict([("k", 2)], k=3), {"k": 3})

        class D(dict):
            pass

        self.assertIs(type(D(a=1)), D)

    def test_encode_keywords(self):
        s = "héllo"
        for _ in range(300):
            self.assertEqual(s.encode(encoding="utf-8"), b"h\xc3\xa9llo")
            self.assertEqual("abc".encode(encoding="ascii"), b"abc")
            self.assertEqual(s.encode("latin-1", errors="strict"), b"h\xe9llo")
            self.assertEqual(s.encode(encoding="ascii", errors="ignore"), b"hllo")
            self.assertEqual(s.encode(errors="replace"), b"h\xc3\xa9llo")
        with self.assertRaises(UnicodeEncodeError):
            s.encode(encoding="ascii")
        with self.assertRaises(LookupError):
            s.encode(encoding="no-such-codec")
        with self.assertRaises(TypeError):
            s.encode(nope=1)

    def test_split_keywords(self):
        for _ in range(300):
            self.assertEqual("a b  c".split(maxsplit=1), ["a", "b  c"])
            self.assertEqual("a,b,c".split(sep=","), ["a", "b", "c"])
            self.assertEqual("a,b,c".split(",", maxsplit=1), ["a", "b,c"])
            self.assertEqual("a b".split(sep=None, maxsplit=-1), ["a", "b"])
            self.assertEqual("".split(sep=","), [""])
            self.assertEqual("x".split(maxsplit=0), ["x"])
        s = "only"
        self.assertIs(s.split(maxsplit=3)[0], s)
        with self.assertRaises(ValueError):
            "a".split(sep="")
        with self.assertRaises(TypeError):
            "a".split(maxsplit="x")
        with self.assertRaises(TypeError):
            "a".split(sep=1)
        with self.assertRaises(TypeError):
            "a".split(nope=1)

        class S(str):
            pass

        self.assertEqual(S("a b").split(maxsplit=1), ["a", "b"])


if __name__ == "__main__":
    unittest.main()
