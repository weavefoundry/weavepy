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

# Raw-pthread spawn/join helpers, implemented natively in `_testinternalcapi`
# (they create a genuine non-Python OS thread). `test_os` imports these from
# `_testcapi` to verify `os.fork()` warns about a multi-threaded process even
# when the extra thread isn't a Python thread.
from _testinternalcapi import (  # noqa: F401
    _spawn_pthread_waiter,
    _end_spawned_pthread,
)

# CPython's test suite gates many tests on attributes of _testcapi;
# expose the couple of constants commonly probed so `hasattr` checks
# behave sensibly.
INT_MAX = 2**31 - 1
INT_MIN = -(2**31)
UINT_MAX = 2**32 - 1
# C `long` is 64-bit on every LP64 Unix and 32-bit on Windows (LLP64).
_LONG_BITS = 32 if sys.platform == "win32" else 64
LONG_MAX = 2 ** (_LONG_BITS - 1) - 1
LONG_MIN = -(2 ** (_LONG_BITS - 1))
ULONG_MAX = 2**_LONG_BITS - 1
LLONG_MAX = 2**63 - 1
LLONG_MIN = -(2**63)
ULLONG_MAX = 2**64 - 1
SHRT_MAX = 2**15 - 1
SHRT_MIN = -(2**15)

# CPython 3.13's C-stack recursion budget (`Include/cpython/pystate.h`).
# WeavePy's tree-walking evaluator enforces `sys.setrecursionlimit` on a
# large reserved native stack, so the CPython default is the faithful
# answer for `test.support.get_c_recursion_limit()`-sized stress loops.
Py_C_RECURSION_LIMIT = 10000

# Allocator flags (`test.support.check_impl_detail` gates). WeavePy's
# allocator is Rust's global allocator: neither pymalloc nor mimalloc.
WITH_PYMALLOC = False
WITH_MIMALLOC = False
USHRT_MAX = 2**16 - 1
CHAR_MAX = 127
CHAR_MIN = -128
UCHAR_MAX = 255
SIZEOF_TIME_T = 8
SIZE_MAX = 2**64 - 1
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
    #
    # A real sub-interpreter starts from the default interpreter config, so
    # config knobs the child flips must not leak back into this interpreter
    # (test_int.test_int_max_str_digits_is_per_interpreter). WeavePy's
    # runtime keeps these process-global; snapshot + restore around the
    # exec gives the caller CPython's isolation semantics.
    import threading

    saved_digits = sys.get_int_max_str_digits()
    saved_recursion = sys.getrecursionlimit()
    before = set(threading.enumerate())
    try:
        exec(code, {"__name__": "__main__"})
    except SystemExit:
        raise
    except BaseException:
        _traceback.print_exc()
        return -1
    finally:
        # Py_EndInterpreter joins the interpreter's non-daemon threads
        # before tearing it down (issue #18808); threads the exec'd code
        # started must have finished by the time this call returns
        # (test_threading.SubinterpThreadingTests.test_threads_join).
        for t in threading.enumerate():
            if t not in before and not t.daemon:
                t.join()
        sys.set_int_max_str_digits(saved_digits)
        sys.setrecursionlimit(saved_recursion)
    return 0
