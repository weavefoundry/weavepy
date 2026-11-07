"""Re-raises from compiled code (an except clause's new raise leaving through
its cleanup, a finally, a with block whose exit doesn't suppress, a bare
`raise`, and frames in between) keep CPython's results: the exception,
its context, the traceback's entries and lines, and each frame's line
afterwards (PEP 626)."""
import sys
import traceback


class Exit:
    def __enter__(self):
        return self

    def __exit__(self, *a):
        return False


def from_none(d, k):
    try:
        return d[k]
    except KeyError:
        raise ValueError(k) from None


def chained(d, k):
    try:
        return d[k]
    except KeyError:
        raise ValueError(k)


def finally_(log, k):
    try:
        raise KeyError(k)
    finally:
        log.append(k)


def with_(k):
    with Exit():
        raise IndexError(k)


def nested(k):
    try:
        try:
            raise TypeError(k)
        finally:
            pass
    except TypeError:
        raise


def outer(f, *a):
    try:
        return f(*a)
    finally:
        pass


def shape(f, *a):
    try:
        outer(f, *a)
    except Exception as e:
        tb = e.__traceback__
        entries = []
        while tb is not None:
            entries.append((tb.tb_frame.f_code.co_name, tb.tb_lineno - tb.tb_frame.f_code.co_firstlineno, tb.tb_frame.f_lineno - tb.tb_frame.f_code.co_firstlineno))
            tb = tb.tb_next
        ctx = type(e.__context__).__name__ if e.__context__ is not None else None
        return (type(e).__name__, e.args, ctx, e.__suppress_context__, entries[1:], sys.exception() is e)
    return None


def run(i):
    log = []
    return [
        shape(from_none, {}, i % 3),
        shape(chained, {}, i % 3),
        shape(finally_, log, i % 3),
        shape(with_, i % 3),
        shape(nested, i % 3),
        log,
    ]


EXPECTED = [
    ("ValueError", (1,), "KeyError", True, [("outer", 2, 4), ("from_none", 4, 4)], True),
    ("ValueError", (1,), "KeyError", False, [("outer", 2, 4), ("chained", 4, 4)], True),
    ("KeyError", (1,), None, False, [("outer", 2, 4), ("finally_", 2, 4)], True),
    ("IndexError", (1,), None, False, [("outer", 2, 4), ("with_", 2, 2)], True),
    ("TypeError", (1,), None, False, [("outer", 2, 4), ("nested", 3, 7)], True),
    [1],
]

# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(1, 3000, 3):
    got = run(i)
    assert got == EXPECTED, (i, got)
print("ok")
