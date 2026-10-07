"""A class's Python dunders called with the instance directly.

`hash()`, `obj[key] = value`, `x in obj`, `a op b` for two instances of
one class, and their comparisons call the class's plain Python method
without a bound method (a leaf in place). The protocol around the call
is unchanged: hash results are normalized, `NotImplemented` from an
operator without a reflected method raises TypeError, a reflected method
still gets its turn, comparisons may return any object, and a method
replaced on the class takes effect at once.
"""

import unittest


class V:
    def __init__(self, x):
        self.x = x

    def __hash__(self):
        return self.x

    def __eq__(self, o):
        return self.x == o.x

    def __lt__(self, o):
        return self.x < o.x

    def __add__(self, o):
        return V(self.x + o.x)

    def __sub__(self, o):
        return NotImplemented

    def __setitem__(self, k, v):
        self.x = v

    def __contains__(self, k):
        return k == self.x


class R:
    def __init__(self, x):
        self.x = x

    def __add__(self, o):
        return NotImplemented

    def __radd__(self, o):
        return "radd"


class Weird:
    def __eq__(self, o):
        return "yes"

    def __hash__(self):
        return 2**80


class InstanceDunderCallTest(unittest.TestCase):
    def test_operators(self):
        a, b = V(2), V(3)
        for _ in range(400):
            self.assertEqual((a + b).x, 5)
            self.assertTrue(a == V(2))
            self.assertTrue(a < b)
            self.assertFalse(b < a)
        with self.assertRaises(TypeError):
            a - b
        # Same class: the reflected method isn't tried.
        with self.assertRaises(TypeError):
            R(1) + R(2)
        self.assertEqual(1 + R(2), "radd")

    def test_hash_and_containers(self):
        a = V(7)
        for _ in range(300):
            self.assertEqual(hash(a), 7)
            self.assertIn(7, a)
            self.assertNotIn(8, a)
        d = {V(1): "one"}
        self.assertEqual(d[V(1)], "one")
        self.assertEqual(hash(Weird()), hash(2**80))
        self.assertEqual(Weird() == Weird(), "yes")

    def test_setitem(self):
        a = V(0)
        for i in range(300):
            a[0] = i
        self.assertEqual(a.x, 299)

    def test_replaced_on_class(self):
        class C:
            def __add__(self, o):
                return 1

            def __hash__(self):
                return 1

        c = C()
        for _ in range(300):
            self.assertEqual(c + c, 1)
            self.assertEqual(hash(c), 1)
        C.__add__ = lambda self, o: 2
        C.__hash__ = lambda self: 2
        self.assertEqual(c + c, 2)
        self.assertEqual(hash(c), 2)
        C.__radd__ = lambda self, o: 3
        C.__add__ = lambda self, o: NotImplemented
        # Same class: the reflected method isn't tried.
        with self.assertRaises(TypeError):
            c + c
        C.__hash__ = None
        with self.assertRaises(TypeError):
            hash(c)


if __name__ == "__main__":
    unittest.main()
