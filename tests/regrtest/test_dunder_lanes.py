"""Python `__getitem__` and `__len__`, class methods read through a class,
and module functions, called from hot loops."""

import math
import sys
import unittest


class Seq:
    def __getitem__(self, i):
        return i * 2

    def __len__(self):
        return 3


class Counted:
    calls = 0

    @classmethod
    def make(cls, a):
        cls.calls += 1
        return (cls.__name__, a)


class Sub(Counted):
    pass


def loop(fn, n=300):
    out = None
    for i in range(n):
        out = fn(i)
    return out


class DunderLaneTest(unittest.TestCase):
    def test_getitem(self):
        s = Seq()
        self.assertEqual(loop(lambda i: s[i]), 598)

        def body(i):
            return s[i] + s[1]

        self.assertEqual(loop(body), 600)

    def test_getitem_replaced(self):
        class C:
            def __getitem__(self, k):
                return "a"

        c = C()

        def body(i):
            return c[i]

        self.assertEqual(loop(body), "a")
        C.__getitem__ = lambda self, k: "b"
        self.assertEqual(loop(body), "b")

        class D(C):
            def __getitem__(self, k):
                return "d"

        d = D()

        def poly(i):
            return (c if i % 2 else d)[i]

        self.assertEqual([poly(0), poly(1)], ["d", "b"])

    def test_getitem_shapes(self):
        class Default:
            def __getitem__(self, k, extra=10):
                return k + extra

        class Raises:
            def __getitem__(self, k):
                raise KeyError(k)

        class Frame:
            def __getitem__(self, k):
                return sys._getframe(1).f_code.co_name

        d, r, f = Default(), Raises(), Frame()

        def body(i):
            return d[i]

        self.assertEqual(loop(body), 309)

        def catch(i):
            try:
                return r[i]
            except KeyError as e:
                return e.args[0]

        self.assertEqual(loop(catch), 299)

        def caller(i):
            return f[i]

        self.assertEqual(loop(caller), "caller")

    def test_getitem_recursion(self):
        class Deep:
            def __getitem__(self, k):
                return self[k - 1] if k else 0

        with self.assertRaises(RecursionError):
            Deep()[10**6]
        self.assertEqual(Deep()[50], 0)

    def test_len(self):
        s = Seq()
        self.assertEqual(loop(lambda i: len(s)), 3)

        class Bad:
            def __init__(self, v):
                self.v = v

            def __len__(self):
                return self.v

        for v, exc in ((-1, ValueError), ("x", TypeError), (2**70, OverflowError)):
            b = Bad(v)

            def body(i, b=b):
                return len(b)

            with self.assertRaises(exc):
                loop(body)
        self.assertEqual(loop(lambda i, b=Bad(True): len(b)), 1)

    def test_classmethod_through_class(self):
        Counted.calls = 0

        def body(i):
            return Counted.make(i)

        self.assertEqual(loop(body), ("Counted", 299))
        self.assertEqual(Counted.calls, 300)
        self.assertEqual(loop(lambda i: Sub.make(i)), ("Sub", 299))
        self.assertEqual(Sub.calls, 600)
        Counted.make = classmethod(lambda cls, a: ("new", cls.__name__, a))
        self.assertEqual(loop(body), ("new", "Counted", 299))
        Counted.make = staticmethod(lambda a: ("static", a))
        self.assertEqual(loop(body), ("static", 299))

    def test_classmethod_through_instance(self):
        class C:
            @classmethod
            def who(cls):
                return cls

        class D(C):
            pass

        d = D()
        self.assertIs(loop(lambda i: d.who()), D)
        self.assertIs(loop(lambda i: D.who()), D)
        bound = D.who
        self.assertIs(bound.__self__, D)

    def test_data_descriptor(self):
        log = []

        class Desc:
            def __get__(self, instance, owner=None):
                log.append((type(instance).__name__, owner.__name__))
                return instance.raw * 2

            def __set__(self, instance, value):
                instance.raw = value

        class Host:
            val = Desc()

            def __init__(self):
                self.raw = 4

        h = Host()
        h.__dict__["val"] = "shadow"
        self.assertEqual(loop(lambda i: h.val), 8)
        self.assertEqual(log[-1], ("Host", "Host"))
        h.val = 5
        self.assertEqual(h.val, 10)
        self.assertIs(Host.__dict__["val"].__class__, Desc)

        class NonData:
            def __get__(self, instance, owner=None):
                return "descr"

        class Host2:
            val = NonData()

        h2 = Host2()
        self.assertEqual(loop(lambda i: h2.val), "descr")
        h2.val = "own"
        self.assertEqual(loop(lambda i: h2.val), "own")

    def test_descriptor_get_raises(self):
        class Desc:
            def __get__(self, instance, owner):
                raise AttributeError("nope")

            def __set__(self, instance, value):
                pass

        class Host:
            val = Desc()

            def __getattr__(self, name):
                return "fallback " + name

        h = Host()
        self.assertEqual(loop(lambda i: h.val), "fallback val")

    def test_enum_value(self):
        import enum

        class Color(enum.Enum):
            RED = 1
            GREEN = 2

        self.assertEqual(loop(lambda i: Color.RED.value), 1)
        self.assertEqual(loop(lambda i: Color.GREEN.name), "GREEN")

    def test_stored_callable_method_form(self):
        class Holder:
            def __init__(self, fn):
                self.fn = fn

        h = Holder(lambda x: x + 1)
        self.assertEqual(loop(lambda i: h.fn(i)), 300)
        h.fn = len
        self.assertEqual(loop(lambda i: h.fn("ab")), 2)

    def test_operator_dunders(self):
        class V:
            def __init__(self, x):
                self.x = x

            def __sub__(self, other):
                if not isinstance(other, V):
                    return NotImplemented
                return V(self.x - other.x)

            def __rsub__(self, other):
                return V(other - self.x)

            def __lt__(self, other):
                return self.x < other.x

            def __eq__(self, other):
                return isinstance(other, V) and self.x == other.x

            __hash__ = None

        class W(V):
            def __rsub__(self, other):
                return "W.__rsub__"

        a, b = V(5), V(3)
        self.assertEqual(loop(lambda i: (a - b).x), 2)
        self.assertEqual(loop(lambda i: (10 - a).x), 5)
        self.assertEqual(loop(lambda i: a - W(1)), "W.__rsub__")
        self.assertIs(loop(lambda i: b < a), True)
        self.assertIs(loop(lambda i: a == V(5)), True)
        with self.assertRaises(TypeError):
            loop(lambda i: a - "s")

        class Boom:
            def __add__(self, other):
                raise KeyError("boom")

        with self.assertRaises(KeyError):
            loop(lambda i: Boom() + 1)

    def test_module_function(self):
        self.assertEqual(loop(lambda i: math.floor(i + 0.5)), 299)
        import types

        mod = types.ModuleType("m")
        mod.f = lambda x: x + 1

        def body(i):
            return mod.f(i)

        self.assertEqual(loop(body), 300)
        mod.f = lambda x: x - 1
        self.assertEqual(loop(body), 298)
        del mod.f
        with self.assertRaises(AttributeError):
            loop(body)


if __name__ == "__main__":
    unittest.main()
