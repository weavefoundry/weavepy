"""`bytes.fromhex` decoded in runs of pairs, and `bytes.join` of
`bytearray` parts read in place.

`fromhex` decodes whole pairs until whitespace or a non-digit; whitespace
is allowed only between pairs, and every other shape reports CPython's
error position. `b"".join` reads `bytes` and `bytearray` parts where they
are; any other part takes the general path and its error.
"""

import unittest


class FromhexTests(unittest.TestCase):
    def test_values(self):
        for cls in (bytes, bytearray):
            self.assertEqual(cls.fromhex(""), b"")
            self.assertEqual(cls.fromhex("0a1B"), b"\x0a\x1b")
            self.assertEqual(cls.fromhex(" ab\tcd\n"), b"\xab\xcd")
            self.assertEqual(cls.fromhex("ab cd ef 01 23"), b"\xab\xcd\xef\x01\x23")
            self.assertEqual(cls.fromhex("\x0bab\x0c"), b"\xab")
            self.assertEqual(cls.fromhex("ab\r\ncd"), b"\xab\xcd")
            self.assertEqual(cls.fromhex("AB" * 1000), b"\xab" * 1000)
            self.assertEqual(cls.fromhex(bytes(range(256)).hex()), bytes(range(256)))

    def test_errors(self):
        odd = "fromhex() arg must contain an even number of hexadecimal digits"
        at = "non-hexadecimal number found in fromhex() arg at position %d"
        cases = (
            ("abc", odd),
            ("a", odd),
            ("a b", at % 1),
            ("zz", at % 0),
            ("ab z", at % 3),
            ("0g", at % 1),
            ("abé", at % 2),
        )
        for text, message in cases:
            for cls in (bytes, bytearray):
                with self.assertRaises(ValueError) as cm:
                    cls.fromhex(text)
                self.assertEqual(str(cm.exception), message)


class JoinTests(unittest.TestCase):
    def test_mixed_parts(self):
        ba = bytearray(b"xy")
        for _ in range(200):
            self.assertEqual(b"-".join([b"a", bytearray(b"b"), b"c"]), b"a-b-c")
            self.assertEqual(b"".join((ba, ba, b"z")), b"xyxyz")
            self.assertEqual(b"ab".join([bytearray()]), b"")
            self.assertEqual(b",".join([]), b"")
        parts = [bytearray(b"ab")] * 2000
        self.assertEqual(len(b"".join(parts)), 4000)
        self.assertIs(type(b"".join([bytearray(b"q")])), bytes)

    def test_other_parts(self):
        self.assertEqual(b"".join([b"a", memoryview(b"m"), bytearray(b"q")]), b"amq")
        with self.assertRaises(TypeError):
            b"".join([b"a", "s"])
        with self.assertRaises(TypeError):
            b"".join([b"a", 5])
        self.assertEqual(bytearray(b",").join([b"a", bytearray(b"b")]), bytearray(b"a,b"))


if __name__ == "__main__":
    unittest.main()
