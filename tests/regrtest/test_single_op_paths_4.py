"""`int(text, base)` and `"".join(reversed(...))` in place.

`int` parses plain digits of a base without the full constructor, and
`str.join` drains a `reversed` snapshot at once. These loops check edge
cases against the full paths' results and errors.
"""

import unittest

N = 2000


class IntBaseTests(unittest.TestCase):
    CASES = [("ff", 16), ("FF", 16), (" -7f\n", 16), ("+10", 2), ("0xff", 16),
             ("0b11", 2), ("1_0", 10), ("z", 36), ("Z", 36), ("12", 3), ("", 16),
             ("-", 16), ("1 2", 10), ("8", 8), ("7fffffffffffffff", 16),
             ("8000000000000000", 16), ("-8000000000000000", 16), ("٣", 10),
             ("10", 0)]
    EXPECT = [255, 255, -127, 2, 255, 3, 10, 35, 35, 5, "VE", "VE", "VE", "VE",
              0x7FFFFFFFFFFFFFFF, 0x8000000000000000, -0x8000000000000000, 3, 10]

    def test_cases(self):
        for _ in range(N):
            out = []
            for text, base in self.CASES:
                try:
                    out.append(int(text, base))
                except ValueError:
                    out.append("VE")
        self.assertEqual(out, self.EXPECT)


