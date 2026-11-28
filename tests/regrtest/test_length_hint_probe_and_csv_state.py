"""list() of instances probes __len__ and __length_hint__ (whose absence
is now remembered per class version), and csv readers read their state
by position: a class that gains __len__ or __length_hint__ after
list() warmed up has it called (its errors raised, a TypeError ignored),
and readers read rows, count lines and end the same through next(),
list() and a for loop, with an attribute set on one, each matching
CPython."""

import csv
import unittest


class It:
    def __init__(self, n):
        self.i, self.n = 0, n

    def __iter__(self):
        return self

    def __next__(self):
        if self.i >= self.n:
            raise StopIteration
        self.i += 1
        return self.i


calls = []


def collect(n, make):
    out = None
    for i in range(n):
        out = list(make())
    return out


class LengthHintTests(unittest.TestCase):
    def test_hooks_added_later(self):
        self.assertEqual(collect(400, lambda: It(3)), [1, 2, 3])

        def hint(self):
            calls.append("hint")
            return 3

        It.__length_hint__ = hint
        try:
            del calls[:]
            self.assertEqual(collect(5, lambda: It(2)), [1, 2])
            self.assertEqual(calls, ["hint"] * 5)
        finally:
            del It.__length_hint__

        def bad_len(self):
            raise RuntimeError("len")

        It.__len__ = bad_len
        try:
            with self.assertRaises(RuntimeError):
                collect(1, lambda: It(2))
        finally:
            del It.__len__

        def type_len(self):
            raise TypeError("ignored")

        It.__len__ = type_len
        try:
            self.assertEqual(collect(3, lambda: It(2)), [1, 2])
        finally:
            del It.__len__
        del calls[:]
        self.assertEqual(collect(400, lambda: It(1)), [1])
        self.assertEqual(calls, [])


class CsvStateTests(unittest.TestCase):
    lines = ["a,b", "1,2", '"x,y",z']

    def test_next_list_and_loop(self):
        want = [["a", "b"], ["1", "2"], ["x,y", "z"]]
        for n in (1, 300):
            for i in range(n):
                r = csv.reader(self.lines)
                got = [next(r), next(r), next(r)]
                with self.assertRaises(StopIteration):
                    next(r)
            self.assertEqual(got, want)
            self.assertEqual(r.line_num, 3)
            for i in range(n):
                rows = list(csv.reader(self.lines))
            self.assertEqual(rows, want)
            for i in range(n):
                r = csv.reader(self.lines)
                rows = [row for row in r]
            self.assertEqual((rows, r.line_num), (want, 3))

    def test_dialect_read_between_rows(self):
        r = csv.reader(self.lines)
        self.assertEqual(next(r), ["a", "b"])
        self.assertEqual(r.dialect.delimiter, ",")
        self.assertEqual(list(r), [["1", "2"], ["x,y", "z"]])
        self.assertEqual(r.line_num, 3)


if __name__ == "__main__":
    unittest.main()
