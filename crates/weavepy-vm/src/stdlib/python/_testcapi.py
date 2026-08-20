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

# `Py_AddPendingCall` fixture (test_capi.test_misc TestPendingCalls):
# queues a Python callback to run at the eval-breaker safe point on the
# *main* thread only. The per-interpreter (any-thread) variant lives on
# `_testinternalcapi.pending_threadfunc`.
from _testinternalcapi import _main_pending_threadfunc as _pending_threadfunc  # noqa: F401

# PEP 509 dict version tag probe (`test_dict_version`): CPython's
# `_testcapi.dict_get_version` wraps `PyDict_GetVersion`; ours reads the
# native side-registry that stamps and advances the tags.
from _testinternalcapi import dict_get_version  # noqa: F401

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
    # PyMem_SetAllocator failure injection: the native hook counts
    # instance allocations and fails the start..stop-th ones with
    # MemoryError (test_capi.test_mem test_set_nomemory).
    import _testinternalcapi

    if stop is None:
        _testinternalcapi.set_nomemory(start)
    else:
        _testinternalcapi.set_nomemory(start, stop)


def remove_mem_hooks():
    import _testinternalcapi

    _testinternalcapi.remove_mem_hooks()


def crash_no_current_thread():
    # PyThreadState_Get() with the GIL released: dies with the fatal
    # banner (test_capi.test_misc test_no_FatalError_infinite_loop).
    import _testinternalcapi

    _testinternalcapi.crash_no_current_thread()


def toggle_reftrace_printer(enabled):
    # PyRefTracer_SetTracer printer: while enabled, the VM prints
    # "CREATE <type>" / "DESTROY <type>" lines for object lifecycle
    # events (test_capi.test_misc TestCEval.test_ceval_decref).
    import _testinternalcapi

    _testinternalcapi.toggle_reftrace_printer(enabled)


def pymem_buffer_overflow():
    # Writes past the end of a PyMem_Malloc'd block; the PYTHONMALLOC
    # debug hooks catch the clobbered pad byte and die fatally.
    import _testinternalcapi

    return _testinternalcapi.pymem_buffer_overflow()


def pymem_api_misuse():
    import _testinternalcapi

    return _testinternalcapi.pymem_api_misuse()


def pymem_malloc_without_gil():
    import _testinternalcapi

    return _testinternalcapi.pymem_malloc_without_gil()


def pyobject_malloc_without_gil():
    import _testinternalcapi

    return _testinternalcapi.pyobject_malloc_without_gil()


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
    # Py_NewInterpreter + PyRun_SimpleString: execute `code` in a real
    # fresh sub-interpreter (own module cache, own `sys.modules`, own
    # `builtins` — test_capi.test_misc test_subinterps asserts distinct
    # ids); uncaught exceptions are printed to stderr (PyErr_Print) and
    # the call reports -1, matching the C helper. Non-daemon threads the
    # code started are joined before returning (Py_EndInterpreter,
    # issue #18808).
    #
    # A real sub-interpreter starts from the default interpreter config,
    # so config knobs the child flips must not leak back into this
    # interpreter (test_int.test_int_max_str_digits_is_per_interpreter).
    # WeavePy keeps a few of those knobs process-global; snapshot +
    # restore around the exec gives the caller CPython's isolation
    # semantics.
    import _testinternalcapi

    saved_digits = sys.get_int_max_str_digits()
    saved_recursion = sys.getrecursionlimit()
    try:
        return _testinternalcapi.run_in_subinterp(code)
    finally:
        sys.set_int_max_str_digits(saved_digits)
        sys.setrecursionlimit(saved_recursion)


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


# ---------------------------------------------------------------------------
# RFC 0060 — the METH_* calling-convention fixtures (test_call).
#
# CPython's C module exposes one function per calling convention, each
# returning its bound `self` plus the arguments it received, with the C
# argument-clinic error messages for misuse. The observable contract is
# entirely about *shapes and messages*, so Python implementations with
# manual checks reproduce it exactly; the vectorcall-specific types
# (`MethodDescriptor*`, `make_vectorcall_class`) stay native because they
# involve `type.__flags__` bits and args-tuple identity.
# ---------------------------------------------------------------------------

def _meth_self():
    return sys.modules[__name__]


def meth_varargs(*args, **kwargs):
    if kwargs:
        raise TypeError("meth_varargs() takes no keyword arguments")
    return (_meth_self(), args)


def meth_varargs_keywords(*args, **kwargs):
    return (_meth_self(), args, kwargs)