class ReversedJoinTests(unittest.TestCase):
    def test_join(self):
        for _ in range(N):
            r = reversed("héllo")
            out = ("".join(r), list(r), "".join(reversed("")),
                   "-".join(reversed(["a", "b"])), "".join(reversed("x")))
            partial = reversed("abc")
            next(partial)
            rest = "".join(partial)
        self.assertEqual(out, ("olléh", [], "", "b-a", "x"))
        self.assertEqual(rest, "ba")

    def test_errors(self):
        for _ in range(N // 10):
            with self.assertRaises(TypeError) as cm:
                "".join(reversed([1, "a"]))
        self.assertEqual(str(cm.exception), "sequence item 1: expected str instance, int found")
        with self.assertRaises(TypeError) as cm:
            "".join(iter(["a", 2]))
        self.assertEqual(str(cm.exception), "sequence item 1: expected str instance, int found")


class MinMaxKeyTests(unittest.TestCase):
    def test_leaf_keys(self):
        words = "the quick brown fox jumps over the lazy dog".split()
        for _ in range(N):
            r = (min(words, key=len), max(words, key=len), min([[1, 2], [3]], key=len),
                 max(words, key=str.upper), min([3, 1, 2]), max([1.5, 2, 2.0]),
                 min(words, key=lambda w: -len(w)), max([2, 1, 2], key=abs))
        self.assertEqual(r, ("the", "quick", [3], "the", 1, 2, "quick", 2))

        class Sized:
            def __len__(self):
                return 5

        self.assertEqual(min([Sized(), "ab"], key=len), "ab")
        with self.assertRaises(TypeError):
            min([1], key=len)


class FormatTests(unittest.TestCase):
    VALUES = [0.0, -0.0, 1.0, 9.9995, 9.99949, 0.00012345, 1e-05, 123456.0, 1234567.0, 0.1, 2.5, 1e+16, 123.456, -0.000999, 99999.5, 5e-324]
    SPECS = ['g', '.1g', '.3g', '.10g', 'G', '#g', '.4', '+.3g', '10.3g', '.0g']
    EXPECT = [['0', '0', '0', '0', '0', '0.00000', '0.0', '+0', '         0', '0'], ['-0', '-0', '-0', '-0', '-0', '-0.00000', '-0.0', '-0', '        -0', '-0'], ['1', '1', '1', '1', '1', '1.00000', '1.0', '+1', '         1', '1'], ['9.9995', '1e+01', '10', '9.9995', '9.9995', '9.99950', '9.999', '+10', '        10', '1e+01'], ['9.99949', '1e+01', '10', '9.99949', '9.99949', '9.99949', '9.999', '+10', '        10', '1e+01'], ['0.00012345', '0.0001', '0.000123', '0.00012345', '0.00012345', '0.000123450', '0.0001234', '+0.000123', '  0.000123', '0.0001'], ['1e-05', '1e-05', '1e-05', '1e-05', '1E-05', '1.00000e-05', '1e-05', '+1e-05', '     1e-05', '1e-05'], ['123456', '1e+05', '1.23e+05', '123456', '123456', '123456.', '1.235e+05', '+1.23e+05', '  1.23e+05', '1e+05'], ['1.23457e+06', '1e+06', '1.23e+06', '1234567', '1.23457E+06', '1.23457e+06', '1.235e+06', '+1.23e+06', '  1.23e+06', '1e+06'], ['0.1', '0.1', '0.1', '0.1', '0.1', '0.100000', '0.1', '+0.1', '       0.1', '0.1'], ['2.5', '2', '2.5', '2.5', '2.5', '2.50000', '2.5', '+2.5', '       2.5', '2'], ['1e+16', '1e+16', '1e+16', '1e+16', '1E+16', '1.00000e+16', '1e+16', '+1e+16', '     1e+16', '1e+16'], ['123.456', '1e+02', '123', '123.456', '123.456', '123.456', '123.5', '+123', '       123', '1e+02'], ['-0.000999', '-0.001', '-0.000999', '-0.000999', '-0.000999', '-0.000999000', '-0.000999', '-0.000999', ' -0.000999', '-0.001'], ['99999.5', '1e+05', '1e+05', '99999.5', '99999.5', '99999.5', '1e+05', '+1e+05', '     1e+05', '1e+05'], ['4.94066e-324', '5e-324', '4.94e-324', '4.940656458e-324', '4.94066E-324', '4.94066e-324', '4.941e-324', '+4.94e-324', ' 4.94e-324', '5e-324']]

    def test_general_formats(self):
        for _ in range(N // 20):
            out = [[format(v, sp) for sp in self.SPECS] for v in self.VALUES]
        self.assertEqual(out, self.EXPECT)
        self.assertEqual(["%g" % v for v in self.VALUES], [format(v, "g") for v in self.VALUES])

    def test_format_builtin(self):
        class F:
            def __format__(self, spec):
                return "F" + spec

        for _ in range(N):
            r = (format(1.5, ".3g"), format(10), format("ab", ">4"), format(F(), "x"),
                 format(True), format(2.0), format(3, "b"))
        self.assertEqual(r, ("1.5", "10", "  ab", "Fx", "True", "2.0", "11"))
        for bad, name in ((5, "int"), (None, "None")):
            with self.assertRaises(TypeError) as cm:
                format(1.5, bad)
            self.assertEqual(str(cm.exception), "format() argument 2 must be str, not " + name)


class NativeShortcutTests(unittest.TestCase):
    def test_fsum_and_dist(self):
        import math
        for _ in range(N):
            r = (math.fsum([0.1] * 10), math.fsum((1, 2**62, -2**62, 0.5)),
                 math.fsum([True, 1.5]), math.dist((1, 2), (4, 6)),
                 math.fsum(x / 10 for x in range(3)))
        self.assertEqual(r, (1.0, 1.5, 2.5, 5.0, 0.30000000000000004))
        with self.assertRaises(TypeError) as cm:
            math.fsum([1, "a"])
        self.assertEqual(str(cm.exception), "must be real number, not str")
        with self.assertRaises(OverflowError):
            math.fsum([1e308, 1e308])

    def test_tuple_of_list(self):
        for _ in range(N):
            r = (tuple([1, "a"]), tuple([]), tuple((2,)))
        self.assertEqual(r, ((1, "a"), (), (2,)))
        self.assertIs(tuple([]), ())

    def test_date_fromisoformat(self):
        import datetime

        class D(datetime.date):
            pass

        for _ in range(N):
            r = (datetime.date.fromisoformat("2024-01-02"),
                 datetime.date.fromisoformat("20240102"),
                 datetime.date.fromisoformat("2024-02-29"),
                 datetime.date.fromisoformat("9999-12-31"),
                 datetime.date.fromisoformat("2024-W01-1"),
                 type(D.fromisoformat("2024-05-06")))
        self.assertEqual(r[:5], (datetime.date(2024, 1, 2), datetime.date(2024, 1, 2),
                                 datetime.date(2024, 2, 29), datetime.date(9999, 12, 31),
                                 datetime.date(2024, 1, 1)))
        self.assertIs(r[5], D)
        for bad in ("2024-02-30", "2023-02-29", "2024-1-02", "0000-01-01", "2024-13-01", "2024-01"):
            with self.assertRaises(ValueError):
                datetime.date.fromisoformat(bad)
        with self.assertRaises(TypeError):
            datetime.date.fromisoformat(20240102)

    def test_maketrans(self):
        for _ in range(N):
            r = (str.maketrans("ab", "cd"), str.maketrans({"a": "b"}),
                 bytes.maketrans(b"a", b"b")[97], "abc".translate(str.maketrans("ab", "xy", "c")))
        self.assertEqual(r, ({97: 99, 98: 100}, {97: "b"}, 98, "xy"))
        with self.assertRaises(ValueError):
            str.maketrans("ab", "c")


class ReversedSumTests(unittest.TestCase):
    def test_sum_reversed(self):
        for _ in range(N):
            r = reversed([1, 2, 3])
            a = sum(r)
            b = list(r)
            c = sum(reversed([1.5, 2, 1e16, -1e16]))
            d = sum(reversed([2**62, 2**62, 2**62]))
            e = sum(reversed([]), 5)
            r2 = reversed([1, 2, 3])
            next(r2)
            f = sum(r2)
        self.assertEqual((a, b, c, d, e, f), (6, [], 3.5, 3 * 2**62, 5, 3))


if __name__ == "__main__":
    unittest.main()
