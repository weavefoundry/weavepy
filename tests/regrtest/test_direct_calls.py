"""Calls between compiled functions that run the callee directly.

A hot loop's call of a plain Python function whose body is compiled runs
the callee straight from the caller's native code and finishes its return
there. Every way a call can end differently (a raise, a frame the callee
looks at, a finalizer its return queues, a swapped `__code__`, a tracer,
deep recursion) must behave as an ordinary call does.
"""

import sys
import unittest

N = 3000
FORCED = "--forced-frame-jit"


def body(a):
    t = [a]
    return t[0] + 1


def loop(n):
    s = 0
    for i in range(n):
        s += body(i)
    return s


def maybe_raise(a):
    t = [a]
    if a == N - 7:
        raise ValueError(a)
    return t[0]


def raise_loop(n):
    s = 0
    for i in range(n):
        s += maybe_raise(i)
    return s


def caller_name(a):
    t = [a]
    if a == N - 3:
        return sys._getframe(1).f_code.co_name
    return t[0]


def frame_loop(n):
    out = None
    for i in range(n):
        r = caller_name(i)
        if isinstance(r, str):
            out = r
    return out


class Noisy:
    def __init__(self, log):
        self.log = log

    def __del__(self):
        self.log.append("del")


def make_noisy(log, a):
    obj = Noisy(log)
    t = [obj]
    return a + len(t)


def finalizer_loop(n, log):
    seen = []
    for i in range(n):
        make_noisy(log, i)
        seen.append(len(log))
    return seen


class Counter:
    def __init__(self):
        self.n = 0

    def bump(self, k):
        t = [k]
        self.n = self.n + t[0]
        return self


def method_loop(n):
    c = Counter()
    for i in range(n):
        c.bump(i)
    return c.n


def depth(k):
    t = [k]
    if k == 0:
        return 0
    return depth(k - 1) + t[0] - k + 1


def first(a):
    t = [a]
    return t[0] * 2


def second(a):
    t = [a]
    return t[0] * 3


def swap_loop(n):
    s = 0
    for i in range(n):
        if i == n // 2:
            first.__code__ = second.__code__
        s += first(i)
    return s


def fresh_loop(n):
    s = 0
    for i in range(n):
        def inner(a):
            t = [a]
            return t[0] + 1
        s += inner(i)
    return s


class DirectCallTest(unittest.TestCase):
    def test_forced_frame_jit(self):
        # The same cases with small bodies compiled by the frame JIT at
        # once (tier 2 off), so every call above runs directly.
        if FORCED in sys.argv:
            self.skipTest("already forced")
        import os
        import subprocess

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

    def test_result(self):
        self.assertEqual(loop(N), sum(i + 1 for i in range(N)))

    def test_raise_propagates_with_traceback(self):
        try:
            raise_loop(N)
        except ValueError as e:
            exc = e
        else:
            self.fail("no raise")
        self.assertEqual(exc.args, (N - 7,))
        names = []
        tb = exc.__traceback__
        while tb is not None:
            names.append(tb.tb_frame.f_code.co_name)
            tb = tb.tb_next
        self.assertEqual(names[-2:], ["raise_loop", "maybe_raise"])

    def test_callee_sees_caller_frame(self):
        self.assertEqual(frame_loop(N), "frame_loop")

    def test_finalizer_runs_before_caller_continues(self):
        log = []
        seen = finalizer_loop(N, log)
        self.assertEqual(seen, list(range(1, N + 1)))

    def test_method_calls(self):
        self.assertEqual(method_loop(N), sum(range(N)))

    def test_recursion(self):
        for _ in range(20):
            self.assertEqual(depth(200), 200)
        with self.assertRaises(RecursionError):
            depth(sys.getrecursionlimit() + 100)
        self.assertEqual(depth(200), 200)

    def test_swapped_code(self):
        saved = first.__code__
        try:
            n = 4000
            expect = sum(i * 2 for i in range(n // 2)) + sum(
                i * 3 for i in range(n // 2, n)
            )
            self.assertEqual(swap_loop(n), expect)
        finally:
            first.__code__ = saved

    def test_fresh_functions(self):
        self.assertEqual(fresh_loop(N), sum(i + 1 for i in range(N)))

    def test_tracer_sees_calls(self):
        loop(N)
        calls = []

        def tracer(frame, event, arg):
            if event == "call" and frame.f_code is body.__code__:
                calls.append(frame.f_locals["a"])
            return None

        sys.settrace(tracer)
        try:
            loop(50)
        finally:
            sys.settrace(None)
        self.assertEqual(calls, list(range(50)))


if __name__ == "__main__":
    unittest.main(argv=[a for a in sys.argv if a != FORCED])
