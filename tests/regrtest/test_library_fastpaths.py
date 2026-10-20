"""Native shortcuts behind library-heavy code keep their exact answers.

- `getattr(obj, dunder)` for a builtin method a plain instance inherits
  (`copy` fetches `__reduce_ex__` this way) binds as the full lookup does,
  defers to an instance-dict entry, and leaves weak proxies alone.
- `str.startswith`/`endswith` with a start position on long strings, ASCII
  or not, count characters, not bytes.
- One-character strings from indexing are the shared Latin-1 objects.
- `math.gcd` over machine words, 128-bit values, power-of-two factors,
  and mixed big and small operands.
"""

import math
import random
import unittest
import weakref


class Plain:
    pass


class LibraryFastpathTest(unittest.TestCase):
    def test_getattr_builtin_dunder(self):
        p = Plain()
        for _ in range(1000):
            m = getattr(p, "__reduce_ex__", None)
            self.assertIs(m.__self__, p)
            self.assertEqual(m, object.__getattribute__(p, "__reduce_ex__"))
            self.assertTrue(hasattr(p, "__reduce_ex__"))
            self.assertEqual(getattr(p, "__format__")(""), str(p))
        p.__dict__["__reduce_ex__"] = "mine"
        self.assertEqual(getattr(p, "__reduce_ex__", None), "mine")
        target = Plain()
        proxy = weakref.proxy(target)
        self.assertIs(getattr(proxy, "__reduce_ex__").__self__, target)

    def test_startswith_positions(self):
        ascii_text = "key = value\n" * 20
        wide_text = "été = ☃\n" * 20
        for text in (ascii_text, wide_text):
            n = len(text)
            for pos in range(0, n + 1, 7):
                for needle in ("key", "ét", "=", "\n", ""):
                    self.assertEqual(
                        text.startswith(needle, pos), text[pos:].startswith(needle)
                    )
                    self.assertEqual(
                        text.endswith(needle, 0, pos), text[:pos].endswith(needle)
                    )
            self.assertFalse(text.startswith("", n + 1))
            self.assertTrue(text.startswith(("zz", text[5]), 5))
        self.assertTrue(wide_text.startswith("☃", 6))

    def test_index_chars_are_shared(self):
        s = "abcé" * 50
        for i in range(len(s)):
            c = s[i]
            self.assertEqual(c, chr(ord(c)))
        self.assertIs(s[0], "a")
        self.assertIs(s[3], "é")
        self.assertIs("".join(["x", "y"])[1], "y")

    def test_gcd(self):
        r = random.Random(7)
        cases = [(0, 0), (0, 5), (-6, 0), (-2**63, 0), (-2**63, -2**63),
                 (True, 4), (False, False), (2**200, 6 * 2**150), (3**80, 3**40 * 7),
                 (2**64 + 1, 2**64 - 1), (25, 2**300 * 5)]
        for _ in range(500):
            a = r.getrandbits(r.choice([5, 60, 64, 100, 128, 200])) << r.randrange(70)
            b = r.getrandbits(r.choice([3, 40, 64, 130, 300])) << r.randrange(70)
            cases.append((a * r.choice([1, -1]), b))
        for a, b in cases:
            g = math.gcd(a, b)
            self.assertIs(type(g), int)
            self.assertGreaterEqual(g, 0)
            if g:
                self.assertEqual(a % g, 0)
                self.assertEqual(b % g, 0)
                self.assertEqual(math.gcd(a // g, b // g), 1)
            else:
                self.assertEqual((a, b), (0, 0))
        self.assertEqual(math.gcd(12, 18, 27), 3)
        self.assertEqual(math.gcd(2**70 * 3, 2**65 * 9, 2**80 * 15), 2**65 * 3)
        self.assertEqual(math.gcd(), 0)
        with self.assertRaises(TypeError):
            math.gcd(1.0, 2)


if __name__ == "__main__":
    unittest.main()