def meth_o(*args, **kwargs):
    if kwargs:
        raise TypeError("meth_o() takes no keyword arguments")
    if len(args) != 1:
        raise TypeError(
            f"meth_o() takes exactly one argument ({len(args)} given)")
    return (_meth_self(), args[0])


def meth_noargs(*args, **kwargs):
    if kwargs:
        raise TypeError("meth_noargs() takes no keyword arguments")
    if args:
        raise TypeError(
            f"meth_noargs() takes no arguments ({len(args)} given)")
    return _meth_self()


def meth_fastcall(*args, **kwargs):
    if kwargs:
        raise TypeError("meth_fastcall() takes no keyword arguments")
    return (_meth_self(), args)


def meth_fastcall_keywords(*args, **kwargs):
    return (_meth_self(), args, kwargs)


class MethInstance:
    """Instance methods under each METH_* convention (self = instance)."""

    def meth_varargs(self, *args, **kwargs):
        if kwargs:
            raise TypeError("meth_varargs() takes no keyword arguments")
        return (self, args)

    def meth_varargs_keywords(self, *args, **kwargs):
        return (self, args, kwargs)

    def meth_o(self, *args, **kwargs):
        if kwargs:
            raise TypeError("meth_o() takes no keyword arguments")
        if len(args) != 1:
            raise TypeError(
                f"meth_o() takes exactly one argument ({len(args)} given)")
        return (self, args[0])

    def meth_noargs(self, *args, **kwargs):
        if kwargs:
            raise TypeError("meth_noargs() takes no keyword arguments")
        if args:
            raise TypeError(
                f"meth_noargs() takes no arguments ({len(args)} given)")
        return self

    def meth_fastcall(self, *args, **kwargs):
        if kwargs:
            raise TypeError("meth_fastcall() takes no keyword arguments")
        return (self, args)

    def meth_fastcall_keywords(self, *args, **kwargs):
        return (self, args, kwargs)


class MethClass:
    """Class methods under each METH_* convention (self = class)."""

    @classmethod
    def meth_varargs(cls, *args, **kwargs):
        if kwargs:
            raise TypeError("meth_varargs() takes no keyword arguments")
        return (cls, args)

    @classmethod
    def meth_varargs_keywords(cls, *args, **kwargs):
        return (cls, args, kwargs)

    @classmethod
    def meth_o(cls, *args, **kwargs):
        if kwargs:
            raise TypeError("meth_o() takes no keyword arguments")
        if len(args) != 1:
            raise TypeError(
                f"meth_o() takes exactly one argument ({len(args)} given)")
        return (cls, args[0])

    @classmethod
    def meth_noargs(cls, *args, **kwargs):
        if kwargs:
            raise TypeError("meth_noargs() takes no keyword arguments")
        if args:
            raise TypeError(
                f"meth_noargs() takes no arguments ({len(args)} given)")
        return cls

    @classmethod
    def meth_fastcall(cls, *args, **kwargs):
        if kwargs:
            raise TypeError("meth_fastcall() takes no keyword arguments")
        return (cls, args)

    @classmethod
    def meth_fastcall_keywords(cls, *args, **kwargs):
        return (cls, args, kwargs)


class MethStatic:
    """Static methods under each METH_* convention (self = None)."""

    @staticmethod
    def meth_varargs(*args, **kwargs):
        if kwargs:
            raise TypeError("meth_varargs() takes no keyword arguments")
        return (None, args)

    @staticmethod
    def meth_varargs_keywords(*args, **kwargs):
        return (None, args, kwargs)

    @staticmethod
    def meth_o(*args, **kwargs):
        if kwargs:
            raise TypeError("meth_o() takes no keyword arguments")
        if len(args) != 1:
            raise TypeError(
                f"meth_o() takes exactly one argument ({len(args)} given)")
        return (None, args[0])

    @staticmethod
    def meth_noargs(*args, **kwargs):
        if kwargs:
            raise TypeError("meth_noargs() takes no keyword arguments")
        if args:
            raise TypeError(
                f"meth_noargs() takes no arguments ({len(args)} given)")
        return None

    @staticmethod
    def meth_fastcall(*args, **kwargs):
        if kwargs:
            raise TypeError("meth_fastcall() takes no keyword arguments")
        return (None, args)

    @staticmethod
    def meth_fastcall_keywords(*args, **kwargs):
        return (None, args, kwargs)


