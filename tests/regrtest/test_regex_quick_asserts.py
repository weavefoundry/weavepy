"""Lookarounds over one character test, and `str.translate` tables.

The matcher answers an assertion whose body tests one character (a
literal or a set, possibly case-folded) or a position, or an alternation
of such tests, without entering itself. Lookaheads and lookbehinds,
negated or not, at the subject's edges, and mixed with bodies it cannot
answer that way must agree with the full matcher. `str.translate` looks
each ASCII character up once per call.
"""

import re
import textwrap
import unittest


class QuickAssertTest(unittest.TestCase):
    def check(self, pattern, text, expected):
        self.assertEqual(re.findall(pattern, text), expected, pattern)

    def test_single_tests(self):
        self.check(r"a(?=b)", "ab ac a", ["a"])
        self.check(r"a(?!b)", "ab ac a", ["a", "a"])
        self.check(r"(?<=-)\w", "a-b c-d -", ["b", "d"])
        self.check(r"(?<!-)\b\w", "a-b c-d", ["a", "c"])
        self.check(r"x(?=[\s,])", "x x,x.x", ["x", "x"])
        self.check(r"(?i:x(?=Y))", "xy xY Xy", ["x", "x", "X"])
        self.check(r"(?<=[^\d\W])\d", "a1 21 _3", ["1", "3"])

    def test_alternations_and_edges(self):
        self.check(r"\w+(?=\s|\Z)", "one two three", ["one", "two", "three"])
        self.check(r"\w+(?=,|$)", "a,b c", ["a", "c"])
        self.check(r"(?<=\s|-)\w", "ab cd-e", ["c", "e"])
        self.check(r"\w(?!\s|\Z)", "ab c", ["a"])
        self.check(r"(?<=a|b)c", "ac bc cc", ["c", "c"])
        self.check(r"(?<!a|b)c", "ac bc cc c", ["c", "c", "c"])
        self.check(r"(?=a|bc)\w", "a bc bd", ["a", "b"])
        self.check(r"(?=(a))\w", "ab", ["a"])
        self.assertEqual(re.split(r"(?<=,)", "a,b,,c"), ["a,", "b,", ",", "c"])
        self.assertEqual(re.sub(r"(?<=\d)(?=\d)", "_", "1234 5"), "1_2_3_4 5")

    def test_textwrap(self):
        text = "The well-known quick--brown fox jumps over-the lazy dog. " * 3
        lines = textwrap.wrap(text, 17)
        self.assertEqual(
            lines,
            ["The well-known", "quick--brown fox", "jumps over-the", "lazy dog. The",
             "well-known quick", "--brown fox jumps", "over-the lazy", "dog. The well-",
             "known quick--", "brown fox jumps", "over-the lazy", "dog."],
        )
        self.assertEqual(
            textwrap.TextWrapper(width=8).wrap("a-b-c-d-e x--y"),
            ["a-b-c-", "d-e x--y"],
        )

    def test_translate(self):
        table = {ord("\t"): " ", ord("a"): "AA", ord("b"): None, 0xE9: 0x45}
        for _ in range(200):
            self.assertEqual("a\tbécab".translate(table), "AA EcAA")
        self.assertEqual("abc".translate(str.maketrans("ab", "xy")), "xyc")
        self.assertEqual("abc".translate([None, None] * 50), "")
        self.assertEqual("abc\u00e9".translate(list(range(98))), "abc\u00e9")


if __name__ == "__main__":
    unittest.main()
