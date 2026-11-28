"""A loop over `filter(None, gen)` steps the generator directly; the
items kept, an item whose `__bool__` runs code (kept, dropped or
raising), the generator's state afterwards and the adapter's own match
the general filter."""

import unittest


def gen(xs):
    for x in xs:
        yield x


class Flag:
    def __init__(self, v):
        self.v = v

    def __bool__(self):
        # Not a frameless leaf: it calls a Python function.
        return bool(_identity(self.v))


def _identity(x):
    return x


class Plain:
    pass


class Boom:
    def __bool__(self):
        raise ValueError("boom")


ITEMS = [0, 1, "", "a", None, 2.5, 0.0, [], [0], (), (1,), {}, {1: 2}, False, True]
EXPECT = [x for x in ITEMS if x]


class FilterGeneratorStep(unittest.TestCase):
    def test_scalars_and_containers(self):
        for _ in range(500):
            out = []
            for x in filter(None, gen(ITEMS)):
                out.append(x)
            self.assertEqual(out, EXPECT)
            out = []
            for x in filter(bool, gen(ITEMS)):
                out.append(x)
            self.assertEqual(out, EXPECT)

    def test_python_bool_items(self):
        for _ in range(300):
            items = [Flag(0), 1, Flag(1), 0, Flag(2), Plain(), 3]
            out = []
            for x in filter(None, gen(items)):
                out.append(x)
            self.assertEqual(out, [items[1], items[2], items[4], items[5], 6 // 2])

    def test_raising_bool(self):
        for _ in range(200):
            g = gen([1, 0, Boom(), 5])
            out = []
            with self.assertRaises(ValueError):
                for x in filter(None, g):
                    out.append(x)
            self.assertEqual(out, [1])
            # The generator went on past the raising item, as CPython's.
            self.assertEqual(list(g), [5])

    def test_break_and_resume(self):
        for _ in range(300):
            g = gen(range(10))
            f = filter(None, g)
            out = []
            for x in f:
                out.append(x)
                if x == 4:
                    break
            self.assertEqual(out, [1, 2, 3, 4])
            self.assertEqual(next(g), 5)
            self.assertEqual(list(f), [6, 7, 8, 9])
            self.assertEqual(list(f), [])

    def test_resume_after_put_back(self):
        for _ in range(200):
            g = gen([0, Flag(1), 0, 2, Flag(0), 3])
            f = filter(None, g)
            out = []
            for x in f:
                out.append(x if not isinstance(x, Flag) else "flag")
                if x == 2:
                    break
            self.assertEqual(out, ["flag", 2])
            self.assertEqual(list(f), [3])

    def test_generator_return_and_empty(self):
        def g2():
            yield 0
            yield 7
            return 99

        for _ in range(300):
            self.assertEqual([x for x in filter(None, g2())], [7])
            self.assertEqual([x for x in filter(None, gen([]))], [])
            self.assertEqual([x for x in filter(None, gen([0, None, ""]))], [])

    def test_generator_raises(self):
        def bad():
            yield 1
            yield 0
            raise KeyError("k")

        for _ in range(200):
            out = []
            with self.assertRaises(KeyError):
                for x in filter(None, bad()):
                    out.append(x)
            self.assertEqual(out, [1])

    def test_sum_of_kept(self):
        for _ in range(300):
            t = 0
            for x in filter(None, (i % 3 for i in range(30))):
                t += x
            self.assertEqual(t, 30)


if __name__ == "__main__":
    unittest.main()
