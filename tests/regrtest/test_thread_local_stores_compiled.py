"""threading.local attribute stores from compiled loops (a global, a
local and an attribute as the receiver), and SimpleQueue put/get: the
latest value read back, each thread's values its own, a replaced value
released, a subclass that overrides __setattr__ (its hook runs), and a
queue's order, Empty, and a hand-off to another thread, each matching
CPython."""

import queue
import threading
import unittest

TL = threading.local()
dead = []


class D:
    def __init__(self, n):
        self.n = n

    def __del__(self):
        dead.append(self.n)


def store_global(n):
    for i in range(n):
        TL.x = i
    return TL.x


def store_local(n, t):
    for i in range(n):
        t.y = i
    return t.y


class LocalStoreTests(unittest.TestCase):
    def test_values(self):
        for n in (1, 300):
            self.assertEqual(store_global(n), n - 1)
            t = threading.local()
            self.assertEqual(store_local(n, t), n - 1)
            self.assertEqual(t.__dict__, {"y": n - 1})

    def test_per_thread(self):
        t = threading.local()
        store_local(300, t)
        seen = []

        def other():
            seen.append(hasattr(t, "y"))
            store_local(300, t)
            seen.append(t.y)

        th = threading.Thread(target=other)
        th.start()
        th.join()
        self.assertEqual(seen, [False, 299])
        self.assertEqual(t.y, 299)

    def test_replaced_values_released(self):
        t = threading.local()
        del dead[:]

        def fill(n):
            for i in range(n):
                t.v = D(i)

        fill(300)
        self.assertEqual(sorted(dead), list(range(299)))
        del t.v
        self.assertEqual(len(dead), 300)

    def test_subclass_hook(self):
        calls = []

        class L(threading.local):
            def __setattr__(self, name, value):
                calls.append(name)
                super().__setattr__(name, value)

        t = L()
        self.assertEqual(store_local(300, t), 299)
        self.assertEqual(len(calls), 300)


class SimpleQueueTests(unittest.TestCase):
    def test_order_and_empty(self):
        q = queue.SimpleQueue()
        for n in (1, 300):
            for i in range(n):
                q.put(i)
            self.assertEqual([q.get() for i in range(n)], list(range(n)))
            with self.assertRaises(queue.Empty):
                q.get_nowait()

    def test_hand_off(self):
        q = queue.SimpleQueue()
        out = []

        def worker():
            for i in range(300):
                out.append(q.get())

        th = threading.Thread(target=worker)
        th.start()
        for i in range(300):
            q.put(i)
        th.join()
        self.assertEqual(out, list(range(300)))


if __name__ == "__main__":
    unittest.main()
