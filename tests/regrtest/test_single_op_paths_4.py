"""`int(text, base)` and `"".join(reversed(...))` in place.

`int` parses plain digits of a base without the full constructor, and
`str.join` drains a `reversed` snapshot at once. These loops check edge
cases against the full paths' results and errors.
"""

import unittest

N = 2000


class IntBaseTests(unittest.TestCase):
    CASES = [("ff", 16), ("FF", 16), (" -7f\n", 16), ("+10", 2), ("0xff", 16),
             ("0b11", 2), ("1_0", 10), ("z", 36), ("Z", 36), ("12", 3), ("", 16),
             ("-", 16), ("1 2", 10), ("8", 8), ("7fffffffffffffff", 16),
             ("8000000000000000", 16), ("-8000000000000000", 16), ("٣", 10),
             ("10", 0)]
    EXPECT = [255, 255, -127, 2, 255, 3, 10, 35, 35, 5, "VE", "VE", "VE", "VE",
              0x7FFFFFFFFFFFFFFF, 0x8000000000000000, -0x8000000000000000, 3, 10]

    def test_cases(self):
        for _ in range(N):
            out = []
            for text, base in self.CASES:
                try:
                    out.append(int(text, base))
                except ValueError:
                    out.append("VE")
        self.assertEqual(out, self.EXPECT)


class ReversedJoinTests(unittest.TestCase):
    def test_join(self):
        for _ in range(N):
            r = reversed("héllo")
            out = ("".join(r), list(r), "".join(reversed("")),
                   "-".join(reversed(["a", "b"])), "".join(reversed("x")))
            partial = reversed("abc")
            next(partial)
            rest = "".join(partial)
        self.assertEqual(out, ("olléh", [], "", "b-a", "x"))
        self.assertEqual(rest, "ba")

    def test_errors(self):
        for _ in range(N // 10):
            with self.assertRaises(TypeError) as cm:
                "".join(reversed([1, "a"]))
        self.assertEqual(str(cm.exception), "sequence item 1: expected str instance, int found")
        with self.assertRaises(TypeError) as cm:
            "".join(iter(["a", 2]))
        self.assertEqual(str(cm.exception), "sequence item 1: expected str instance, int found")


class MinMaxKeyTests(unittest.TestCase):
    def test_leaf_keys(self):
        words = "the quick brown fox jumps over the lazy dog".split()
        for _ in range(N):
            r = (min(words, key=len), max(words, key=len), min([[1, 2], [3]], key=len),
                 max(words, key=str.upper), min([3, 1, 2]), max([1.5, 2, 2.0]),
                 min(words, key=lambda w: -len(w)), max([2, 1, 2], key=abs))
        self.assertEqual(r, ("the", "quick", [3], "the", 1, 2, "quick", 2))

        class Sized:
            def __len__(self):
                return 5

        self.assertEqual(min([Sized(), "ab"], key=len), "ab")
        with self.assertRaises(TypeError):
            min([1], key=len)


if __name__ == "__main__":
    unittest.main()
