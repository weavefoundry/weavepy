"""Methods that add to a `__slots__` member keep their meaning in hot loops.

A method whose body is `self.x += k; return self.x` runs in line in a
compiled loop once it has run a few times, for a field in an instance
dictionary and for a `__slots__` member alike. Whatever the loop meets
afterwards (another class, an unset or non-int member, an overflow, a
replaced method or code object, a subclass whose instances can shadow
the method) gives the result the plain call gives.
"""

import unittest

N = 6000


class Slotted:
    __slots__ = ("a", "b", "c")

    def __init__(self):
        self.a = 1
        self.b = 2.0
        self.c = "x"

    def tick(self):
        self.a += 1
        return self.a

    def bump(self, n):
        self.a += n
        return self.a


class Plain:
    def __init__(self):
        self.a = 1

    def tick(self):
        self.a += 1
        return self.a


def ticks(obj, n):
    total = 0
    for _ in range(n):
        total += obj.tick()
    return total


def bumps(obj, n, by):
    total = 0
    for _ in range(n):
        total += obj.bump(by)
    return total


def expected_ticks(start, n):
    # start+1 + start+2 + ... + start+n
    return n * start + n * (n + 1) // 2


class SlotUpdateTests(unittest.TestCase):
    def test_tick_and_bump(self):
        s = Slotted()
        self.assertEqual(ticks(s, N), expected_ticks(1, N))
        self.assertEqual(s.a, 1 + N)
        s2 = Slotted()
        self.assertEqual(bumps(s2, N, 3), sum(1 + 3 * k for k in range(1, N + 1)))
        self.assertEqual(s2.a, 1 + 3 * N)
        self.assertEqual((s2.b, s2.c), (2.0, "x"))

    def test_both_kinds_of_class_in_one_loop(self):
        objs = [Slotted(), Plain(), Slotted(), Plain()]
        total = 0
        for i in range(N):
            total += objs[i % 4].tick()
        per = N // 4
        self.assertEqual(total, 4 * expected_ticks(1, per))
        self.assertTrue(all(o.a == 1 + per for o in objs))

    def test_overflow_into_a_big_int(self):
        s = Slotted()
        ticks(s, N)
        s.a = 2**63 - 3
        got = [s.tick() for _ in range(6)]
        self.assertEqual(got, [2**63 - 2 + k for k in range(6)])
        self.assertEqual(ticks(s, 10), sum(2**63 + 3 + k for k in range(1, 11)))
        s.a = -(2**63) + 1
        self.assertEqual(bumps(s, 3, -1), sum(-(2**63) + 1 - k for k in range(1, 4)))

    def test_member_of_another_type(self):
        s = Slotted()
        ticks(s, N)
        s.a = 1.5
        self.assertEqual(ticks(s, 4), 2.5 + 3.5 + 4.5 + 5.5)
        s.a = True
        self.assertEqual(s.tick(), 2)
        s.a = "ab"
        with self.assertRaises(TypeError):
            ticks(s, 3)
        self.assertEqual(s.a, "ab")

    def test_unset_member(self):
        s = Slotted()
        ticks(s, N)
        del s.a
        with self.assertRaises(AttributeError):
            ticks(s, 3)
        s.a = 10
        self.assertEqual(ticks(s, 3), 11 + 12 + 13)

    def test_method_replaced_on_the_class(self):
        class C:
            __slots__ = ("a",)

            def __init__(self):
                self.a = 0

            def tick(self):
                self.a += 1
                return self.a

        c = C()
        self.assertEqual(ticks(c, N), expected_ticks(0, N))
        C.tick = lambda self: -1
        self.assertEqual(ticks(c, 50), -50)
        self.assertEqual(c.a, N)

    def test_code_object_swapped(self):
        class C:
            __slots__ = ("a",)

            def __init__(self):
                self.a = 0

            def tick(self):
                self.a += 1
                return self.a

        def other(self):
            self.a += 100
            return self.a

        c = C()
        ticks(c, N)
        C.tick.__code__ = other.__code__
        self.assertEqual(c.tick(), N + 100)
        self.assertEqual(ticks(c, 10), sum(N + 100 + 100 * k for k in range(1, 11)))

    def test_subclass_instances_can_shadow_the_method(self):
        class Sub(Slotted):
            pass  # no __slots__: its instances have a dictionary

        s = Sub()
        self.assertEqual(ticks(s, N), expected_ticks(1, N))
        s.tick = lambda: -7
        self.assertEqual(ticks(s, 20), -140)
        del s.tick
        self.assertEqual(s.tick(), N + 2)
        base = Slotted()
        self.assertEqual(ticks(base, N), expected_ticks(1, N))

    def test_subclass_with_more_slots(self):
        class More(Slotted):
            __slots__ = ("d",)

            def __init__(self):
                super().__init__()
                self.d = 9

        m = More()
        b = Slotted()
        total = 0
        for i in range(N):
            total += (m if i % 2 else b).tick()
        self.assertEqual(total, 2 * expected_ticks(1, N // 2))
        self.assertEqual((m.a, m.d, b.a), (1 + N // 2, 9, 1 + N // 2))

    def test_descriptor_installed_over_the_member(self):
        class C:
            __slots__ = ("a",)

            def __init__(self):
                self.a = 0

            def tick(self):
                self.a += 1
                return self.a

        c = C()
        ticks(c, N)
        seen = []
        member = C.__dict__["a"]

        class Watch:
            def __get__(self, obj, cls):
                seen.append("get")
                return member.__get__(obj, cls)

            def __set__(self, obj, value):
                seen.append("set")
                member.__set__(obj, value)

        C.a = Watch()
        self.assertEqual(c.tick(), N + 1)
        self.assertEqual(seen, ["get", "set", "get"])
        self.assertEqual(ticks(c, 5), sum(N + 1 + k for k in range(1, 6)))

    def test_fixture_shape(self):
        # Reads, float stores, both ticks and a string compare in one loop.
        p, s = Plain(), Slotted()
        p.b, p.c = 2.0, "x"
        total = 0
        for _ in range(N):
            total += p.a + s.a
            p.b = p.b + 0.5
            s.b = s.b + 0.5
            total += p.tick() + s.tick()
            if p.c == s.c:
                total += 1
        self.assertEqual(p.a, 1 + N)
        self.assertEqual(s.a, 1 + N)
        self.assertEqual(p.b, 2.0 + N * 0.5)
        self.assertEqual(s.b, 2.0 + N * 0.5)
        self.assertEqual(total, 2 * sum(range(1, N + 1)) + 2 * expected_ticks(1, N) + N)


if __name__ == "__main__":
    unittest.main()
