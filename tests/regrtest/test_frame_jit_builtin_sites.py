"""Builtin calls from compiled code at sites settled on one builtin.

A compiled `CALL` whose site settled on a builtin checks the callee against
that builtin by address and runs its leaf half or its body on the operands
in place. A different callee at the same site (a rebound or shadowed
name, another builtin, a Python function) must take the general path; a
body that raises must leave the right traceback; operands must be released
right after the call (`__del__` timing); a body that runs Python code must
see the caller's frame whole and keep the operands alive even when that
code rebinds the caller's locals; and `sys.setprofile` installed from a
callback must see the calls after it.
"""

import builtins
import os
import subprocess
import sys
import unittest

FORCED = "--forced-frame-jit"
N = 3000


def lengths(xs, n):
    t = 0
    for i in range(n):
        t += len(xs)
    return t


def rebind_len(n):
    global len
    t = 0
    try:
        for i in range(n):
            if i == n // 2:
                len = lambda x: 100  # noqa: E731
            t += len("ab")
    finally:
        del len
    return t


def mixed_callees(n):
    fs = [len, abs, str.upper, lambda x: 7]
    t = []
    for i in range(n):
        f = fs[(i // 50) % len(fs)]
        try:
            t.append(f("ab"))
        except TypeError:
            t.append(None)
    return t


def setdefaults(d, n):
    t = 0
    for i in range(n):
        t += d.setdefault("a", 2)
        t += d.pop("z", 0)
    return t


def pops(d, keys):
    out = []
    for k in keys:
        try:
            out.append(d.pop(k))
        except KeyError as e:
            out.append(("missing", e.args[0], e.__traceback__.tb_lineno))
    return out


POP_LINE = pops.__code__.co_firstlineno + 4


def raising_pop(d, n):
    for i in range(n):
        d.pop(i)


class Noisy:
    log = []

    def __init__(self, tag):
        self.tag = tag

    def __del__(self):
        Noisy.log.append(("del", self.tag))


def del_timing(n):
    d = {"k": None}
    for i in range(n):
        d.setdefault("k", Noisy(i))
        Noisy.log.append(("after", i))


class FrameKey:
    """A key whose `__eq__` looks at its caller's frame."""

    seen = []

    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return 1

    def __eq__(self, other):
        f = sys._getframe(1)
        while f is not None and f.f_code.co_name != "frame_lookups":
            f = f.f_back
        FrameKey.seen.append(f.f_lineno if f is not None else None)
        return isinstance(other, FrameKey) and self.v == other.v


def frame_lookups(d, n):
    t = 0
    for i in range(n):
        t += d.setdefault(FrameKey(1), 0)
    return t


LOOKUP_LINE = frame_lookups.__code__.co_firstlineno + 3


class Rebinder:
    """A key whose `__eq__` drops the caller's own reference to the dict."""

    def __hash__(self):
        return 2

    def __eq__(self, other):
        f = sys._getframe(1)
        while f is not None and f.f_code.co_name != "rebinding":
            f = f.f_back
        if f is not None:
            f.f_locals["d"] = None
        return True


def rebinding(n):
    out = 0
    for i in range(n):
        d = {Rebinder(): i}
        out += d.setdefault(Rebinder(), -1)
    return out


class MyList(list):
    def append(self, x):
        super().append(x * 2)


class PlainList(list):
    pass


def appends(lst, n):
    for i in range(n):
        lst.append(i)
        lst.count(i)
    return lst


def kw_calls(n):
    t = 0
    for i in range(n):
        t += int("11", base=2) + len(sorted([3, 1, 2], reverse=True))
    return t


def profiled(n, events):
    t = 0
    for i in range(n):
        if i == n // 2:
            sys.setprofile(lambda frame, event, arg: events.append((event, getattr(arg, "__name__", None))))
        t += len("abc")
        t += "a,b".count(",")
    sys.setprofile(None)
    return t


class BuiltinSiteTest(unittest.TestCase):
    def test_leaf_and_rebinding(self):
        self.assertEqual(lengths([1, 2], N), 2 * N)
        self.assertEqual(rebind_len(N), (N // 2) * 2 + (N - N // 2) * 100)
        self.assertEqual(rebind_len(N), (N // 2) * 2 + (N - N // 2) * 100)

    def test_patched_builtins(self):
        saved = builtins.len
        try:
            self.assertEqual(lengths("abc", N), 3 * N)
            builtins.len = lambda x: 1
            self.assertEqual(lengths("abc", N), N)
        finally:
            builtins.len = saved
        self.assertEqual(lengths("abc", N), 3 * N)

    def test_mixed_callees(self):
        out = mixed_callees(N)
        for i, v in enumerate(out):
            want = [2, None, "AB", 7][(i // 50) % 4]
            self.assertEqual(v, want, i)

    def test_native_body(self):
        self.assertEqual(setdefaults({"a": 1}, N), N)
        d = {}
        self.assertEqual(setdefaults(d, N), 2 * N)
        self.assertEqual(d, {"a": 2})

    def test_raise_traceback(self):
        d = {1: "a", 3: "c"}
        out = pops(d, [1, 2, 3, 4] * 1000)
        self.assertEqual(out[:4], ["a", ("missing", 2, POP_LINE), "c", ("missing", 4, POP_LINE)])
        try:
            raising_pop({}, N)
        except KeyError as e:
            tb = e.__traceback__
        else:
            self.fail("no KeyError")
        while tb.tb_next is not None:
            tb = tb.tb_next
        self.assertEqual(tb.tb_frame.f_code.co_name, "raising_pop")
        self.assertEqual(tb.tb_lineno, raising_pop.__code__.co_firstlineno + 2)

    def test_del_timing(self):
        Noisy.log.clear()
        del_timing(N)
        log = Noisy.log
        # Each default dies right after its `setdefault`, before the next
        # statement.
        for i in range(N):
            self.assertEqual(log[2 * i : 2 * i + 2], [("del", i), ("after", i)], i)

    def test_callback_sees_frame(self):
        FrameKey.seen.clear()
        d = {FrameKey(1): 5}
        self.assertEqual(frame_lookups(d, N), 5 * N)
        self.assertEqual(set(FrameKey.seen), {LOOKUP_LINE})

    def test_callback_rebinds_receiver(self):
        self.assertEqual(rebinding(N), sum(range(N)))

    def test_subclasses(self):
        a = appends(MyList(), 500)
        self.assertEqual(a, [2 * i for i in range(500)])
        b = appends(PlainList(), 500)
        self.assertEqual(b, list(range(500)))
        self.assertIs(type(b), PlainList)

    def test_keywords(self):
        self.assertEqual(kw_calls(N), N * 6)

    def test_profile_from_compiled_code(self):
        events = []
        self.assertEqual(profiled(N, events), N * 4)
        calls = [name for ev, name in events if ev == "c_call"]
        self.assertEqual(calls.count("len"), N - N // 2)
        self.assertEqual(calls.count("count"), N - N // 2)

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
