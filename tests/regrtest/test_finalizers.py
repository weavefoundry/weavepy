"""RFC 0024 — `__del__` finalizers integrated with the cycle GC."""

import gc


# ---------------------------------------------------------------------------
# Simple cycle with `__del__` — finaliser should fire and read its
# own attributes (cycle-clear is deferred until after __del__).
# ---------------------------------------------------------------------------

run_log = []


class Tracked:
    def __init__(self, tag):
        self.tag = tag

    def __del__(self):
        run_log.append(self.tag)


def make_cycle():
    a = Tracked("a")
    b = Tracked("b")
    a.next = b
    b.next = a


make_cycle()
recovered = gc.collect()
assert recovered >= 2, recovered
assert sorted(run_log) == ["a", "b"], run_log


# ---------------------------------------------------------------------------
# `__del__` runs at most once even if the GC re-encounters the object.
# ---------------------------------------------------------------------------

run_log.clear()


class CountedFinaliser:
    def __init__(self, tag):
        self.tag = tag
        self.count = 0

    def __del__(self):
        self.count += 1
        run_log.append(self.tag)


def cycle_once():
    x = CountedFinaliser("x")
    y = CountedFinaliser("y")
    x.next = y
    y.next = x


cycle_once()
gc.collect()
gc.collect()
gc.collect()
assert sorted(run_log) == ["x", "y"], run_log


# ---------------------------------------------------------------------------
# Non-cycle finaliser — when there's no cycle, refcounting handles it
# (we don't currently fire __del__ for plain refcounted decrements,
# but `gc.collect()` after `del` still works for sanity).
# ---------------------------------------------------------------------------

run_log.clear()


class Single:
    def __init__(self):
        self.self = self

    def __del__(self):
        run_log.append("single")


s = Single()
del s
gc.collect()
assert run_log == ["single"], run_log


print("__del__ finalizers ok")
