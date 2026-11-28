"""Frameless callbacks call str methods registered as opaque (count,
zfill, title, rjust, partition, index, isupper, ...); results and errors
match the full calls, and other receivers still take them."""

import unittest


class S(str):
    def count(self, x, *a):
        return 99


class LeafOpaqueStrMethods(unittest.TestCase):
    def test_methods(self):
        ws = ["alpha", "Beta", "gamma", "delta", "banana"]
        for _ in range(200):
            self.assertEqual(list(map(lambda s: s.zfill(7), ws)),
                             ["00alpha", "000Beta", "00gamma", "00delta", "0banana"])
            self.assertEqual(sorted(ws, key=lambda w: (w.count("a"), w)),
                             ["Beta", "delta", "alpha", "gamma", "banana"])
            self.assertEqual(list(map(lambda s: s.title(), ["ab cd"])), ["Ab Cd"])
            self.assertEqual(list(map(lambda s: s.rjust(4, "."), ["ab"])), ["..ab"])
            self.assertEqual(list(map(lambda s: s.partition("-"), ["a-b"])), [("a", "-", "b")])
            self.assertEqual(list(map(lambda s: s.isupper(), ["AB", "Ab"])), [True, False])
            self.assertEqual(list(map(lambda s: s.rfind("a"), ws)), [4, 3, 4, 4, 5])

    def test_bytes_methods(self):
        bs = [b"alpha", b"Beta", b"banana"]
        for _ in range(200):
            self.assertEqual(sorted(bs, key=lambda b: b.count(b"a")), [b"Beta", b"alpha", b"banana"])
            self.assertEqual(list(map(lambda b: b.upper().hex(), [b"ab"])), ["4142"])
            self.assertEqual(list(map(lambda b: b.partition(b"n"), [b"banana"])), [(b"ba", b"n", b"ana")])
            with self.assertRaises(ValueError):
                list(map(lambda b: b.index(b"z"), [b"abc"]))
        ba = bytearray(b"xy")
        self.assertEqual(list(map(lambda b: b.upper(), [ba])), [bytearray(b"XY")])

    def test_errors_and_subclasses(self):
        for _ in range(100):
            with self.assertRaises(ValueError):
                list(map(lambda s: s.index("z"), ["abc"]))
            with self.assertRaises(TypeError):
                list(map(lambda s: s.zfill("x"), ["abc"]))
            self.assertEqual(list(map(lambda s: s.count("a"), [S("aaa")])), [99])


if __name__ == "__main__":
    unittest.main()
