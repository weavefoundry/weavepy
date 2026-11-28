"""A class's native `__next__` stepped directly, and the CSV reader's state.

Iterating an instance whose class has a native `__next__` (an
`io.StringIO`) calls it without looking it up each step; a subclass's
own `__next__`, or one assigned later, wins. The CSV reader keeps
`line_num` and its re-entry check as before.
"""

import csv
import io
import unittest

N = 3000


class Shout(io.StringIO):
    def __next__(self):
        return super().__next__().upper()


class NativeNextTests(unittest.TestCase):
    def test_stringio_lines(self):
        text = "a\nb\nc\n"
        for _ in range(N // 10):
            r = list(io.StringIO(text))
        self.assertEqual(r, ["a\n", "b\n", "c\n"])
        self.assertEqual(list(Shout(text)), ["A\n", "B\n", "C\n"])
        s = io.StringIO(text)
        self.assertEqual(next(s), "a\n")
        self.assertEqual(list(s), ["b\n", "c\n"])

    def test_assigned_later(self):
        class Lines(io.StringIO):
            pass

        for _ in range(N // 10):
            self.assertEqual(list(Lines("x\ny\n")), ["x\n", "y\n"])
        def stop(self):
            raise StopIteration

        Lines.__next__ = stop
        self.assertEqual(list(Lines("x\ny\n")), [])
        del Lines.__next__
        self.assertEqual(list(Lines("x\n")), ["x\n"])


class CsvReaderStateTests(unittest.TestCase):
    def test_line_num(self):
        text = "a,b\n1,2\n3,\"4\n5\"\n"
        for _ in range(N // 10):
            r = csv.reader(io.StringIO(text))
            rows = []
            nums = []
            for row in r:
                rows.append(row)
                nums.append(r.line_num)
        self.assertEqual(rows, [["a", "b"], ["1", "2"], ["3", "4\n5"]])
        self.assertEqual(nums, [1, 2, 4])

    def test_reentry(self):
        reader = None

        def lines():
            yield "a,b\n"
            next(reader)
            yield "c,d\n"

        reader = csv.reader(lines())
        self.assertEqual(next(reader), ["a", "b"])
        with self.assertRaises((csv.Error, ValueError, StopIteration)):
            next(reader)


if __name__ == "__main__":
    unittest.main()
