"""`return hash(self.a)` in a frameless `__hash__` hashes the attribute
directly when it hashes natively; every value and every fallback matches
the builtin."""

import unittest


class One:
    def __init__(self, a):
        self.a = a

    def __eq__(self, o):
        return isinstance(o, One) and self.a == o.a

    def __hash__(self):
        return hash(self.a)


class Plain:
    pass


class LeafHashSingle(unittest.TestCase):
    def test_values(self):
        p = Plain()
        vals = [0, -1, 7, 2**61 - 1, 2**62, True, None, 1.5, -0.0, "s", "", b"b",
                (1, "x"), p, One(3), frozenset({1})]
        for _ in range(200):
            for v in vals:
                self.assertEqual(hash(One(v)), hash(v))
        self.assertEqual(len({One(i % 7) for i in range(200)}), 7)

    def test_nan_and_unhashable(self):
        nan = float("nan")
        for _ in range(200):
            self.assertEqual(hash(One(nan)), hash(nan))
            with self.assertRaises(TypeError):
                hash(One([1]))

    def test_shadowed_hash(self):
        ns = {}
        exec(
            "class S:\n"
            "    def __init__(self, a): self.a = a\n"
            "    def __hash__(self): return hash(self.a)\n",
            ns,
        )
        S = ns["S"]
        for _ in range(200):
            self.assertEqual(hash(S(5)), hash(5))
        ns["hash"] = lambda x: 4242
        for _ in range(200):
            self.assertEqual(hash(S(5)), 4242)


if __name__ == "__main__":
    unittest.main()
