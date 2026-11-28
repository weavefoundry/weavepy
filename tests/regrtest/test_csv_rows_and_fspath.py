"""csv readers over lists and tuples from compiled loops (the first row,
line_num, the end, a caller-held source iterator left exhausted, a
record spanning lines, a non-str line's error), and os.fspath of
pathlib paths (before and after their string is cached, a subclass
overriding __fspath__ or __str__), each matching CPython."""

import csv
import os
import pathlib
import unittest


class Odd(pathlib.PurePosixPath):
    def __fspath__(self):
        return "odd"


class Shout(pathlib.PurePosixPath):
    def __str__(self):
        return "SHOUT"


class CsvFspathTests(unittest.TestCase):
    def test_rows(self):
        lines = ["a,b", "1,2"]
        for _ in range(300):
            r = csv.reader(lines)
            self.assertEqual(r.line_num, 0)
            self.assertEqual(next(r), ["a", "b"])
            self.assertEqual(r.line_num, 1)
            self.assertEqual(list(r), [["1", "2"]])
            self.assertEqual(r.line_num, 2)
            with self.assertRaises(StopIteration):
                next(r)
            self.assertEqual(list(csv.reader(("x",))), [["x"]])
            self.assertEqual(list(csv.reader(['"a\n', 'b",c'])), [["a\nb", "c"]])

    def test_held_source(self):
        for _ in range(300):
            src = ["a"]
            it = iter(src)
            self.assertEqual(list(csv.reader(it)), [["a"]])
            src.append("b")
            self.assertEqual(next(it, "end"), "end")

    def test_bad_line(self):
        with self.assertRaises(csv.Error):
            for _ in range(300):
                list(csv.reader(["a", 1]))

    def test_fspath(self):
        def run(n, p):
            out = None
            for i in range(n):
                out = os.fspath(p)
            return out

        for n in (1, 300):
            p = pathlib.PurePosixPath("/usr", "lib")
            self.assertEqual(run(n, p), "/usr/lib")
            self.assertEqual(run(n, pathlib.PurePosixPath("/x")), "/x")
            self.assertEqual(run(n, Odd("/y")), "odd")
            self.assertEqual(run(n, Shout("/z")), "SHOUT")
            self.assertEqual(run(n, pathlib.Path("/tmp")), "/tmp")


if __name__ == "__main__":
    unittest.main()
