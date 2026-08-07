"""WeavePy `_lsprof` — the deterministic-profiler core behind `cProfile`.

CPython implements this in C (`Modules/_lsprof.c`) as a
`sys.setprofile`-level callback that aggregates per-code-object call
counts and timings. WeavePy's VM fires the same profile events
(`call` / `return` / `c_call` / `c_return` / `c_exception` — RFC 0031
+ RFC 0053 WS5), so the aggregation layer can live in Python: the
dispatcher below reproduces `_lsprof`'s entry/subentry accounting,
recursion tracking, and user-object normalization.

The observable surface matches `_lsprof`:

- ``Profiler(timer=None, timeunit=0.0, subcalls=True, builtins=True)``
- ``enable(subcalls=True, builtins=True)`` / ``disable()`` / ``clear()``
- ``getstats()`` -> list of ``profiler_entry`` rows, whose ``calls``
  fields are lists of ``profiler_subentry`` rows.
"""

import sys as _sys
import time as _time
import types as _types
import operator as _operator
from collections import namedtuple as _namedtuple

__all__ = ["Profiler", "profiler_entry", "profiler_subentry"]

profiler_entry = _namedtuple(
    "profiler_entry",
    ["code", "callcount", "reccallcount", "totaltime", "inlinetime", "calls"],
)
profiler_subentry = _namedtuple(
    "profiler_subentry",
    ["code", "callcount", "reccallcount", "totaltime", "inlinetime"],
)


def _report_unraisable(exc, obj):
    """CPython's `PyErr_WriteUnraisable(pObj)`: route a swallowed
    exception through `sys.unraisablehook` (object = the profiler,
    no err_msg) so `support.catch_unraisable_exception` observes it."""
    hook = getattr(_sys, "unraisablehook", None)
    if hook is None:
        return
    args = _types.SimpleNamespace(
        exc_type=type(exc),
        exc_value=exc,
        exc_traceback=exc.__traceback__,
        err_msg=None,
        object=obj,
    )
    try:
        hook(args)
    except Exception:
        pass


def _normalize(func):
    """`normalizeUserObj` — replace a C callable with a descriptive
    string so entries don't pin ``__self__`` references."""
    self_obj = getattr(func, "__self__", None)
    name = getattr(func, "__name__", None) or "?"
    if self_obj is not None and not isinstance(self_obj, type(_sys)):
        # Bound native callable. CPython's `normalizeUserObj` keys the
        # display string off `m_self`:
        #   * `self` is a *type* (unbound `dict.fromkeys`, or a
        #     classmethod)      -> "<built-in method T.name>"
        #   * `self` is an instance (`[].append`) -> the owning type's
        #     method-descriptor repr, "<method 'name' of 'T' objects>".
        # We synthesize the latter directly from `type(self).__name__`
        # rather than `repr(type(self).name)`, so it is correct
        # regardless of how the descriptor object itself reprs.
        if isinstance(self_obj, type):
            return "<built-in method %s.%s>" % (self_obj.__name__, name)
        return "<method '%s' of '%s' objects>" % (name, type(self_obj).__name__)
    module = getattr(func, "__module__", None)
    if module:
        return "<built-in method %s.%s>" % (module, name)
    return "<built-in method %s>" % (name,)


class _CallEntry:
    """Aggregated stats for one code object / builtin (CPython's
    ``ProfilerEntry``)."""

    __slots__ = (
        "code",
        "callcount",
        "recursionLevel",
        "reccallcount",
        "tt",
        "it",
        "calls",
    )

    def __init__(self, code):
        self.code = code
        self.callcount = 0
        self.recursionLevel = 0
        self.reccallcount = 0
        self.tt = 0.0
        self.it = 0.0
        self.calls = {}


class _SubEntry:
    __slots__ = ("entry", "callcount", "recursionLevel", "reccallcount", "tt", "it")

    def __init__(self, entry):
        self.entry = entry
        self.callcount = 0
        self.recursionLevel = 0
        self.reccallcount = 0
        self.tt = 0.0
        self.it = 0.0


