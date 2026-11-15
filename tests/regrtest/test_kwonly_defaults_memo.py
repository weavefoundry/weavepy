"""A function's keyword-only defaults fill a positional call's missing
keyword-only parameters, call after call, through replaced
`__kwdefaults__`, replaced `__code__`, copies of the function, and
functions whose defaults don't cover every keyword-only parameter."""

import types


def ko(a, *rest, x=1, y=2, z=3):
    return (a, rest, x, y, z)


def partial_defaults(a, *, x, y=2):
    return (a, x, y)


def other(a, *rest, y=20, x=10, z=30, w=40):
    return (a, rest, x, y, z, w)


def calls(f, n):
    out = None
    for i in range(n):
        out = f(i, i)
    return out


N = 3000
for _ in range(3):
    assert calls(ko, N) == (N - 1, (N - 1,), 1, 2, 3)

# Replaced keyword-only defaults are what later calls bind.
ko.__kwdefaults__ = {"x": 7, "y": 8, "z": 9}
assert calls(ko, N) == (N - 1, (N - 1,), 7, 8, 9)
ko.__kwdefaults__ = {"z": 0, "y": 5, "x": 6}
assert calls(ko, N) == (N - 1, (N - 1,), 6, 5, 0)
del ko.__kwdefaults__
try:
    ko(1)
except TypeError as e:
    assert "keyword-only" in str(e), e
else:
    raise AssertionError("missing keyword-only arguments not reported")

# A copy keeps its own defaults; a replaced code object binds its own
# parameters from them.
ko.__kwdefaults__ = {"x": 1, "y": 2, "z": 3}
twin = types.FunctionType(ko.__code__, ko.__globals__, "twin", None, None, {"x": "a", "y": "b", "z": "c"})
assert calls(twin, N) == (N - 1, (N - 1,), "a", "b", "c")
assert calls(ko, N) == (N - 1, (N - 1,), 1, 2, 3)


def fresh(a, *rest, x=1, y=2, z=3):
    return (a, rest, x, y, z)


for _ in range(N):
    fresh(0)
fresh.__code__ = other.__code__
try:
    fresh(0)
except TypeError as e:
    assert "w" in str(e), e
else:
    raise AssertionError("the new code's uncovered parameter not reported")
fresh.__kwdefaults__ = {"x": 1, "y": 2, "z": 3, "w": 4}
assert calls(fresh, N) == (N - 1, (N - 1,), 1, 2, 3, 4)

# Defaults that don't cover every keyword-only parameter.
for i in range(N):
    try:
        partial_defaults(i)
    except TypeError as e:
        assert "'x'" in str(e), e
    else:
        raise AssertionError("missing keyword-only argument not reported")
    assert partial_defaults(i, x=i) == (i, i, 2)
