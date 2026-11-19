"""`reversed()` of text and bytes, builtin type calls, and `accumulate`'s end.

`reversed(s)` of a `str` or `bytes` builds its iterator without the
general type call; builtin type constructors are reused across calls; and
a function-less `accumulate` over a list or tuple ends in place. Results,
errors, and the iterators' later behavior are unchanged.
"""

import itertools
import unittest

N = 3000


class ReversedTextTests(unittest.TestCase):
    def test_str_and_bytes(self):
        for _ in range(N):
            r = "".join(reversed("hello world"))
        self.assertEqual(r, "dlrow olleh")
        for _ in range(N):
            r = list(reversed("héllo✓"))
        self.assertEqual(r, ["✓", "o", "l", "l", "é", "h"])
        for _ in range(N):
            r = bytes(reversed(b"abc"))
        self.assertEqual(r, b"cba")
        self.assertEqual(list(reversed("")), [])
        it = reversed("ab")
        self.assertEqual(it.__length_hint__(), 2)
        self.assertEqual(next(it), "b")
        self.assertEqual(list(it), ["a"])
        self.assertEqual(list(it), [])

    def test_type_calls(self):
        for _ in range(N):
            r = (int("ff", 16), str(5), complex(1, 2), bool(3), frozenset([1]))
        self.assertEqual(r, (255, "5", 1 + 2j, True, frozenset([1])))
        with self.assertRaises(TypeError):
            reversed(3)
        with self.assertRaises(TypeError):
            reversed("ab", "c")
        with self.assertRaises(ValueError):
            int("zz", 10)

    def test_accumulate_ends(self):
        xs = [5, 1, 4, 2, 3]
        for _ in range(N):
            total = 0
            for x in itertools.accumulate(xs):
                total += x
            r = list(itertools.accumulate(tuple(xs)))
        self.assertEqual(total, 5 + 6 + 10 + 12 + 15)
        self.assertEqual(r, [5, 6, 10, 12, 15])
        a = itertools.accumulate([1, 2])
        self.assertEqual(list(a), [1, 3])
        self.assertEqual(list(a), [])
        self.assertRaises(StopIteration, next, a)
        b = itertools.accumulate([1.5, 2.5])
        for _ in b:
            pass
        self.assertEqual(next(b, "end"), "end")
        self.assertEqual(list(itertools.accumulate([1, 2], initial=10)), [10, 11, 13])
        self.assertEqual(list(itertools.accumulate([2, 3], lambda a, b: a * b)), [2, 6])
        self.assertEqual(list(itertools.accumulate(["a", "b"])), ["a", "ab"])
        self.assertEqual(list(itertools.accumulate([])), [])


if __name__ == "__main__":
    unittest.main()
