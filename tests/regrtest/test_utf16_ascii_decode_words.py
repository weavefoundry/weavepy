"""UTF-16 decoding of ASCII text, eight bytes at a time.

ASCII text in UTF-16 is checked a word at a time and its low bytes packed
straight into the string; every length (word-aligned or not), both byte
orders, a BOM, and the first non-ASCII unit in any position must decode as
the general decoder does.
"""

import codecs
import unittest


class Utf16AsciiTests(unittest.TestCase):
    def test_lengths_and_orders(self):
        for n in range(0, 40):
            text = "".join(chr(32 + (i * 7) % 95) for i in range(n))
            for enc in ("utf-16", "utf-16-le", "utf-16-be"):
                data = text.encode(enc)
                self.assertEqual(data.decode(enc), text, (n, enc))
                self.assertEqual(codecs.decode(data, enc), text)

    def test_control_and_boundary_characters(self):
        text = "a\x00b\x7fc\n\r\t" * 9
        for enc in ("utf-16-le", "utf-16-be"):
            self.assertEqual(text.encode(enc).decode(enc), text)

    def test_non_ascii_anywhere(self):
        base = "abcdefghijklmnopqrstuvwxyz"
        for pos in range(len(base) + 1):
            for ch in ("\x80", "é", "Ā", "€", "\U0001F600"):
                text = base[:pos] + ch + base[pos:]
                for enc in ("utf-16-le", "utf-16-be", "utf-16"):
                    self.assertEqual(text.encode(enc).decode(enc), text)

    def test_errors_still_raised(self):
        data = "abcdefgh".encode("utf-16-le") + b"\x00\xd8"
        with self.assertRaises(UnicodeDecodeError):
            data.decode("utf-16-le")
        self.assertEqual(data.decode("utf-16-le", "replace"), "abcdefgh�")
        with self.assertRaises(UnicodeDecodeError):
            ("abcd".encode("utf-16-le") + b"x").decode("utf-16-le")

    def test_incremental(self):
        dec = codecs.getincrementaldecoder("utf-16")()
        data = ("hello world, " * 10).encode("utf-16")
        out = "".join(dec.decode(data[i:i + 7]) for i in range(0, len(data), 7))
        out += dec.decode(b"", final=True)
        self.assertEqual(out, "hello world, " * 10)


if __name__ == "__main__":
    unittest.main()
