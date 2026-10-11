"""A call's released arguments and locals finalize before the caller goes on.

CPython runs a `__del__` inside the decrement that drops the last
reference: an argument only the call held dies when the call's operands
are released, and a callee's local dies with its frame, both before the
caller's next instruction. The call paths that finish a call in place (a
frameless leaf callee, a store-only constructor, a compiled callee's
direct return) must run the finalizer just as promptly, in the
interpreter's core loop, in the frame JIT, and in tier 2.
"""

import os
import subprocess
import sys
import unittest

N = 3000
MODE = "--mode"


class Tracked:
    log = []

    def __init__(self, name):
        self.name = name

    def __del__(self):
        Tracked.log.append(self.name)


class Bare:
    def __del__(self):
        Tracked.log.append(None)


class Keeps:
    __slots__ = ("kept",)

    def __init__(self, kept, dropped):
        self.kept = kept


class KeepsDict:
    def __init__(self, kept, dropped):
        self.kept = kept


def first(kept, dropped):
    return kept


def make(i):
    t = Tracked(i)
    return i + 1


def leaf_loop(n, log):
    s = 0
    for i in range(n):
        s += first(i, Bare())
        log.append(len(Tracked.log))
    return s


def slot_ctor_loop(n, log):
    for i in range(n):
        k = Keeps(i, Bare())
        log.append(len(Tracked.log))
    return k


def dict_ctor_loop(n, log):
    for i in range(n):
        k = KeepsDict(i, Bare())
        log.append(len(Tracked.log))
    return k


def local_loop(n, log):
    s = 0
    for i in range(n):
        s += make(i)
        log.append(len(Tracked.log))
    return s


def builtin_loop(n, log):
    s = 0
    for i in range(n):
        s += len([Bare()])
        log.append(len(Tracked.log))
    return s


# Tier 2 keeps a loop's temporaries pinned past their last use (the
# interpreter and the frame JIT release them at once).
PINS = sys.implementation.name == "weavepy" and MODE not in sys.argv


class CallReleaseTimingTest(unittest.TestCase):
    def setUp(self):
        Tracked.log.clear()

    def check(self, run, expect_result=None):
        log = []
        r = run(N, log)
        if expect_result is not None:
            self.assertEqual(r, expect_result)
        self.assertEqual(log, list(range(1, N + 1)))
        return r

    @unittest.skipIf(PINS, "tier 2 pins the temporary argument")
    def test_leaf_call_argument(self):
        self.check(leaf_loop, sum(range(N)))

    @unittest.skipIf(PINS, "tier 2 pins the temporary argument")
    def test_slot_constructor_argument(self):
        self.assertEqual(self.check(slot_ctor_loop).kept, N - 1)

    @unittest.skipIf(PINS, "tier 2 pins the temporary argument")
    def test_dict_constructor_argument(self):
        self.assertEqual(self.check(dict_ctor_loop).kept, N - 1)

    @unittest.skipIf(PINS, "tier 2 pins the temporary argument")
    def test_leaf_builtin_argument(self):
        self.check(builtin_loop, N)

    def test_callee_local(self):
        self.check(local_loop, sum(range(1, N + 1)))

    def test_other_tiers(self):
        if MODE in sys.argv:
            self.skipTest("already in a mode")
        modes = {
            # The frame JIT compiling small bodies at once (tier 2 off).
            "frame-jit": dict(
                WEAVEPY_JIT="0",
                WEAVEPY_FRAME_JIT_TUNE="8,1,3,0",
                WEAVEPY_FRAME_JIT_HOT="50",
            ),
            # The interpreter's core loop alone.
            "core-loop": dict(WEAVEPY_JIT="0", WEAVEPY_FRAME_JIT="0"),
        }
        for name, extra in modes.items():
            env = dict(os.environ)
            env.update(extra)
            r = subprocess.run(
                [sys.executable, __file__, MODE],
                env=env,
                capture_output=True,
                text=True,
            )
            self.assertEqual(r.returncode, 0, name + ": " + r.stderr)


if __name__ == "__main__":
    unittest.main(argv=[a for a in sys.argv if a != MODE])
