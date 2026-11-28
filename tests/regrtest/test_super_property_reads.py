"""`super().name` of a property.

A property later in the MRO whose getter is a plain Python function of
`self` (`email`'s `Domain.domain` reads `super().value`) runs its getter
as an inline activation once warm. Its result, its exceptions (an
`AttributeError` with no `__getattr__` fallback), the explicit two-argument
form, and a property replaced mid-loop all read as CPython reads them.
"""

import unittest

N = 3000


class Base(list):
    @property
    def value(self):
        return "".join(self)


class Mid(Base):
    pass


class Child(Mid):
    @property
    def value(self):
        return "child:" + super().value

    @property
    def explicit(self):
        return super(Child, self).value

    def __getattr__(self, name):
        return ("fallback", name)


class Raising:
    @property
    def v(self):
        raise AttributeError("from getter")


class RaisingChild(Raising):
    def __getattr__(self, name):
        return "fallback"

    def w(self):
        return super().v


class SuperPropertyTests(unittest.TestCase):
    def test_plain_getter(self):
        c = Child(["a", "b"])
        for _ in range(N):
            r = c.value
            e = c.explicit
        self.assertEqual(r, "child:ab")
        self.assertEqual(e, "ab")

    def test_getter_attribute_error_propagates(self):
        r = RaisingChild()
        for _ in range(N):
            with self.assertRaises(AttributeError):
                r.w()
        self.assertEqual(r.v, "fallback")

    def test_property_replaced(self):
        class A:
            @property
            def p(self):
                return 1

        class B(A):
            def get(self):
                return super().p

        b = B()
        out = []
        for i in range(N):
            if i == N // 2:
                A.p = property(lambda self: 2)
            out.append(b.get())
        self.assertEqual(out[0], 1)
        self.assertEqual(out[-1], 2)

    def test_getter_with_state(self):
        class A:
            def __init__(self):
                self.n = 0

            @property
            def tick(self):
                self.n += 1
                return self.n

        class B(A):
            def step(self):
                return super().tick * 10

        b = B()
        for _ in range(N):
            r = b.step()
        self.assertEqual(r, N * 10)
        self.assertEqual(b.n, N)

    def test_released_operands(self):
        dead = []

        class D:
            def __del__(self):
                dead.append(1)

        class A:
            @property
            def p(self):
                return D()

        class B(A):
            def f(self):
                return super().p

        b = B()
        for _ in range(100):
            b.f()
        self.assertEqual(len(dead), 100)


if __name__ == "__main__":
    unittest.main()
