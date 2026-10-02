"""Compiled code runs a method that only updates a field in line.

A method whose body is `self.n += k; return self.n` runs without a call
once the tier-2 helper has run it: each loop below runs long enough for
that, then the receiver, its class, the method or the field changes under
the compiled code, and every change must reach the generic path with the
interpreter's result.
"""

import sys
import unittest


class Counter:
    def __init__(self):
        self.n = 0

    def bump(self, by):
        self.n += by
        return self.n

    def tick(self):
        self.n += 1
        return self.n


def bump_loop(c, n):
    t = 0
    for _ in range(n):
        t += c.bump(2)
    return t


def tick_loop(c, n):
    t = 0
    for _ in range(n):
        t += c.tick()
    return t


def tick_all(items, n):
    for _ in range(n):
        for c in items:
            c.tick()
    return [c.n for c in items]


def triangle(k, n):
    # The sum of k, 2k, ..., nk.
    return k * n * (n + 1) // 2


class InlineMethodTests(unittest.TestCase):
    N = 3000

    def test_updates(self):
        c = Counter()
        self.assertEqual(bump_loop(c, self.N), triangle(2, self.N))
        self.assertEqual(c.n, 2 * self.N)
        d = Counter()
        self.assertEqual(tick_loop(d, self.N), triangle(1, self.N))
        self.assertEqual(tick_all([Counter() for _ in range(5)], self.N), [self.N] * 5)

    def test_instance_shadows_the_method(self):
        c = Counter()
        self.assertEqual(bump_loop(c, self.N), triangle(2, self.N))
        c.bump = lambda by: 1000
        self.assertEqual(bump_loop(c, 10), 10000)
        self.assertEqual(c.n, 2 * self.N)
        del c.bump
        self.assertEqual(bump_loop(c, 1), 2 * self.N + 2)

    def test_other_instance_shadows_the_method(self):
        c = Counter()
        self.assertEqual(tick_loop(c, self.N), triangle(1, self.N))
        d = Counter()
        d.tick = lambda: -1
        self.assertEqual(tick_loop(d, 10), -10)
        self.assertEqual(tick_loop(c, 10), 10 * self.N + triangle(1, 10))
        e = Counter()
        self.assertEqual(tick_loop(e, self.N), triangle(1, self.N))

    def test_class_rebinds_the_method(self):
        class C(Counter):
            pass

        c = C()
        self.assertEqual(tick_loop(c, self.N), triangle(1, self.N))

        def tick(self):
            self.n += 10
            return -self.n

        C.tick = tick
        self.assertEqual(tick_loop(c, 1), -(self.N + 10))
        del C.tick
        self.assertEqual(tick_loop(c, 1), self.N + 11)

    def test_code_reassignment(self):
        class C:
            def __init__(self):
                self.n = 0

            def tick(self):
                self.n += 1
                return self.n

        def other(self):
            self.n += 100
            return 0

        c = C()
        self.assertEqual(tick_loop(c, self.N), triangle(1, self.N))
        saved = C.tick.__code__
        C.tick.__code__ = other.__code__
        self.assertEqual(tick_loop(c, 5), 0)
        self.assertEqual(c.n, self.N + 500)
        C.tick.__code__ = saved
        self.assertEqual(tick_loop(c, 1), self.N + 501)

    def test_overflow_and_field_types(self):
        c = Counter()
        self.assertEqual(bump_loop(c, self.N), triangle(2, self.N))
        c.n = sys.maxsize - 3
        self.assertEqual(bump_loop(c, 4), 4 * sys.maxsize + 8)
        self.assertEqual(c.n, sys.maxsize + 5)
        c.n = 0.5
        self.assertEqual(bump_loop(c, 2), 7.0)
        c.n = True
        self.assertEqual(tick_loop(c, 3), 2 + 3 + 4)
        c.n = 0
        self.assertEqual(bump_loop(c, self.N), triangle(2, self.N))

    def test_receiver_changes_class(self):
        class A:
            def __init__(self):
                self.n = 0

            def tick(self):
                self.n += 1
                return self.n

        class B:
            def __init__(self):
                self.n = 0

            def tick(self):
                self.n += 2
                return self.n

        a = A()
        self.assertEqual(tick_loop(a, self.N), triangle(1, self.N))
        a.__class__ = B
        self.assertEqual(tick_loop(a, 3), 3 * self.N + triangle(2, 3))
        self.assertEqual(tick_all([A(), B(), A()], self.N), [self.N, 2 * self.N, self.N])

    def test_materialized_dict(self):
        c = Counter()
        self.assertEqual(tick_loop(c, self.N), triangle(1, self.N))
        vars(c)["n"] = 10
        self.assertEqual(tick_loop(c, 3), 11 + 12 + 13)

    def test_profiler_sees_every_call(self):
        c = Counter()
        self.assertEqual(tick_loop(c, self.N), triangle(1, self.N))
        calls = []

        def profile(frame, event, arg):
            if event == "call" and frame.f_code.co_name == "tick":
                calls.append(1)

        sys.setprofile(profile)
        try:
            tick_loop(c, 50)
        finally:
            sys.setprofile(None)
        self.assertEqual(len(calls), 50)
        self.assertEqual(c.n, self.N + 50)


class Slotted:
    __slots__ = ("n", "m")

    def __init__(self):
        self.n = 0
        self.m = 0

    def tick(self):
        self.n += 1
        return self.n

    def bump(self, by):
        self.m += by
        return self.m


class SlottedUpdateTests(unittest.TestCase):
    N = 3000

    def test_updates(self):
        s = Slotted()
        self.assertEqual(tick_loop(s, self.N), triangle(1, self.N))
        self.assertEqual(bump_loop(s, self.N), triangle(2, self.N))
        self.assertEqual((s.n, s.m), (self.N, 2 * self.N))

    def test_deleted_slot(self):
        s = Slotted()
        self.assertEqual(tick_loop(s, self.N), triangle(1, self.N))
        del s.n
        with self.assertRaises(AttributeError):
            tick_loop(s, 1)
        s.n = 5
        self.assertEqual(tick_loop(s, 2), 6 + 7)

    def test_slot_value_types(self):
        s = Slotted()
        self.assertEqual(bump_loop(s, self.N), triangle(2, self.N))
        s.m = 0.25
        self.assertEqual(bump_loop(s, 2), 2.25 + 4.25)
        s.m = sys.maxsize
        self.assertEqual(bump_loop(s, 1), sys.maxsize + 2)
        s.m = "x"
        with self.assertRaises(TypeError):
            bump_loop(s, 1)

    def test_class_replaces_the_slot(self):
        class S:
            __slots__ = ("n",)

            def __init__(self):
                self.n = 0

            def tick(self):
                self.n += 1
                return self.n

        s = S()
        self.assertEqual(tick_loop(s, self.N), triangle(1, self.N))
        slot = S.n
        seen = []
        S.n = property(lambda self: 100, lambda self, v: seen.append(v))
        self.assertEqual(tick_loop(s, 2), 200)
        self.assertEqual(seen, [101, 101])
        S.n = slot
        self.assertEqual(tick_loop(s, 1), self.N + 1)

    def test_subclass_with_dict(self):
        class D(Slotted):
            pass

        d = D()
        self.assertEqual(tick_loop(d, self.N), triangle(1, self.N))
        d.tick = lambda: -1
        self.assertEqual(tick_loop(d, 3), -3)
        self.assertEqual(tick_all([Slotted(), D(), Slotted()], 5), [5, 5, 5])


if __name__ == "__main__":
    unittest.main()
