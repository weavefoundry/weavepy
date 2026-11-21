"""`cls.__mro__` and `cls.__bases__` read as one tuple each.

As in CPython, `C.__mro__ is C.__mro__`: the class keeps the tuple while
its links stay the same, a `__bases__` assignment gives new ones, and a
class whose tuples were read is still collected once nothing else holds
it. A tuple subclass passed to `type()` as the bases is kept as given.
"""

import gc
import unittest
import weakref

N = 3000


class A:
    pass


class B:
    pass


def links(c):
    return c.__mro__, c.__bases__


class ClassLinkTupleTests(unittest.TestCase):
    def test_identity(self):
        class C(A):
            pass

        first = links(C)
        for _ in range(N):
            r = links(C)
        self.assertIs(r[0], first[0])
        self.assertIs(r[1], first[1])
        self.assertEqual(r, ((C, A, object), (A,)))
        self.assertIs(int.__mro__, int.__mro__)
        self.assertEqual(object.__bases__, ())

    def test_bases_assignment(self):
        class D(A):
            pass

        before = links(D)
        out = []
        for i in range(N):
            if i == N // 2:
                D.__bases__ = (B,)
            out.append(links(D))
        self.assertEqual(out[0], before)
        self.assertEqual(out[-1], ((D, B, object), (B,)))
        self.assertIsNot(out[-1][0], before[0])
        self.assertIs(out[-1][0], D.__mro__)
        # The tuple read before the assignment keeps what it held.
        self.assertEqual(before[0], (D, A, object))

    def test_collected(self):
        refs = []
        for i in range(200):
            K = type("K%d" % i, (A,), {})
            links(K)
            refs.append(weakref.ref(K))
            del K
        gc.collect()
        self.assertEqual(sum(r() is not None for r in refs), 0)

    def test_held_tuple_keeps_class(self):
        K = type("Held", (A,), {})
        mro = K.__mro__
        r = weakref.ref(K)
        del K
        gc.collect()
        self.assertIsNotNone(r())
        self.assertIs(mro[0], r())
        del mro
        gc.collect()
        self.assertIsNone(r())

    def test_tuple_subclass_bases(self):
        class T(tuple):
            pass

        bases = T((A,))
        K = type("K", bases, {})
        for _ in range(500):
            b = K.__bases__
        self.assertIs(b, bases)
        self.assertEqual(K.__mro__, (K, A, object))


if __name__ == "__main__":
    unittest.main()
