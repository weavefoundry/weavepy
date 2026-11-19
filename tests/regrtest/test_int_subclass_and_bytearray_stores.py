"""`int()` of an `int` subclass's instance, and `bytearray` item stores.

`int(x)` of an `int` subclass's instance whose class keeps `int`'s own
`__int__` (`re`'s named opcode constants) answers its value as an exact
`int` in place; a class with its own `__int__` keeps it. `buf[i] = n` of
a `bytearray` by an int index stores in place, and out-of-range indexes
and values raise as before. A compiled pattern's parse tree iterates its
items directly; patterns compile and match as before.
"""

import re
import unittest

N = 3000


class Plain(int):
    pass


class Own(int):
    def __int__(self):
        return 42


class Big(int):
    pass


class IntSubclassTests(unittest.TestCase):
    def test_values_are_exact(self):
        p, o, b = Plain(7), Own(7), Big(2**70)
        for _ in range(N):
            r = (int(p), int(o), int(b), int(True))
        self.assertEqual(r, (7, 42, 2**70, 1))
        self.assertEqual([type(x) for x in r], [int, int, int, int])

    def test_class_gains_int(self):
        class Late(int):
            pass

        x = Late(3)
        out = []
        for i in range(N):
            if i == N // 2:
                Late.__int__ = lambda self: 99
            out.append(int(x))
        self.assertEqual((out[0], out[-1]), (3, 99))


class ByteArrayStoreTests(unittest.TestCase):
    def test_stores(self):
        b = bytearray(8)
        for i in range(N):
            b[i % 8] = i % 256
            b[-1] = True
        self.assertEqual(b[-1], 1)
        self.assertEqual(b[0], (N - 8) % 256)

    def test_errors(self):
        b = bytearray(2)
        for _ in range(100):
            with self.assertRaises(ValueError):
                b[0] = 256
            with self.assertRaises(ValueError):
                b[0] = -1
            with self.assertRaises(IndexError):
                b[2] = 1
            with self.assertRaises(TypeError):
                b[0] = "a"
        self.assertEqual(b, bytearray(2))

    def test_view_sees_stores(self):
        b = bytearray(4)
        m = memoryview(b)
        for i in range(N):
            b[i % 4] = 7
        self.assertEqual(bytes(m), b"\x07\x07\x07\x07")
        m.release()


class RegexCompileTests(unittest.TestCase):
    def test_patterns(self):
        for i in range(200):
            p = re.compile(r"(?P<a>\d+)\s*(?P<b>[a-z]+)%d|[^\W\d]\w*x%d" % (i, i))
        self.assertEqual(p.groupindex["b"], 2)
        self.assertEqual(p.match("12 ab199").groups(), ("12", "ab"))
        self.assertEqual(p.match("zz9x199").group(), "zz9x199")
        c = re.compile(r"(?i)[a-fA-F0-9_]{2,4}(?=-)|[^xyz]+?")
        self.assertEqual(c.findall("Ab_-q"), ["Ab_", "-", "q"])


if __name__ == "__main__":
    unittest.main()
