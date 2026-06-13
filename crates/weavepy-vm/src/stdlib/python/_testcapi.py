"""Pure-Python stand-in for CPython's ``_testcapi`` test helper.

The traceback conformance tests use two C hooks:

- ``exception_print(exc)`` calls ``PyErr_Display``, which since 3.13
  routes through ``traceback._print_exception_bltin`` (that is how the
  tests can monkeypatch ``_colorize.can_colorize`` and see colors).
- ``traceback_print(tb, file)`` calls ``PyTraceBack_Print``, which
  prints the classic header plus frame lines but never the PEP 657
  caret/anchor decoration lines.
"""

import sys
import traceback as _traceback

# CPython's test suite gates many tests on attributes of _testcapi;
# expose the couple of constants commonly probed so `hasattr` checks
# behave sensibly.
INT_MAX = 2**31 - 1
INT_MIN = -(2**31)
PY_SSIZE_T_MAX = sys.maxsize
PY_SSIZE_T_MIN = -sys.maxsize - 1
# <float.h> limits (IEEE 754 binary32 / binary64, the only layouts we
# support): test_float's rounding tests read these off _testcapi.
FLT_MAX = 3.4028234663852886e38
FLT_MIN = 1.1754943508222875e-38
DBL_MAX = sys.float_info.max
DBL_MIN = sys.float_info.min


def exception_print(exc):
    # PyErr_Display(NULL, exc, exc.__traceback__)
    _traceback._print_exception_bltin(exc)


def traceback_print(tb, file):
    # PyTraceBack_Print(tb, file): header + frames, no caret lines.
    text = "Traceback (most recent call last):\n" + "".join(
        _traceback.format_tb(tb)
    )
    kept = [
        line
        for line in text.splitlines()
        if line.strip() and not set(line.strip()) <= set("^~")
    ]
    file.write("\n".join(kept) + "\n")


def bad_get(self, obj, cls):
    # C helper used as a `__get__` replacement (bpo-25750): it calls the
    # owning class mid-dispatch, which clobbers the descriptor out of
    # the class dict — the regression test just checks we don't crash.
    return cls()


def run_in_subinterp(code):
    # Py_NewInterpreter + PyRun_SimpleString: execute `code` in a fresh
    # interpreter namespace; uncaught exceptions are printed to stderr
    # (PyErr_Print) and the call reports -1, matching the C helper.
    try:
        exec(code, {"__name__": "__main__"})
    except SystemExit:
        raise
    except BaseException:
        _traceback.print_exc()
        return -1
    return 0
