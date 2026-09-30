"""Rebinding globals while code that reads them stays hot.

Rebinding an existing global replaces its value in place, which leaves
the module dict's key layout (and every cached global load) standing;
caches that remember a global's value must still see the new one.
These cases rebind functions, classes and scalars from inside
functions, at module level, and through `globals()`, while loops that
read them run long enough to be specialized and compiled.
"""

import unittest

COUNTER = 0
SCALE = 2


def double(x):
    return x * 2


def triple(x):
    return x * 3


def apply_all(n):
    total = 0
    for i in range(n):
        total += double(i)
    return total


def scaled(n):
    total = 0
    for i in range(n):
        total += i * SCALE
    return total


def bump(n):
    global COUNTER
    for _ in range(n):
        COUNTER += 1
    return COUNTER


def rebind_double(fn):
    global double
    double = fn


class GlobalRebindingTests(unittest.TestCase):
    def tearDown(self):
        global double, SCALE
        double = GlobalRebindingTests._double
        SCALE = 2

    def test_rebound_function_is_seen(self):
        for _ in range(5):
            self.assertEqual(apply_all(1000), 999000)
        rebind_double(triple)
        for _ in range(5):
            self.assertEqual(apply_all(1000), 1498500)
        rebind_double(lambda x: 0)
        self.assertEqual(apply_all(1000), 0)

    def test_rebound_scalar_is_seen(self):
        global SCALE
        for _ in range(5):
            self.assertEqual(scaled(100), 9900)
        SCALE = 5
        self.assertEqual(scaled(100), 24750)
        globals()["SCALE"] = 7
        self.assertEqual(scaled(100), 34650)

    def test_counter_in_loop(self):
        global COUNTER
        COUNTER = 0
        self.assertEqual(bump(10000), 10000)
        self.assertEqual(bump(5), 10005)
        self.assertEqual(COUNTER, 10005)

    def test_new_global_shadows_builtin(self):
        def uses_len(xs):
            return len(xs)

        for _ in range(200):
            self.assertEqual(uses_len([1, 2, 3]), 3)
        globals()["len"] = lambda xs: -1
        try:
            self.assertEqual(uses_len([1, 2, 3]), -1)
        finally:
            del globals()["len"]
        self.assertEqual(uses_len([1, 2, 3]), 3)

    def test_module_level_rebinding(self):
        ns = {}
        exec(
            "def f():\n"
            "    return X\n"
            "X = 0\n"
            "seen = []\n"
            "for X in range(500):\n"
            "    seen.append(f())\n",
            ns,
        )
        self.assertEqual(ns["seen"], list(range(500)))


GlobalRebindingTests._double = double

if __name__ == "__main__":
    unittest.main()
