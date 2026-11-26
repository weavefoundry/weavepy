"""random.Random's natively served methods from compiled loops while no
instance attribute shadows `_randbelow` or `getrandbits` (now found by
the names' hashes): seeded sequences equal to CPython's, an instance
attribute that shadows getrandbits taking over (and removed again), and
a subclass overriding _randbelow, each matching CPython."""

import random
import unittest


def draws(n, r):
    return [r.randint(1, 100) for i in range(n)] + [r.randrange(7) for i in range(n)]


class RandomShadowTests(unittest.TestCase):
    def test_seeded(self):
        a = draws(300, random.Random(1234))
        b = draws(300, random.Random(1234))
        self.assertEqual(a, b)
        self.assertEqual(draws(5, random.Random(42)), [82, 15, 4, 95, 36, 1, 1, 1, 5, 0])

    def test_instance_shadow(self):
        r = random.Random(7)
        draws(300, r)
        r.getrandbits = lambda k: 0
        self.assertEqual([r.randrange(10) for i in range(300)], [0] * 300)
        del r.getrandbits
        r.seed(7)
        self.assertEqual(draws(50, r), draws(50, random.Random(7)))

    def test_subclass_override(self):
        class Fixed(random.Random):
            def _randbelow(self, n):
                return n - 1

        r = Fixed(3)
        self.assertEqual([r.randrange(10) for i in range(300)], [9] * 300)


if __name__ == "__main__":
    unittest.main()
