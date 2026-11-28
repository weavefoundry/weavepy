"""Compiled attribute stores call a class's Python `__setattr__` or a
property's Python setter directly; the values stored, the name passed,
a setter's ignored result, raises (caught in the storing frame or
past it), stores in loops and from `self.x = v`, the receiver's and
value's lifetimes, and a hook replaced later match the general store."""

import gc
import sys
import unittest
import weakref


class Log:
    def __init__(self):
        object.__setattr__(self, "log", [])

    def __setattr__(self, k, v):
        self.log.append((k, v))
        object.__setattr__(self, k, v)


class ViaDict:
    def __setattr__(self, k, v):
        self.__dict__[k] = v * 2


class ViaSuper:
    def __setattr__(self, k, v):
        if k.startswith("_"):
            raise AttributeError(k)
        super().__setattr__(k, v)
        return "ignored"


class Frozen:
    __slots__ = ("a",)

    def __init__(self, a):
        object.__setattr__(self, "a", a)

    def __setattr__(self, k, v):
        raise AttributeError(f"frozen: {k}")


class Prop:
    def __init__(self):
        self._x = 0
        self.calls = 0

    @property
    def x(self):
        return self._x

    @x.setter
    def x(self, v):
        # Not a frameless leaf: it calls and stores twice.
        self.calls = self.calls + 1
        self._x = abs(v)


class InitStores:
    def __init__(self, a, b):
        self.a = a
        self.b = b

    def __setattr__(self, k, v):
        object.__setattr__(self, k, (k, v))


class Tracked:
    deleted = 0

    def __del__(self):
        Tracked.deleted += 1


class Hold:
    def __setattr__(self, k, v):
        object.__setattr__(self, k, v)


class SetattrHookDirect(unittest.TestCase):
    def test_log_hook(self):
        o = Log()
        for i in range(2000):
            o.x = i
        self.assertEqual(o.x, 1999)
        self.assertEqual(len(o.log), 2000)
        self.assertEqual(o.log[0], ("x", 0))
        self.assertEqual(o.log[-1], ("x", 1999))

    def test_dict_hook_and_names(self):
        o = ViaDict()
        for i in range(2000):
            o.a = i
            o.b = -i
        self.assertEqual((o.a, o.b), (3998, -3998))
        self.assertEqual(sorted(o.__dict__), ["a", "b"])

    def test_super_hook_result_ignored(self):
        o = ViaSuper()
        for i in range(2000):
            o.v = i
        self.assertEqual(o.v, 1999)
        self.assertNotIn("_p", o.__dict__)

    def test_raise_caught_here(self):
        o = ViaSuper()
        n = 0
        for i in range(2000):
            try:
                o._p = i
            except AttributeError as e:
                n += 1
                self.assertEqual(e.args, ("_p",))
        self.assertEqual(n, 2000)

    def test_raise_past_frame(self):
        def store(o, v):
            o.a = v

        for i in range(1000):
            f = Frozen(i)
            with self.assertRaises(AttributeError):
                store(f, i + 1)
            self.assertEqual(f.a, i)

    def test_property_setter(self):
        p = Prop()
        for i in range(2000):
            p.x = -i
        self.assertEqual(p.x, 1999)
        self.assertEqual(p.calls, 2000)

    def test_self_stores_in_init(self):
        for i in range(2000):
            o = InitStores(i, "b")
            self.assertEqual(o.a, ("a", i))
            self.assertEqual(o.b, ("b", "b"))

    def test_lifetimes(self):
        h = Hold()
        for _ in range(500):
            h.t = Tracked()
        before = Tracked.deleted
        t = Tracked()
        r = weakref.ref(t)
        h.t = t
        del t
        self.assertIsNotNone(r())
        h.t = None
        self.assertIsNone(r())
        self.assertEqual(Tracked.deleted, before + 2)
        # The receiver's references are what they were.
        n = sys.getrefcount(h)
        for i in range(1000):
            h.u = i
        self.assertEqual(sys.getrefcount(h), n)

    def test_hook_replaced(self):
        class C:
            def __setattr__(self, k, v):
                object.__setattr__(self, k, v + 1)

        c = C()
        for i in range(1000):
            c.x = i
        self.assertEqual(c.x, 1000)

        def other(self, k, v):
            object.__setattr__(self, k, -v)

        C.__setattr__ = other
        for i in range(1000):
            c.x = i
        self.assertEqual(c.x, -999)
        del C.__setattr__
        for i in range(1000):
            c.x = i
        self.assertEqual(c.x, 999)

    def test_hook_with_generator_caller(self):
        o = Log()

        def g():
            for i in range(300):
                o.y = i
                yield i

        self.assertEqual(sum(g()), sum(range(300)))
        self.assertEqual(o.y, 299)

    def test_collect_during(self):
        class Cyc:
            def __setattr__(self, k, v):
                gc.collect() if v % 500 == 0 else None
                object.__setattr__(self, k, v)

        c = Cyc()
        for i in range(1500):
            c.z = i
        self.assertEqual(c.z, 1499)


if __name__ == "__main__":
    unittest.main()
