"""Native functions called from compiled loops, which run in place, give
what the interpreter gives: results, callbacks into Python (which see the
whole stack), errors with tracebacks naming the calling line, and natives
that read their caller's frame or namespace."""

import functools
import json
import operator
import sys
import traceback
import warnings


def callers():
    names = []
    f = sys._getframe(1)
    while f is not None:
        names.append(f.f_code.co_name)
        f = f.f_back
    return names


def reduce_loop(n):
    total = 0
    seen = None
    for i in range(n):
        total += functools.reduce(operator.add, (i, 1, 2))
        if i == n - 1:
            seen = functools.reduce(lambda a, b: callers(), (1, 2))
    return total, seen


for _ in range(4):
    total, seen = reduce_loop(300)
    assert total == sum(i + 3 for i in range(300))
    assert "reduce_loop" in seen, seen


def json_loop(n):
    out = []
    for i in range(n):
        out.append(json.dumps({"i": i}))
    return out


for _ in range(3):
    assert json_loop(200)[-1] == '{"i": 199}'


# An error from a native, and from a callback, at the calling line.
def failing(n):
    for i in range(n):
        if i == n - 1:
            functools.reduce(operator.add, (1, "x"))  # line marker: native
    return None


def failing_callback(n):
    def boom(a, b):
        raise ValueError("boom")

    for i in range(n):
        if i == n - 1:
            functools.reduce(boom, (1, 2))
    return None


for _ in range(3):
    try:
        failing(300)
    except TypeError:
        tb = traceback.extract_tb(sys.exc_info()[2])
        assert tb[-1].name == "failing", tb
        assert "line marker: native" in tb[-1].line, tb[-1].line
    else:
        raise AssertionError("expected TypeError")
    try:
        failing_callback(300)
    except ValueError:
        tb = traceback.extract_tb(sys.exc_info()[2])
        names = [t.name for t in tb]
        assert names[-2:] == ["failing_callback", "boom"], names
    else:
        raise AssertionError("expected ValueError")


# Natives that read the caller's frame.
def warn_loop(n):
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        for i in range(n):
            if i == n - 1:
                warnings.warn("here", UserWarning, stacklevel=1)
    return caught


for _ in range(3):
    caught = warn_loop(300)
    assert len(caught) == 1 and caught[0].filename == __file__, caught


def eval_loop(n):
    x = 5
    total = 0
    for i in range(n):
        total += eval("x + i")
    return total


for _ in range(3):
    assert eval_loop(200) == sum(5 + i for i in range(200))


def vars_loop(n):
    class O:
        pass

    o = O()
    o.a = 1
    got = None
    for i in range(n):
        got = vars(o)
    local = vars()
    assert "o" in local and "got" in local
    return got


for _ in range(3):
    assert vars_loop(200) == {"a": 1}
print("ok")