# The C-to-Python call probes. In CPython these route through
# `PyObject_VectorcallDict` / `PyObject_Vectorcall` / `PyVectorcall_Call`
# / `PyCFunction_Call`; WeavePy's single native call path *is* what those
# APIs reach, so the probes reduce to plain calls with the same argument
# splitting the C wrappers perform.

def pyobject_fastcalldict(func, args, kwargs):
    return func(*(args or ()), **(kwargs or {}))


def pyobject_vectorcall(func, args, kwnames):
    args = tuple(args or ())
    kwnames = tuple(kwnames or ())
    n = len(args) - len(kwnames)
    if n < 0:
        raise ValueError("kwnames longer than args")
    return func(*args[:n], **dict(zip(kwnames, args[n:])))


def pyvectorcall_call(func, args, kwargs=None):
    return func(*args, **(kwargs or {}))


def pycfunction_call(func, args, kwargs=None):
    return func(*args, **(kwargs or {}))


def has_vectorcall_flag(t):
    """True when `type.__flags__` carries Py_TPFLAGS_HAVE_VECTORCALL."""
    return bool(t.__flags__ & (1 << 11))


def _vectorcall_overridden(*args, **kwargs):
    return "overridden"


def function_setvectorcall(f):
    """`PyFunction_SetVectorcall`: after this, calling `f` returns
    "overridden". WeavePy functions have no separate vectorcall pointer;
    rebinding `__code__` changes the call result through the same single
    dispatch path (and deopts call-site specializations, which is the
    other property the tests probe)."""
    f.__code__ = _vectorcall_overridden.__code__


# The PEP 590 fixture types (native: `type.__flags__` bits +
# args-tuple-identity `tp_call` need interpreter support).
from _testinternalcapi import (  # noqa: F401
    MethodDescriptorBase,
    MethodDescriptorDerived,
    MethodDescriptorNopGet,
    MethodDescriptor2,
    make_vectorcall_class,
)


# ---------------------------------------------------------------------------
# RFC 0060 — frame C-API probes (CPython `Modules/_testcapi/frame.c`),
# exercised by test_frame.TestCAPI.
# ---------------------------------------------------------------------------

import _weave_frame as _wframe


def _check_frame(frame):
    import types

    if not isinstance(frame, types.FrameType):
        raise TypeError("argument must be a frame")


def frame_getlocals(frame):
    _check_frame(frame)
    return frame.f_locals


def frame_getglobals(frame):
    _check_frame(frame)
    return frame.f_globals


def frame_getbuiltins(frame):
    _check_frame(frame)
    return frame.f_builtins


def frame_getlasti(frame):
    _check_frame(frame)
    return frame.f_lasti


def frame_getgenerator(frame):
    _check_frame(frame)
    gen = _wframe.generator(frame)
    if gen is None:
        raise ValueError("frame has no generator")
    return gen


def frame_getvar(frame, name):
    # PyFrame_GetVar: the name must be a str; a missing or unbound
    # variable is a NameError.
    _check_frame(frame)
    if not isinstance(name, str):
        raise TypeError("name must be a str")
    try:
        return frame.f_locals[name]
    except KeyError:
        raise NameError(f"variable {name!r} does not exist") from None


def frame_getvarstring(frame, name):
    return frame_getvar(frame, name.decode("utf-8"))


def frame_new(code, globals, locals):
    return _wframe.frame_new(code, globals, locals)


class _UnraisableHookArgs:
    """The shape `sys.unraisablehook` receives (CPython's
    UnraisableHookArgs struct sequence)."""

    __slots__ = ("exc_type", "exc_value", "exc_traceback", "err_msg", "object")

    def __init__(self, exc_type, exc_value, exc_traceback, err_msg, obj):
        self.exc_type = exc_type
        self.exc_value = exc_value
        self.exc_traceback = exc_traceback
        self.err_msg = err_msg
        self.object = obj


def _fromformat_msg(fmt, args):
    """The PyUnicode_FromFormat subset PyErr_FormatUnraisable feeds it:
    literal text, %%, and the object conversions (%R repr, %S str,
    %A ascii, %d/%i/%s/%U stringified)."""
    out = []
    it = iter(args)
    i = 0
    n = len(fmt)
    while i < n:
        c = fmt[i]
        if c != "%":
            out.append(c)
            i += 1
            continue
        i += 1
        if i >= n:
            break
        conv = fmt[i]
        i += 1
        if conv == "%":
            out.append("%")
            continue
        try:
            a = next(it)
        except StopIteration:
            break
        if conv == "R":
            out.append(repr(a))
        elif conv == "A":
            out.append(ascii(a))
        else:
            out.append(str(a))
    return "".join(out)


