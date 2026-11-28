"""Integers past 64 bits: from text, to bytes, in arithmetic, and their methods.

`int(text, base)` of digits that don't fit a machine word, `to_bytes` of
any size, sums, differences, products, shifts and masks of a wide value
with a small one, and methods called on wide values (and on floats,
tuples and bytes) from one call site that sees several kinds of receiver.
Each result is checked against the same value built another way.
"""

import random
import unittest

HEX = "0123456789abcdef"


def by_digits(text, base):
    """The value of `text` in `base`, a digit at a time."""
    v = 0
    for ch in text:
        v = v * base + HEX.index(ch.lower())
    return v


def bytes_of(v, length, little=False):
    out = []
    for _ in range(length):
        out.append(v & 0xFF)
        v >>= 8
    if v:
        raise OverflowError("int too big to convert")
    return bytes(out if little else reversed(out))


class TextTests(unittest.TestCase):
    def test_hex_text_of_every_length(self):
        rng = random.Random(11)
        for n in list(range(1, 70)) + [100, 127, 128, 129, 200, 500]:
            for _ in range(8):
                text = "".join(rng.choice(HEX + "ABCDEF") for _ in range(n))
                want = by_digits(text, 16)
                self.assertEqual(int(text, 16), want, text)
                self.assertEqual(int(text, base=16), want)
                self.assertEqual(int("  " + text + "\n", 16), want)
                self.assertEqual(int("-" + text, 16), -want)
                self.assertEqual(int("+" + text, 16), want)
                self.assertEqual(int("0x" + text, 16), want)
                self.assertEqual(format(want, "x"), text.lower().lstrip("0") or "0")

    def test_boundaries(self):
        cases = {
            "7fffffffffffffff": 2**63 - 1,
            "8000000000000000": 2**63,
            "ffffffffffffffff": 2**64 - 1,
            "10000000000000000": 2**64,
            "0" * 40: 0,
            "0" * 39 + "1": 1,
            "0" * 20 + "f" * 16: 2**64 - 1,
            "f" * 32: 2**128 - 1,
        }
        for text, want in cases.items():
            self.assertEqual(int(text, 16), want, text)
            self.assertIs(type(int(text, 16)), int)
        self.assertEqual(int("-8000000000000000", 16), -(2**63))
        self.assertEqual(int("-8000000000000001", 16), -(2**63) - 1)

    def test_other_bases_and_decimal(self):
        rng = random.Random(12)
        for base in (2, 3, 8, 10, 16, 32, 36):
            digits = "0123456789abcdefghijklmnopqrstuvwxyz"[:base]
            for n in (1, 13, 19, 20, 40, 64, 65, 90, 300):
                text = "".join(rng.choice(digits) for _ in range(n))
                want = 0
                for ch in text:
                    want = want * base + digits.index(ch)
                self.assertEqual(int(text, base), want, (base, text))
                self.assertEqual(int("-" + text, base), -want)
                if base == 10:
                    self.assertEqual(int(text), want)
                    self.assertEqual(int(" +" + text + " "), want)
                    self.assertEqual(str(want), text.lstrip("0") or "0")

    def test_malformed_text_still_raises(self):
        for text in ("", " ", "g", "12g4" * 10, "f" * 20 + "_", "_" + "f" * 20, "f__f" * 8,
                     "--" + "f" * 20, "+-" + "f" * 20, "0x", "f" * 20 + " f", "١" * 3 + "g"):
            with self.assertRaises(ValueError, msg=repr(text)):
                int(text, 16)
        self.assertEqual(int("f_f" * 8, 16), by_digits("ff" * 8, 16))
        self.assertEqual(int("١٢٣" * 8), int("123" * 8))
        with self.assertRaises(ValueError):
            int("9" * 30, 8)


class BytesTests(unittest.TestCase):
    values = [0, 1, 255, 256, 2**63 - 1, 2**63, 2**64 - 1, 2**64, 2**127, 2**128 - 1, 2**128,
              2**200 + 12345, int("12345678123456781234567812345678", 16)]

    def test_to_bytes(self):
        for v in self.values:
            for length in (0, 1, 2, 7, 8, 9, 15, 16, 17, 26, 32, 64):
                for little in (False, True):
                    order = "little" if little else "big"
                    try:
                        want = bytes_of(v, length, little)
                    except OverflowError:
                        for call in (lambda: v.to_bytes(length, order),
                                     lambda: v.to_bytes(length, byteorder=order),
                                     lambda: v.to_bytes(length=length, byteorder=order)):
                            with self.assertRaises(OverflowError):
                                call()
                        continue
                    self.assertEqual(v.to_bytes(length, order), want, (v, length, order))
                    self.assertEqual(v.to_bytes(length, byteorder=order, signed=False), want)
                    self.assertEqual(v.to_bytes(length=length, byteorder=order), want)
                    if not little:
                        self.assertEqual(v.to_bytes(length), want)
                    self.assertEqual(int.from_bytes(want, order), v)
                    if want and not want[-1 if little else 0] & 0x80:
                        self.assertEqual(v.to_bytes(length, order, signed=True), want)

    def test_negative_and_odd_arguments(self):
        for v in (-1, -(2**64), -(2**127) - 5):
            with self.assertRaises(OverflowError):
                v.to_bytes(32)
            b = v.to_bytes(32, "big", signed=True)
            self.assertEqual(int.from_bytes(b, "big", signed=True), v)
        with self.assertRaises(ValueError):
            (2**70).to_bytes(16, "middle")
        with self.assertRaises(ValueError):
            (2**70).to_bytes(-1)
        self.assertEqual(True.to_bytes(2), b"\x00\x01")
        self.assertEqual((0).to_bytes(0), b"")


