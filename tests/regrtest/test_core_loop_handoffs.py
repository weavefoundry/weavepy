"""The core loop's in-place handoffs and its scalar arms keep exact semantics.

An instruction the core loop runs out of line (a cache-hit helper, a lane,
or the full leaf arms) goes on in the same loop when the activation is
unchanged. Releases, finalizers, raises, tracebacks and line events must
come out exactly as if every instruction ran on its own, and the unary
arms must agree with the full handler on every operand shape.
"""

import sys
import unittest
import warnings


class Slotted:
    __slots__ = ("a",)

    def __init__(self, a):
        self.a = a


class WithProp:
    def __init__(self, v):
        self._v = v

    @property
    def v(self):
        return self._v


class Plain:
    k = 3


LOG = []


class Noisy:
    def __init__(self, tag):
        self.tag = tag

    def __del__(self):
        LOG.append(("del", self.tag))


def unary_table(values):
    out = []
    for v in values:
        out.append(not v)
        try:
            out.append(-v)
        except TypeError:
            out.append("neg-type")
        try:
            out.append(~v)
        except TypeError:
            out.append("inv-type")
    return out


def handoff_mix(n):
    # Attribute and global reads that take the helper, `not` and
    # comprehension-free builtins in a loop with plain local arithmetic.
    p = WithProp(2)
    s = Slotted(5)
    total = 0
    for i in range(n):
        total += p.v + s.a + Plain.k + len("abc") + abs(-i)
        flag = not (i & 1)
        if flag:
            total -= -i
        t = (i, i + 1, i + 2, i + 3)
        total += t[3] - t[0]
    return total


def release_order(n):
    for i in range(n):
        x = Noisy(i)
        LOG.append(("bound", i))
        x = None
        LOG.append(("after", i))
    return LOG


def raise_in_helper(objs):
    seen = 0
    for o in objs:
        seen += o.v  # line A
    return seen


class CoreUnaryTest(unittest.TestCase):
    def test_unary_shapes(self):
        values = [0, 1, -1, 7, 2**62, -(2**63), 2**63 - 1, None, True, False,
                  0.0, -1.5, "", "x", (), (1,), [], [0]]
        expect = []
        for v in values:
            expect.append(not v)
            try:
                expect.append(-v)
            except TypeError:
                expect.append("neg-type")
            try:
                expect.append(~v)
            except TypeError:
                expect.append("inv-type")
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", DeprecationWarning)
            for _ in range(50):
                got = unary_table(values)
        self.assertEqual(got, expect)
        # The overflowing negation is a long, not a wrapped int.
        self.assertEqual(-(-(2**63)), 2**63)
        self.assertEqual(type(not 5), bool)

    def test_invert_bool_warns(self):
        with warnings.catch_warnings(record=True) as w:
            warnings.simplefilter("always", DeprecationWarning)
            for _ in range(3):
                unary_table([True])
        self.assertTrue(any(issubclass(x.category, DeprecationWarning) for x in w))


def branch_counts(pairs):
    counts = [0] * 6
    for a, b in pairs:
        if a < b:
            counts[0] += 1
        if a <= b:
            counts[1] += 1
        if a == b:
            counts[2] += 1
        if not a != b:
            counts[3] += 1
        if a > b:
            counts[4] += 1
        while a >= b:
            counts[5] += 1
            break
    return counts


def raise_after_compare(xs):
    for x in xs:
        if x > 2:
            raise ValueError(x)  # line B
    return None


class CompareBranchTest(unittest.TestCase):
    def test_counts(self):
        nan = float("nan")
        pairs = [(1, 2), (2, 1), (3, 3), (1.5, 2), (2, 1.5), (2.0, 2), (-1, -1.0),
                 (2**53, float(2**53)), (2**53 + 1, float(2**53)), (nan, 1.0),
                 (1.0, nan), (nan, nan), (True, 1), (False, 0.5), (-(2**63), 2**63 - 1)]
        expect = [0] * 6
        for a, b in pairs:
            expect[0] += a < b
            expect[1] += a <= b
            expect[2] += a == b
            expect[3] += not a != b
            expect[4] += a > b
            expect[5] += a >= b
        for _ in range(100):
            self.assertEqual(branch_counts(pairs), expect)

    def test_traceback_line(self):
        for _ in range(100):
            self.assertIsNone(raise_after_compare([0, 1, 2]))
        try:
            raise_after_compare([0, 1, 2, 3])
        except ValueError as e:
            tb = e.__traceback__
        else:
            self.fail("no ValueError")
        while tb.tb_next is not None:
            tb = tb.tb_next
        self.assertEqual(tb.tb_lineno, raise_after_compare.__code__.co_firstlineno + 3)


class HandoffTest(unittest.TestCase):
    def test_mixed_loop(self):
        def reference(n):
            total = 0
            for i in range(n):
                total += 2 + 5 + 3 + 3 + i
                if not (i & 1):
                    total += i
                total += 3
            return total

        for n in (0, 1, 7, 500, 3000):
            self.assertEqual(handoff_mix(n), reference(n))

    def test_release_order(self):
        # (Few enough iterations that the loop stays off tier 2.)
        LOG.clear()
        release_order(30)
        expect = []
        for i in range(30):
            expect += [("bound", i), ("del", i), ("after", i)]
        self.assertEqual(LOG, expect)

    def test_raise_from_helper(self):
        objs = [WithProp(1)] * 300 + [Plain()]
        try:
            raise_in_helper(objs)
        except AttributeError as e:
            tb = e.__traceback__
        else:
            self.fail("no AttributeError")
        while tb.tb_next is not None:
            tb = tb.tb_next
        self.assertEqual(tb.tb_frame.f_code.co_name, "raise_in_helper")
        line = raise_in_helper.__code__.co_firstlineno + 3
        self.assertEqual(tb.tb_lineno, line)

    def test_line_events(self):
        lines = []

        def tracer(frame, event, arg):
            if frame.f_code is handoff_mix.__code__ and event == "line":
                lines.append(frame.f_lineno - handoff_mix.__code__.co_firstlineno)
            return tracer

        handoff_mix(50)
        sys.settrace(tracer)
        try:
            r = handoff_mix(3)
        finally:
            sys.settrace(None)
        self.assertEqual(r, handoff_mix(3))
        # Every body line of each iteration, in order.
        self.assertEqual(lines.count(10), 2)
        self.assertEqual(lines[:4], [3, 4, 5, 6])


if __name__ == "__main__":
    unittest.main()
