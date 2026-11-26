"""hex(), oct() and bin() from compiled loops (leaf halves for exact ints,
big ints and bools): small, negative, zero, 64-bit edge and big values,
bools, int subclasses and __index__ objects (the full path), and type
errors, each matching CPython."""

import unittest


class Idx:
    def __index__(self):
        return 10


class MyInt(int):
    pass


def radix(n, x):
    out = None
    for i in range(n):
        out = (hex(x), oct(x), bin(x))
    return out


class RadixTests(unittest.TestCase):
    def test_values(self):
        for n in (1, 300):
            self.assertEqual(radix(n, 255), ("0xff", "0o377", "0b11111111"))
            self.assertEqual(radix(n, 0), ("0x0", "0o0", "0b0"))
            self.assertEqual(radix(n, -5), ("-0x5", "-0o5", "-0b101"))
            self.assertEqual(radix(n, True), ("0x1", "0o1", "0b1"))
            m = -(2**63)
            self.assertEqual(radix(n, m)[0], "-0x8000000000000000")
            self.assertEqual(radix(n, 2**64 + 1)[0], "0x10000000000000001")
            self.assertEqual(radix(n, -(2**70))[2], "-0b1" + "0" * 70)
            self.assertEqual(radix(n, Idx()), ("0xa", "0o12", "0b1010"))
            self.assertEqual(radix(n, MyInt(9)), ("0x9", "0o11", "0b1001"))

    def test_errors(self):
        for bad in (1.5, "1", None):
            with self.assertRaises(TypeError):
                radix(300, bad)


if __name__ == "__main__":
    unittest.main()
