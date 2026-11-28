"""`template % args` whose conversions are all bare `%s`, over instances
(with `str` and `int` arguments beside them), renders in one pass; the
text, a frameless or full `__str__`, `__str__` raising or returning a
non-string, `%%`, argument-count errors, tuple and `str` subclasses,
mappings, and exceptions match the general formatting."""

import unittest


class K:
    def __init__(self, a):
        self.a = a

    def __str__(self):
        return "K%d" % self.a


class Leaf:
    def __str__(self):
        return "leaf"


class NoStr:
    pass


class Bad:
    def __str__(self):
        return 3


class Raiser:
    def __str__(self):
        raise KeyError("s")


class T(tuple):
    pass


class S(str):
    def __str__(self):
        return "S!"


class M:
    def __getitem__(self, k):
        return "item"

    def __str__(self):
        return "M"


class PercentBareSInstances(unittest.TestCase):
    def test_text(self):
        k = K(4)
        leaf = Leaf()
        for i in range(2000):
            self.assertEqual("%s" % k, "K4")
            self.assertEqual("%s" % leaf, "leaf")
            self.assertEqual("<%s|%s>" % (k, leaf), "<K4|leaf>")
            self.assertEqual("%s=%s (%s%%)" % ("x", k, i), "x=K4 (%d%%)" % i)
            self.assertEqual("[%s]" % (K(i),), "[K%d]" % i)
        self.assertTrue(("%s" % NoStr()).startswith("<"))

    def test_errors(self):
        k = K(1)
        for i in range(300):
            with self.assertRaises(TypeError) as cm:
                "%s" % Bad()
            self.assertEqual(str(cm.exception), "__str__ returned non-string (type int)")
            with self.assertRaises(KeyError):
                "%s and %s" % (k, Raiser())
            with self.assertRaises(TypeError) as cm:
                "%s %s" % k
            self.assertEqual(str(cm.exception), "not enough arguments for format string")
            with self.assertRaises(TypeError) as cm:
                "%s" % (k, k)
            self.assertEqual(
                str(cm.exception), "not all arguments converted during string formatting"
            )
            with self.assertRaises(TypeError):
                "x" % k

    def test_other_shapes(self):
        for i in range(300):
            self.assertEqual("%s" % T((K(2),)), "K2")
            self.assertEqual("%s %s" % T((K(2), 3)), "K2 3")
            self.assertEqual("%s" % S("v"), "S!")
            self.assertEqual("%s" % M(), "M")
            self.assertEqual("x" % M(), "x")
            self.assertEqual("%s" % ValueError("bad"), "bad")
            self.assertEqual("%s|%s" % (K(1), 2.5), "K1|2.5")
            self.assertEqual("%s|%s" % (K(1), True), "K1|True")
            self.assertEqual("%5s|%s" % (K(1), K(2)), "   K1|K2")


if __name__ == "__main__":
    unittest.main()
