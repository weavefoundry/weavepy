"""Several accesses of one object's fields in a row each see the object as it is.

A compiled loop that reads and writes several fields of the same local
checks that object's layout once and reuses the check for the accesses
that follow. Anything in between that can change the object (a store of
a new attribute, a store that replaces a non-scalar value, a call, a
rebinding of the local, an alias) must be seen by the next access.
"""

import unittest

N = 4000


class P:
    def __init__(self, a=1, b=2.0, c=3):
        self.a = a
        self.b = b
        self.c = c

    def tick(self):
        self.a += 1
        return self.a


class S:
    __slots__ = ("a", "b", "c")

    def __init__(self, a=1, b=2.0, c=3):
        self.a = a
        self.b = b
        self.c = c


def reads(p, n):
    total = 0
    for _ in range(n):
        total += p.a + p.c
        total += p.c - p.a
    return total


def read_write(p, n):
    for _ in range(n):
        p.b = p.b + 0.5
        p.a = p.a + p.c
        p.c = p.c + 1
    return p.a, p.b, p.c


def two_receivers(p, q, n):
    total = 0
    for _ in range(n):
        total += p.a + q.a
        p.c = q.c + 1
        q.c = p.c + 1
        total += p.c + q.c
    return total


def with_ticks(p, n):
    total = 0
    for _ in range(n):
        total += p.a
        total += p.tick()
        total += p.a + p.c
    return total


def new_attribute_between(p, n):
    total = 0
    for i in range(n):
        total += p.a
        if i == n // 2:
            p.extra = i  # a new name: the instance's values move
        total += p.c
    return total


def heap_store_between(p, n):
    total = 0
    for i in range(n):
        total += p.a
        p.b = [i]  # replaces a float with a list, then back
        total += p.c
        p.b = 2.5
        total += p.a
    return total


def rebinding_between(p, q, n):
    total = 0
    for i in range(n):
        x = p
        total += x.a
        x = q
        total += x.a
        total += x.c
    return total


def call_between(p, n, f):
    total = 0
    for i in range(n):
        total += p.a
        f(p, i)
        total += p.a + p.c
    return total


def lane_change_between(p, n):
    total = 0.0
    for i in range(n):
        total += p.c
        p.c = p.c + 1
        total += p.a
    return total


class ConsecutiveAccessTests(unittest.TestCase):
    kinds = (P, S)

    def test_reads(self):
        for cls in self.kinds:
            self.assertEqual(reads(cls(5, 2.0, 7), N), N * (12 + 2))

    def test_reads_and_writes(self):
        for cls in self.kinds:
            p = cls()
            a, b, c = read_write(p, N)
            self.assertEqual(b, 2.0 + 0.5 * N)
            self.assertEqual(c, 3 + N)
            self.assertEqual(a, 1 + sum(3 + k for k in range(N)))

    def test_two_receivers_and_aliases(self):
        for cls in self.kinds:
            p, q = cls(), cls(10, 0.0, 0)
            got = two_receivers(p, q, N)
            want = 0
            pc, qc = 3, 0
            for _ in range(N):
                want += 1 + 10
                pc = qc + 1
                qc = pc + 1
                want += pc + qc
            self.assertEqual(got, want)
            # The same object under both names.
            r = cls(1, 0.0, 0)
            got = two_receivers(r, r, N)
            want = 0
            c = 0
            for _ in range(N):
                want += 2
                c = c + 1
                c = c + 1
                want += 2 * c
            self.assertEqual(got, want)

    def test_method_updates_between_reads(self):
        p = P()
        got = with_ticks(p, N)
        want = 0
        a = 1
        for _ in range(N):
            want += a
            a += 1
            want += a
            want += a + 3
        self.assertEqual(got, want)

    def test_new_attribute_between_reads(self):
        p = P()
        self.assertEqual(new_attribute_between(p, N), N * 4)
        self.assertEqual(p.extra, N // 2)
        for k in range(40):
            setattr(p, "more%d" % k, k)
        self.assertEqual(new_attribute_between(p, N), N * 4)

    def test_heap_store_between_reads(self):
        for cls in self.kinds:
            p = cls()
            self.assertEqual(heap_store_between(p, N), N * 5)
            self.assertEqual(p.b, 2.5)

    def test_rebinding_between_reads(self):
        for cls in self.kinds:
            self.assertEqual(rebinding_between(cls(1, 0.0, 2), cls(10, 0.0, 20), N), N * 31)
        self.assertEqual(rebinding_between(P(1, 0.0, 2), S(10, 0.0, 20), N), N * 31)

    def test_call_between_reads(self):
        def grow(p, i):
            if i % 1000 == 999:
                setattr(p, "g%d" % i, i)
            p.a += 1

        def dict_view(p, i):
            if i == 10:
                p.__dict__["c"] = 100

        p = P()
        got = call_between(p, N, grow)
        self.assertEqual(got, sum((1 + i) + (2 + i) + 3 for i in range(N)))
        p = P()
        got = call_between(p, N, dict_view)
        self.assertEqual(got, sum(1 + 1 + (3 if i < 10 else 100) for i in range(N)))

        class Other:
            a = 7
            c = 9

        def swap_class(p, i):
            if i == N // 2:
                p.__class__ = Q

        class Q(P):
            pass

        p = P()
        self.assertEqual(call_between(p, N, swap_class), N * 5)
        self.assertIs(type(p), Q)

    def test_lane_change_between_reads(self):
        for cls in self.kinds:
            p = cls(1, 0.0, 0)
            self.assertEqual(lane_change_between(p, N), sum(range(N)) + N)
            p.c = 0.5
            self.assertEqual(lane_change_between(p, N), sum(k + 0.5 for k in range(N)) + N)

    def test_deleted_then_restored(self):
        for cls in self.kinds:
            p = cls()
            self.assertEqual(reads(p, N), N * (4 + 2))
            del p.c
            with self.assertRaises(AttributeError):
                reads(p, 3)
            p.c = 4
            self.assertEqual(reads(p, N), N * (5 + 3))


if __name__ == "__main__":
    unittest.main()
