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

    def test_bounded_whitespace_rsplit(self):
        for whitespace in (" ", "\t", "\n", "\x1c", "\x1f", "\u0085", "\u2003"):
            text = whitespace + "a" + whitespace + "β" + whitespace
            self.assertEqual(text.rsplit(), ["a", "β"])
            self.assertEqual(text.rsplit(None, 0), [whitespace + "a" + whitespace + "β"])
            self.assertEqual(text.rsplit(None, 1), [whitespace + "a", "β"])
            self.assertEqual(text.rsplit(None, 2), ["a", "β"])
            self.assertEqual(whitespace.rsplit(None, 0), [])
        self.assertEqual(" a\ud800 b ".rsplit(None, 1), [" a\ud800", "b"])
        self.assertEqual(" a ".rsplit(maxsplit=1), ["a"])
        self.assertEqual("".rsplit(None, 0), [])
        self.assertEqual("aaa".rsplit("aa", 1), ["a", ""])

    def test_prefix_suffix_bounds(self):
        for text in ("", "abc", "aéΣ😀z", "a\ud800z"):
            for needle in ("", "a", "z", "éΣ", "😀", text):
                self.assertEqual(text.startswith(needle), text[:len(needle)] == needle)
                self.assertEqual(text.endswith(needle), not needle or text[-len(needle):] == needle)
                for start in (-100, -2, -1, 0, 1, len(text), len(text) + 1, 100):
                    for end in (-100, -1, 0, 1, len(text), 100):
                        adjusted_start = max(0, start + len(text)) if start < 0 else start
                        adjusted_end = min(len(text), max(0, end + len(text) if end < 0 else end))
                        valid = adjusted_start <= adjusted_end
                        part = text[start:end]
                        self.assertEqual(text.startswith(needle, start, end),
                                         valid and part.startswith(needle))
                        self.assertEqual(text.endswith(needle, start, end),
                                         valid and part.endswith(needle))
            self.assertTrue(text.startswith((text,)))
            self.assertTrue(text.endswith((text,)))
            self.assertTrue(text.startswith(text, None, None))
            self.assertTrue(text.endswith(text, 0, 2**63 - 1))
            self.assertFalse(text.startswith("", 2**63 - 1))
            self.assertTrue(text.startswith(text, -2**63))
            self.assertTrue(text.endswith("", None, -2**63))
        self.assertTrue("aéΣ😀z".startswith("éΣ", 1, None))
        self.assertTrue("aéΣ😀z".endswith("😀", None, -1))
        with self.assertRaises(TypeError):
            "abc".startswith("a", "bad")
        with self.assertRaises(TypeError):
            "abc".endswith("c", 0, "bad")


if __name__ == "__main__":
    unittest.main()
