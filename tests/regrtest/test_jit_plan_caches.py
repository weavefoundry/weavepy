"""Compiled leaf plans read method, class and global caches in line.

A leaf method that calls another leaf method, reads a class constant or
reads a module global compiles (once it has run enough) to native code
that checks each cache itself. Every test defines its own methods (so its
own plans) and runs them long enough to compile; then the cached fact
changes, and each call must see it. The drivers read `locals()` on a path
they never take, which keeps the tier-2 compiler (whose calls take other
paths) off them.
"""

import types
import unittest

WARM = 33000

SCALE = 2


class Val:
    def __init__(self, v):
        self.v = v


class ConstP:
    FWD = 1

    def __init__(self, x, y):
        self.x = x
        self.y = y
        self.d = 1

    def get(self):
        return (self.y if self.d == ConstP.FWD else self.x).v


class ConstQ(ConstP):
    pass


class Strength:
    def __init__(self, value):
        self.value = value

    @staticmethod
    def weaker(a, b):
        return a.value > b.value

    @staticmethod
    def weakest(a, b):
        return a if Strength.weaker(a, b) else b


class StaticC:
    def __init__(self, s, t):
        self.s = s
        self.t = t

    def get(self):
        return Strength.weakest(self.s, self.t).value


def drive(o, n):
    if n < 0:
        return locals()
    t = 0
    for _ in range(n):
        t += o.get()
    return t


def set_scale(v):
    global SCALE
    SCALE = v


class PlanCacheTests(unittest.TestCase):
    def test_instance_attribute_shadows_method(self):
        class P:
            def __init__(self, x):
                self.x = x

            def pick(self):
                return self.x

            def get(self):
                return self.pick().v

        p = P(Val(2))
        self.assertEqual(drive(p, WARM), 2 * WARM)
        p.pick = lambda: Val(10)
        self.assertEqual(drive(p, 10), 100)
        self.assertEqual(drive(P(Val(3)), 10), 30)

    def test_method_replaced_on_class(self):
        class P:
            def __init__(self, x):
                self.x = x

            def pick(self):
                return self.x

            def get(self):
                return self.pick().v

        p = P(Val(2))
        self.assertEqual(drive(p, WARM), 2 * WARM)
        P.pick = lambda self: Val(5)
        self.assertEqual(drive(p, 10), 50)

    def test_subclass_receiver(self):
        class P:
            def __init__(self, x):
                self.x = x

            def pick(self):
                return self.x

            def get(self):
                return self.pick().v

        class Q(P):
            def pick(self):
                return Val(7)

        self.assertEqual(drive(P(Val(2)), WARM), 2 * WARM)
        self.assertEqual(drive(Q(Val(2)), 10), 70)
        self.assertEqual(drive(P(Val(4)), 10), 40)

    def test_class_constant_rebound(self):
        q = ConstQ(Val(1), Val(2))
        self.assertEqual(drive(q, WARM), 2 * WARM)
        ConstQ.FWD = 0
        self.assertEqual(drive(q, 10), 20)
        ConstP.FWD = 0
        self.assertEqual(drive(q, 10), 10)
        ConstP.FWD = 1
        self.assertEqual(drive(q, 10), 20)

    def test_global_rebound(self):
        class P:
            def __init__(self, x):
                self.x = x

            def get(self):
                return self.x.v * SCALE

        p = P(Val(3))
        try:
            self.assertEqual(drive(p, WARM), 6 * WARM)
            set_scale(5)
            self.assertEqual(drive(p, 10), 150)
            globals()["SCALE"] = 7
            self.assertEqual(drive(p, 10), 210)
            del globals()["SCALE"]
            with self.assertRaises(NameError):
                p.get()
            globals()["SCALE"] = 11
            self.assertEqual(drive(p, 10), 330)
        finally:
            set_scale(2)

    def test_shared_code_other_globals(self):
        class P:
            def __init__(self, x):
                self.x = x

            def get(self):
                return self.x.v * SCALE

        p = P(Val(3))
        self.assertEqual(drive(p, WARM), 6 * WARM)
        ns = dict(globals())
        ns["SCALE"] = 100

        class Q(P):
            get = types.FunctionType(P.get.__code__, ns)

        self.assertEqual(drive(Q(Val(3)), 10), 3000)
        self.assertEqual(drive(p, 10), 60)

    def test_static_method_through_class(self):
        c = StaticC(Strength(1), Strength(2))
        self.assertEqual(drive(c, WARM), 2 * WARM)
        Strength.weaker = staticmethod(lambda x, y: x.value < y.value)
        self.assertEqual(drive(c, 10), 10)

if __name__ == "__main__":
    unittest.main()
