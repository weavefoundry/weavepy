"""`template.format(**d)` formats from the mapping the call built.

An exact string's `format` called with only a `**` mapping of string keys
reads it as `format_map` would; whatever that can't render plainly
(positional fields, missing names, values with their own `__format__`)
keeps the general call and its errors.
"""

import unittest

N = 3000


class Money:
    def __format__(self, spec):
        return "$" + spec


class FormatKwargsTests(unittest.TestCase):
    def test_plain_values(self):
        D = {"a": 1, "b": "x", "c": 2.5}
        for _ in range(N):
            r = "{a}-{b}-{c:.1f}".format(**D)
        self.assertEqual(r, "1-x-2.5")
        self.assertEqual(D, {"a": 1, "b": "x", "c": 2.5})

    def test_errors_match(self):
        for _ in range(100):
            with self.assertRaises(KeyError):
                "{zz}".format(**{"a": 1})
            with self.assertRaises(IndexError):
                "{0}".format(**{"a": 1})

    def test_custom_format_and_attributes(self):
        class P:
            x = 7

        for _ in range(N):
            r = "{m:5}|{p.x}".format(**{"m": Money(), "p": P()})
        self.assertEqual(r, "$5|7")

    def test_merged_mappings(self):
        D = {"a": 1}
        for _ in range(N):
            r = "{a}{b}".format(**D, b=2)
        self.assertEqual(r, "12")


if __name__ == "__main__":
    unittest.main()
