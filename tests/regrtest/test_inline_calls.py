"""Plain Python calls the core loop switches to in place.

A call of a plain function (positional parameters only, no cells) that
its call site has seen before runs in a pooled activation, and its return
hands the result straight back. Each loop here runs long enough for the
sites to settle; then the callee, its arguments or its frame change, and
every call must still behave as the interpreter's general call does.
"""

import sys
import unittest

WARM = 3000


def add(a, b):
    return [a + b][0]


def ident(x):
    return [x][0]


class Box:
    def __init__(self, v):
        self.v = v

    def get(self):
        return [self.v][0]

    def put(self, v):
        self.v = [v][0]
        return self


def loop_add(n):
    t = 0
    for i in range(n):
        t += add(i, 1)
    return t


def loop_methods(n):
    b = Box(0)
    t = 0
    for i in range(n):
        t += b.put(i).get()
    return t


def depth(n):
    if n == 0:
        return [sys._getframe(0).f_code.co_name, sys._getframe(1).f_code.co_name]
    return depth(n - 1)


class InlineCallTests(unittest.TestCase):
    def test_results(self):
        self.assertEqual(loop_add(WARM), sum(range(WARM)) + WARM)
        self.assertEqual(loop_methods(WARM), sum(range(WARM)))

    def test_rebinding_the_callee(self):
        global add
        saved = add
        try:
            self.assertEqual(loop_add(WARM), sum(range(WARM)) + WARM)
            add = lambda a, b: a - b
            self.assertEqual(loop_add(10), sum(range(10)) - 10)

            def add(a, b=100):
                return a + b

            self.assertEqual(loop_add(10), sum(range(10)) + 10)
        finally:
            add = saved

    def test_code_replaced(self):
        def f(x):
            return [x][0]

        def g(x):
            return [x * 2][0]

        def run(n):
            t = 0
            for i in range(n):
                t += f(i)
            return t

        self.assertEqual(run(WARM), sum(range(WARM)))
        f.__code__ = g.__code__
        self.assertEqual(run(10), 2 * sum(range(10)))

    def test_frames_and_errors(self):
        for _ in range(WARM):
            ident(1)
        self.assertEqual(depth(5), ["depth", "depth"])

        def boom(x):
            return [x][1]

        def caller(n):
            for i in range(n):
                boom(i)

        try:
            caller(5)
        except IndexError as e:
            tb = e.__traceback__
        else:
            self.fail("no IndexError")
        names = []
        while tb:
            names.append(tb.tb_frame.f_code.co_name)
            tb = tb.tb_next
        self.assertEqual(names[-2:], ["caller", "boom"])

    def test_recursion_limit(self):
        def down(n):
            return [down(n + 1)][0]

        with self.assertRaises(RecursionError):
            down(0)
        self.assertEqual(loop_add(10), sum(range(10)) + 10)

    def test_finalizers_run_on_return(self):
        seen = []

        class Note:
            def __del__(self):
                seen.append(1)

        def make(i):
            n = Note()
            return [i][0]

        for i in range(WARM):
            make(i)
        self.assertEqual(len(seen), WARM)


if __name__ == "__main__":
    unittest.main()