def err_formatunraisable(exc, fmt=None, *args):
    """`PyErr_FormatUnraisable`: route `exc` through
    `sys.unraisablehook` with a message formatted by the
    PyUnicode_FromFormat grammar — the format arrives as *bytes* (a C
    `char*`); an undecodable one leaves err_msg NULL, like FromFormatV
    failing (test_exceptions test_err_formatunraisable). Fires the PEP
    578 `sys.unraisablehook` audit event with the resolved hook first
    (test_audit test_unraisablehook)."""
    import sys
    import types

    err_msg = None
    if fmt is not None:
        f = fmt
        if isinstance(f, (bytes, bytearray)):
            try:
                f = bytes(f).decode("utf-8")
            except UnicodeDecodeError:
                f = None
        if f is not None:
            err_msg = _fromformat_msg(f, args)
    # The C API reports the *caller's* line: a fresh exception carries
    # no traceback, so synthesize one from the calling frame (CPython's
    # unraisable machinery does the equivalent via PyTraceBack_Here).
    tb = getattr(exc, "__traceback__", None) if exc is not None else None
    if exc is not None and tb is None:
        try:
            frame = sys._getframe(1)
            tb = types.TracebackType(None, frame, frame.f_lasti, frame.f_lineno)
            exc.__traceback__ = tb
        except BaseException:
            tb = None
    hookargs = _UnraisableHookArgs(
        type(exc) if exc is not None else None,
        exc,
        tb,
        err_msg,
        None,
    )
    hook = getattr(sys, "unraisablehook", None)
    if hook is not None:
        sys.audit("sys.unraisablehook", hook, hookargs)
        hook(hookargs)
        return None
    # A None hook (swap_attr in the test) falls back to the default
    # stderr writer, like CPython's _PyErr_WriteUnraisableDefaultHook.
    from _weave_capi_misc import _default_unraisable_write

    _default_unraisable_write(hookargs)
    return None


# ---------------------------------------------------------------------------
# PEP 669 monitoring C-API fixtures (test_monitoring.TestCApiEventGeneration).
# The fire primitives (`PyMonitoring_Fire*Event`) and scope management
# (`PyMonitoring_EnterScope`/`ExitScope`) live in native code so that
# firing a synthetic event does not itself generate interpreter events;
# only the CodeLike container is Python. Its `_active` list is the
# analogue of the C fixture's `monitoring_states` array: one per-state
# bitmask of tools, snapshotted by `monitoring_enter_scope` and pruned
# in place when a callback returns `sys.monitoring.DISABLE`.

from _testinternalcapi import (  # noqa: F401
    monitoring_enter_scope,
    monitoring_exit_scope,
    fire_event_py_start,
    fire_event_py_resume,
    fire_event_py_return,
    fire_event_c_return,
    fire_event_py_yield,
    fire_event_call,
    fire_event_line,
    fire_event_jump,
    fire_event_branch,
    fire_event_py_throw,
    fire_event_raise,
    fire_event_c_raise,
    fire_event_reraise,
    fire_event_exception_handled,
    fire_event_py_unwind,
    fire_event_stop_iteration,
)


class CodeLike:
    """`_testcapi.CodeLike`: a code-like object carrying PEP 669
    monitoring state slots (`PyCodeLikeObject` in Modules/_testcapi/
    monitoring.c)."""

    def __init__(self, num_events):
        self._num_events = num_events
        self._active = [0] * num_events

    def __repr__(self):
        return f"CodeLike(num_events={self._num_events})"


# ---------------------------------------------------------------------------
# PyArg_ParseTuple / PyArg_ParseTupleAndKeywords fixture family
# (Modules/_testcapi/getargs.c, exercised by test_capi.test_getargs).
# The conversion engine — a Python port of Python/getargs.c — lives in
# the `_weave_getargs` frozen helper.

import _weave_getargs as _ga


class _CFunc:
    """Builtin-function stand-in: callable but *not* a descriptor, so
    stashing a fixture on a test class does not bind `self` (the tests
    do `class C: getargs = _testcapi.getargs_...`)."""

    def __init__(self, func, name=None):
        self._func = func
        self.__name__ = name or getattr(func, "__name__", "fixture")

    def __call__(self, *args, **kwargs):
        return self._func(*args, **kwargs)

    def __repr__(self):
        return "<built-in function %s>" % self.__name__


def get_args(*args):
    return args


