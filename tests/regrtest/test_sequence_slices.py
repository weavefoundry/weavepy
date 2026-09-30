"""Slices of every built-in sequence, over the step and bound shapes.

Each type slices its own storage (a tuple's items, a `bytes` buffer, a
string's code points), so these cases compare every shape against the
same selection made by explicit indexing.
"""

import unittest

STEPS = (None, 1, 2, 3, -1, -2, -3)
BOUNDS = (None, -100, -5, -1, 0, 1, 3, 5, 100)


def expected(seq, start, stop, step):
    return [seq[i] for i in range(*slice(start, stop, step).indices(len(seq)))]


class SequenceSliceTests(unittest.TestCase):
    def check(self, seq, rebuild):
        for start in BOUNDS:
            for stop in BOUNDS:
                for step in STEPS:
                    with self.subTest(start=start, stop=stop, step=step):
                        got = seq[start:stop:step]
                        self.assertIs(type(got), type(seq))
                        self.assertEqual(got, rebuild(expected(seq, start, stop, step)))

    def test_tuple(self):
        self.check(tuple(range(9)), tuple)
        self.check((), tuple)

    def test_list(self):
        self.check(list(range(9)), list)

    def test_bytes(self):
        self.check(bytes(range(9)), bytes)
        self.check(b"", bytes)

    def test_bytearray(self):
        self.check(bytearray(range(9)), bytearray)
        data = bytearray(b"abc")
        part = data[:]
        part[0] = 0x7A
        self.assertEqual(data, b"abc")

    def test_str(self):
        self.check("abcdéfghi", "".join)
        self.check("日本語テキストです", "".join)
        self.check("", "".join)

    def test_str_with_surrogates(self):
        text = "a\ud800b\udc00c\ud83d"
        self.check(text, "".join)
        self.assertEqual(text[1:2], "\ud800")
        self.assertEqual(text[::2], "abc")

    def test_large_sequences(self):
        # A small slice of a large sequence (the case `re` hits when it
        # chunks a 64K-entry charset map).
        big = bytes(65536)
        self.assertEqual(big[256:512], bytes(256))
        chunks = {big[i:i + 256] for i in range(0, 65536, 256)}
        self.assertEqual(chunks, {bytes(256)})
        items = tuple(range(100000))
        self.assertEqual(items[50000:50003], (50000, 50001, 50002))
        self.assertEqual(items[-3:], (99997, 99998, 99999))
        self.assertEqual(items[99990::4], (99990, 99994, 99998))

    def test_bad_steps_and_indices(self):
        for seq in ((1, 2), b"ab", bytearray(b"ab"), "ab", [1, 2]):
            with self.assertRaises(ValueError):
                seq[::0]
            with self.assertRaises(TypeError):
                seq["a":]


if __name__ == "__main__":
    unittest.main()
