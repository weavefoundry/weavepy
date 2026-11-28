"""Slices of `bytearray`s and of `str`, `list` and `tuple` subclasses.

They slice in place where the class keeps its base's `__getitem__`: a
`bytearray` gives a new `bytearray`, a subclass's slice the base's exact
type, as CPython's do; a class with its own `__getitem__` keeps it.
"""

import unittest

N = 3000


class S(str):
    pass


class L(list):
    pass


class T(tuple):
    pass


class G(list):
    def __getitem__(self, i):
        return "mine"


class SliceTests(unittest.TestCase):
    def test_results_and_types(self):
        s, l, t, g = S("hello"), L([1, 2, 3]), T((1, 2, 3)), G([1, 2])
        b = bytearray(b"hello")
        for _ in range(N):
            r = (s[1:3], l[1:], t[:2], g[0:1], b[1:3], s[::2], l[::-1], b[::2], b[:])
        self.assertEqual(
            [(type(x).__name__, x) for x in r],
            [
                ("str", "el"), ("list", [2, 3]), ("tuple", (1, 2)), ("str", "mine"),
                ("bytearray", bytearray(b"el")), ("str", "hlo"), ("list", [3, 2, 1]),
                ("bytearray", bytearray(b"hlo")), ("bytearray", bytearray(b"hello")),
            ],
        )

    def test_copies_are_independent(self):
        b = bytearray(b"abc")
        l = L([1, 2])
        for _ in range(N):
            c = b[:]
            m = l[:]
        c[0] = 120
        m.append(3)
        self.assertEqual((b, l), (bytearray(b"abc"), [1, 2]))

    def test_bounds(self):
        s = S("hello")
        b = bytearray(b"hello")
        for _ in range(N):
            r = (s[-2:], s[10:], b[-3:-1], b[3:1])
        self.assertEqual(r, ("lo", "", bytearray(b"ll"), bytearray()))

    def test_temporary_container_finalizes(self):
        dead = []

        class D(list):
            def __del__(self):
                dead.append(1)

        for _ in range(N):
            r = D([1, 2, 3])[:]
        self.assertEqual(len(dead), N)
        self.assertEqual((type(r), r), (list, [1, 2, 3]))

    def test_class_gains_getitem(self):
        class K(list):
            pass

        k = K([1, 2, 3])
        out = []
        for i in range(N):
            if i == N // 2:
                K.__getitem__ = lambda self, i: "own"
            out.append(k[:])
        self.assertEqual(out[0], [1, 2, 3])
        self.assertEqual(out[-1], "own")


if __name__ == "__main__":
    unittest.main()
