"""The csv reader over a list or tuple of lines, a `StringIO` or a text file
yields what it yields over a generator of the same lines.

Lines of plain and quoted fields are split directly and other one-line
records run through the parser in place; records that span
lines, lines the parser raises on, non-string items, and the quoting styles
that convert fields take the full path, as do records that run past a
stream's buffered block. Rows, errors, `line_num` and the stream's `tell()`
agree, past the compile threshold.
"""

import csv
import io
import os
import tempfile
import unittest

LINES = [
    "abc,12,3.5,hello world,x\n",
    "a,b,c",
    "a,b,\r\n",
    ",,\r",
    "\n",
    "",
    "one\n",
    "x\ty;z\n",
    'p,"q,r",s\n',
    'p,q"r,s\n',
    "p\\,q,r\n",
    "héllo,wörld,日本\n",
    "a,,b\n",
    "a,b\nc\n",
    "a,b\rc\n",
    "a,b\n\n",
    " a, b ,c \n",
    "1,2.5,,x\n",
    '"",""""\n',
    '"a""b","c""",d\r\n',
    '"a"b,c\n',
    '"a\rb",c\n',
    'x,"y"\n',
    '"x",\n',
    '"x","y"',
    'a"b",c\n',
    '"é,""ü""",日\n',
    '"q"\r\r\n',
    '"abc""\n',
]

DIALECTS = [
    {},
    {"delimiter": "\t"},
    {"delimiter": ";"},
    {"quoting": csv.QUOTE_ALL},
    {"quoting": csv.QUOTE_NONE, "escapechar": "\\"},
    {"quoting": csv.QUOTE_NONE, "quotechar": None},
    {"escapechar": "\\"},
    {"quotechar": "'"},
    {"skipinitialspace": True},
    {"strict": True},
    {"quoting": csv.QUOTE_NONNUMERIC},
    {"quoting": csv.QUOTE_STRINGS},
    {"quoting": csv.QUOTE_NOTNULL},
    {"delimiter": "é"},
    {"doublequote": False},
    {"doublequote": False, "escapechar": "\\"},
    {"quotechar": "é"},
]


def outcome(reader):
    out = []
    while True:
        try:
            row = next(reader)
        except StopIteration:
            return out
        except Exception as e:  # noqa: BLE001 - compared below
            out.append((type(e).__name__, str(e), reader.line_num))
            return out
        out.append((row, reader.line_num))


class PlainLineTests(unittest.TestCase):
    def check(self, lines, **kw):
        want = outcome(csv.reader((line for line in lines), **kw))
        self.assertEqual(outcome(csv.reader(list(lines), **kw)), want, (lines, kw))
        self.assertEqual(outcome(csv.reader(tuple(lines), **kw)), want, (lines, kw))

    def test_each_line(self):
        for _ in range(30):
            for kw in DIALECTS:
                for line in LINES:
                    self.check([line, "z,y\n", line], **kw)

    def test_known_rows(self):
        for _ in range(200):
            r = csv.reader(["a,b,c\n", "1,,3\r\n", "x,\n", "\n", 'q,"w,e"\n', "é,ü"])
            self.assertEqual(list(r), [["a", "b", "c"], ["1", "", "3"], ["x", ""], [],
                                       ["q", "w,e"], ["é", "ü"]])
            self.assertEqual(r.line_num, 6)

    def test_records_across_lines(self):
        for _ in range(50):
            for kw in DIALECTS:
                self.check(['a,"multi\n', 'line",x\n', "b,c\n"], **kw)
                self.check(['"open\n'], **kw)
                self.check(['"x"y,z\n', '"a""b",c\n', '"q"\n'], **kw)
                self.check(["a\\\n", "b,c\n"], **kw)

    def test_bad_items(self):
        r = csv.reader(["a,b\n", 7, "c\n"])
        self.assertEqual(next(r), ["a", "b"])
        with self.assertRaises(csv.Error):
            next(r)
        self.assertEqual(next(r), ["c"])

    def test_field_limit(self):
        old = csv.field_size_limit(8)
        try:
            for _ in range(50):
                self.check(["abcdefgh\n", "abcdefghi\n", "a,bcdefgh\n"])
                with self.assertRaises(csv.Error):
                    list(csv.reader(["abcdefghi\n"]))
                self.assertEqual(list(csv.reader(["abcdefgh\n"])), [["abcdefgh"]])
        finally:
            csv.field_size_limit(old)

    def test_list_changes(self):
        lines = ["a,b\n", "c,d\n"]
        r = csv.reader(lines)
        self.assertEqual(next(r), ["a", "b"])
        lines.append("e\n")
        lines[1] = 'x,"y"\n'
        self.assertEqual(list(r), [["x", "y"], ["e"]])
        self.assertEqual(r.line_num, 3)

    def test_streams(self):
        parts = []
        for i in range(3000):
            if i % 7 == 0:
                parts.append('%d,"multi\nline %d",x\r\n' % (i, i))
            elif i % 5 == 0:
                parts.append('%d,"q,""%d""",y\n' % (i, i))
            elif i % 11 == 0:
                parts.append("\n")
            else:
                parts.append("%d,plain %d,é\n" % (i, i))
        text = "".join(parts)
        for newline in ("", None):
            want = outcome(csv.reader(line for line in io.StringIO(text, newline=newline)))
            self.assertEqual(outcome(csv.reader(io.StringIO(text, newline=newline))), want)
            with tempfile.TemporaryDirectory() as d:
                path = os.path.join(d, "rows.csv")
                with open(path, "w", encoding="utf-8", newline="") as f:
                    f.write(text)
                with open(path, encoding="utf-8", newline=newline) as f:
                    self.assertEqual(outcome(csv.reader(f)), want)
                with open(path, encoding="utf-8", newline=newline) as f:
                    r = csv.reader(f)
                    next(r)
                    next(r)
                    with self.assertRaises(OSError):
                        f.tell()
                    self.assertEqual(f.readline(), "2,plain 2,é\n")

    def test_dict_reader(self):
        lines = ["k,v\n"] + ["%d,%d\n" % (i, i * i) for i in range(500)]
        rows = list(csv.DictReader(lines))
        self.assertEqual(rows[7], {"k": "7", "v": "49"})
        self.assertEqual(len(rows), 500)


if __name__ == "__main__":
    unittest.main()
