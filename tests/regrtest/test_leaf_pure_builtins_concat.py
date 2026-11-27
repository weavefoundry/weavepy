"""Frameless callbacks call round, pow, repr, hex, oct and bin on native
numbers, and add strings; results and errors match the full calls."""

import unittest


class LeafPureBuiltinsConcat(unittest.TestCase):
    def test_numeric_builtins(self):
        xs = [i * 0.37 for i in range(40)] + [2.5, 3.5, -0.5]
        ints = list(range(-5, 40))
        for _ in range(200):
            self.assertEqual(list(map(lambda x: round(x, 1), xs[:5])), [0.0, 0.4, 0.7, 1.1, 1.5])
            self.assertEqual(sorted(xs, key=lambda x: round(x, 1))[:2], [-0.5, 0.0])
            self.assertEqual(list(map(lambda x: round(x), [2.5, 3.5, -0.5])), [2, 4, 0])
            self.assertEqual(min(ints, key=lambda x: pow(x - 25, 2)), 25)
            self.assertEqual(list(map(lambda x: pow(x, -1), [2, 4])), [0.5, 0.25])
            self.assertEqual(list(map(lambda x: hex(x), [255, -1, 0])), ["0xff", "-0x1", "0x0"])
            self.assertEqual(list(map(lambda x: oct(x) + bin(x), [8])), ["0o100b1000"])
            self.assertEqual(list(map(lambda x: repr(x), [1, 1.5, "a'b", None, True])),
                             ["1", "1.5", '"a\'b"', "None", "True"])

    def test_concat(self):
        words = ["b", "a", "c"]
        for _ in range(200):
            self.assertEqual(list(map(lambda s: s + "!", words)), ["b!", "a!", "c!"])
            self.assertEqual(sorted(words, key=lambda s: "z" + s), ["a", "b", "c"])
            self.assertEqual(list(map(lambda s: repr(len(s)) + s, words)), ["1b", "1a", "1c"])
            self.assertEqual(list(map(lambda s: s + "é" + s, ["x"])), ["xéx"])

    def test_errors(self):
        for _ in range(100):
            with self.assertRaises(TypeError):
                list(map(lambda s: s + 1, ["a"]))
            with self.assertRaises(TypeError):
                list(map(lambda x: hex(x), [1.5]))
            with self.assertRaises(ZeroDivisionError):
                list(map(lambda x: pow(x, -1), [0]))


if __name__ == "__main__":
    unittest.main()