def get_kwargs(*args, **kwargs):
    return kwargs


def _pt1(fmt):
    # ParseTuple fixture: one converted unit out, e.g. getargs_b.
    def fixture(*args):
        return _ga.parse_tuple(args, fmt)[0]

    return _CFunc(fixture, "getargs_" + fmt.replace("*", "_star").replace(
        "#", "_hash"))


getargs_b = _pt1("b")
getargs_B = _pt1("B")
getargs_h = _pt1("h")
getargs_H = _pt1("H")
getargs_i = _pt1("i")
getargs_I = _pt1("I")
getargs_k = _pt1("k")
getargs_l = _pt1("l")
getargs_n = _pt1("n")
getargs_L = _pt1("L")
getargs_K = _pt1("K")
getargs_f = _pt1("f")
getargs_d = _pt1("d")
getargs_D = _pt1("D")
getargs_p = _pt1("p")
getargs_c = _pt1("c")
getargs_C = _pt1("C")
getargs_s = _pt1("s")
getargs_s_star = _pt1("s*")
getargs_s_hash = _pt1("s#")
getargs_z = _pt1("z")
getargs_z_star = _pt1("z*")
getargs_z_hash = _pt1("z#")
getargs_y = _pt1("y")
getargs_y_star = _pt1("y*")
getargs_y_hash = _pt1("y#")
getargs_S = _pt1("S")
getargs_Y = _pt1("Y")
getargs_U = _pt1("U")


def getargs_tuple(*args):
    a, group = _ga.parse_tuple(args, "i(ii)")
    return (a,) + group


def getargs_keywords(*args, **kwargs):
    res = _ga.vgetargskeywords(
        args,
        kwargs,
        "(ii)i|(i(ii))(iii)i",
        ["arg1", "arg2", "arg3", "arg4", "arg5"],
    )
    # The C fixture's ten int slots default to -1; an unfilled unit
    # leaves every one of its leaves untouched.
    leaf_counts = [2, 1, 3, 3, 1]
    out = []
    for val, n in zip(res, leaf_counts):
        if val is _ga.NULL:
            out.extend([-1] * n)
        else:
            flat = _ga._flatten(val)
            out.extend(flat)
    return tuple(out)


def getargs_keyword_only(*args, **kwargs):
    res = _ga.vgetargskeywords(
        args, kwargs, "i|i$i", ["required", "optional", "keyword_only"]
    )
    return tuple(-1 if v is _ga.NULL else v for v in res)


def getargs_positional_only_and_keywords(*args, **kwargs):
    res = _ga.vgetargskeywords(args, kwargs, "i|ii", ["", "", "keyword"])
    return tuple(-1 if v is _ga.NULL else v for v in res)


# The tests stash these on test classes; builtins never bind.
getargs_keywords = _CFunc(getargs_keywords)
getargs_keyword_only = _CFunc(getargs_keyword_only)
getargs_positional_only_and_keywords = _CFunc(
    getargs_positional_only_and_keywords
)


def getargs_empty(*args, **kwargs):
    if kwargs:
        _ga.vgetargskeywords(args, kwargs, "|:getargs_empty", [])
    else:
        _ga.parse_tuple(args, "|:getargs_empty")
    return 1


def getargs_es(*args):
    res = _ga.parse_tuple(args, "O|s")
    arg = res[0]
    enc = res[1].decode("utf-8") if len(res) > 1 else None
    return _ga.parse_one(arg, "es", _ga._Va([enc]))


def getargs_et(*args):
    res = _ga.parse_tuple(args, "O|s")
    arg = res[0]
    enc = res[1].decode("utf-8") if len(res) > 1 else None
    return _ga.parse_one(arg, "et", _ga._Va([enc]))


def getargs_es_hash(*args):
    res = _ga.parse_tuple(args, "O|sY")
    arg = res[0]
    enc = res[1].decode("utf-8") if len(res) > 1 else None
    buf = res[2] if len(res) > 2 else None
    return _ga.parse_one(arg, "es#", _ga._Va([enc, buf]))


def getargs_et_hash(*args):
    res = _ga.parse_tuple(args, "O|sY")
    arg = res[0]
    enc = res[1].decode("utf-8") if len(res) > 1 else None
    buf = res[2] if len(res) > 2 else None
    return _ga.parse_one(arg, "et#", _ga._Va([enc, buf]))


def getargs_w_star(*args):
    buf = _ga.parse_tuple(args, "w*")[0]
    buf[0] = ord("[")
    buf[len(buf) - 1] = ord("]")
    return bytes(buf)


