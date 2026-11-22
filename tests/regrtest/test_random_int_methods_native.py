"""`random.Random`'s `randint`, `randrange`, `choice` and `shuffle` run
natively.

For the `Random` class itself, with plain int arguments, each draws exactly
as `_randbelow_with_getrandbits` does (the same numbers from the same
state), so seeded streams match CPython's. A subclass, an instance that
overrides `_randbelow` or `getrandbits`, other arguments, every error, and
any call a trace or profile hook could see take the Python methods.
"""

import random
import unittest


def randbelow(r, n):
    k = n.bit_length()
    x = r.getrandbits(k)
    while x >= n:
        x = r.getrandbits(k)
    return x


class RandomIntMethodsTests(unittest.TestCase):
    def test_streams_match_the_algorithm(self):
        for seed in (0, 1, 12345, 2**40 + 7):
            a, b = random.Random(seed), random.Random(seed)
            for _ in range(500):
                self.assertEqual(a.randint(1, 6), 1 + randbelow(b, 6))
                self.assertEqual(a.randint(-5, 5), -5 + randbelow(b, 11))
                self.assertEqual(a.randint(0, 2**40), randbelow(b, 2**40 + 1))
                self.assertEqual(a.randint(0, 2**62), randbelow(b, 2**62 + 1))
                self.assertEqual(a.randrange(10), randbelow(b, 10))
                self.assertEqual(a.randrange(3, 30), 3 + randbelow(b, 27))
                self.assertEqual(a.randrange(3, 30, 1), 3 + randbelow(b, 27))
                xs = ["p", "q", "r"]
                self.assertEqual(a.choice(xs), xs[randbelow(b, 3)])
                self.assertEqual(a.choice("abcde"), "abcde"[randbelow(b, 5)])
                self.assertEqual(a.choice((7, 8)), (7, 8)[randbelow(b, 2)])
                self.assertEqual(a.random(), b.random())
            self.assertEqual(a.getstate(), b.getstate())

    def test_shuffle_matches_the_algorithm(self):
        for seed in (0, 7, 99):
            a, b = random.Random(seed), random.Random(seed)
            for n in (0, 1, 2, 10, 100):
                xs = list(range(n))
                ys = list(range(n))
                a.shuffle(xs)
                for i in reversed(range(1, n)):
                    j = randbelow(b, i + 1)
                    ys[i], ys[j] = ys[j], ys[i]
                self.assertEqual(xs, ys)
            self.assertEqual(a.getstate(), b.getstate())
        words = ["a", "b", "c"]
        random.shuffle(words)
        self.assertEqual(sorted(words), ["a", "b", "c"])
        t = random.Random(1)
        arr = bytearray(b"abc")
        t.shuffle(arr)
        self.assertEqual(sorted(arr), [97, 98, 99])
        with self.assertRaises(TypeError):
            t.shuffle((1, 2))

    def test_uniform_matches(self):
        for seed in (0, 3):
            a, b = random.Random(seed), random.Random(seed)
            for _ in range(300):
                self.assertEqual(a.uniform(1.5, 9.25), 1.5 + (9.25 - 1.5) * b.random())
                self.assertEqual(a.uniform(-3, 40), -3 + (40 - -3) * b.random())
                self.assertEqual(a.uniform(10, 2), 10 + (2 - 10) * b.random())
        g = random.Random(1)
        g.random = [0.5].pop
        self.assertEqual(g.uniform(0, 10), 5.0)
        self.assertEqual(random.Random(2).uniform(2**80, 2**80), float(2**80))

    def test_other_shapes(self):
        r = random.Random(3)
        self.assertEqual(r.randint(True, 3) in (1, 2, 3), True)
        self.assertIn(r.choice("héllo"), "héllo")
        self.assertIn(r.randrange(0, 10, 2), range(0, 10, 2))
        self.assertIn(r.randint(2**70, 2**70 + 1), (2**70, 2**70 + 1))
        with self.assertRaises(ValueError):
            r.randint(5, 1)
        with self.assertRaises(ValueError):
            r.randrange(0)
        with self.assertRaises(ValueError):
            r.randrange(5, 5)
        with self.assertRaises(IndexError):
            r.choice([])
        with self.assertRaises(TypeError):
            r.randint(1.5, 2)
        self.assertIn(random.randint(1, 3), (1, 2, 3))
        self.assertIn(random.choice([4, 5]), (4, 5))
        self.assertIn(random.SystemRandom().randint(1, 3), (1, 2, 3))

    def test_overrides(self):
        class Fixed(random.Random):
            def _randbelow(self, n):
                return 0

        f = Fixed(1)
        self.assertEqual([f.randint(5, 9) for _ in range(5)], [5] * 5)
        self.assertEqual(f.choice("xyz"), "x")
        g = random.Random(1)
        g._randbelow = lambda n: n - 1
        self.assertEqual(g.randint(5, 9), 9)
        h = random.Random(1)
        h.getrandbits = lambda k: 0
        self.assertEqual(h.randrange(10), 0)


class TracedTests(unittest.TestCase):
    def test_trace_sees_the_python_methods(self):
        import sys

        seen = []

        def tracer(frame, event, arg):
            if event == "call":
                seen.append(frame.f_code.co_name)
            return None

        r = random.Random(5)
        sys.settrace(tracer)
        try:
            r.randint(1, 4)
            r.choice([1, 2])
            r.shuffle([1, 2, 3])
        finally:
            sys.settrace(None)
        self.assertIn("randint", seen)
        self.assertIn("choice", seen)
        self.assertIn("shuffle", seen)


if __name__ == "__main__":
    unittest.main()
