"""Calls of functions with keyword-only parameters in compiled loops.

A positional call of a pure leaf with keyword-only defaults caches its
site's shape, so the framed call binds it and compiled code lowers it in
line; changed `__defaults__` and `__kwdefaults__`, a required keyword-only
parameter, methods, and a site that sees another function all behave as
in CPython. Run it past the compile threshold.
"""


def fkw(a, b=2, *, c=3):
    return a + b + c


def freq(a, *, c):
    return a + c


def fother(a, b=20, *, c=30):
    return a * 100 + b + c


class K:
    def m(self, a, *, s=5):
        return a + s


def run(n, g):
    t = 0
    for i in range(n):
        t += g(i)
    return t


def loop(n):
    t = 0
    for i in range(n):
        t += fkw(i)
    return t


def loop_kw(n):
    t = 0
    for i in range(n):
        t += fkw(i, c=4) + fkw(i, b=1)
    return t


def loop_method(n, k):
    t = 0
    for i in range(n):
        t += k.m(i)
    return t


N = 3000
base = sum(range(N))
assert loop(N) == base + 5 * N
assert loop_kw(N) == 2 * base + 6 * N + 4 * N
assert loop_method(N, K()) == base + 5 * N
assert run(N, fkw) == base + 5 * N
assert run(N, fother) == 100 * base + 50 * N

# Changed defaults take effect at once.
fkw.__kwdefaults__ = {"c": 10}
assert loop(10) == sum(range(10)) + 12 * 10
fkw.__defaults__ = (0,)
assert loop(10) == sum(range(10)) + 10 * 10
fkw.__kwdefaults__ = None
try:
    loop(1)
except TypeError as e:
    assert "c" in str(e)
else:
    raise AssertionError("missing keyword-only argument")
fkw.__kwdefaults__ = {"c": 3}
fkw.__defaults__ = (2,)
assert loop(10) == sum(range(10)) + 5 * 10

# A required keyword-only parameter still raises.
try:
    run(1, freq)
except TypeError:
    pass
else:
    raise AssertionError("missing required keyword-only argument")

# The global rebound to another function at the cached site.
saved = fkw
fkw = fother
assert loop(10) == 100 * sum(range(10)) + 50 * 10
fkw = saved
assert loop(N) == base + 5 * N
print("ok")
