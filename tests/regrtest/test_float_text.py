"""Float text and direct JSON number output preserve Python spellings."""

import json
import math
import struct
import unittest


class FloatTextTests(unittest.TestCase):
    def test_native_scalar_strings(self):
        for value, expected in (
            (None, "None"), (True, "True"), (False, "False"),
            (0, "0"), (-(2 ** 63), "-9223372036854775808"),
            (2 ** 63 - 1, "9223372036854775807"),
            (2 ** 63, "9223372036854775808"),
            (0j, "0j"), (complex(-0.0, -0.0), "(-0-0j)"),
            (complex(1.7976931348623157e308, 5e-324),
             "(1.7976931348623157e+308+5e-324j)"),
            (complex(math.nan, -math.inf), "(nan-infj)"),
        ):
            self.assertEqual(str(value), expected)
            self.assertEqual(repr(value), expected)

    def test_scalar_subclass_overrides(self):
        for base in (int, float, complex):
            class Number(base):
                def __str__(self):
                    return "custom str"

                def __repr__(self):
                    return "custom repr"

            value = Number(3)
            self.assertEqual(str(value), "custom str")
            self.assertEqual(repr(value), "custom repr")

    def test_notation_boundaries(self):
        cases = [
            (0.0, "0.0"), (-0.0, "-0.0"),
            (1e-5, "1e-05"), (1e-4, "0.0001"),
            (9.999999999999999e-5, "9.999999999999999e-05"),
            (1e15, "1000000000000000.0"), (1e16, "1e+16"),
            (1.2345678901234567, "1.2345678901234567"),
            (5e-324, "5e-324"),
            (2.2250738585072014e-308, "2.2250738585072014e-308"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
        ]
        for value, expected in cases:
            self.assertEqual(repr(value), expected)
            self.assertEqual(str(value), expected)
            self.assertEqual(json.dumps(value), expected)
            self.assertEqual(struct.pack(">d", float(expected)), struct.pack(">d", value))

    def test_shortest_decimal_ties_round_to_even(self):
        for bits, expected in (
            (0x43184D2117000795, "1710050989048293.2"),
            (0xC2705A9CE42D6280, "-1123835331286.1562"),
            (0x42EDDA6F6B94BC64, "262592075179491.12"),
        ):
            value = struct.unpack(">d", struct.pack(">Q", bits))[0]
            self.assertEqual(repr(value), expected)
            self.assertEqual(json.dumps([value]), "[" + expected + "]")
            self.assertEqual(struct.pack(">d", float(expected)), struct.pack(">Q", bits))

    def test_wide_json_buffers_and_number_subclasses(self):
        class Int(int):
            def __repr__(self):
                raise AssertionError("JSON must use the integer value")

        class Float(float):
            def __repr__(self):
                raise AssertionError("JSON must use the floating-point value")

        values = [0, -(2 ** 63), 2 ** 63 - 1, 2 ** 100, Int(42),
                  -0.0, 1.2345678901234567, Float(1e-5), True, False]
        for indent in (None, "", "  ", "\ud800"):
            for separators in ((", ", ": "), ("\udfff", "\ud800")):
                for ensure_ascii in (True, False):
                    encoder = json.JSONEncoder(indent=indent, separators=separators,
                                               ensure_ascii=ensure_ascii)
                    for value in (values, ["\ud800", values], {"\udfff": values}):
                        self.assertEqual(encoder.encode(value),
                                         "".join(encoder.iterencode(value, _one_shot=False)))

    def test_nonfinite(self):
        for value, expected in ((math.inf, "Infinity"), (-math.inf, "-Infinity"),
                                (math.nan, "NaN"), (-math.nan, "NaN")):
            self.assertEqual(json.dumps([value]), "[" + expected + "]")
            with self.assertRaises(ValueError):
                json.dumps([value], allow_nan=False)
        self.assertEqual(repr(complex(-0.0, -0.0)), "(-0-0j)")
        self.assertEqual(repr(complex(1e16, -1e-5)), "(1e+16-1e-05j)")


if __name__ == "__main__":
    unittest.main()
