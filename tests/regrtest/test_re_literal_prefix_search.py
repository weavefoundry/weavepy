"""`re` searches whose patterns start with a literal find every match.

One-character and longer literal prefixes, prefixes that overlap
themselves (`aab` in `aaab`), matches at the start, the very end and past
`pos`/`endpos` bounds, misses, prefixes followed by more pattern, case
folding, and subjects that are bytes, bytearrays, memoryviews, ASCII text
and non-ASCII text all give what a plain scan of every position gives.
"""

import re
import unittest


def scan(pattern, subject, pos=0, endpos=None):
    """The first match by trying `match` at every position."""
    endpos = len(subject) if endpos is None else endpos
    for i in range(pos, endpos + 1):
        m = pattern.match(subject, i, endpos)
        if m:
            return m.span()
    return None


PATTERNS = [b"x", b"ab", b"aab", b"abab", b"\xfe\xff\x00\x01", b"a.c", b"ab+c", b"a[bc]d", b"zz", b"\x00"]
SUBJECTS = [b"", b"a", b"aaab", b"xxabababcab", b"cabcabd", b"abbbbc acd abd",
            bytes(range(256)) * 3, b"a" * 50 + b"ab", b"zzz", b"\x00\x00z"]


class PrefixSearchTests(unittest.TestCase):
    def test_bytes_like(self):
        for raw in PATTERNS:
            p = re.compile(raw)
            for subj in SUBJECTS:
                for kind in (bytes, bytearray, memoryview):
                    s = kind(subj)
                    m = p.search(s)
                    self.assertEqual(m.span() if m else None, scan(p, bytes(subj)), (raw, subj, kind))
                    for pos, endpos in ((1, None), (2, len(subj) - 1), (0, 3), (len(subj), None)):
                        if endpos is not None and endpos < 0:
                            continue
                        m = p.search(s, pos, endpos if endpos is not None else len(subj))
                        want = scan(p, bytes(subj), pos, endpos)
                        self.assertEqual(m.span() if m else None, want, (raw, subj, pos, endpos))

    def test_text(self):
        for raw in ("x", "ab", "aab", "é", "éa", "a.c", "日本"):
            p = re.compile(raw)
            for subj in ("", "aaab", "xxabababcab", "caébéa", "cabcabd", "日本語の日本", "a" * 40 + "ab"):
                m = p.search(subj)
                self.assertEqual(m.span() if m else None, scan(p, subj), (raw, subj))

    def test_findall_and_ignorecase(self):
        self.assertEqual(re.findall(b"ab", b"abxabyab"), [b"ab"] * 3)
        self.assertEqual(re.findall(b"aa", b"aaaaa"), [b"aa", b"aa"])
        self.assertEqual([m.span() for m in re.finditer(b"aab", b"aaabaab")], [(1, 4), (4, 7)])
        self.assertEqual(re.search(b"AB", b"xxab", re.I).span(), (2, 4))
        self.assertEqual(re.search("Straße", "eine STRASSE", re.I), None)
        self.assertEqual(re.sub(b"ab", b"-", bytearray(b"abcab")), b"-c-")

    def test_many(self):
        p = re.compile(rb"\xfe\xff\x00\x01")
        data = bytearray(bytes(range(256)) * 100)
        for i in range(300):
            self.assertEqual(p.search(data).span(), (254, 258))
            self.assertEqual(p.search(data, 300).span(), (510, 514))


if __name__ == "__main__":
    unittest.main()
