"""One-byte `bytes` objects are shared, and `bytes.join` of a list joins in place.

As in CPython, every one-byte `bytes` result is the one object for that
byte (`bytes([97]) is b"a"`), so splitting into short pieces allocates
none. A shared object must never change: concatenation, `+=`, slicing
and `bytearray` round trips give new values and leave it alone. A
compiled `sep.join(parts)` of a list or tuple reads the parts in place;
a `bytearray` part, a non-bytes part (which raises), and other iterables
must still behave as the full call does.
"""

import unittest


def joins(sep, parts, n=200):
    out = None
    for _ in range(n):
        out = sep.join(parts)
    return out


class OneByteTests(unittest.TestCase):
    def test_identity(self):
        for i in range(256):
            one = bytes([i])
            self.assertIs(one, bytes([i]))
        pieces = b"a b c d".split(b" ")
        self.assertEqual(pieces, [b"a", b"b", b"c", b"d"])
        self.assertIs(pieces[0], bytes([97]))
        self.assertIs(b"x,y".split(b",")[1], bytes([121]))

    def test_shared_objects_stay_unchanged(self):
        a = bytes([97])
        b = a
        b += b"z"
        self.assertEqual(a, b"a")
        self.assertEqual(b, b"az")
        c = bytes([97]) * 3
        self.assertEqual(c, b"aaa")
        self.assertEqual(bytes([97]), b"a")
        ba = bytearray(bytes([98]))
        ba[0] = 99
        self.assertEqual(bytes([98]), b"b")
        self.assertEqual(bytes(ba), b"c")


class BytesJoinTests(unittest.TestCase):
    def test_lists_and_tuples(self):
        self.assertEqual(joins(b" ", [b"abc", b"de"]), b"abc de")
        self.assertEqual(joins(b"", (b"x", b"y", b"z")), b"xyz")
        self.assertEqual(joins(b"-", []), b"")
        self.assertEqual(joins(b"-", [bytearray(b"q"), b"r"]), b"q-r")

    def test_errors_and_other_iterables(self):
        for _ in range(100):
            with self.assertRaises(TypeError):
                b" ".join([b"a", "b"])
            with self.assertRaises(TypeError):
                b" ".join((b"a", 1))
        self.assertEqual(joins(b",", (x for x in [b"1", b"2"]), 1), b"1,2")
        self.assertEqual(joins(b",", iter([b"1", b"2"]), 1), b"1,2")
        self.assertEqual(joins(b",", {b"k": 1}), b"k")


if __name__ == "__main__":
    unittest.main()
