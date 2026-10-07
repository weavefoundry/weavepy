"""Instance operators run directly from compiled code.

A compiled `a - b` over two instances of one class whose `__sub__` is a
plain Python method (and that defines no `__rsub__`) calls the method
straight from native code. Its `NotImplemented` must still be the
operator's `TypeError`, whether the method returns in place or after it
leaves for the interpreter; an augmented operator must still prefer the
in-place method; a reflected method or a mixed pair must take the full
protocol; and a raise inside the method must propagate.
"""

import os
import subprocess
import sys
import unittest

FORCED = "--forced-frame-jit"


class V:
    __slots__ = ("x", "y")

    def __init__(self, x, y):
        self.x = x
        self.y = y

    def __sub__(self, o):
        return V(self.x - o.x, self.y - o.y)

    def __add__(self, o):
        if o.x < 0:
            return NotImplemented
        return V(self.x + o.x, self.y + o.y)

    def __mul__(self, o):
        if o.x == -1:
            raise ValueError("bad")
        return V(self.x * o.x, self.y * o.y)

    def __truediv__(self, o):
        # Leaves the direct path (a call the compiled code hands to the
        # interpreter) before its `NotImplemented`.
        if o.x == 0:
            sorted([3, 1, 2], key=lambda v: -v)
            return NotImplemented
        return V(self.x / o.x, self.y / o.y)


class W(V):
    __slots__ = ()

    def __isub__(self, o):
        self.x -= o.x
        return self


class R:
    def __init__(self, v):
        self.v = v

    def __sub__(self, o):
        return R(self.v - o.v)

    def __rsub__(self, o):
        return "reflected"


def chain(a, b, n):
    s = 0.0
    for _ in range(n):
        c = a.x + b.x
        d = (a - b) - b
        s += d.x + c
    return s


def add_until(a, b, n):
    out = None
    for _ in range(n):
        out = a + b
    return out


def mul_until(a, b, n):
    out = None
    for _ in range(n):
        out = a * b
    return out


def div_until(a, b, n):
    out = None
    for _ in range(n):
        out = a / b
    return out


def isub_loop(w, b, n):
    for _ in range(n):
        w -= b
    return w


def sub_loop(a, b, n):
    out = None
    for _ in range(n):
        out = a - b
    return out


class InstanceOperatorTest(unittest.TestCase):
    def test_results(self):
        a, b = V(10.0, 4.0), V(1.0, 1.0)
        self.assertEqual(chain(a, b, 3000), 3000 * (8.0 + 11.0))

    def test_not_implemented_is_type_error(self):
        a = V(1.0, 1.0)
        add_until(a, V(2.0, 2.0), 3000)
        with self.assertRaisesRegex(TypeError, r"for \+: 'V' and 'V'"):
            add_until(a, V(-1.0, 0.0), 3000)

    def test_not_implemented_after_leaving(self):
        a = V(1.0, 1.0)
        div_until(a, V(2.0, 2.0), 3000)
        with self.assertRaisesRegex(TypeError, r"for /: 'V' and 'V'"):
            div_until(a, V(0.0, 1.0), 3000)

    def test_raise_propagates(self):
        a = V(1.0, 1.0)
        mul_until(a, V(2.0, 2.0), 3000)
        with self.assertRaisesRegex(ValueError, "bad"):
            mul_until(a, V(-1.0, 1.0), 3000)

    def test_inplace_prefers_inplace_method(self):
        w = W(100.0, 0.0)
        r = isub_loop(w, V(1.0, 0.0), 50)
        self.assertIs(r, w)
        self.assertEqual(w.x, 50.0)

    def test_reflected_and_mixed(self):
        self.assertEqual(sub_loop(R(5), R(2), 3000).v, 3)
        self.assertEqual(sub_loop(3, R(2), 3000), "reflected")
        self.assertEqual(sub_loop(W(5.0, 0.0), V(2.0, 0.0), 3000).x, 3.0)

    def test_forced_frame_jit(self):
        if FORCED in sys.argv:
            self.skipTest("already forced")
        env = dict(os.environ)
        env.update(
            WEAVEPY_JIT="0",
            WEAVEPY_FRAME_JIT_TUNE="8,1,3,0",
            WEAVEPY_FRAME_JIT_HOT="50",
        )
        r = subprocess.run(
            [sys.executable, __file__, FORCED],
            env=env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 0, r.stderr)


if __name__ == "__main__":
    unittest.main(argv=[a for a in sys.argv if a != FORCED])
