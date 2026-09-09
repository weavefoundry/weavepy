"""String allocation fast paths must preserve Unicode and iterable behavior."""

import unittest


class StringFastPaths(unittest.TestCase):
    def test_ascii_case_boundaries(self):
        self.assertEqual("it's a TEST_123word".title(), "It'S A Test_123Word")
        self.assertEqual("123ABC".capitalize(), "123abc")
        self.assertEqual("aZ\x00_9!".swapcase(), "Az\x00_9!")
        for method in ("upper", "lower", "title", "capitalize", "casefold", "swapcase"):
            self.assertEqual(getattr("", method)(), "")
        ascii_text = "".join(chr(i) for i in range(128))
        self.assertEqual(ascii_text.lower(), ascii_text.casefold())
        self.assertEqual(ascii_text.upper().lower(), ascii_text.lower())
        self.assertEqual(ascii_text.swapcase().swapcase(), ascii_text)

    def test_unicode_fallback(self):
        self.assertEqual("Straße".upper(), "STRASSE")
        self.assertEqual("Straße".casefold(), "strasse")
        self.assertEqual("ΟΣ ΟΣΑ".lower(), "ος οσα")
        self.assertEqual("aΣ\u0301".lower(), "aς\u0301")
        self.assertEqual("ǳABC".capitalize(), "ǲabc")
        self.assertEqual("\ud800ABC".lower(), "\ud800abc")

    def test_join_shapes_and_identity(self):
        text = "a long string that isn't interned " * 5
        for factory in (list, tuple, iter):
            self.assertIs("different separator".join(factory([text])), text)
            self.assertEqual("|".join(factory([])), "")
            self.assertEqual("雪".join(factory(["a", "", "é"])), "a雪雪é")
        self.assertEqual("\ud800".join(["a", "b"]), "a\ud800b")
        self.assertEqual("|".join(["a", "\ud800"]), "a|\ud800")

    def test_join_subclasses_and_iteration(self):
        class Text(str):
            def __str__(self):
                raise AssertionError("join must use the native string value")

        class Items(list):
            def __iter__(self):
                return iter(["override", "values"])

        self.assertEqual("|".join([Text("a"), Text("b")]), "a|b")
        self.assertIs(type("|".join([Text("a")])), str)
        self.assertEqual("|".join(Items(["ignored"])), "override|values")
        events = []

        def values():
            events.append(1)
            yield "a"
            yield 42
            events.append(2)
            yield "b"

        with self.assertRaises(TypeError):
            "|".join(values())
        self.assertEqual(events, [1, 2])

    def test_bounded_whitespace_split(self):
        for whitespace in (" ", "\t", "\n", "\x1c", "\x1f", "\u0085", "\u2003"):
            text = whitespace + "a" + whitespace + "β" + whitespace
            self.assertEqual(text.split(), ["a", "β"])
            self.assertEqual(text.split(None, 0), ["a" + whitespace + "β" + whitespace])
            self.assertEqual(text.split(None, 1), ["a", "β" + whitespace])
            self.assertEqual(text.split(None, 2), ["a", "β"])
            self.assertEqual(whitespace.split(None, 0), [])
        self.assertEqual(" a\ud800 b ".split(None, 1), ["a\ud800", "b "])
        self.assertEqual(" a ".split(maxsplit=1), ["a"])
        self.assertEqual("".split(None, 0), [])
        self.assertEqual("a,,b,".split(",", 2), ["a", "", "b,"])


if __name__ == "__main__":
    unittest.main()
