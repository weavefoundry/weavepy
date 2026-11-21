"""`StringIO.read(n)` in a character-at-a-time loop (`shlex`'s lexer).

An ASCII run with no `\\r` is copied straight out of the buffer; every
other read (non-ASCII text, carriage returns under newline translation,
surrogates, a position past the end, a closed stream) keeps the general
path's answer.
"""

import io
import shlex
import unittest


def drain(stream, size=1):
    out = []
    while True:
        piece = stream.read(size)
        if not piece:
            return out
        out.append(piece)


class StringIOReadTests(unittest.TestCase):
    def test_char_loop(self):
        for _ in range(300):
            s = io.StringIO("ab\r\ncdéf\rgh\nxyz")
            self.assertEqual(
                drain(s),
                ["a", "b", "\r", "\n", "c", "d", "é", "f", "\r", "g", "h", "\n", "x", "y", "z"],
            )
            self.assertEqual(s.tell(), 15)

    def test_sizes(self):
        for _ in range(300):
            s = io.StringIO("hello world")
            self.assertEqual(s.read(3), "hel")
            self.assertEqual(s.read(0), "")
            self.assertEqual(s.read(-5), "lo world")
            self.assertEqual(s.read(2), "")
            self.assertEqual(s.tell(), 11)
            self.assertEqual(drain(io.StringIO("abcdefg"), 3), ["abc", "def", "g"])

    def test_newline_translation(self):
        s = io.StringIO("line1\r\nline2\rline3", newline=None)
        self.assertEqual(s.read(4), "line")
        self.assertEqual(s.read(), "1\nline2\nline3")
        s = io.StringIO()
        s.write("q\r\nw")
        s.seek(0)
        self.assertEqual((s.read(1), s.read(1), s.read(5)), ("q", "\r", "\nw"))

    def test_past_the_end(self):
        s = io.StringIO("abc", newline="")
        s.seek(10)
        self.assertEqual(s.read(1), "")
        self.assertEqual(s.tell(), 10)
        s.write("z")
        self.assertEqual(s.getvalue(), "abc" + "\0" * 7 + "z")

    def test_surrogates(self):
        s = io.StringIO("ab\udcffcd")
        self.assertEqual([s.read(1) for _ in range(5)], ["a", "b", "\udcff", "c", "d"])

    def test_writes_between_reads(self):
        s = io.StringIO("abc")
        self.assertEqual(s.read(1), "a")
        s.write("XY")
        self.assertEqual(s.read(1), "")
        s.seek(1)
        self.assertEqual(s.read(), "XY")

    def test_closed(self):
        s = io.StringIO("x")
        s.close()
        with self.assertRaisesRegex(ValueError, "^I/O operation on closed file$"):
            s.read(1)
        b = io.BytesIO(b"x")
        b.close()
        with self.assertRaisesRegex(ValueError, r"^I/O operation on closed file\.$"):
            b.read(1)

    def test_shlex(self):
        for _ in range(200):
            self.assertEqual(
                shlex.split("cmd --flag 'quoted arg' \"another one\" plain"),
                ["cmd", "--flag", "quoted arg", "another one", "plain"],
            )


if __name__ == "__main__":
    unittest.main()
