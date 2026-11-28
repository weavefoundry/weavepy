"""Builtin types called with keywords that name positional parameters.

`int(text, base=16)`, `enumerate(xs, start=1)`, `str(b, encoding=...)`
and `bytes(s, encoding=...)` take the positional call's path when the
keywords name, in order, the parameters after the positional ones. Any
other keyword shape (a positional-only parameter by keyword, keywords in
another order, unknown names) takes the full call, with CPython's results
and errors.
"""

import unittest


class BuiltinTypeKeywordTests(unittest.TestCase):
    def test_values(self):
        for _ in range(300):
            self.assertEqual(int("ff", base=16), 255)
            self.assertEqual(int("101", base=2), 5)
            self.assertEqual(list(enumerate("ab", start=1)), [(1, "a"), (2, "b")])
            self.assertEqual(list(enumerate(iterable="ab")), [(0, "a"), (1, "b")])
            self.assertEqual(list(enumerate(iterable="a", start=5)), [(5, "a")])
            self.assertEqual(str(b"hi", encoding="ascii"), "hi")
            self.assertEqual(str(b"h\xff", "latin-1", errors="strict"), "h\xff")
            self.assertEqual(str(object=5), "5")
            self.assertEqual(bytes("hé", encoding="utf-8"), b"h\xc3\xa9")
            self.assertEqual(bytearray("ab", encoding="ascii"), bytearray(b"ab"))

    def test_errors(self):
        with self.assertRaises(TypeError):
            int(x="5")
        with self.assertRaises(TypeError):
            int(base=16)
        with self.assertRaises(ValueError):
            int("zz", base=16)
        self.assertEqual(list(enumerate(start=1, iterable="a")), [(1, "a")])
        with self.assertRaises(TypeError):
            enumerate("a", begin=1)
        with self.assertRaises(UnicodeDecodeError):
            str(b"\xff", encoding="ascii")
        with self.assertRaises(LookupError):
            bytes("a", encoding="no-such")
        with self.assertRaises(TypeError):
            str(5, encoding="utf-8")

    def test_strict_zip_and_map(self):
        xs, ys = [1, 2, 3], "abc"
        for _ in range(300):
            self.assertEqual(list(zip(xs, ys, strict=True)), [(1, "a"), (2, "b"), (3, "c")])
            self.assertEqual(list(zip(xs, ys[:2], strict=False)), [(1, "a"), (2, "b")])
            self.assertEqual(list(map(lambda a, b: a * 2 + len(b), xs, ys, strict=True)), [3, 5, 7])
        with self.assertRaises(ValueError) as cm:
            list(zip(xs, ys[:2], strict=True))
        self.assertIn("zip() argument 2 is shorter than argument 1", str(cm.exception))
        with self.assertRaises(ValueError):
            list(map(pow, [1, 2], [3], strict=True))
        self.assertEqual(list(zip(xs, ys, strict=1)), [(1, "a"), (2, "b"), (3, "c")])
        with self.assertRaises(TypeError):
            zip(xs, nope=True)

    def test_subclasses_keep_their_call(self):
        class I(int):
            pass

        self.assertIs(type(I("ff", base=16)), I)
        self.assertEqual(I("ff", base=16), 255)


if __name__ == "__main__":
    unittest.main()
