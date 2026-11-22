"""`csv.writer.writerow` of rows of strings, numbers, bools and `None`.

Lists and tuples of plain values write what CPython writes under every
quoting mode; a bool is `True` or `False`, quoted under
`QUOTE_NONNUMERIC` only as a number would be; a row with an object whose
`__str__` changes the row being written sees the change, as iterating it
live does; any iterable is a row; a non-iterable is a `csv.Error`.
"""

import csv
import io
import unittest


def write(rows, **kw):
    f = io.StringIO()
    w = csv.writer(f, **kw)
    for row in rows:
        w.writerow(row)
    return f.getvalue()


ROWS = [
    [1, "text", 1.5],
    (0, -7, 2**40, 1e300, -0.0, float("inf")),
    [True, False, None, ""],
    ["a,b", 'q"uote', "line\nbreak", " space "],
    [],
    [None],
    [""],
]


class PlainRowTests(unittest.TestCase):
    def test_modes(self):
        expected = {
            csv.QUOTE_MINIMAL: (
                '1,text,1.5\r\n0,-7,1099511627776,1e+300,-0.0,inf\r\n'
                'True,False,,\r\n"a,b","q""uote","line\nbreak", space \r\n'
                '\r\n""\r\n""\r\n'
            ),
            csv.QUOTE_ALL: (
                '"1","text","1.5"\r\n"0","-7","1099511627776","1e+300","-0.0","inf"\r\n'
                '"True","False","",""\r\n"a,b","q""uote","line\nbreak"," space "\r\n'
                '\r\n""\r\n""\r\n'
            ),
            csv.QUOTE_NONNUMERIC: (
                '1,"text",1.5\r\n0,-7,1099511627776,1e+300,-0.0,inf\r\n'
                'True,False,"",""\r\n"a,b","q""uote","line\nbreak"," space "\r\n'
                '\r\n""\r\n""\r\n'
            ),
        }
        for mode, want in expected.items():
            self.assertEqual(write(ROWS, quoting=mode), want, mode)
        got = write([[1, "a", None, ""], ["b"]], quoting=csv.QUOTE_STRINGS)
        self.assertEqual(got, '1,"a",,""\r\n"b"\r\n')
        got = write([[1, "a", None, ""]], quoting=csv.QUOTE_NOTNULL)
        self.assertEqual(got, '"1","a",,""\r\n')
        with self.assertRaises(csv.Error):
            write([[None]], quoting=csv.QUOTE_NONE)

    def test_live_iteration(self):
        row = [1, None, 3]

        class Grow:
            def __str__(self):
                row.append("added")
                return "g"

        row[1] = Grow()
        self.assertEqual(write([row]), "1,g,3,added\r\n")

    def test_other_iterables(self):
        self.assertEqual(write([(x for x in (1, 2)), range(3), "ab"]), "1,2\r\n0,1,2\r\na,b\r\n")
        with self.assertRaises(csv.Error):
            write([5])

    def test_round_trip(self):
        rows = [[str(i), "text", "1.5"] for i in range(300)]
        text = write(rows)
        self.assertEqual(list(csv.reader(io.StringIO(text))), rows)


if __name__ == "__main__":
    unittest.main()
