"""A `bytes` iterator feeding native adapters (`map`, `filter`, `zip`,
`enumerate`, `''.join(map(...))`) steps in place; the items, partial
consumption, the iterator's state afterwards, and `urllib.parse.quote`
match the general steps."""

import unittest
from urllib.parse import quote, quote_from_bytes


class BytesCursorSteps(unittest.TestCase):
    def test_map_and_join(self):
        table = {b: chr(b).upper() for b in range(97, 123)}
        for _ in range(300):
            self.assertEqual("".join(map(table.__getitem__, b"hello")), "HELLO")
            self.assertEqual(list(map(lambda b: b + 1, b"\x00\x01\xff")), [1, 2, 256])
            self.assertEqual(sum(map(int, b"abc")), 97 + 98 + 99)

    def test_filter_zip_enumerate(self):
        for _ in range(300):
            self.assertEqual(list(filter(None, b"\x00a\x00b")), [97, 98])
            self.assertEqual(list(zip(b"ab", b"xyz")), [(97, 120), (98, 121)])
            self.assertEqual(list(enumerate(b"hi")), [(0, 104), (1, 105)])

    def test_partial_consumption(self):
        for _ in range(300):
            it = iter(b"abcdef")
            m = map(chr, it)
            self.assertEqual(next(m), "a")
            self.assertEqual(list(zip(m, range(2))), [("b", 0), ("c", 1)])
            # zip took "d" from the map before range ended it.
            self.assertEqual(next(it), ord("e"))
            self.assertEqual(list(m), ["f"])
            self.assertEqual(list(it), [])

    def test_quote(self):
        for i in range(500):
            self.assertEqual(quote("a b/c?d=e&f"), "a%20b/c%3Fd%3De%26f")
            self.assertEqual(quote_from_bytes(b"\x00\xff~"), "%00%FF~")
            self.assertEqual(quote("x" * (i % 7)), "x" * (i % 7))


if __name__ == "__main__":
    unittest.main()
