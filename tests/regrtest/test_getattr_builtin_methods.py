"""`getattr` of a string's or container's method runs in place.

The bound method is the one the attribute load makes (equal to it, bound
to the same receiver); a missing name still raises or takes the default,
and dunders and `hasattr` keep their own paths.
"""

import unittest

N = 3000


class GetattrBuiltinMethodTests(unittest.TestCase):
    def test_bound_like_attribute_load(self):
        s, L, d = "ab", [1], {"k": 2}
        for _ in range(N):
            g = getattr(s, "upper")
            h = getattr(L, "append")
            k = getattr(d, "get")
            t = getattr((1, 2, 1), "count")
            b = getattr(b"x", "hex")
        self.assertEqual(g, s.upper)
        self.assertIs(g.__self__, s)
        self.assertEqual(g(), "AB")
        self.assertEqual(h, L.append)
        h(5)
        self.assertEqual(L, [1, 5])
        self.assertEqual(k("k"), 2)
        self.assertEqual(t(1), 2)
        self.assertEqual(b(), "78")

    def test_missing_and_default(self):
        for _ in range(N):
            r = getattr("ab", "nope", "dflt")
        self.assertEqual(r, "dflt")
        with self.assertRaises(AttributeError):
            getattr([], "nope")
        self.assertEqual(getattr([1], "__len__")(), 1)
        self.assertTrue(hasattr("", "join"))
        self.assertFalse(hasattr("", "nope"))

    def test_subclass_receiver(self):
        class S(str):
            def upper(self):
                return "mine"

        for _ in range(N):
            r = getattr(S("a"), "upper")()
        self.assertEqual(r, "mine")


if __name__ == "__main__":
    unittest.main()
