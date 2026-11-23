"""A property setter shared by several classes, storing into `__slots__`.

A base class's `@p.setter` that does `self._p = v` runs frameless for
each subclass whose instances keep `_p` in a `__slots__` member (with or
without an instance `__dict__` beside it), and for plain subclasses, one
after another at one site. The stores must land where the full
`__setattr__` puts them: a later read, `__dict__`, a readonly or
defaulted member left alone, a class whose `__setattr__` is its own, and
a member shadowed by a class attribute.
"""

import unittest


class Base:
    @property
    def p(self):
        return self._p

    @p.setter
    def p(self, v):
        self._p = v


class A(Base):
    __slots__ = ("_p",)


class B(Base):
    __slots__ = ("x", "_p")


class C(Base):
    pass


class Strict(Base):
    __slots__ = ("_p",)

    def __setattr__(self, name, value):
        object.__setattr__(self, name, value * 10)


class Shadow(Base):
    _p = property(lambda self: "shadow", lambda self, v: None)


def store_all(cls, n):
    out = []
    for i in range(n):
        o = cls()
        o.p = i
        out.append(o)
    return out


class SharedSetterTests(unittest.TestCase):
    def test_sequence_of_classes(self):
        for cls in (A, B, C, A, B, C):
            objs = store_all(cls, 400)
            self.assertEqual([o.p for o in objs], list(range(400)))
            self.assertEqual([o._p for o in objs[:3]], [0, 1, 2])

    def test_slots_beside_dict(self):
        objs = store_all(B, 300)
        o = objs[-1]
        self.assertEqual(o.p, 299)
        self.assertNotIn("_p", o.__dict__)
        o.extra = 1
        self.assertEqual(o.__dict__, {"extra": 1})
        c = store_all(C, 300)[-1]
        self.assertEqual(c.__dict__, {"_p": 299})

    def test_own_setattr_and_shadowing(self):
        store_all(A, 300)
        s = store_all(Strict, 300)
        self.assertEqual([o.p for o in s[:3]], [0, 100, 200])
        sh = store_all(Shadow, 300)
        self.assertEqual(sh[5].p, "shadow")

    def test_heap_values(self):
        for cls in (A, B):
            objs = []
            for i in range(300):
                o = cls()
                o.p = [i]
                objs.append(o)
            self.assertEqual(objs[7].p, [7])
            objs[7].p = objs[8]
            self.assertIs(objs[7].p, objs[8])


if __name__ == "__main__":
    unittest.main()
