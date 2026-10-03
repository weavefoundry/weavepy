"""A compiled leaf plan's last attribute store lands at once.

An effect leaf (a method whose only side effects are attribute stores)
buffers its stores until it returns; a store nothing after it can decline
lands directly instead. Each loop here runs long enough for the plan to
compile; then the receiver changes shape, and every store must behave as
the interpreter's would. The drivers read `locals()` on a path they never
take, which keeps the tier-2 compiler (whose calls take other paths) off
them.
"""

import unittest

WARM = 33000


class Cell:
    def __init__(self):
        self.v = 0


class Node:
    def __init__(self, x, peer):
        self.x = x
        self.peer = peer

    def put(self):
        self.peer.v = self.x


def drive(o, n):
    if n < 0:
        return locals()
    for _ in range(n):
        o.put()


class Noisy:
    deaths = 0

    def __del__(self):
        Noisy.deaths += 1


class PlanStoreTests(unittest.TestCase):
    def test_plain_store(self):
        c = Cell()
        n = Node(7, c)
        drive(n, WARM)
        self.assertEqual(c.v, 7)
        n.x = "s"
        drive(n, 3)
        self.assertEqual(c.v, "s")

    def test_receiver_is_self(self):
        n = Node(5, None)
        n.peer = n
        drive(n, WARM)
        self.assertEqual(n.v, 5)
        m = Node(6, None)
        m.peer = m
        drive(m, 1)
        self.assertEqual(vars(m), {"x": 6, "peer": m, "v": 6})

    def test_setter_and_finalizer(self):
        c = Cell()
        n = Node(1, c)
        drive(n, WARM)

        class Watched:
            def __init__(self):
                self.seen = []

            @property
            def v(self):
                return self.seen[-1]

            @v.setter
            def v(self, value):
                self.seen.append(value)

        w = Watched()
        n.peer = w
        drive(n, 2)
        self.assertEqual(w.seen, [1, 1])
        n.peer = c
        c.v = Noisy()
        before = Noisy.deaths
        drive(n, 1)
        self.assertEqual(Noisy.deaths, before + 1)
        self.assertEqual(c.v, 1)

    def test_slots_receiver(self):
        class Slotted:
            __slots__ = ("v",)

        c = Cell()
        n = Node(3, c)
        drive(n, WARM)
        s = Slotted()
        n.peer = s
        drive(n, 1)
        self.assertEqual(s.v, 3)

    def test_scalar_stores_in_place(self):
        class Src:
            def __init__(self, x):
                self.x = x

        class Dst:
            def __init__(self):
                self.v = 0
                self.w = 1

        class Copy:
            def __init__(self, a, b):
                self.a = a
                self.b = b

            def put(self):
                self.b.v = self.a.x

        src, dst = Src(1), Dst()
        c = Copy(src, dst)
        drive(c, WARM)
        self.assertEqual(dst.v, 1)
        for value in (2.5, True, None, -7, 2**62):
            src.x = value
            drive(c, 1)
            self.assertIs(type(dst.v), type(value))
            self.assertEqual(dst.v, value)
        # A heap value, over a scalar and then over itself.
        src.x = [1]
        drive(c, 2)
        self.assertIs(dst.v, src.x)
        src.x = 3
        drive(c, 1)
        self.assertEqual(dst.v, 3)
        # Another instance of the class, without the field yet.
        fresh = Dst.__new__(Dst)
        c.b = fresh
        drive(c, 1)
        self.assertEqual(vars(fresh), {"v": 3})
        # An instance whose dictionary was handed out.
        other = Dst()
        d = other.__dict__
        c.b = other
        src.x = 9
        drive(c, 1)
        self.assertEqual(d["v"], 9)
        # The class changes under the cached version.
        c.b = dst
        Dst.extra = 1
        src.x = 11
        drive(c, 1)
        self.assertEqual(dst.v, 11)
        Dst.v = property(lambda self: -1, lambda self, value: None)
        drive(c, 1)
        self.assertEqual(dst.v, -1)


if __name__ == "__main__":
    unittest.main()
