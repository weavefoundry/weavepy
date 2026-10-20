"""Constructors of `__slots__` classes whose `__init__` only stores.

A hot `cls(args)` of a class whose instances lay their slots out, with an
`__init__` that is nothing but `self.name = <parameter or constant>`
stores, fills the fresh instance's slots straight from the arguments once
every store's site has proved its slot. Recycled instances keep their
slot storage for the next instance of the same class. Every way such a
construction can differ (an unset slot, a parameter stored twice or not
at all, a default, the instance stored into itself, a class changed after
warm-up, a subclass's layout, a deleted slot) must behave as the
ordinary call does.
"""

import gc
import sys
import unittest

N = 3000
FORCED = "--forced-frame-jit"


class V:
    __slots__ = ("x", "y", "z")

    def __init__(self, x, y, z):
        self.x = x
        self.y = y
        self.z = z


class Partial:
    __slots__ = ("a", "b", "c")

    def __init__(self, a, b):
        self.b = b
        self.a = a


class Twice:
    __slots__ = ("p", "q", "tag", "n")

    def __init__(self, v, w=None):
        self.p = v
        self.q = v
        self.tag = "t"
        self.n = 3


class Selfish:
    __slots__ = ("me", "v", "__weakref__")

    def __init__(self, v):
        self.me = self
        self.v = v


class Base:
    __slots__ = ("u", "w")

    def __init__(self, u, w):
        self.u = u
        self.w = w


class Derived(Base):
    __slots__ = ("extra",)


class Other:
    __slots__ = ("k", "m")

    def __init__(self):
        pass


class Plain:
    def __init__(self, x):
        self.x = x


class Changing:
    __slots__ = ("x", "y")

    def __init__(self, x, y):
        self.x = x
        self.y = y


def build_vs(n):
    out = []
    for i in range(n):
        out.append(V(i, i + 1, i + 2))
    return out


def build_partial(n):
    s = 0
    for i in range(n):
        p = Partial(i, -i)
        s += p.a + p.b
    return s, p


def build_twice(n):
    for i in range(n):
        t = Twice([i])
    return t


def build_selfish(n):
    for i in range(n):
        s = Selfish(i)
    return s


def build_derived(n):
    for i in range(n):
        d = Derived(i, -i)
    return d


def churn(n):
    s = 0
    for i in range(n):
        v = V(i, i, i)
        s += v.z
    return s


def build_changing(n):
    for i in range(n):
        c = Changing(i, i + 1)
    return c


class SlotStoreInitTest(unittest.TestCase):
    def test_forced_frame_jit(self):
        # The same cases with small bodies compiled by the frame JIT at
        # once (tier 2 off).
        if FORCED in sys.argv:
            self.skipTest("already forced")
        import os
        import subprocess

        env = dict(os.environ)
        env.update(
            WEAVEPY_JIT="0",
            WEAVEPY_FRAME_JIT_TUNE="8,1,3,0",
            WEAVEPY_FRAME_JIT_HOT="50",
        )
        r = subprocess.run(
            [sys.executable, __file__, FORCED],
            env=env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_values(self):
        vs = build_vs(N)
        for i, v in enumerate(vs):
            self.assertEqual((v.x, v.y, v.z), (i, i + 1, i + 2))

    def test_unset_slot(self):
        s, p = build_partial(N)
        self.assertEqual(s, 0)
        self.assertEqual((p.a, p.b), (N - 1, 1 - N))
        with self.assertRaises(AttributeError):
            p.c

    def test_shared_parameter_constants_and_default(self):
        t = build_twice(N)
        self.assertIs(t.p, t.q)
        self.assertEqual(t.p, [N - 1])
        # The list is held by both slots only (and the call's argument).
        self.assertEqual(sys.getrefcount(t.p), 3)
        self.assertEqual((t.tag, t.n), ("t", 3))

    def test_instance_stored_into_itself(self):
        import weakref

        s = build_selfish(N)
        self.assertIs(s.me, s)
        self.assertEqual(s.v, N - 1)
        r = weakref.ref(s)
        del s
        gc.collect()
        self.assertIsNone(r())

    def test_subclass_layout(self):
        d = build_derived(N)
        self.assertEqual((d.u, d.w), (N - 1, 1 - N))
        with self.assertRaises(AttributeError):
            d.extra

    def test_recycled_instances_start_unset(self):
        self.assertEqual(churn(N), sum(range(N)))
        for _ in range(100):
            o = Other()
            with self.assertRaises(AttributeError):
                o.k
            with self.assertRaises(AttributeError):
                o.m
            p = Plain(5)
            self.assertEqual(p.x, 5)
            self.assertEqual(vars(p), {"x": 5})

    def test_deleted_slot(self):
        vs = build_vs(10)
        del vs[3].y
        with self.assertRaises(AttributeError):
            vs[3].y
        vs[3].y = "back"
        self.assertEqual(vs[3].y, "back")
        self.assertEqual(build_vs(N)[-1].y, N)

    def test_class_changed_after_warm_up(self):
        c = build_changing(N)
        self.assertEqual((c.x, c.y), (N - 1, N))
        log = []
        saved = Changing.x
        try:
            Changing.x = property(lambda self: 42, lambda self, v: log.append(v))
            c = build_changing(5)
            self.assertEqual(c.x, 42)
            self.assertEqual(log, [0, 1, 2, 3, 4])
            self.assertEqual(c.y, 5)
        finally:
            Changing.x = saved
        c = build_changing(N)
        self.assertEqual((c.x, c.y), (N - 1, N))


if __name__ == "__main__":
    unittest.main(argv=[a for a in sys.argv if a != FORCED])
