"""An out-of-range index of a list, tuple or string raises its IndexError
from compiled code; the message, the traceback line, negative indices, and
every other receiver match CPython."""

import traceback
import unittest


class L(list):
    def __getitem__(self, i):
        return "sub"


class IndexErrorInPlace(unittest.TestCase):
    def test_messages(self):
        cases = [([], 0, "list index out of range"), ([1], 1, "list index out of range"),
                 ([1, 2], -3, "list index out of range"), ((), 0, "tuple index out of range"),
                 ((1,), -2, "tuple index out of range"), ("", 0, "string index out of range"),
                 ("ab", 5, "string index out of range"), ("éa", 5, "string index out of range")]
        for _ in range(300):
            for seq, i, msg in cases:
                try:
                    seq[i]
                except IndexError as e:
                    self.assertEqual(str(e), msg)
                else:
                    self.fail("no raise")
            self.assertEqual([1, 2][-2], 1)
            self.assertEqual("ab"[-1], "b")
            self.assertEqual(L()[5], "sub")

    def test_traceback_line(self):
        def f(xs):
            return xs[3]
        for _ in range(300):
            try:
                f([])
            except IndexError as e:
                tb = traceback.extract_tb(e.__traceback__)
                self.assertEqual(tb[-1].name, "f")
                self.assertIn("xs[3]", tb[-1].line)

    def test_count_in_loop(self):
        data = [[1], [], [2, 3], []]
        for _ in range(300):
            n = 0
            for d in data:
                try:
                    d[0]
                except IndexError:
                    n += 1
            self.assertEqual(n, 2)


if __name__ == "__main__":
    unittest.main()