class ArithmeticTests(unittest.TestCase):
    def test_wide_with_small(self):
        rng = random.Random(13)
        for _ in range(400):
            big = rng.getrandbits(rng.choice((64, 65, 100, 128, 200))) | (1 << 64)
            small = rng.randrange(-(2**40), 2**40) or 7
            total = big + small
            self.assertEqual(total - small, big)
            self.assertEqual(small + big, total)
            self.assertEqual(total - big, small)
            self.assertEqual(big - small + small, big)
            prod = big * small
            self.assertEqual(small * big, prod)
            self.assertEqual(prod // small, big)
            self.assertEqual(prod % small, 0)
            self.assertEqual((-big) + small, -(big - small))
            self.assertEqual(int(str(total)), total)

    def test_results_that_fit_a_word_again(self):
        big = 2**64
        self.assertEqual(big - (2**64 - 5), 5)
        self.assertIs(type(big - 1), int)
        self.assertEqual((big + 5) - big, 5)
        self.assertEqual(big + (-(2**63)), 2**63)
        self.assertEqual((2**70) * 0, 0)
        self.assertEqual((big - 1) + 1, big)
        self.assertEqual(big - 1, 18446744073709551615)

    def test_shifts_and_masks(self):
        rng = random.Random(14)
        for _ in range(300):
            v = rng.getrandbits(128) | (1 << 127)
            text = format(v, "032x")
            for shift in (0, 1, 4, 32, 48, 60, 64, 96, 124, 127, 128, 200):
                self.assertEqual(v >> shift, v // (1 << shift), (v, shift))
            self.assertEqual((v >> 64) & 0xFFFF, int(text[12:16], 16))
            self.assertEqual(v & 0xFFFFFFFFFFFF, int(text[20:], 16))
            self.assertEqual(0xFF & v, int(text[30:], 16))
            self.assertEqual(v & (2**63 - 1), v % (2**63))
            self.assertEqual(v & 0, 0)
            self.assertEqual((-v) & 0xFF, (256 - (v % 256)) % 256)
            self.assertEqual(v & -1, v)
            self.assertEqual((-v) >> 3, -((v + 7) // 8))
            self.assertEqual(v.bit_length(), len(bin(v)) - 2)
            self.assertEqual((-v).bit_length(), v.bit_length())


class ReceiverKindTests(unittest.TestCase):
    """One call site, several kinds of builtin receiver, many times over."""

    def test_bit_length_site(self):
        values = [5, 2**70, True, 0, -(2**90), 255]
        want = [3, 71, 1, 0, 91, 8]
        got = []
        for i in range(6000):
            got.append(values[i % 6].bit_length())
        self.assertEqual(got, want * 1000)

    def test_conjugate_and_real_sites(self):
        values = [3, 2.5, 2**80, 1 + 2j, True]
        want = [3, 2.5, 2**80, 1 - 2j, 1]
        for i in range(5000):
            self.assertEqual(values[i % 5].conjugate(), want[i % 5])

    def test_count_and_index_sites(self):
        values = [(1, 2, 1), [1, 1, 1], b"\x01\x01", bytearray(b"\x01"), range(1, 2), "1a1"]
        needles = [1, 1, b"\x01", b"\x01", 1, "1"]
        want = [2, 3, 2, 1, 1, 2]
        for i in range(6000):
            k = i % 6
            self.assertEqual(values[k].count(needles[k]), want[k], k)
        for i in range(3000):
            k = i % 2
            self.assertEqual(((7, 8), [8, 7])[k].index(8), 1 - k)

    def test_hex_sites(self):
        values = [1.5, b"\xab", bytearray(b"\x01\x02"), 2.0]
        want = [(1.5).hex(), "ab", "0102", (2.0).hex()]
        for i in range(4000):
            self.assertEqual(values[i % 4].hex(), want[i % 4])

    def test_to_bytes_site(self):
        values = [7, 2**100, True, 2**64 - 1]
        for i in range(4000):
            v = values[i % 4]
            self.assertEqual(int.from_bytes(v.to_bytes(16)), int(v))

    def test_methods_of_subclasses_and_shadows(self):
        class MyInt(int):
            def bit_length(self):
                return -1

        class MyTuple(tuple):
            def count(self, x):
                return "mine"

        values = [2**70, MyInt(2**70), 5, MyInt(5)]
        want = [71, -1, 3, -1]
        for i in range(4000):
            self.assertEqual(values[i % 4].bit_length(), want[i % 4])
        seqs = [(1, 1), MyTuple((1, 1)), (1,)]
        want = [2, "mine", 1]
        for i in range(3000):
            self.assertEqual(seqs[i % 3].count(1), want[i % 3])


if __name__ == "__main__":
    unittest.main()