def getargs_w_star_opt(*args):
    buf = _ga.parse_tuple(args, "w*|w*i")[0]
    buf[0] = ord("[")
    buf[len(buf) - 1] = ord("]")
    return bytes(buf)


def gh_99240_clear_args(*args):
    _ga.parse_tuple(args, "eses", _ga._Va(["idna", "idna"]))
    return None


def argparsing(*args):
    # Bug #6012 fixture: "O|Oi" round-trip reporting success as 1.
    _ga.parse_tuple(args, "O|Oi:argparsing")
    return 1


def parse_tuple_and_keywords(sub_args, sub_kwargs, sub_format, sub_keywords):
    """The test wrapper around PyArg_ParseTupleAndKeywords itself."""
    if not isinstance(sub_format, str):
        raise TypeError(
            "parse_tuple_and_keywords() argument 3 must be str, not %s"
            % type(sub_format).__name__
        )
    if type(sub_keywords) not in (list, tuple):
        raise ValueError(
            "parse_tuple_and_keywords: "
            "sub_keywords must be either list or tuple"
        )
    if len(sub_keywords) > 8:
        raise ValueError(
            "parse_tuple_and_keywords: too many keywords in sub_keywords"
        )
    kwlist = []
    for o in sub_keywords:
        if isinstance(o, str):
            o.encode("utf-8")  # PyUnicode_AsUTF8 up front
            kwlist.append(o)
        elif isinstance(o, bytes):
            kwlist.append(o)
        else:
            raise ValueError(
                "parse_tuple_and_keywords: keywords must be str or bytes"
            )

    results = _ga.vgetargskeywords(
        tuple(sub_args), sub_kwargs, sub_format, kwlist,
        _ga._Va(zeroed=True),
    )

    # C wrapper: all-object formats echo the filled buffers back.
    objects_only = True
    count = 0
    for ch in sub_format:
        if ch.isalnum():
            if ch not in "OSUY":
                objects_only = False
                break
            count += 1
    if not objects_only:
        return None
    flat = []
    for v in results:
        flat.extend(_ga._flatten(v))
    while len(flat) < count:
        flat.append(_ga.NULL)
    return tuple(None if v is _ga.NULL else v for v in flat[:count])


def test_w_code_invalid():
    # Modules/_testcapi/getargs.c: every 'w'-code format lacking the
    # '*' suffix must be rejected with SystemError.
    keywords = ["a", "b", "c", "d"]
    formats_3 = ["O|w#$O", "O|w$O", "O|w#O", "O|wO"]
    formats_4 = ["O|w#O$O", "O|wO$O", "O|Ow#O", "O|OwO", "O|Ow#$O", "O|Ow$O"]
    for fmt, kw in [(f, {"c": None}) for f in formats_3] + [
        (f, {"d": None}) for f in formats_4
    ]:
        try:
            _ga.vgetargskeywords((None,), kw, fmt, keywords)
        except SystemError:
            continue
        raise AssertionError("test_w_code_invalid_suffix: %s" % fmt)
    return None


# ---------------------------------------------------------------------------
# Assorted C-API surface constants (test_capi legs gate on these at
# import time).

SIZEOF_VOID_P = 8
SIZEOF_WCHAR_T = 4
SIZEOF_PID_T = 4
# Grammar start tokens (Include/compile.h).
Py_single_input = 256
Py_file_input = 257
Py_eval_input = 258
# PyTime_t is int64_t (Include/cpython/pytime.h).
PyTime_MIN = -(2**63)
PyTime_MAX = 2**63 - 1


def PyTime_Time():
    import time as _time

    return _time.time()


PyTime_TimeRaw = PyTime_Time


def PyTime_Monotonic():
    import time as _time

    return _time.monotonic()


PyTime_MonotonicRaw = PyTime_Monotonic


def PyTime_PerfCounter():
    import time as _time

    return _time.perf_counter()


PyTime_PerfCounterRaw = PyTime_PerfCounter


class instancemethod:
    """`PyInstanceMethod_Type` (Objects/classobject.c): wraps any
    callable as an unbound instance method.  Class access hands back
    the bare callable; instance access binds it."""

    def __init__(self, func):
        self.__func__ = func

    def __get__(self, obj, owner=None):
        if obj is None:
            return self.__func__
        import types as _types

        return _types.MethodType(self.__func__, obj)

    def __call__(self, *args, **kwargs):
        return self.__func__(*args, **kwargs)

    @property
    def __doc__(self):
        return self.__func__.__doc__


