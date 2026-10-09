"""A trace function installed by a call that hot code makes.

Hot code runs keyword calls, `f(*args, **kwargs)` calls, builtin calls
and class constructions through the core loop's lanes. When such a call
installs a trace function and sets its caller's `f_trace` (as
`pdb.set_trace(commands=...)` does), the caller's next instruction must
report its events, not run on untraced until the function returns.
"""

import os
import subprocess
import sys
import unittest

FORCED = "--forced-frame-jit"
N = 3000


def install(*, tracer):
    caller = sys._getframe(1)
    caller.f_trace = tracer
    caller.f_trace_opcodes = True
    sys.settrace(tracer)


def install_spread(*args, **kwargs):
    caller = sys._getframe(1)
    caller.f_trace = kwargs["tracer"]
    sys.settrace(kwargs["tracer"])


def keyword_caller(n, tracer):
    total = 0
    for i in range(n):
        if i == n - 1:
            install(tracer=tracer)
            total += 1
        total += i
    return total


def spread_caller(n, tracer):
    total = 0
    extra = (1, 2)
    for i in range(n):
        if i == n - 1:
            install_spread(*extra, tracer=tracer)
            total += 1
        total += i
    return total


class Recorder:
    def __init__(self, name):
        self.name = name
        self.events = []

    def __call__(self, frame, event, arg):
        if frame.f_code.co_name == self.name:
            self.events.append(event)
        return self


class LaneTraceInstallTest(unittest.TestCase):
    def run_traced(self, func):
        rec = Recorder(func.__name__)
        try:
            result = func(N, rec)
        finally:
            sys.settrace(None)
        return result, rec.events

    def test_keyword_call_installs_tracer(self):
        result, events = self.run_traced(keyword_caller)
        self.assertEqual(result, sum(range(N)) + 1)
        self.assertIn("opcode", events)
        self.assertIn("line", events)
        self.assertEqual(events[-1], "return")
        self.assertLess(events.index("line"), len(events) - 1)

    def test_spread_call_installs_tracer(self):
        result, events = self.run_traced(spread_caller)
        self.assertEqual(result, sum(range(N)) + 1)
        self.assertIn("line", events)
        self.assertEqual(events[-1], "return")
        self.assertLess(events.index("line"), len(events) - 1)

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
