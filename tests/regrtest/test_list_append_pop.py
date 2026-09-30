"""`list.append` and `list.pop` on a local list, run in line.

The core loop's fused method call appends and pops directly. These
cases check the results and every error against the full methods:
negative and boolean indexes, empty lists, out-of-range indexes, and
subclasses, which keep the full path.
"""

import unittest


class ListAppendPopTests(unittest.TestCase):
    def test_append_and_pop_round_trip(self):
        xs = []
        for i in range(100):
            xs.append(i)
            xs.append((i, "x"))
        popped = [xs.pop() for _ in range(4)]
        self.assertEqual(popped, [(99, "x"), 99, (98, "x"), 98])
        self.assertEqual(len(xs), 196)

    def test_pop_indexes(self):
        for _ in range(20):
            xs = [1, 2, 3, 4]
            self.assertEqual(xs.pop(0), 1)
            self.assertEqual(xs.pop(-1), 4)
            self.assertEqual(xs.pop(True), 3)
            self.assertEqual(xs.pop(False), 2)
            self.assertEqual(xs, [])

    def test_pop_errors(self):
        for _ in range(20):
            with self.assertRaisesRegex(IndexError, "pop from empty list"):
                [].pop()
            with self.assertRaisesRegex(IndexError, "pop from empty list"):
                [].pop(0)
            with self.assertRaisesRegex(IndexError, "pop index out of range"):
                [1].pop(5)
            with self.assertRaisesRegex(IndexError, "pop index out of range"):
                [1].pop(-2)
            with self.assertRaises(TypeError):
                [1].pop("0")

    def test_subclass_overrides(self):
        class Logged(list):
            def append(self, item):
                super().append(("logged", item))

            def pop(self, *args):
                return ("popped", super().pop(*args))

        xs = Logged()
        for i in range(3):
            xs.append(i)
        self.assertEqual(xs, [("logged", 0), ("logged", 1), ("logged", 2)])
        self.assertEqual(xs.pop(), ("popped", ("logged", 2)))

    def test_append_keeps_objects_alive(self):
        xs = []
        for _ in range(10):
            xs.append(object())
        self.assertEqual(len({id(x) for x in xs}), 10)


if __name__ == "__main__":
    unittest.main()