class _Context:
    """One live stack slot (CPython's ``ProfilerContext``)."""

    __slots__ = ("entry", "subentry", "previous", "t0", "subt")

    def __init__(self, entry, subentry, previous, now):
        self.entry = entry
        self.subentry = subentry
        self.previous = previous
        self.t0 = now
        # Time spent in sub-calls while this context was on top;
        # subtracted from the elapsed span to get inline time.
        self.subt = 0.0


class Profiler:
    """Profiler(timer=None, timeunit=None, subcalls=True, builtins=True)

    Builds a profiler object using the specified timer function.
    """

    def __init__(self, timer=None, timeunit=0.0, subcalls=True, builtins=True):
        self._timer = timer
        self._timeunit = float(timeunit or 0.0)
        self._subcalls = bool(subcalls)
        self._builtins = bool(builtins)
        self._entries = {}
        self._current = None
        self._enabled = False

    # -- timing ------------------------------------------------------

    def _now(self):
        # CPython `CallExternalTimer`: with a timeunit the result is
        # read as an integer (PyLong_AsLongLong), otherwise as a double
        # (PyFloat_AsDouble); a conversion failure (bpo-3895: a timer
        # returning a type) is swallowed into the unraisable hook and
        # timed as 0.0 rather than crashing during dealloc/disable.
        timer = self._timer
        if timer is None:
            return _time.perf_counter()
        result = timer()
        try:
            if self._timeunit > 0.0:
                return _operator.index(result)
            return float(result)
        except BaseException as exc:
            _report_unraisable(exc, self)
            return 0.0

    def _scale(self, delta):
        if self._timer is None:
            return delta
        if self._timeunit:
            return delta * self._timeunit
        # External timers returning float seconds scale by 1; integer
        # timers without a unit report raw counts like CPython.
        return delta

    # -- lifecycle ---------------------------------------------------

    def enable(self, subcalls=True, builtins=True):
        # CPython 3.13's `_lsprof` drives `sys.monitoring` and claims
        # the PROFILER_ID tool slot; a second concurrent profiler is a
        # ValueError. Mirror the registration (the actual event stream
        # still arrives through the `sys.setprofile` dispatcher below).
        mon = getattr(_sys, "monitoring", None)
        if mon is not None and not self._enabled:
            mon.use_tool_id(mon.PROFILER_ID, "cProfile")
        self._subcalls = bool(subcalls)
        self._builtins = bool(builtins)
        self._enabled = True
        _sys.setprofile(self._dispatch)

    def disable(self):
        was_enabled = self._enabled
        _sys.setprofile(None)
        mon = getattr(_sys, "monitoring", None)
        if mon is not None and was_enabled:
            try:
                mon.free_tool_id(mon.PROFILER_ID)
            except Exception:
                pass
        self._enabled = False
        # CPython's C profiler observes its own `disable` call as one
        # `{method 'disable' of '_lsprof.Profiler' objects}` row (the
        # c_call fires before the hook is torn down); the Python
        # machinery frames themselves are filtered in `_dispatch`, so
        # synthesize the row here — only when profiling was live.
        if was_enabled:
            entry = self._get_entry(
                "<method 'disable' of '_lsprof.Profiler' objects>"
            )
            entry.callcount += 1
        # Close every context still on the stack so partially-profiled
        # frames report their time-so-far (CPython flushes on disable
        # via `flush_unmatched`).
        now = self._now()
        while self._current is not None:
            self._pop_context(now)

    def clear(self):
        self._entries = {}
        self._current = None

    # -- event plumbing ----------------------------------------------

    def _dispatch(self, frame, event, arg):
        # The profiler's own machinery is invisible in CPython (it runs
        # in C); filter events originating from this file's frames.
        if frame.f_code.co_filename == __file__:
            return
        if event == "call":
            self._push(frame.f_code)
        elif event == "return":
            self._pop(frame.f_code)
        elif event == "c_call":
            if self._builtins:
                self._push(_normalize(arg))
        elif event in ("c_return", "c_exception"):
            if self._builtins:
                self._pop(_normalize(arg))

    def _get_entry(self, code):
        entry = self._entries.get(code)
        if entry is None:
            entry = _CallEntry(code)
            self._entries[code] = entry
        return entry

    def _push(self, code):
        now = self._now()
        if self._timer is not None and not self._enabled:
            # gh-120289 (`initContext` in _lsprof.c): the external timer
            # disabled the profiler mid-call — report and bail instead
            # of pushing a context onto a dead profiler.
            _report_unraisable(
                RuntimeError("the profiler was disabled during the timer call"),
                self,
            )
            return
        entry = self._get_entry(code)
        entry.callcount += 1
        if entry.recursionLevel:
            entry.reccallcount += 1
        entry.recursionLevel += 1
        subentry = None
        if self._subcalls and self._current is not None:
            caller = self._current.entry
            subentry = caller.calls.get(code)
            if subentry is None:
                subentry = _SubEntry(entry)
                caller.calls[code] = subentry
            subentry.callcount += 1
            if subentry.recursionLevel:
                subentry.reccallcount += 1
            subentry.recursionLevel += 1
        self._current = _Context(entry, subentry, self._current, now)

    def _pop(self, code):
        ctx = self._current
        if ctx is None:
            return
        # Unmatched return (profiler enabled mid-call-stack): ignore
        # returns whose code doesn't match the top context.
        if ctx.entry.code != code:
            return
        now = self._now()
        if self._timer is not None and not self._enabled:
            # gh-120289 (`Stop` in _lsprof.c): the external timer
            # disabled the profiler mid-return; `disable()` already
            # flushed the context stack.
            _report_unraisable(
                RuntimeError("the profiler was disabled during the timer call"),
                self,
            )
            return
        self._pop_context(now)

    def _pop_context(self, now):
        ctx = self._current
        tt = self._scale(now - ctx.t0)
        it = tt - self._scale(ctx.subt)
        if it < 0:
            it = 0.0
        entry = ctx.entry
        entry.recursionLevel -= 1
        if entry.recursionLevel == 0:
            entry.tt += tt
        entry.it += it
        sub = ctx.subentry
        if sub is not None:
            sub.recursionLevel -= 1
            if sub.recursionLevel == 0:
                sub.tt += tt
            sub.it += it
        parent = ctx.previous
        if parent is not None:
            parent.subt += now - ctx.t0
        self._current = parent

    # CPython 3.13 exposes the raw `sys.monitoring` callbacks as
    # methods (`test_cprofile.test_crash_with_not_enough_args` probes
    # their arity). WeavePy's event stream arrives via the profile
    # hook instead, so these translate to the same accounting.
    def _pystart_callback(self, code, instruction_offset):
        self._push(code)

    def _pyreturn_callback(self, code, instruction_offset, retval):
        self._pop(code)

    def _ccall_callback(self, code, instruction_offset, callable, self_arg):
        if self._builtins:
            self._push(_normalize(callable))

    def _creturn_callback(self, code, instruction_offset, callable, self_arg):
        if self._builtins:
            self._pop(_normalize(callable))

    # -- reporting ----------------------------------------------------

    def getstats(self):
        """getstats() -> list of profiler_entry objects"""
        if self._enabled:
            # CPython requires disable() before getstats(); mirror the
            # lenient behavior of flushing without stopping.
            pass
        out = []
        for entry in self._entries.values():
            calls = []
            for sub in entry.calls.values():
                calls.append(
                    profiler_subentry(
                        sub.entry.code,
                        sub.callcount,
                        sub.reccallcount,
                        sub.tt,
                        sub.it,
                    )
                )
            out.append(
                profiler_entry(
                    entry.code,
                    entry.callcount,
                    entry.reccallcount,
                    entry.tt,
                    entry.it,
                    calls or None,
                )
            )
        return out
