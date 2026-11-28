"""Arithmetic and comparisons over int and float subclasses (IntEnum, a
plain `class I(int)`, a float subclass) from compiled loops: exact int,
float and bool results on either side, big ints, in-place operators, a
division by zero, a class that overrides the operator, its reflection or
its in-place form (and one that gains an override later), IntFlag's own
`|`, and finalizers of temporaries, each matching CPython."""

import enum
import unittest


class I(int):
    pass


class F(float):
    pass


class Num(enum.IntEnum):
    A = 1
    B = 2


class Perm(enum.IntFlag):
    R = 4
    W = 2


class Over(int):
    def __add__(self, o):
        return "add"

    def __lt__(self, o):
        return "lt"


class ROver(int):
    def __radd__(self, o):
        return "radd"

    def __gt__(self, o):
        return "gt"


class IOver(int):
    def __iadd__(self, o):
        return "iadd"


class Late(int):
    pass


log = []


class D(int):
    def __del__(self):
        log.append(int(self))


def ops(n, a, b):
    out = None
    for i in range(n):
        out = (a + b, a - b, a * b, a // b, a % b, a / b, a ** 2, a & b, a | b, a ^ b, a << 1, a >> 1)
    return out


def fops(n, a, b):
    out = None
    for i in range(n):
        out = (a + b, a - b, a * b, a // b, a % b, a / b, a ** 2)
    return out


def rops(n, a, b):
    out = None
    for i in range(n):
        out = (b + a, b - a, b * a, b / a)
    return out


def cmps(n, a, b):
    out = None
    for i in range(n):
        out = (a < b, a <= b, a == b, a != b, a > b, a >= b, b < a, b == a)
    return out


def iadd(n, a, b):
    out = None
    for i in range(n):
        x = a
        x += b
        out = x
    return out


class IntSubclassOperatorTests(unittest.TestCase):
    def check_types(self, got, want):
        self.assertEqual(got, want)
        self.assertEqual([type(x) for x in got], [type(x) for x in want])

    def test_intenum_and_plain_subclass(self):
        for a in (Num.B, I(2)):
            want = ops(1, 2, 3)
            for n in (1, 400):
                self.check_types(ops(n, a, 3), want)
                self.check_types(ops(n, a, I(3)), want)
                self.check_types(rops(n, a, 7), rops(1, 2, 7))
                self.check_types(cmps(n, a, 3), cmps(1, 2, 3))
                self.check_types(cmps(n, a, 2.0), cmps(1, 2, 2.0))
                self.check_types(cmps(n, a, Num.B), cmps(1, 2, 2))

    def test_mixed_and_big(self):
        big = 1 << 80
        for n in (1, 400):
            self.check_types(ops(n, I(big), 3), ops(1, big, 3))
            self.check_types(fops(n, Num.B, 1.5), fops(1, 2, 1.5))
            self.check_types(fops(n, F(2.5), 2), fops(1, 2.5, 2))
            self.check_types(cmps(n, F(2.5), 2), cmps(1, 2.5, 2))
            self.check_types(cmps(n, I(big), big + 1), cmps(1, big, big + 1))
            self.assertEqual(iadd(n, Num.A, 4), 5)
            self.assertIs(type(iadd(n, Num.A, 4)), int)
            self.assertIs(type(iadd(n, Num.A, True)), int)

    def test_errors(self):
        with self.assertRaises(ZeroDivisionError):
            ops(300, Num.A, 0)
        with self.assertRaises(TypeError):
            ops(300, Num.A, "x")
        with self.assertRaises(TypeError):
            cmps(300, Num.A, "x")

    def test_equality_with_other_types(self):
        def eqs(n, a, b):
            out = None
            for i in range(n):
                out = (a == b, a != b)
            return out

        for n in (1, 400):
            self.assertEqual(eqs(n, Num.A, "x"), (False, True))
            self.assertEqual(eqs(n, Num.A, 1), (True, False))
            self.assertEqual(eqs(n, Num.A, 1.0), (True, False))

    def test_overrides(self):
        def add(n, a, b):
            out = None
            for i in range(n):
                out = (a + b, b + a)
            return out

        def lt(n, a, b):
            out = None
            for i in range(n):
                out = (a < b, b < a)
            return out

        for n in (1, 400):
            self.assertEqual(add(n, Over(1), 2), ("add", 3))
            self.assertEqual(add(n, ROver(1), 2), (3, "radd"))
            self.assertEqual(lt(n, Over(1), 2), ("lt", False))
            self.assertEqual(lt(n, ROver(1), 2), (True, "gt"))
            self.assertEqual(iadd(n, IOver(1), 2), "iadd")
            self.assertEqual(Perm.R | Perm.W, 6)
            self.assertIs(type(ops(n, Perm.R, Perm.W)[8]), Perm)

    def test_override_added_later(self):
        self.assertEqual(ops(400, Late(4), 2)[0], 6)
        Late.__add__ = lambda self, o: "late"
        try:
            self.assertEqual(ops(400, Late(4), 2)[0], "late")
            self.assertEqual(ops(400, Late(4), 2)[1], 2)
        finally:
            del Late.__add__
        self.assertEqual(ops(400, Late(4), 2)[0], 6)

    def test_temporaries_finalized(self):
        def tmp(n):
            total = 0
            for i in range(n):
                total += D(i) + 1
            return total

        del log[:]
        self.assertEqual(tmp(300), sum(range(1, 301)))
        self.assertEqual(sorted(log), list(range(300)))


if __name__ == "__main__":
    unittest.main()
