"""Frameless callbacks call int and float methods that read their receiver
(as_integer_ratio, conjugate, is_integer, bit_count, hex); results, errors,
and the try/except functions that use them match CPython."""

import fractions
import statistics
import unittest


def exact_ratio(x):
    try:
        return x.as_integer_ratio()
    except AttributeError:
        pass
    return (x, None)


class LeafNumberMethods(unittest.TestCase):
    def test_methods(self):
        xs = list(range(-5, 30)) + [2 ** 70]
        for _ in range(200):
            self.assertEqual(list(map(exact_ratio, [3, 0.5, -2.25, "s"])),
                             [(3, 1), (1, 2), (-9, 4), ("s", None)])
            self.assertEqual(max(xs, key=lambda x: x.bit_count()), 15)
            self.assertEqual(list(map(lambda x: x.as_integer_ratio()[0], xs))[-1], 2 ** 70)
            self.assertEqual(list(map(lambda f: f.hex(), [1.0, 0.5])), ["0x1.0000000000000p+0", "0x1.0000000000000p-1"])
            self.assertEqual(list(map(lambda f: f.is_integer(), [1.0, 1.5])), [True, False])
            self.assertEqual(list(map(lambda i: i.conjugate() + i.is_integer(), [4])), [5])

    def test_errors(self):
        for _ in range(100):
            with self.assertRaises(OverflowError):
                list(map(lambda f: f.as_integer_ratio(), [float("inf")]))
            with self.assertRaises(ValueError):
                list(map(lambda f: f.as_integer_ratio(), [float("nan")]))

    def test_statistics(self):
        data = list(range(20))
        for _ in range(100):
            self.assertEqual(statistics.mean(data), 9.5)
            self.assertEqual(statistics.mean([1, 2.5]), 1.75)
            self.assertEqual(statistics.mean([fractions.Fraction(1, 3), 1]), fractions.Fraction(2, 3))


if __name__ == "__main__":
    unittest.main()
