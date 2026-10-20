"""Keyword calls between compiled functions that run the callee directly.

A hot `CALL_KW` of a plain Python function whose body is compiled binds
the keywords through the site's cached permutation and runs the callee
straight from the caller's native code. Every way such a call can differ
(defaults and keyword-only defaults, a rebound `__defaults__` or
`__kwdefaults__`, a method receiver, a raise, a frame the callee looks
at, a tracer, deep recursion, a finalizer) must behave as the ordinary
call does.
"""

import sys
import unittest

N = 3000
FORCED = "--forced-frame-jit"


def kw(a, b=2, *, c=3, d=4):
    t = [a, b, c, d]
    return t[0] * 1000 + t[1] * 100 + t[2] * 10 + t[3]


def kw_loop(n):
    s = 0
    for i in range(n):
        s += kw(i, c=5)
        s += kw(i, b=6)
        s += kw(a=i, d=7, b=1)
    return s


def expected_kw(n):
    s = 0
    for i in range(n):
        s += i * 1000 + 200 + 50 + 4
        s += i * 1000 + 600 + 30 + 4
        s += i * 1000 + 100 + 30 + 7
    return s


class Box:
    def __init__(self):
        self.n = 0

    def bump(self, k, *, twice=False):
        t = [k]
        self.n += t[0] * (2 if twice else 1)
        return self.n


def method_loop(n):
    b = Box()
    for i in range(n):
        b.bump(i, twice=(i % 2 == 0))
    return b.n


def maybe_raise(a, *, at):
    t = [a]
    if a == at:
        raise ValueError(a)
    return t[0]


def raise_loop(n):
    s = 0
    for i in range(n):
        s += maybe_raise(i, at=n - 7)
    return s


def caller_name(a, *, last):
    t = [a]
    if a == last:
        return sys._getframe(1).f_code.co_name
    return t[0]


def frame_loop(n):
    r = None
    for i in range(n):
        r = caller_name(i, last=n - 3)
        if isinstance(r, str):
            return r
    return r


def depth(n, *, step=1):
    t = [n]
    if t[0] <= 0:
        return 0
    return depth(n - step, step=step) + 1


class Tracked:
    log = []

    def __init__(self, name):
        self.name = name

    def __del__(self):
        Tracked.log.append(self.name)


def takes(a, *, dropped):
    t = [a]
    return t[0]


def finalizer_loop(n, log):
    s = 0
    for i in range(n):
        s += takes(i, dropped=Tracked(i))
        log.append(len(Tracked.log))
    return s


def defaults_loop(n):
    s = 0
    for i in range(n):
        s += kw(i, c=0)
    return s


class DirectKeywordCallTest(unittest.TestCase):
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

    def test_permutations_and_defaults(self):
        self.assertEqual(kw_loop(N), expected_kw(N))

    def test_method_with_keyword_only(self):
        expect = sum(i * (2 if i % 2 == 0 else 1) for i in range(N))
        self.assertEqual(method_loop(N), expect)

    def test_rebound_defaults(self):
        saved, saved_kw = kw.__defaults__, kw.__kwdefaults__
        try:
            self.assertEqual(defaults_loop(N), sum(i * 1000 + 204 for i in range(N)))
            kw.__defaults__ = (9,)
            self.assertEqual(defaults_loop(N), sum(i * 1000 + 904 for i in range(N)))
            kw.__kwdefaults__ = {"c": 3, "d": 8}
            self.assertEqual(defaults_loop(N), sum(i * 1000 + 908 for i in range(N)))
        finally:
            kw.__defaults__, kw.__kwdefaults__ = saved, saved_kw
        self.assertEqual(defaults_loop(N), sum(i * 1000 + 204 for i in range(N)))

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

    def test_recursion(self):
        for _ in range(20):
            self.assertEqual(depth(200, step=1), 200)
        with self.assertRaises(RecursionError):
            depth(sys.getrecursionlimit() + 100, step=1)
        self.assertEqual(depth(200, step=2), 100)

    def test_finalizer_runs_before_caller_continues(self):
        if FORCED not in sys.argv and sys.implementation.name == "weavepy":
            self.skipTest("tier 2 pins the temporary argument")
        Tracked.log.clear()
        log = []
        self.assertEqual(finalizer_loop(N, log), sum(range(N)))
        self.assertEqual(log, list(range(1, N + 1)))

    def test_tracer_sees_calls(self):
        kw_loop(N)
        calls = []

        def tracer(frame, event, arg):
            if event == "call" and frame.f_code is kw.__code__:
                calls.append(frame.f_locals["c"])
            return None

        sys.settrace(tracer)
        try:
            kw_loop(5)
        finally:
            sys.settrace(None)
        self.assertEqual(calls, [5, 3, 3] * 5)


if __name__ == "__main__":
    unittest.main(argv=[a for a in sys.argv if a != FORCED])
