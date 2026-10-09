"""Builtin methods called in place: `set.add`, and methods of an `int`.

`s.add(x)` inserts in place unless a stored key could only be compared
by a user `__eq__`, which must then run as the full call runs it. A
function whose body calls a builtin method on an `int` argument
(`x.bit_length()`) evaluates without a frame, and its result must match
the ordinary call's.
"""

import unittest


class Collide:
    """Hashes like the int 7 and counts its comparisons."""

    calls = 0

    def __hash__(self):
        return hash(7)

    def __eq__(self, other):
        Collide.calls += 1
        return other == 7


def bits(x):
    return x.bit_length()


def as_bytes(x):
    return x.to_bytes(2, "big")


class BuiltinMethodLeafTest(unittest.TestCase):
    def test_set_add(self):
        s = set()
        for i in range(3000):
            s.add(i & 63)
            s.add("k%d" % (i & 7))
        self.assertEqual(len(s), 64 + 8)

    def test_set_add_user_eq(self):
        s = set()
        c = Collide()
        s.add(c)
        for i in range(3000):
            s.add(i & 15)
        self.assertGreater(Collide.calls, 0)
        self.assertEqual(len(s), 16)
        self.assertIn(c, s)

    def test_int_methods(self):
        total = 0
        for i in range(3000):
            total += bits(i)
        self.assertEqual(total, sum(i.bit_length() for i in range(3000)))
        for i in range(3000):
            self.assertEqual(as_bytes(i), i.to_bytes(2, "big"))
        self.assertEqual(bits(-5), 3)
        self.assertEqual(bits(True), 1)
        with self.assertRaises(OverflowError):
            as_bytes(1 << 20)


if __name__ == "__main__":
    unittest.main()
