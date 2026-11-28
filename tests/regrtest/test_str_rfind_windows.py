"""`str.rfind`, `rindex`, `rpartition` and `rsplit` find the rightmost match.

Needles of one character and longer, empty needles, needles that overlap
themselves, short and long haystacks, ASCII and non-ASCII text (character
offsets, not byte offsets), and `start`/`end` windows including negative
and out-of-range ones all agree with a plain scan from the right.
"""

import unittest


def ref_rfind(s, sub, start=None, end=None):
    n = len(s)
    lo, hi, _ = slice(start, end).indices(n)
    if start is not None and start > n:
        return -1
    for i in range(hi - len(sub), lo - 1, -1):
        if s[i:i + len(sub)] == sub:
            return i
    return -1


TEXTS = ["", "a", "abcabc", "aaaa", "line 1 of text\nline 5 of text\n" * 40,
         "héllo wörld héllo", "日本語の日本語", "x" * 100 + "ab" + "x" * 100, "aXbXaXbX" * 30]
NEEDLES = ["", "a", "ab", "abc", "aa", "line 5 ", "héllo", "ö", "日本", "Xb", "missing", "x" * 70]


class RfindTests(unittest.TestCase):
    def test_against_scan(self):
        for s in TEXTS:
            for sub in NEEDLES:
                self.assertEqual(s.rfind(sub), ref_rfind(s, sub), (s[:20], sub))
                for start, end in ((1, None), (0, 5), (-10, None), (None, -3), (3, 2), (0, 1000), (2, 2)):
                    want = ref_rfind(s, sub, start, end)
                    self.assertEqual(s.rfind(sub, start, end), want, (s[:20], sub, start, end))

    def test_relatives(self):
        s = "a,b,,c" * 20
        self.assertEqual(s.rindex(","), len(s) - 2)
        with self.assertRaises(ValueError):
            s.rindex("zz")
        self.assertEqual(s.rpartition(",,"), (s[:s.rfind(",,")], ",,", s[s.rfind(",,") + 2:]))
        self.assertEqual("a.b.c".rsplit(".", 1), ["a.b", "c"])
        self.assertEqual("x--y--z".rsplit("--", 1), ["x--y", "z"])
        self.assertEqual("ééé".rfind("é"), 2)
        self.assertEqual("".rfind(""), 0)
        self.assertEqual("abc".rfind("", 5), -1)
        self.assertEqual("abc".rfind("", 3), 3)

    def test_window_edges(self):
        # Matches placed across each boundary the search's windows could
        # have, in bytes, bytearrays and text.
        for size in (300, 1000, 1300, 5000, 22000, 90000, 200000):
            edges = [size - e + d for e in (256, 1280, 5376, 21760) for d in range(-6, 3)]
            for at in [0, 1, size // 2, size - 300, size - 8, size - 5] + edges:
                if at < 0 or at + 5 > size:
                    continue
                base = bytearray(b"." * size)
                base[at:at + 5] = b"<ab>!"
                base = bytes(base[:size])
                for kind in (bytes, bytearray):
                    hay = kind(base)
                    self.assertEqual(hay.rfind(b"<ab>!"), base.find(b"<ab>!"), (size, at))
                    self.assertEqual(hay.rfind(b"ab"), base.find(b"ab"), (size, at))
                    self.assertEqual(hay.rfind(b"zz"), -1)
                text = base.decode()
                self.assertEqual(text.rfind("<ab>!"), text.find("<ab>!"), (size, at))
        self.assertEqual((b"a" * 1000).rfind(b"aa"), 998)
        self.assertEqual(("ab" * 500).rfind("ab"), 998)
        self.assertEqual((b"xy" * 500).rfind(b"yx"), 997)
        self.assertEqual((b"x" * 1000).rfind(b"xx", 0, 500), 498)
        self.assertEqual((b"x" * 1000).rfind(b"xy"), -1)

    def test_many(self):
        text = "".join("line %d of text\n" % i for i in range(2000))
        for _ in range(100):
            self.assertEqual(text.rfind("line 5 "), text.find("line 5 "))


if __name__ == "__main__":
    unittest.main()
