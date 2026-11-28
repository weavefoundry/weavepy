"""A Python `__next__` that stores attributes before `raise StopIteration`.

A frameless iterator step may end at a bare `raise StopIteration` without
building the exception; the stores made ahead of the raise must land once,
for every way the iterator is driven (a `for` loop, `list()`, `sum()`, a
comprehension, `next()` with and without a default).
"""

import unittest


class Counter:
    def __init__(self, n):
        self.i = 0
        self.n = n
        self.calls = 0

    def __iter__(self):
        return self

    def __next__(self):
        self.calls += 1
        i = self.i
        if i >= self.n:
            self.done = True
            raise StopIteration
        self.i = i + 1
        return i


class Bumper:
    def __init__(self):
        self.i = 0

    def __iter__(self):
        return self

    def __next__(self):
        self.i += 1
        if self.i > 3:
            raise StopIteration
        return self.i


class IteratorStoreTests(unittest.TestCase):
    ROUNDS = 5000

    def test_for_loop(self):
        for _ in range(self.ROUNDS):
            it = Counter(3)
            seen = []
            for x in it:
                seen.append(x)
            self.assertEqual(seen, [0, 1, 2])
            self.assertEqual((it.calls, it.i, it.done), (4, 3, True))

    def test_list_and_sum(self):
        for _ in range(self.ROUNDS):
            it = Bumper()
            self.assertEqual(list(it), [1, 2, 3])
            self.assertEqual(it.i, 4)
            self.assertEqual(list(it), [])
            self.assertEqual(it.i, 5)
            c = Counter(4)
            self.assertEqual(sum(c), 6)
            self.assertEqual((c.calls, c.done), (5, True))

    def test_comprehension(self):
        for _ in range(self.ROUNDS):
            c = Counter(2)
            self.assertEqual([x * 2 for x in c], [0, 2])
            self.assertEqual(c.calls, 3)

    def test_next_without_default(self):
        for _ in range(self.ROUNDS):
            c = Counter(1)
            self.assertEqual(next(c), 0)
            with self.assertRaises(StopIteration):
                next(c)
            self.assertEqual((c.calls, c.done), (2, True))
            with self.assertRaises(StopIteration):
                next(c)
            self.assertEqual(c.calls, 3)

    def test_next_with_default(self):
        for _ in range(self.ROUNDS):
            c = Counter(1)
            self.assertEqual(next(c, "d"), 0)
            self.assertEqual(next(c, "d"), "d")
            self.assertEqual((c.calls, c.done), (2, True))
            self.assertEqual(next(c, None), None)
            self.assertEqual(c.calls, 3)

    def test_traceback_names_next(self):
        c = Counter(0)
        for _ in range(self.ROUNDS):
            try:
                next(c)
            except StopIteration as e:
                tb = e.__traceback__
        names = []
        while tb is not None:
            names.append(tb.tb_frame.f_code.co_name)
            tb = tb.tb_next
        self.assertIn("__next__", names)
        self.assertEqual(c.calls, self.ROUNDS)


if __name__ == "__main__":
    unittest.main()
