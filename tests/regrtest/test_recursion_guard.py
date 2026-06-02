"""RFC 0037 (WS1) — Python-level recursion guard.

WeavePy evaluates Python by recursive descent, so unbounded Python
recursion used to overflow the native stack and abort the process.
These assertions check that `sys.setrecursionlimit` is now enforced:
infinite recursion raises `RecursionError`, the interpreter recovers
cleanly afterwards, and the limit-setting edge cases match CPython.
"""

import sys

# ---------------------------------------------------------------------------
# get/set round-trip.
# ---------------------------------------------------------------------------

original = sys.getrecursionlimit()
assert isinstance(original, int)
assert original >= 1

sys.setrecursionlimit(150)
assert sys.getrecursionlimit() == 150


# ---------------------------------------------------------------------------
# Infinite recursion raises RecursionError (instead of crashing).
# ---------------------------------------------------------------------------

def runaway(n=0):
    return runaway(n + 1)


raised = False
try:
    runaway()
except RecursionError as exc:
    raised = True
    assert "recursion" in str(exc)
assert raised, "expected RecursionError from infinite recursion"


# ---------------------------------------------------------------------------
# The interpreter recovers and keeps running normally after the unwind.
# ---------------------------------------------------------------------------

def fib(n):
    return n if n < 2 else fib(n - 1) + fib(n - 2)


assert fib(12) == 144


# ---------------------------------------------------------------------------
# mutual recursion is also bounded.
# ---------------------------------------------------------------------------

def ping(n):
    return pong(n + 1)


def pong(n):
    return ping(n + 1)


raised = False
try:
    ping(0)
except RecursionError:
    raised = True
assert raised, "expected RecursionError from mutual recursion"


# ---------------------------------------------------------------------------
# Edge cases for setrecursionlimit().
# ---------------------------------------------------------------------------

# Limit below 1 is a ValueError.
try:
    sys.setrecursionlimit(0)
    raised = False
except ValueError:
    raised = True
assert raised, "expected ValueError for setrecursionlimit(0)"

# Setting a limit at/below the current depth raises RecursionError so a
# program cannot lower the limit out from under its own live stack.
def deep_then_lower(n):
    if n > 0:
        return deep_then_lower(n - 1)
    try:
        sys.setrecursionlimit(1)
    except RecursionError:
        return "too-low"
    return "unexpected"


sys.setrecursionlimit(1000)
assert deep_then_lower(40) == "too-low"

# Restore something sane for any later in-process use.
sys.setrecursionlimit(original)
assert sys.getrecursionlimit() == original


print("recursion guard ok")
