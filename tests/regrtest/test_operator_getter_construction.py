"""Building `itemgetter`, `attrgetter` and `methodcaller` objects.

The exact classes build their instances natively, as CPython's C
constructors do: the same repr, pickling and calls as the Python
`__init__` gives. A subclass, keywords to `methodcaller`, a non-`str`
name, or a class changed since (its `__init__` replaced) go through the
Python methods.
"""

import operator
import pickle
import unittest


class GetterConstructionTests(unittest.TestCase):
    def test_fields_and_calls(self):
        for _ in range(300):
            g = operator.itemgetter(1)
            self.assertEqual(g([5, 6]), 6)
            g2 = operator.itemgetter(0, 2)
            self.assertEqual(g2("abc"), ("a", "c"))
            a = operator.attrgetter("real", "imag")
            self.assertEqual(a(3 + 4j), (3.0, 4.0))
            m = operator.methodcaller("split", ",")
            self.assertEqual(m("a,b"), ["a", "b"])
        self.assertEqual(repr(operator.itemgetter(1, "x")), "operator.itemgetter(1, 'x')")
        self.assertEqual(repr(operator.attrgetter("a.b")), "operator.attrgetter('a.b')")
        self.assertEqual(repr(operator.methodcaller("f", 1)), "operator.methodcaller('f', 1)")

    def test_pickle_and_identity(self):
        for obj in (operator.itemgetter(2), operator.attrgetter("x"), operator.methodcaller("f", 1)):
            back = pickle.loads(pickle.dumps(obj))
            self.assertEqual(repr(back), repr(obj))
            self.assertIsNot(back, obj)
        self.assertIsNot(operator.methodcaller("f"), operator.methodcaller("f"))

    def test_other_shapes(self):
        m = operator.methodcaller("split", sep=",")
        self.assertEqual(repr(m), "operator.methodcaller('split', sep=',')")
        self.assertEqual(m("a,b"), ["a", "b"])
        with self.assertRaises(TypeError):
            operator.attrgetter(1)
        with self.assertRaises(TypeError):
            operator.methodcaller(5)
        with self.assertRaises(TypeError):
            operator.itemgetter()

        try:
            class G(operator.itemgetter):
                pass
        except TypeError:
            return  # (CPython's C class isn't an acceptable base.)
        for _ in range(100):
            self.assertIs(type(G(1)), G)
            self.assertEqual(G(1)([7, 8]), 8)

    def test_patched_class(self):
        calls = []
        cls = operator.attrgetter
        orig = cls.__init__

        def init(self, *attrs):
            calls.append(attrs)
            orig(self, *attrs)

        try:
            cls.__init__ = init
        except TypeError:
            self.skipTest("an immutable type (CPython's C class)")
        try:
            for _ in range(50):
                operator.attrgetter("real")
        finally:
            cls.__init__ = orig
        self.assertEqual(len(calls), 50)
        self.assertEqual(operator.attrgetter("real")(5), 5)


if __name__ == "__main__":
    unittest.main()