# ---------------------------------------------------------------------------
# PyRun_StringFlags / PyRun_FileExFlags fixtures (test_capi.test_run).


def _run_checked(source, start, globals, locals):
    # PyEval_EvalCode: globals must be a real dict (subclass ok) —
    # SystemError otherwise; locals may be any mapping — TypeError
    # otherwise.
    if not isinstance(globals, dict):
        raise SystemError("PyEval_EvalCodeEx: globals must be a dict")
    if locals is None:
        locals = globals
    if not hasattr(type(locals), "__getitem__") or isinstance(
        locals, (list, tuple)
    ):
        raise TypeError("locals must be a mapping")
    mode = {256: "single", 257: "exec", 258: "eval"}.get(start, "exec")
    if isinstance(source, bytes):
        # The C parser reports bad UTF-8 as a SyntaxError.
        try:
            source = source.decode("utf-8")
        except UnicodeDecodeError as e:
            raise SyntaxError("(unicode error) %s" % e) from None
    code = compile(source, "<string>", mode)
    result = eval(code, globals, locals)
    return result if mode == "eval" else None


def run_stringflags(source, start, globals=None, locals=None, flags=None):
    return _run_checked(source, start, globals, locals)


def run_fileexflags(filename, start, globals=None, locals=None, closeit=0,
                    flags=None):
    import os as _os

    with open(_os.fsdecode(filename), "rb") as fp:
        source = fp.read()
    return _run_checked(source, start, globals, locals)


# ---------------------------------------------------------------------------
# structmember.h typed-member fixture (test_capi.test_structmembers):
# `PyMember_SetOne`'s conversion, truncation-warning and overflow
# semantics, one property per member code.


def _sm_int_member(name, c_min, c_max, hard_min, hard_max, warn_msg,
                   use_index=True):
    slot = "_" + name

    def conv(value):
        import warnings as _warnings

        if use_index:
            if not isinstance(value, int):
                f = getattr(type(value), "__index__", None)
                if f is None:
                    raise TypeError(
                        "an integer is required (got type %s)"
                        % type(value).__name__
                    )
                value = f(value)
                if not isinstance(value, int):
                    raise TypeError(
                        "__index__ returned non-int (type %s)"
                        % type(value).__name__
                    )
        elif not isinstance(value, int):
            raise TypeError(
                "an integer is required (got type %s)"
                % type(value).__name__
            )
        if isinstance(value, bool):
            value = int(value)
        value = int(value)
        if value < hard_min or value > hard_max:
            raise OverflowError(
                "Python int too large to convert to C %s" % name
            )
        if value < c_min or value > c_max:
            _warnings.warn(warn_msg, RuntimeWarning, stacklevel=3)
            span = c_max - c_min + 1
            value = (value - c_min) % span + c_min
        return value

    def getter(self):
        return getattr(self, slot)

    def setter(self, value):
        object.__setattr__(self, slot, conv(value))

    return property(getter, setter), conv


_LLONG_MIN, _LLONG_MAX = -(2**63), 2**63 - 1
_ULLONG_MAX = 2**64 - 1


class _StructMembers:
    """`test_structmembersType`: one attribute per structmember code."""

    _members = {}

    def __init__(self, *args):
        defaults = [
            ("T_BOOL", False),
            ("T_BYTE", 0),
            ("T_UBYTE", 0),
            ("T_SHORT", 0),
            ("T_USHORT", 0),
            ("T_INT", 0),
            ("T_UINT", 0),
            ("T_LONG", 0),
            ("T_ULONG", 0),
            ("T_PYSSIZET", 0),
            ("T_FLOAT", 0.0),
            ("T_DOUBLE", 0.0),
            ("T_STRING_INPLACE", ""),
        ]
        if len(args) > len(defaults):
            raise TypeError("too many arguments")
        object.__setattr__(self, "_T_LONGLONG", 0)
        object.__setattr__(self, "_T_ULONGLONG", 0)
        for (name, default), value in zip(
            defaults, list(args) + [d for _, d in defaults[len(args) :]]
        ):
            if name == "T_BOOL":
                object.__setattr__(self, "_T_BOOL", bool(value))
            elif name in ("T_FLOAT", "T_DOUBLE"):
                object.__setattr__(self, "_" + name, float(value))
            elif name == "T_STRING_INPLACE":
                object.__setattr__(self, "_" + name, str(value))
            else:
                object.__setattr__(self, "_" + name, int(value))

    @property
    def T_BOOL(self):
        return self._T_BOOL

    @T_BOOL.setter
    def T_BOOL(self, value):
        if not isinstance(value, bool):
            raise TypeError("attribute value type must be bool")
        object.__setattr__(self, "_T_BOOL", value)

    @property
    def T_STRING_INPLACE(self):
        return self._T_STRING_INPLACE

    @T_STRING_INPLACE.setter
    def T_STRING_INPLACE(self, value):
        raise TypeError("readonly attribute")

    @T_STRING_INPLACE.deleter
    def T_STRING_INPLACE(self):
        raise TypeError("readonly attribute")

    @property
    def T_FLOAT(self):
        return self._T_FLOAT

    @T_FLOAT.setter
    def T_FLOAT(self, value):
        object.__setattr__(self, "_T_FLOAT", float(value))

    @property
    def T_DOUBLE(self):
        return self._T_DOUBLE

    @T_DOUBLE.setter
    def T_DOUBLE(self, value):
        object.__setattr__(self, "_T_DOUBLE", float(value))


