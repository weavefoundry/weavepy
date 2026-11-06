"""`f_lineno` of live frames read from compiled code (as `logging`'s
`findCaller` walks its callers), which the native read settles in place,
keeps CPython's results: the running frame's own line at each read, a
caller's line at its call, a generator's frame, and a finished frame's
last line."""
import sys


def here():
    f = sys._getframe()
    a = f.f_lineno
    b = f.f_lineno
    return a - here.__code__.co_firstlineno, b - here.__code__.co_firstlineno


def callers():
    f = sys._getframe(1)
    out = []
    while f is not None and len(out) < 2:
        out.append((f.f_code.co_name, f.f_lineno - f.f_code.co_firstlineno))
        f = f.f_back
    return out


def outer():
    return callers()


def gen():
    f = sys._getframe()
    yield f
    yield f.f_lineno - gen.__code__.co_firstlineno


def done():
    return sys._getframe()


def run(i):
    assert here() == (2, 3), here()
    got = outer()
    assert got[0] == ("outer", 1) and got[1][0] == "run", got
    g = gen()
    f = next(g)
    assert next(g) == 3
    d = done()
    assert d.f_lineno - done.__code__.co_firstlineno == 1


# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    run(i)
print("ok")
