"""bytes and bytearray methods from compiled loops (leaf halves over
plain operands): results and types for bytes and bytearray receivers,
start/end arguments, None separators, errors such as index misses and
bad fill bytes, and memoryview and array operands (the full path), each
matching CPython."""

import array
import unittest


def ops(n, b):
    out = None
    for i in range(n):
        out = (
            b.upper(), b.lower(), b.strip(), b.lstrip(b" h"), b.rstrip(),
            b.count(b"l"), b.count(b"l", 3), b.rfind(b"o"), b.index(b"e"),
            b.rindex(b"l", 0, 5), b.partition(b" "), b.rpartition(b"o"),
            b.removeprefix(b" h"), b.removesuffix(b"d "), b.center(16, b"*"),
            b.ljust(15), b.rjust(15, b"-"), b.zfill(14), b.capitalize(),
            b.title(), b.swapcase(), b.isalnum(), b.isalpha(), b.isdigit(),
            b.isspace(), b.islower(), b.isupper(), b.split(), b.split(b"l", 1),
            b.rsplit(None, 1), b.replace(b"l", b"L"), b.replace(b"l", b"", 2),
            b.splitlines(), b.expandtabs(4),
        )
    return out


class BytesLeafTests(unittest.TestCase):
    def test_bytes_and_bytearray(self):
        for n in (1, 300):
            r = ops(n, b" hello world ")
            self.assertEqual(r[0], b" HELLO WORLD ")
            self.assertEqual(r[5:8], (3, 3, 8))
            self.assertEqual(r[27], [b"hello", b"world"])
            self.assertEqual(r[28], [b" he", b"lo world "])
            self.assertEqual(r[30], b" heLLo worLd ")
            self.assertEqual(r[31], b" heo world ")
            a = ops(n, bytearray(b" hello world "))
            self.assertEqual([type(x).__name__ for x in a[:5]], ["bytearray"] * 5)
            self.assertEqual(a[27], [bytearray(b"hello"), bytearray(b"world")])
            self.assertEqual(list(map(bytes, [x for x in a if isinstance(x, bytearray)])),
                             [x for x in r if isinstance(x, bytes)])

    def test_errors_and_other_operands(self):
        b = b"abc"
        for i in range(300):
            with self.assertRaises(ValueError):
                b.index(b"z")
            with self.assertRaises(TypeError):
                b.center(5, b"ab")
            with self.assertRaises(TypeError):
                b.split("b")
        self.assertEqual(b.split(memoryview(b"b")), [b"a", b"c"])
        self.assertEqual(b.replace(array.array("b", b"b"), b"X"), b"aXc")
        self.assertEqual(b"a\tb".expandtabs(tabsize=2), b"a b")
        self.assertEqual(b"a,b".split(sep=b","), [b"a", b"b"])


if __name__ == "__main__":
    unittest.main()