for _name, _lo, _hi, _hlo, _hhi, _msg, _idx in [
    ("T_BYTE", -128, 127, LONG_MIN, LONG_MAX,
     "Truncation of value to char", True),
    ("T_UBYTE", 0, 255, LONG_MIN, LONG_MAX,
     "Truncation of value to unsigned char", True),
    ("T_SHORT", SHRT_MIN, SHRT_MAX, LONG_MIN, LONG_MAX,
     "Truncation of value to short", True),
    ("T_USHORT", 0, 2**16 - 1, LONG_MIN, LONG_MAX,
     "Truncation of value to unsigned short", True),
    ("T_INT", INT_MIN, INT_MAX, LONG_MIN, LONG_MAX,
     "Truncation of value to int", True),
    ("T_UINT", 0, UINT_MAX, LONG_MIN, ULONG_MAX,
     "Writing negative value into unsigned field", True),
    ("T_LONG", LONG_MIN, LONG_MAX, LONG_MIN, LONG_MAX,
     "", True),
    ("T_ULONG", 0, ULONG_MAX, LONG_MIN, ULONG_MAX,
     "Writing negative value into unsigned field", True),
    ("T_LONGLONG", _LLONG_MIN, _LLONG_MAX, _LLONG_MIN, _LLONG_MAX,
     "", True),
    ("T_ULONGLONG", 0, _ULLONG_MAX, LONG_MIN, _ULLONG_MAX,
     "Writing negative value into unsigned field", True),
    ("T_PYSSIZET", -(2**63), 2**63 - 1, -(2**63), 2**63 - 1,
     "", False),
]:
    _prop, _ = _sm_int_member(_name, _lo, _hi, _hlo, _hhi, _msg, _idx)
    setattr(_StructMembers, _name, _prop)
del _name, _lo, _hi, _hlo, _hhi, _msg, _idx, _prop


_test_structmembersType_OldAPI = _StructMembers


class _StructMembersNew(_StructMembers):
    pass


_test_structmembersType_NewAPI = _StructMembersNew


# RFC 0068 WS3 — per-family C-API fixture shims (test_capi per-leg
# suites). Each module defines `__all__`; the star imports splice its
# fixtures into this namespace. Kept last so nothing above is shadowed.
# test_misc splices every `test_*` fixture into a TestCase namespace
# (`locals().update(get_test_funcs(_testcapi))`), where a plain function
# would wrongly bind `self` like a method; a C builtin doesn't. Wrap the
# zero-argument self-test fixtures so they behave like builtins there.
test_pymem_alloc0 = _CFunc(test_pymem_alloc0)
test_w_code_invalid = _CFunc(test_w_code_invalid)

from _weave_capi_bin import *  # noqa: E402,F401,F403
from _weave_capi_cont import *  # noqa: E402,F401,F403
from _weave_capi_num import *  # noqa: E402,F401,F403
from _weave_capi_text import *  # noqa: E402,F401,F403
from _weave_capi_misc import *  # noqa: E402,F401,F403

# Native PyErr_Restore (kept *after* the star imports so it wins over
# the Python-level port in _weave_capi_misc): raising from a Python
# fixture would land the fixture's own frame in the traceback, but the
# test asserts `caught.__traceback__.tb_next is tb` by identity
# (test_exceptions test_err_restore).
import _testinternalcapi as _tic_native  # noqa: E402

err_restore = _tic_native._err_restore
del _tic_native
