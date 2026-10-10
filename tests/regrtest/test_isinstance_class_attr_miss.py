"""`isinstance` misses on instances skip the `__class__` read only when
nothing can answer it differently.

An instance whose class overrides neither `__class__` nor
`__getattribute__` is an instance of exactly its MRO. A `__class__`
property, a `__getattribute__` that reports another class, and
a `__getattribute__` installed on a base after warm-up must still be consulted.
"""

import unittest


class Plain:
    pass


class Liar:
    @property
    def __class__(self):
        return str


class Sneaky:
    def __getattribute__(self, name):
        if name == "__class__":
            return int
        return object.__getattribute__(self, name)


class IsinstanceClassAttrTest(unittest.TestCase):
    def test_plain_miss(self):
        p = Plain()
        for _ in range(2000):
            self.assertFalse(isinstance(p, str))
            self.assertFalse(isinstance(p, (int, str)))
            self.assertTrue(isinstance(p, Plain))

    def test_property_and_getattribute(self):
        for _ in range(2000):
            self.assertTrue(isinstance(Liar(), str))
            self.assertTrue(isinstance(Sneaky(), int))
            self.assertFalse(isinstance(Sneaky(), str))

    def test_installed_after_warmup(self):
        class Base:
            pass

        class Child(Base):
            pass

        c = Child()
        for _ in range(2000):
            self.assertFalse(isinstance(c, bytes))

        def getattribute(self, name):
            if name == "__class__":
                return float
            return object.__getattribute__(self, name)

        Base.__getattribute__ = getattribute
        for _ in range(2000):
            self.assertTrue(isinstance(c, float))
        del Base.__getattribute__
        for _ in range(2000):
            self.assertFalse(isinstance(c, float))

    def test_class_assignment(self):
        class A:
            pass

        class B:
            pass

        a = A()
        for _ in range(500):
            self.assertFalse(isinstance(a, B))
        a.__class__ = B
        self.assertTrue(isinstance(a, B))
        self.assertFalse(isinstance(a, A))


class FloatRatioAndCodecTest(unittest.TestCase):
    def test_as_integer_ratio(self):
        from fractions import Fraction
        import math
        values = [0.0, -0.0, 1.0, -1.0, 0.5, 12.3, -99.9, 1e-300, 5e-324,
                  2.0 ** 62, 2.0 ** 63, 2.0 ** 64, 1e300, 3.141592653589793,
                  2.0 ** -62, 2.0 ** -63, 1.5 * 2.0 ** -62, 1.0 / 3, 1e16 + 2]
        for v in values:
            n, d = v.as_integer_ratio()
            self.assertIs(type(n), int)
            self.assertIs(type(d), int)
            self.assertGreater(d, 0)
            self.assertEqual(math.gcd(n, d), 1 if n else d)
            self.assertEqual(Fraction(n, d), Fraction(v))
        self.assertEqual((-0.0).as_integer_ratio(), (0, 1))
        self.assertEqual((0.25).as_integer_ratio(), (1, 4))

    def test_encode_decode_with_errors(self):
        for _ in range(500):
            self.assertEqual("abc".encode("utf-8", "strict"), b"abc")
            self.assertEqual("\xe9".encode("latin-1", "strict"), b"\xe9")
            self.assertEqual("\xe9".encode("ascii", "replace"), b"?")
            self.assertEqual("\xe9".encode("ascii", "ignore"), b"")
            self.assertEqual(b"abc".decode("utf-8", "strict"), "abc")
            self.assertEqual(b"\xff".decode("utf-8", "replace"), "�")
            self.assertEqual(b"\xff".decode("ascii", "surrogateescape"), "\udcff")
        with self.assertRaises(UnicodeEncodeError):
            "\xe9".encode("ascii", "strict")
        with self.assertRaises(UnicodeDecodeError):
            b"\xff".decode("utf-8", "strict")
        with self.assertRaises(LookupError):
            "\xe9".encode("ascii", "no-such-handler")


if __name__ == "__main__":
    unittest.main()
