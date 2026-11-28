"""Exceptions a builtin call raises, caught by compiled code's handlers.

`[].pop()`, `{}.pop(k)`, `"abc".index("z")`, `min([])` and `next(it)`
raise in place: the code's own handler runs, with `sys.exc_info()`, the
traceback's line, `finally` and implicit chaining as CPython has them,
and an uncaught one reaches the caller. Run it past the compile
threshold.
"""

import sys


def caught(xs):
    try:
        xs.pop()
    except IndexError as e:
        assert sys.exc_info()[1] is e
        return ("index", str(e), e.__traceback__.tb_lineno)
    finally:
        xs.append(0)
    return "no raise"


def several(d, s, it):
    out = []
    for f in (lambda: d.pop("k"), lambda: s.index("z"), lambda: min([]), lambda: next(it)):
        try:
            f()
        except (KeyError, ValueError, StopIteration) as e:
            out.append(type(e).__name__)
    return out


def chained():
    try:
        try:
            [].pop()
        except IndexError:
            {}.pop("k")
    except KeyError as e:
        return type(e.__context__).__name__


def uncaught():
    return [].pop()


def caller():
    try:
        uncaught()
    except IndexError as e:
        tb = e.__traceback__
        while tb.tb_next:
            tb = tb.tb_next
        return tb.tb_frame.f_code.co_name


line = caught.__code__.co_firstlineno + 2
for i in range(3000):
    xs = []
    assert caught(xs) == ("index", "pop from empty list", line)
    assert xs == [0]
    assert caught([1, 2]) == "no raise"
    assert several({}, "abc", iter([])) == ["KeyError", "ValueError", "ValueError", "StopIteration"]
    assert chained() == "IndexError"
    assert caller() == "uncaught"
    assert sys.exc_info() == (None, None, None)
