"""`startswith` and `endswith` of `bytes` and `bytearray` agree with slicing.

Every pairing of a `bytes` or `bytearray` receiver with a `bytes`,
`bytearray` or `memoryview` affix, empty affixes and receivers, affixes
longer than the receiver, a receiver tested against itself, tuples of
affixes, `start`/`end` windows, and the `TypeError` for a `str` or `int`
affix, run past the compile threshold.
"""

import unittest

DATA = [b"", b"a", b"ab", b"\x00\x01\x02", b"hello world", bytes(range(256))]
AFFIXES = [b"", b"a", b"ab", b"\x00\x01", b"world", b"hello world!", bytes(range(250, 256))]


class AffixTests(unittest.TestCase):
    def test_pairs(self):
        for _ in range(60):
            for data in DATA:
                for affix in AFFIXES:
                    head = data[: len(affix)] == affix
                    tail = len(affix) <= len(data) and data[len(data) - len(affix):] == affix
                    for recv in (data, bytearray(data)):
                        for a in (affix, bytearray(affix), memoryview(affix)):
                            self.assertEqual(recv.startswith(a), head, (recv, a))
                            self.assertEqual(recv.endswith(a), tail, (recv, a))

    def test_self_tuples_windows(self):
        for _ in range(60):
            ba = bytearray(b"abcabc")
            self.assertTrue(ba.startswith(ba))
            self.assertTrue(ba.endswith(ba))
            self.assertTrue(b"abc".startswith((b"x", b"ab")))
            self.assertFalse(b"abc".endswith((b"x", bytearray(b"b"))))
            self.assertTrue(b"abcabc".startswith(b"ca", 2))
            self.assertTrue(b"abcabc".endswith(b"ab", 0, 5))
            self.assertFalse(b"abc".startswith(b"", 4))

    def test_errors(self):
        for recv in (b"abc", bytearray(b"abc")):
            with self.assertRaises(TypeError):
                recv.startswith("a")
            with self.assertRaises(TypeError):
                recv.endswith(97)
            with self.assertRaises(TypeError):
                recv.startswith()


if __name__ == "__main__":
    unittest.main()
