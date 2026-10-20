# A caller's f_lineno, read through f_back while it runs a call, is the
# call's line on every call, including the one on which tier 2 first
# runs the caller natively (it used to report the caller's `def` line).

import sys


def callee():
    f = sys._getframe()
    return f.f_lineno, f.f_back.f_lineno


def caller():
    r = callee()
    return r


def run(n=3000):
    return [caller() for _ in range(n)]


res = run()
want = (10, 14)
bad = [(i, r) for i, r in enumerate(res) if r != want]
assert not bad, bad[:5]
print("test_tier2_caller_lineno: OK")
