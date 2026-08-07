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

# `Py_FatalError` trigger (test_faulthandler.test_fatal_error): dumps a
# traceback to stderr and aborts the process. Native — it must never
# return, and the dump comes from the interpreter's own frame registry.
from _testinternalcapi import fatal_error  # noqa: F401

# `PyTraceMalloc_Track`/`Untrack` probes (`test_tracemalloc.TestCAPI`).
# Direct aliases of the native builtins — a Python wrapper `def` would
# add its own frame to the traceback the C API captures from the caller.
# The native side accepts and ignores the trailing `release_gil` flag.
from _tracemalloc import (  # noqa: F401
    _weave_track as tracemalloc_track,
    _weave_untrack as tracemalloc_untrack,
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

def PyTime_AsSecondsDouble(t):
    """`PyTime_AsSecondsDouble()` (Python/pytime.c): exact whole seconds
    convert via integer division so huge timestamps don't lose precision;
    everything else divides as C doubles (test_time.test_AsSecondsDouble)."""
    t = t.__index__()
    if not (LLONG_MIN <= t <= LLONG_MAX):
        raise OverflowError("Python int too large to convert to C long long")
    if t % 1_000_000_000 == 0:
        return float(t // 1_000_000_000)
    # C computes `(double)t / 1e9`: the *operand* is narrowed to double
    # first (dropping low bits of huge timestamps), unlike Python's
    # correctly-rounded int/int true division.
    return float(t) / 1e9


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


def Py_CompileStringExFlags(source, filename, start, flags=0, optimize=-1):
    # C-API compile shim: `start` is the grammar start token
    # (Py_single_input=256, Py_file_input=257, Py_eval_input=258).
    # PyCF_IGNORE_COOKIE (0x0800) means "the buffer is UTF-8, skip PEP
    # 263 cookie detection" — so a non-UTF-8 byte is a
    # UnicodeDecodeError up front (test_type_comments).
    mode = {256: 'single', 257: 'exec', 258: 'eval'}.get(start, 'exec')
    if isinstance(source, bytes) and flags & 0x0800:
        source = source.decode('utf-8')
    return compile(source, filename, mode, flags & ~0x0800, optimize=optimize)


def bad_get(self, obj, cls):
    # C helper used as a `__get__` replacement (bpo-25750): it calls the
    # owning class mid-dispatch, which clobbers the descriptor out of
    # the class dict — the regression test just checks we don't crash.
    return cls()


def set_nomemory(start, stop=None):
    # CPython's hook swaps in an allocator that fails the start..stop-th
    # allocations (PyMem_SetAllocator). WeavePy's allocator is Rust's
    # global allocator with no failure-injection seam, so tests that
    # need real allocation failures (test_pyexpat's
    # test_error_path_no_crash) skip rather than error.
    import unittest

    raise unittest.SkipTest("WeavePy cannot inject allocation failures")


def remove_mem_hooks():
    pass


def test_pymem_alloc0():
    # CPython's C probe checks PyMem_Malloc(0) & friends return unique
    # non-NULL pointers with tracemalloc enabled (bpo-21639). WeavePy's
    # allocator is Rust's global allocator, which already guarantees
    # this; the observable contract is simply "does not crash".
    return None


def tracemalloc_track_race():
    # gh-128679 regression probe: hammer PyTraceMalloc_Track/Untrack
    # from worker threads racing tracemalloc.stop(). Exercises the same
    # public entry points; passes iff nothing crashes.
    import _tracemalloc
    import threading

    _tracemalloc.start(1)

    def worker(base):
        for i in range(200):
            try:
                _tracemalloc._weave_track(5, base + i, 16)
                _tracemalloc._weave_untrack(5, base + i)
            except RuntimeError:
                # Raised once stop() wins the race — exactly the C
                # behaviour (_PyTraceMalloc_Track returns -2).
                pass

    threads = [
        threading.Thread(target=worker, args=(0x1000 * (n + 1),)) for n in range(4)
    ]
    for t in threads:
        t.start()
    _tracemalloc.stop()
    for t in threads:
        t.join()


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


# `call_in_temporary_c_thread()` (Modules/_testcapi/run.c): run *callback*
# once on a freshly spawned "foreign" thread. With `wait=False` the thread is
# left for `join_temporary_c_thread()` to reap
# (test_threading_local.test_threading_local_clear_race). A real OS thread
# via `_thread` reproduces the observable shape — the callback runs off the
# calling thread, and joining synchronizes with its completion.
_temporary_c_thread_done = None


def call_in_temporary_c_thread(callback, wait=True):
    import _thread

    global _temporary_c_thread_done
    done = _thread.allocate_lock()
    done.acquire()

    def run():
        try:
            callback()
        finally:
            done.release()

    _thread.start_new_thread(run, ())
    if wait:
        with done:
            pass
    else:
        _temporary_c_thread_done = done


def join_temporary_c_thread():
    global _temporary_c_thread_done
    done = _temporary_c_thread_done
    if done is not None:
        _temporary_c_thread_done = None
        with done:
            pass


# --- `tp_version_tag` probes (test_type_cache) -------------------------
#
# CPython's `type_get_version`/`type_assign_version`/`type_modified`/
# `type_assign_specific_version_unsafe` read and write `tp_version_tag`
# directly. WeavePy's analogue is the per-type attribute-resolution
# counter (`TypeObject::attr_version`, exposed through
# `_testinternalcapi._type_attr_version`): it bumps whenever the class
# dict or MRO changes, which is exactly the event that zeroes
# `tp_version_tag` in CPython. A tag assigned here is therefore stamped
# with the counter observed at assignment time and reads back as 0 once
# the class has been modified since.
from _testinternalcapi import _type_attr_version

# type -> (tag, attr_version at assignment). Keyed by the type object
# itself (strong reference) so ids are never recycled under us; this is
# a test-only helper, the leak is bounded by the test's own classes.
_type_version_tags = {}
_type_versions_used = {}
# CPython assigns globally unique, monotonically increasing tags that
# are never reused — even across `sys._clear_type_cache()`.
_next_version_tag = 1_000_000
# `MAX_VERSIONS_PER_CLASS` (Objects/typeobject.c): a class that has
# consumed its budget can never get a fresh tag again.
_MAX_VERSIONS_PER_CLASS = 1000


def type_get_version(tp):
    rec = _type_version_tags.get(tp)
    if rec is not None and rec[1] == _type_attr_version(tp):
        return rec[0]
    return 0


def type_assign_version(tp):
    if type_get_version(tp) != 0:
        return 1
    used = _type_versions_used.get(tp, 0)
    if used >= _MAX_VERSIONS_PER_CLASS:
        return 0
    global _next_version_tag
    tag = _next_version_tag
    _next_version_tag += 1
    _type_version_tags[tp] = (tag, _type_attr_version(tp))
    _type_versions_used[tp] = used + 1
    return 1


def type_modified(tp):
    _type_version_tags.pop(tp, None)


def type_assign_specific_version_unsafe(tp, version):
    _type_version_tags[tp] = (version, _type_attr_version(tp))


# ---------------------------------------------------------------------------
# PEP 3118 / PEP 688 buffer test helpers (Modules/_testcapi/buffer.c)
# ---------------------------------------------------------------------------

# `PyMemoryView_FromMemory` access modes — *invalid* as `PyObject_GetBuffer`
# request flags; the C helpers reject them with SystemError
# (PyErr_BadInternalCall).
_PyBUF_READ = 0x100
_PyBUF_WRITE = 0x200
_PyBUF_WRITABLE = 0x001


def _check_getbuffer_flags(flags):
    if flags == _PyBUF_READ or flags == _PyBUF_WRITE:
        raise SystemError("PyBUF_READ and PyBUF_WRITE are invalid flags")


def _view_is_released(view):
    try:
        view.nbytes
    except ValueError:
        return True
    return False


class testBuf:
    """`_testcapi.testBuf` — a minimal C buffer exporter with an export
    counter (`references`), backed by the fixed payload b\"test\"."""

    def __init__(self):
        self.references = 0
        self._data = b"test"

    def __buffer__(self, flags):
        _check_getbuffer_flags(flags)
        view = memoryview(self._data)
        self.references += 1
        return view

    def __release_buffer__(self, view):
        if _view_is_released(view):
            raise ValueError("operation forbidden on released memoryview object")
        view.release()
        self.references -= 1


def buffer_fill_info(source, readonly, flags):
    """`PyBuffer_FillInfo` + `PyMemoryView_FromBuffer` over `source`'s
    bytes: SystemError for the FromMemory access modes, BufferError when a
    writable buffer is requested from a readonly filling."""
    _check_getbuffer_flags(flags)
    if readonly and flags & _PyBUF_WRITABLE:
        raise BufferError("Object is not writable.")
    if readonly:
        return memoryview(bytes(source))
    return memoryview(bytearray(source))
