"""`all()` and `any()` over generator objects.

Items whose truth is known natively and doesn't decide the answer are
skipped while the generator runs; the first deciding item stops it, and
items with their own `__bool__` are judged by it. The generator must be
left suspended right after the deciding item, and errors must propagate.
"""

import unittest


class Truthy:
    calls = 0

    def __init__(self, value):
        self.value = value

    def __bool__(self):
        Truthy.calls += 1
        return self.value


def numbers(n, stop_at=None):
    for i in range(n):
        if i == stop_at:
            yield 0
        else:
            yield i + 1


class AllAnyGeneratorTest(unittest.TestCase):
    def test_all(self):
        self.assertTrue(all(numbers(1000)))
        g = numbers(1000, stop_at=10)
        self.assertFalse(all(g))
        self.assertEqual(next(g), 12)

    def test_any(self):
        def zeros(n, hit):
            for i in range(n):
                yield 1 if i == hit else 0.0

        self.assertFalse(any(zeros(1000, -1)))
        g = zeros(1000, 5)
        self.assertTrue(any(g))
        self.assertEqual(next(g), 0.0)

    def test_custom_truth(self):
        def items():
            yield 1
            yield Truthy(True)
            yield Truthy(False)
            yield 2

        Truthy.calls = 0
        self.assertFalse(all(items()))
        self.assertEqual(Truthy.calls, 2)

    def test_errors(self):
        def bad():
            yield 1
            raise KeyError("boom")

        with self.assertRaises(KeyError):
            all(bad())


if __name__ == "__main__":
    unittest.main()
