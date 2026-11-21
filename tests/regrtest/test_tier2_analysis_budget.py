"""Functions whose analysis for typed native code is costly still run right.

The typed compiler analyzes a function once for every loop it has to
leave out, and types the body of each method the function calls. Each
attempt works within a budget, and a method that didn't resolve is
remembered for the rest of the attempt. Whatever the verdict, the
function computes the same results: when the budget runs out, when one
loop's attempt fails and another's succeeds, and when a call site's
method changes after the attempt.
"""

import os
import subprocess
import sys
import unittest

ROUNDS = 400


class Shape:
    def __init__(self, n):
        self.n = n
        self.items = [n, n + 1]

    def label(self):
        # A body the typed compiler doesn't take (it formats a string).
        return "shape-%d" % self.n

    def size(self):
        return self.n * 2


def many_loops(shapes, pairs, text):
    """A loop of every kind the typed compiler leaves out, then a kernel."""
    out = []
    for s in shapes:
        out.append(s.label())
    for a, b in pairs:
        out.append(a + b)
    for ch in text:
        out.append(ch.upper())
    for s in shapes:
        out.append(len(s.items))
    for i, s in enumerate(shapes):
        out.append((i, s.label()))
    for s in reversed(shapes):
        out.append(s.size())
    for k, v in {"a": 1, "b": 2}.items():
        out.append(k * v)
    for s in shapes:
        try:
            out.append(s.missing)
        except AttributeError:
            out.append(None)
    for word in text.split():
        out.append(word[::-1])
    for s in shapes:
        out.append([x for x in s.items if x % 2])
    for s in shapes:
        out.append(str(s.n) + s.label())
    for s in shapes:
        out.append({s.n: s.label()})
    total = 0
    for i in range(50):
        total += i * i
    out.append(total)
    return out


def repeated_method_sites(s):
    """The same unresolvable method at several sites of a loop-free body."""
    return s.label() + s.label() + s.label() + "/" + s.label()


def sum_sizes(shapes, n):
    total = 0
    for i in range(n):
        for s in shapes:
            total += s.size()
    return total


class AnalysisBudgetTests(unittest.TestCase):
    def test_many_carved_loops_keep_their_results(self):
        shapes = [Shape(1), Shape(2), Shape(3)]
        pairs = [(1, 2), (3, 4)]
        want = None
        for _ in range(ROUNDS):
            got = many_loops(shapes, pairs, "ab cd")
            if want is None:
                want = got
            self.assertEqual(got, want)
        self.assertEqual(want[0], "shape-1")
        self.assertEqual(want[-1], sum(i * i for i in range(50)))
        self.assertIn(None, want)

    def test_repeated_method_sites(self):
        s = Shape(7)
        for _ in range(ROUNDS):
            self.assertEqual(
                repeated_method_sites(s), "shape-7shape-7shape-7/shape-7"
            )

    def test_method_rebound_after_the_attempts(self):
        shapes = [Shape(1), Shape(2)]
        for _ in range(ROUNDS):
            self.assertEqual(sum_sizes(shapes, 3), 18)
        original = Shape.size
        try:
            Shape.size = lambda self: self.n * 10
            for _ in range(ROUNDS):
                self.assertEqual(sum_sizes(shapes, 3), 90)
        finally:
            Shape.size = original
        for _ in range(ROUNDS):
            self.assertEqual(sum_sizes(shapes, 3), 18)

    def test_long_loop_in_one_call(self):
        # One call whose loops run long: an attempt that ran out of budget
        # would wait for the loop's own work, with the same result.
        shapes = [Shape(i) for i in range(5)]
        pairs = [(i, i + 1) for i in range(5)]

        def driver(n):
            acc = 0
            for i in range(n):
                acc += len(many_loops(shapes, pairs, "x y z"))
            return acc

        one = len(many_loops(shapes, pairs, "x y z"))
        self.assertEqual(driver(3000), 3000 * one)


class TightBudgetTests(unittest.TestCase):
    """The same checks with budgets small enough that attempts run out."""

    @unittest.skipIf(os.environ.get("WEAVEPY_T2_FREE"), "the tight run itself")
    def test_tight_budgets(self):
        for free, passes in (("0", "0"), ("0", "1"), ("64", "2")):
            env = dict(os.environ, WEAVEPY_T2_FREE=free, WEAVEPY_T2_PASSES=passes)
            r = subprocess.run(
                [sys.executable, __file__, "AnalysisBudgetTests"],
                env=env,
                capture_output=True,
                text=True,
                timeout=240,
            )
            self.assertEqual(r.returncode, 0, r.stderr[-2000:])
            self.assertIn("OK", r.stderr)


if __name__ == "__main__":
    unittest.main()
