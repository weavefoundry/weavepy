"""Calls and returns the core loop's native code switches itself keep the
interpreter's semantics: tracebacks and line numbers, recursion limits,
frames seen from callees, generators, finalizers, and callees that raise.

Each caller and callee runs long enough to be compiled (a loop by its back
edges, a loop-free function by its calls) before the checked shape.
"""

import sys
import traceback
import unittest

CALLS = 30000


class Node:
    def __init__(self, ref, pos):
        self.reference = ref if ref is not None else self
        self.pos = pos

    def find(self, update=False):
        reference = self.reference
        if reference.pos != self.pos:
            reference = reference.find(update)
            if update:
                self.reference = reference
        return reference


def callee_line():
    return sys._getframe(1).f_lineno


def caller():
    # (Enough in-line work for the body to compile.)
    x = 1
    y = x + 2
    z = y * 3 - x
    line = callee_line()
    return line, x + y + z - z - y


def maybe_raise(i):
    if i == -1:
        raise ValueError("boom %d" % i)
    return i + 1


def calls_maybe_raise(i):
    a = i + 1
    b = a * 2
    c = b - a
    t = maybe_raise(i)
    return t * 2 + (c - c)


def depth(n):
    if n == 0:
        return 0
    a = n + 1
    b = a * 2 - a
    return depth(n - 1) + 1 + (b - a)


def gen(n):
    for i in range(n):
        yield helper(i)


def helper(i):
    return i * 2


class Dying:
    count = 0

    def __del__(self):
        Dying.count += 1


def make_and_drop(i):
    a = i + 1
    b = a * 2 - a
    d = Dying()
    return b - 1


class FrameJitCallTests(unittest.TestCase):
    def test_recursive_methods(self):
        root = Node(None, 7)
        mid = Node(root, 3)
        leaf = Node(mid, 5)
        t = 0
        for _ in range(CALLS):
            t += leaf.find().pos
        self.assertEqual(t, 7 * CALLS)
        self.assertIs(leaf.find(True), root)
        self.assertIs(leaf.reference, root)

    def test_callee_sees_caller_line(self):
        first = caller()
        for _ in range(CALLS):
            self.assertEqual(caller(), first)

    def test_raise_through_compiled_frames(self):
        for i in range(CALLS):
            self.assertEqual(calls_maybe_raise(i), 2 * (i + 1))
        try:
            calls_maybe_raise(-1)
        except ValueError as e:
            frames = traceback.extract_tb(e.__traceback__)
        else:
            self.fail("no ValueError")
        self.assertEqual([f.name for f in frames][-2:], ["calls_maybe_raise", "maybe_raise"])
        self.assertEqual(
            [f.line for f in frames][-2:],
            ["t = maybe_raise(i)", 'raise ValueError("boom %d" % i)'],
        )

    def test_recursion_limit(self):
        for _ in range(200):
            self.assertEqual(depth(50), 50)
        with self.assertRaises(RecursionError):
            depth(sys.getrecursionlimit() + 100)
        self.assertEqual(depth(50), 50)

    def test_generators_calling_compiled_functions(self):
        for _ in range(300):
            self.assertEqual(sum(gen(100)), 9900)

    def test_finalizers_in_callees(self):
        Dying.count = 0
        t = 0
        for i in range(CALLS):
            t += make_and_drop(i)
        self.assertEqual(t, sum(range(CALLS)))
        self.assertEqual(Dying.count, CALLS)

    def test_constructors_and_methods(self):
        class P:
            def __init__(self, x):
                self.x = x

            def plus(self, o):
                return P(self.x + o.x)

        a, b = P(1), P(2)
        c = a
        for _ in range(CALLS):
            c = a.plus(b)
        self.assertEqual(c.x, 3)

        class Q(P):
            def plus(self, o):
                r = super().plus(o)
                r.x += 1
                return r

        q = Q(1)
        for _ in range(CALLS):
            c = q.plus(b)
        self.assertEqual(c.x, 4)


if __name__ == "__main__":
    unittest.main()
