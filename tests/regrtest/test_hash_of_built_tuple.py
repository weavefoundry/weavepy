"""`__hash__` bodies that return `hash((self.a, self.b))`.

Such a body hashes the items without building the tuple once it runs as a
compiled leaf; the result must equal `hash` of the real tuple for every
kind of item (ints, strings, floats, `None`, bools, nested tuples,
instances hashed by identity, and items that need the full path: a NaN,
an instance with its own `__hash__`), with one to three items, and a
module that rebinds `hash` must still get its own function.
"""

import unittest


class Two:
    __slots__ = ("a", "b")

    def __init__(self, a, b):
        self.a = a
        self.b = b

    def __hash__(self):
        return hash((self.a, self.b))

    def __eq__(self, other):
        return self.a == other.a and self.b == other.b


class One:
    def __init__(self, a):
        self.a = a

    def __hash__(self):
        return hash((self.a,))


class Three:
    def __init__(self, a, b, c):
        self.a, self.b, self.c = a, b, c

    def __hash__(self):
        return hash((self.a, self.b, self.c))


class Plain:
    pass


class Custom:
    def __hash__(self):
        return 12345


NAN = float("nan")
ITEMS = [
    0, 1, -1, -2, 2**61 - 1, 2**62, -(2**63), "", "key", "\u20ac" * 3,
    1.5, -0.0, 1e300, None, True, False, (1, "a"), ((1, 2), (3,)),
    Plain(), Custom(), NAN, b"bytes",
]


class HashOfBuiltTupleTests(unittest.TestCase):
    def test_pairs_match_tuple_hash(self):
        for a in ITEMS:
            for b in ITEMS[::3]:
                obj = Two(a, b)
                want = hash((a, b))
                for _ in range(60):
                    self.assertEqual(hash(obj), want, (a, b))

    def test_one_and_three_items(self):
        for a in ITEMS:
            o1, o3 = One(a), Three(a, "x", 7)
            for _ in range(60):
                self.assertEqual(hash(o1), hash((a,)))
                self.assertEqual(hash(o3), hash((a, "x", 7)))

    def test_lookups_use_it(self):
        keys = [Two(i, str(i)) for i in range(64)]
        d = {k: i for i, k in enumerate(keys)}
        for _ in range(100):
            for i in range(64):
                self.assertEqual(d[Two(i, str(i))], i)
        self.assertNotIn(Two(1, "2"), d)

    def test_rebound_hash(self):
        ns = {}
        exec(
            "hash = lambda t: 42\n"
            "class K:\n"
            "    def __init__(self, a, b): self.a, self.b = a, b\n"
            "    def __hash__(self): return hash((self.a, self.b))\n",
            ns,
        )
        k = ns["K"](1, 2)
        for _ in range(300):
            self.assertEqual(hash(k), 42)
        ns["hash"] = lambda t: len(t) + 100
        for _ in range(300):
            self.assertEqual(hash(k), 102)

    def test_unhashable_item_raises(self):
        obj = Two([1], 2)
        for _ in range(60):
            with self.assertRaises(TypeError):
                hash(obj)


if __name__ == "__main__":
    unittest.main()
