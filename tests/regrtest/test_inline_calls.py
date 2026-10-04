"""Python calls the core loop switches to in place.

A call of a function that its call site has seen before runs in a pooled
activation (with its own cells when it has cell variables, and its
`*args` tuple built by the site), and its return hands the result
straight back; `with` loads its `__enter__` and `__exit__` from the site,
and a method call site remembers the leaf methods of several classes.
Each loop here runs long enough for the sites to settle; then the callee,
its arguments or its frame change, and every call must still behave as
the interpreter's general call does.
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

    def test_cell_variables(self):
        def adder(n):
            def add(x):
                return x + n
            return add

        def counter(start):
            count = start

            def bump():
                nonlocal count
                count += 1
                return count
            bump()
            return [bump, count]

        def shadow(a, b):
            def get():
                return a, b
            a = a * 2
            return get

        def peek(n):
            def inner():
                return n
            return sorted(sys._getframe().f_locals)

        def fails(n):
            def inner():
                return n
            return [inner][n]

        made = [adder(i) for i in range(WARM)]
        self.assertEqual([f(1) for f in made[:3]], [1, 2, 3])
        self.assertEqual(sum(f(0) for f in made), sum(range(WARM)))
        for i in range(WARM):
            bump, seen = counter(i)
            self.assertEqual(seen, i + 1)
            self.assertEqual(bump(), i + 2)
            self.assertEqual(shadow(i, 1)(), (2 * i, 1))
        self.assertEqual(peek(3), ["inner", "n"])
        for i in range(WARM):
            self.assertEqual(fails(0)(), 0)
        with self.assertRaises(IndexError):
            fails(1)

    def test_cell_variable_finalizers(self):
        seen = []

        class Note:
            def __del__(self):
                seen.append(1)

        def hold(i):
            n = Note()

            def keep():
                return n
            return i

        def escape(i):
            n = Note()

            def keep():
                return n
            return keep

        for i in range(WARM):
            hold(i)
        self.assertEqual(len(seen), WARM)
        kept = [escape(i) for i in range(100)]
        self.assertEqual(len(seen), WARM)
        del kept
        self.assertEqual(len(seen), WARM + 100)

    def test_with_statements(self):
        log = []

        class CM:
            def __enter__(self):
                return self

            def __exit__(self, *exc):
                log.append(exc[0])
                return False

        def use(cm):
            with cm as c:
                return c

        cm = CM()
        for _ in range(WARM):
            self.assertIs(use(cm), cm)
        self.assertEqual(log, [None] * WARM)
        log.clear()
        # Special lookup reads the class, never the instance.
        cm.__enter__ = lambda: "instance"
        self.assertIs(use(cm), cm)
        # A rebound class attribute takes effect at once.
        CM.__enter__ = lambda self: "rebound"
        self.assertEqual(use(cm), "rebound")
        CM.__enter__ = staticmethod(lambda: "static")
        self.assertEqual(use(cm), "static")

        class Suppress(CM):
            def __exit__(self, kind, value, tb):
                log.append(kind)
                return True

        def boom(cm):
            with cm:
                raise KeyError(1)
            return "suppressed"

        for _ in range(WARM):
            self.assertEqual(boom(Suppress()), "suppressed")
        self.assertEqual(log[-1], KeyError)
        with self.assertRaises(KeyError):
            boom(CM())

    def test_star_args(self):
        def va(*args):
            return args

        def some(a, b=2, *rest):
            return a, b, rest

        def run(n):
            t = 0
            for i in range(n):
                t += len(va(i, i, i)) + len(va()) + some(i)[1] + len(some(i, 1, 2, 3)[2])
            return t

        self.assertEqual(run(WARM), WARM * 7)
        self.assertEqual(va(1, 2), (1, 2))
        self.assertEqual(some(1, 5, 6), (1, 5, (6,)))
        self.assertIs(type(va()), tuple)

    def test_polymorphic_leaf_sites(self):
        class Base:
            FORWARD = 1

            def __init__(self, k):
                self.k = k
                self.direction = 1

            def output(self):
                return self.k if self.direction == Base.FORWARD else -self.k

        class A(Base):
            pass

        class B(Base):
            pass

        class C:
            def __init__(self, k):
                self.k = k

            def output(self):
                return self.k * 10

        class D(C):
            pass

        objs = [A(1), B(2), C(3), D(4)]

        def total(n):
            t = 0
            for i in range(n):
                for o in objs:
                    t += o.output()
            return t

        self.assertEqual(total(WARM), WARM * (1 + 2 + 30 + 40))
        # A class's method changes: only its instances see the new one.
        B.output = lambda self: 100
        self.assertEqual(total(10), 10 * (1 + 100 + 30 + 40))
        # A function's code changes under the same class version.
        def other(self):
            return 7
        C.output.__code__ = other.__code__
        self.assertEqual(total(10), 10 * (1 + 100 + 7 + 7))
        # An instance attribute shadows its class's method.
        objs[0].output = lambda: 1000
        self.assertEqual(total(10), 10 * (1000 + 100 + 7 + 7))
        del objs[0].output
        objs[1].direction = 0
        self.assertEqual(total(10), 10 * (1 + 100 + 7 + 7))
        del B.output
        self.assertEqual(total(10), 10 * (1 - 2 + 7 + 7))

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
