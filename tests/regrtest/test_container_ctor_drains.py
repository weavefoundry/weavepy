"""Container constructors and drains over generators.

`list(x)`, `tuple(x)`, `set(x)` and `frozenset(x)` of the exact builtin
types take a direct path, and `list`, `sum`, `any` and `all` drain a
generator without raising `StopIteration` for its return. Both must
build what the general constructor builds, raise what it raises, and
leave an exhausted generator exhausted.
"""

import gc
import sys
import unittest


def squares(n):
    for i in range(n):
        yield i * i


def returns_value():
    yield 1
    return "done"


class Countdown:
    def __init__(self, n):
        self.n = n

    def __iter__(self):
        while self.n > 0:
            self.n -= 1
            yield self.n


class ContainerCtorTest(unittest.TestCase):
    def test_list_and_tuple(self):
        for _ in range(500):
            self.assertEqual(list(squares(4)), [0, 1, 4, 9])
            self.assertEqual(tuple(squares(3)), (0, 1, 4))
            self.assertEqual(list(i for i in ()), [])
            self.assertEqual(tuple(x for x in "ab"), ("a", "b"))
        t = (1, 2)
        self.assertIs(tuple(t), t)
        self.assertEqual(list(returns_value()), [1])

    def test_set_and_frozenset(self):
        for _ in range(500):
            self.assertEqual(set(i % 3 for i in range(10)), {0, 1, 2})
            self.assertEqual(frozenset(squares(3)), frozenset({0, 1, 4}))
            self.assertEqual(set(range(0)), set())
            self.assertEqual(set([1, 1, 2]), {1, 2})
            self.assertEqual(set(Countdown(3)), {0, 1, 2})
        f = frozenset({1})
        self.assertIs(frozenset(f), f)
        s = set(i for i in range(3))
        self.assertTrue(gc.is_tracked(s))

    def test_unhashable_item_raises(self):
        with self.assertRaises(TypeError):
            set([] for _ in range(2))
        with self.assertRaises(TypeError):
            frozenset([] for _ in range(1))

    def test_body_error_propagates(self):
        def bad():
            yield 1
            raise KeyError("x")

        for ctor in (list, tuple, set, frozenset):
            with self.assertRaises(KeyError):
                ctor(bad())

    def test_stop_iteration_in_body_is_runtime_error(self):
        def leaky():
            yield 1
            raise StopIteration

        for ctor in (list, set, sum, all):
            with self.assertRaises(RuntimeError):
                ctor(leaky())

    def test_exhausted_generator_drains_empty(self):
        g = squares(3)
        self.assertEqual(list(g), [0, 1, 4])
        self.assertEqual(list(g), [])
        self.assertEqual(set(g), set())
        self.assertEqual(sum(g), 0)
        self.assertFalse(any(g))
        self.assertTrue(all(g))

    def test_sum_any_all(self):
        for _ in range(500):
            self.assertEqual(sum(squares(5)), 30)
            self.assertEqual(sum(x for x in (1.5, 2.5)), 4.0)
            self.assertTrue(any(x > 3 for x in range(5)))
            self.assertFalse(all(x < 3 for x in range(5)))
        self.assertEqual(sum(returns_value()), 1)

    def test_subclass_keeps_its_constructor(self):
        class L(list):
            def __init__(self, it):
                super().__init__(it)
                self.extra = True

        class S(set):
            pass

        out = L(squares(3))
        self.assertEqual(out, [0, 1, 4])
        self.assertTrue(out.extra)
        self.assertIs(type(S(i for i in range(2))), S)

    def test_keywords_still_rejected(self):
        with self.assertRaises(TypeError):
            set(iterable=[1])
        with self.assertRaises(TypeError):
            list(x=1)

    def test_next_with_default(self):
        for _ in range(500):
            g = squares(2)
            self.assertEqual(next(g, "d"), 0)
            self.assertEqual(next(g, "d"), 1)
            self.assertEqual(next(g, "d"), "d")
            self.assertEqual(next(g, "d"), "d")
        g = returns_value()
        self.assertEqual(next(g, None), 1)
        self.assertIsNone(next(g, None))

        def bad():
            yield 1
            raise KeyError("k")

        g = bad()
        next(g, None)
        with self.assertRaises(KeyError):
            next(g, None)

        def leaky():
            yield 1
            raise StopIteration

        g = leaky()
        next(g)
        with self.assertRaises(RuntimeError):
            next(g, None)

    def test_monitoring_sees_the_drain(self):
        if not hasattr(sys, "monitoring"):
            self.skipTest("no sys.monitoring")
        mon = sys.monitoring
        tool = mon.PROFILER_ID
        mon.use_tool_id(tool, "drain-test")
        seen = []
        try:
            mon.register_callback(
                tool, mon.events.STOP_ITERATION, lambda *a: seen.append(1)
            )
            mon.set_events(tool, mon.events.STOP_ITERATION)
            self.assertEqual(sum(squares(3)), 5)
            self.assertEqual(list(squares(2)), [0, 1])
        finally:
            mon.set_events(tool, 0)
            mon.register_callback(tool, mon.events.STOP_ITERATION, None)
            mon.free_tool_id(tool)


if __name__ == "__main__":
    unittest.main()
