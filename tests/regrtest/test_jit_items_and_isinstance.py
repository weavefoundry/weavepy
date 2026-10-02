"""Native `dict.items()` loops and `isinstance` against a tuple of classes.

A tier-2 loop `for k, v in d.items()` takes each key and value straight
from the dict, and `isinstance(x, (A, B))` checks a flat tuple of plain
classes without the general call. Each loop here runs long enough to
compile; then the dict or the classes change, and every result must match
the interpreter's.
"""

import unittest

WARM = 3000

TABLE = {str(i): i for i in range(20)}


def total():
    t = 0
    for k, v in TABLE.items():
        t += v
    return t


def keys_len(d):
    n = 0
    for k, v in d.items():
        n += len(k)
    return n


def mutate_during(d):
    for k, v in d.items():
        d[k + "!"] = v


def churn_during(d):
    for k, v in d.items():
        del d[k]
        d[k + "?"] = v


def overwrite_during(d):
    t = 0
    for k, v in d.items():
        d[k] = v + 1
        t += v
    return t


class Base:
    pass


class Sub(Base):
    pass


class Masked:
    @property
    def __class__(self):
        return Base


def kinds(xs, spec):
    n = 0
    for x in xs:
        if isinstance(x, spec):
            n += 1
    return n


class ItemsAndIsinstanceTests(unittest.TestCase):
    def test_items_loop(self):
        for _ in range(WARM):
            r = total()
        self.assertEqual(r, sum(range(20)))
        TABLE["x"] = 1.5
        try:
            self.assertEqual(total(), sum(range(20)) + 1.5)
        finally:
            del TABLE["x"]
        self.assertEqual(total(), sum(range(20)))
        d = {"ab": 1, "c": 2}
        for _ in range(WARM):
            r = keys_len(d)
        self.assertEqual(r, 3)
        self.assertEqual(keys_len({}), 0)

    def test_items_mutation(self):
        for _ in range(WARM):
            overwrite_during({"a": 1, "b": 2})
        d = {"a": 1, "b": 2}
        self.assertEqual(overwrite_during(d), 3)
        self.assertEqual(d, {"a": 2, "b": 3})
        with self.assertRaises(RuntimeError):
            mutate_during({"a": 1, "b": 2})
        with self.assertRaises(RuntimeError):
            churn_during({"a": 1, "b": 2})

    def test_isinstance_tuples(self):
        xs = [1, "s", 2.0, Sub(), Base(), None, Masked()]
        for _ in range(WARM):
            r = kinds(xs, (int, str))
        self.assertEqual(r, 2)
        self.assertEqual(kinds(xs, (Base,)), 3)
        self.assertEqual(kinds(xs, (float, Sub)), 2)
        self.assertEqual(kinds(xs, (bool, type(None))), 1)
        self.assertEqual(kinds(xs, ()), 0)
        self.assertEqual(kinds(xs, (int, (str, float))), 3)
        import numbers

        self.assertEqual(kinds(xs, (numbers.Number, Base)), 5)
        with self.assertRaises(TypeError):
            kinds(xs, (int, 3))


if __name__ == "__main__":
    unittest.main()
