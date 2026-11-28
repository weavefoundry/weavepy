"""Compiled `mc(obj)` of an `operator.methodcaller` whose method is a
Python function of `obj`'s plain class calls the method directly; the
results with no, one and two stored arguments, defaults, raises (caught in
the frame or past it), stored keywords, `*args` methods, an instance
attribute shadowing the method, native receivers, and a method replaced
later match the general call."""

import operator
import unittest


class C:
    def __init__(self, v):
        self.v = v
        self.calls = 0

    def get(self):
        self.calls += 1
        return self.v

    def add(self, a):
        return self.v + a

    def mix(self, a, b):
        return (self.v, a, b)

    def dflt(self, a, b=10):
        return a + b

    def star(self, *a):
        return len(a)

    def kw(self, *, k=0):
        return k

    def boom(self, a):
        if a == 3:
            raise KeyError(a)
        return a


class MethodcallerDirect(unittest.TestCase):
    def test_results(self):
        c = C(5)
        get = operator.methodcaller("get")
        add = operator.methodcaller("add", 2)
        mix = operator.methodcaller("mix", "a", "b")
        for i in range(2000):
            self.assertEqual(get(c), 5)
            self.assertEqual(add(c), 7)
            self.assertEqual(mix(c), (5, "a", "b"))
        self.assertEqual(c.calls, 2000)

    def test_other_shapes(self):
        c = C(1)
        d1 = operator.methodcaller("dflt", 1)
        d2 = operator.methodcaller("dflt", 1, 2)
        st = operator.methodcaller("star", 1, 2, 3)
        kw = operator.methodcaller("kw", k=4)
        up = operator.methodcaller("upper")
        for i in range(1000):
            self.assertEqual(d1(c), 11)
            self.assertEqual(d2(c), 3)
            self.assertEqual(st(c), 3)
            self.assertEqual(kw(c), 4)
            self.assertEqual(up("ab"), "AB")
            self.assertEqual(list(map(operator.methodcaller("add", i), [C(1), C(2)])), [1 + i, 2 + i])

    def test_raises(self):
        c = C(0)
        n = 0
        for i in range(1000):
            try:
                operator.methodcaller("boom", i % 5)(c)
            except KeyError:
                n += 1
        self.assertEqual(n, 200)

        def call(mc, o):
            return mc(o)

        bad = operator.methodcaller("boom", 3)
        for i in range(500):
            with self.assertRaises(KeyError):
                call(bad, c)
            with self.assertRaises(AttributeError):
                operator.methodcaller("missing")(c)

    def test_shadowed_and_replaced(self):
        c = C(2)
        get = operator.methodcaller("get")
        out = []
        for i in range(1000):
            out.append(get(c))
        c.get = lambda: "instance"
        for i in range(1000):
            out.append(get(c))
        del c.get
        C.get = lambda self: "class"
        for i in range(1000):
            out.append(get(c))
        self.assertEqual(
            (out.count(2), out.count("instance"), out.count("class")), (1000, 1000, 1000)
        )


if __name__ == "__main__":
    unittest.main()
