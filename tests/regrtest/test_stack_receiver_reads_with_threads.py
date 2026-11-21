"""Attribute reads off temporaries while a second thread is alive.

A compiled read whose receiver is a temporary (`self._lock.__exit__`,
`make().field`) replaces the receiver with the value in its stack slot.
Once another thread exists, reference counts are shared and the cheap
release no longer applies; the read must still answer from the site's
memos, release the receiver on time, and give the same values as with
one thread.
"""

import collections
import sys
import threading
import unittest
import weakref

ROUNDS = 3000

Point = collections.namedtuple("Point", "x y")


class Guarded:
    kind = "guarded"
    limit = 10

    def __init__(self):
        self._lock = threading.Lock()
        self.entered = 0

    def __enter__(self):
        self.entered += 1
        return self._lock.__enter__()

    def __exit__(self, *args):
        # A native method read off a temporary, called with `*args`.
        return self._lock.__exit__(*args)


class Node:
    label = "node"

    def __init__(self, n):
        self.value = n
        self.child = None

    def __del__(self):
        Node.dropped += 1


Node.dropped = 0


def make_node(n):
    return Node(n)


def make_point(n):
    return Point(n, n + 1)


def frame_name():
    return sys._getframe().f_code.co_name


class Holder:
    def __init__(self):
        self.node = Node(0)
        self.point = Point(1, 2)
        self.guard = Guarded()

    def chain(self):
        return self.node.value + self.point.y + self.guard.limit

    def kinds(self):
        return self.node.label + "/" + self.guard.kind

    def namespace(self):
        return self.node.__dict__["value"]


def exercise(test):
    g = Guarded()
    h = Holder()
    for i in range(ROUNDS):
        with g:
            test.assertTrue(g._lock.locked())
        test.assertFalse(g._lock.locked())
        test.assertEqual(make_point(i).y, i + 1)
        test.assertEqual(make_node(i).value, i)
        test.assertEqual(make_node(i).label, "node")
        test.assertEqual(h.chain(), 0 + 2 + 10)
        test.assertEqual(h.kinds(), "node/guarded")
        test.assertEqual(h.namespace(), 0)
        test.assertEqual(frame_name(), "frame_name")
        test.assertEqual(type(h.node).label, "node")
    test.assertEqual(g.entered, ROUNDS)


class StackReceiverTests(unittest.TestCase):
    def test_single_thread(self):
        Node.dropped = 0
        exercise(self)
        self.assertEqual(Node.dropped, 2 * ROUNDS + 1)

    def test_with_an_idle_thread(self):
        stop = threading.Event()
        t = threading.Thread(target=stop.wait)
        t.start()
        try:
            Node.dropped = 0
            exercise(self)
            self.assertEqual(Node.dropped, 2 * ROUNDS + 1)
        finally:
            stop.set()
            t.join()

    def test_temporaries_die_at_the_read(self):
        stop = threading.Event()
        t = threading.Thread(target=stop.wait)
        t.start()
        try:
            refs = []

            def read(n):
                node = Node(n)
                refs.append(weakref.ref(node))
                return node

            for i in range(500):
                self.assertEqual(read(i).value, i)
                self.assertIsNone(refs[-1]())
        finally:
            stop.set()
            t.join()

    def test_two_threads_reading(self):
        errors = []

        def work():
            try:
                g = Guarded()
                for i in range(ROUNDS):
                    with g:
                        pass
                    if make_point(i).x != i or make_node(i).value != i:
                        errors.append(i)
            except Exception as e:  # pragma: no cover
                errors.append(e)

        ts = [threading.Thread(target=work) for _ in range(3)]
        for t in ts:
            t.start()
        for t in ts:
            t.join()
        self.assertEqual(errors, [])


if __name__ == "__main__":
    unittest.main()
