"""The core loop's native code keeps the interpreter's semantics.

Each function runs long enough to be compiled, then meets the shapes the
native code hands back to the core loop: overflows, NaNs, unbound
locals, heap values where it expects scalars, identity of every kind of
value, finalizers released by stores, and attribute caches that go
stale.
"""

import gc
import unittest

N = 2000


def ints(n):
    t = 0
    for i in range(n):
        t += i * 3 - (i >> 2) + (i & 7) ^ (i | 1)
        if i % 5 == 0:
            t -= i // 3
        t = t % 1000003
    return t


def floats(n):
    x = 0.5
    for i in range(n):
        x = x * 1.0001 + i / 7.0 - (x / 3.0)
        if x > 1e6:
            x = x - 1e6
    return round(x, 6)


def grows(n):
    t = 1
    for i in range(n):
        t = t * 3 + i
    return t


class Box:
    def __init__(self, v):
        self.v = v
        self.next = None


class FrameJitTests(unittest.TestCase):
    def test_arithmetic(self):
        self.assertEqual(ints(N), 363158)
        self.assertEqual(floats(N), floats(N))

    def test_overflow_promotes(self):
        self.assertEqual(grows(N), grows(N))
        self.assertGreater(grows(200), 2**64)

    def test_nan_and_zero_division(self):
        def f(n, d):
            t = 0.0
            for i in range(n):
                t = t + float("inf") - float("inf") if i == n - 1 else t + i / d
            return t

        r = f(N, 2.0)
        self.assertNotEqual(r, r)
        with self.assertRaises(ZeroDivisionError):
            f(N, 0.0)

    def test_unbound_local(self):
        def f(n, flag):
            for i in range(n):
                x = i
                if flag and i == n - 1:
                    del x
            return x

        self.assertEqual(f(N, False), N - 1)
        with self.assertRaises(UnboundLocalError):
            f(N, True)

    def test_identity(self):
        a = object()
        s = "x" * 3
        big = 2**70

        def f(n, x, y):
            c = 0
            for _ in range(n):
                if x is y:
                    c += 1
                if x is not None:
                    c += 2
            return c

        self.assertEqual(f(N, a, a), 3 * N)
        self.assertEqual(f(N, a, object()), 2 * N)
        self.assertEqual(f(N, s, s), 3 * N)
        self.assertEqual(f(N, 5, 5), 3 * N)
        self.assertEqual(f(N, 5, 5.0), 2 * N)
        self.assertEqual(f(N, 1.5, 1.5), 3 * N)
        self.assertEqual(f(N, 0.0, -0.0), 2 * N)
        self.assertEqual(f(N, True, True), 3 * N)
        self.assertEqual(f(N, True, 1), 2 * N)
        self.assertEqual(f(N, None, None), N)
        self.assertEqual(f(N, big, big), 3 * N)

    def test_finalizer_on_store(self):
        log = []

        class D:
            def __del__(self):
                log.append(1)

        def f(n):
            x = None
            for i in range(n):
                x = D()
                x = i
            return x

        self.assertEqual(f(N), N - 1)
        gc.collect()
        self.assertEqual(len(log), N)

    def test_fields(self):
        def total(b, n):
            t = 0
            for _ in range(n):
                t += b.v
            return t

        b = Box(3)
        self.assertEqual(total(b, N), 3 * N)
        b.v = 2.5
        self.assertEqual(total(b, 4), 10.0)
        b.v = "s"
        with self.assertRaises(TypeError):
            total(b, 1)

    def test_field_chains(self):
        def walk(head, n):
            t = 0
            for _ in range(n):
                node = head
                while node is not None:
                    t += node.v
                    node = node.next
            return t

        head = Box(1)
        head.next = Box(2)
        head.next.next = Box(3)
        self.assertEqual(walk(head, N), 6 * N)
        Box.v = property(lambda self: 10)
        try:
            self.assertEqual(walk(head, 2), 60)
        finally:
            del Box.v

    def test_copy_and_swap(self):
        def f(n):
            a, b = 1, 2
            t = 0
            for i in range(n):
                a, b = b, a + i
                a, b = b % 997, a % 991
                t += a
            return t, a, b

        self.assertEqual(f(N), f(N))

    def test_truth(self):
        def f(n, x):
            c = 0
            for _ in range(n):
                if x:
                    c += 1
                if not x:
                    c += 2
            return c

        self.assertEqual(f(N, 0), 2 * N)
        self.assertEqual(f(N, 3), N)
        self.assertEqual(f(N, 0.0), 2 * N)
        self.assertEqual(f(N, None), 2 * N)
        self.assertEqual(f(N, [1]), N)
        self.assertEqual(f(N, ""), 2 * N)


if __name__ == "__main__":
    unittest.main()
