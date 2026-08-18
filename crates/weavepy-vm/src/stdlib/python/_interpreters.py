"""CPython 3.13's low-level `_interpreters` module (PEP 734 plumbing).

WeavePy backs it with the RFC 0031 `_xxsubinterpreters` native core: each
sub-interpreter is a real, isolated `crate::Interpreter` (own module
cache, builtins, `sys.modules`). The registry bookkeeping CPython keeps
in the runtime (whence, refcounts) lives here; the process-wide
interpreter table lives in the native module.

Graded consumer today: `test_capi.test_misc.SubinterpreterTest`
(`_interpreters.InterpreterError` raised by
`run_in_subinterp_with_config` on an invalid PEP 684 config).
"""

import _xxsubinterpreters as _xx

__all__ = [
    "InterpreterError",
    "InterpreterNotFoundError",
    "NotShareableError",
    "new_config",
    "get_config",
    "create",
    "destroy",
    "list_all",
    "get_current",
    "get_main",
    "is_running",
    "whence",
    "incref",
    "decref",
    "exec",
    "run_string",
    "run_func",
    "call",
    "set___main___attrs",
    "is_shareable",
    "capture_exception",
    "WHENCE_UNKNOWN",
    "WHENCE_RUNTIME",
    "WHENCE_LEGACY_CAPI",
    "WHENCE_CAPI",
    "WHENCE_XI",
    "WHENCE_STDLIB",
]


class InterpreterError(Exception):
    """A cross-interpreter operation failed."""


class InterpreterNotFoundError(InterpreterError):
    """An interpreter was not found."""


class NotShareableError(ValueError):
    """An object cannot be sent to another interpreter.

    CPython 3.13 derives this from ValueError (test_interpreters
    TestInterpreterPrepareMain.test_not_shareable catches ValueError).
    """


WHENCE_UNKNOWN = 0
WHENCE_RUNTIME = 1
WHENCE_LEGACY_CAPI = 2
WHENCE_CAPI = 3
WHENCE_XI = 4
WHENCE_STDLIB = 5

# PyInterpreterConfig presets (Include/cpython/initconfig.h): the field
# sets `new_config()` starts from. Values mirror CPython 3.13 with the
# GIL enabled (`Py_GIL_DISABLED` false).
_CONFIG_PRESETS = {
    "isolated": dict(
        use_main_obmalloc=False,
        allow_fork=False,
        allow_exec=False,
        allow_threads=True,
        allow_daemon_threads=False,
        check_multi_interp_extensions=True,
        gil="own",
    ),
    "legacy": dict(
        use_main_obmalloc=True,
        allow_fork=True,
        allow_exec=True,
        allow_threads=True,
        allow_daemon_threads=True,
        check_multi_interp_extensions=False,
        gil="shared",
    ),
    "empty": dict(
        use_main_obmalloc=False,
        allow_fork=False,
        allow_exec=False,
        allow_threads=False,
        allow_daemon_threads=False,
        check_multi_interp_extensions=False,
        gil="default",
    ),
}
_CONFIG_PRESETS["default"] = _CONFIG_PRESETS["isolated"]


def new_config(name="isolated", /, **overrides):
    """Return a PyInterpreterConfig namespace for the named preset,
    updated with `overrides` (CPython's `_interpreters.new_config`,
    graded by test_capi.test_misc InterpreterConfigTests)."""
    import types

    if not isinstance(name, str):
        raise TypeError(f"unsupported config name {name!r}")
    if name == "":
        # The empty string selects the default preset (CPython's
        # `interp_config_from_str(NULL)` path — LowLevelTests
        # test_new_config "default ('')").
        name = "default"
    try:
        fields = dict(_CONFIG_PRESETS[name])
    except KeyError:
        raise ValueError(f"unsupported config name {name!r}") from None
    for key, value in overrides.items():
        if key not in fields:
            raise ValueError(f"unsupported config {key!r}")
        if key == "gil":
            if not isinstance(value, str):
                raise TypeError(
                    f"expected str for config.gil, got {value!r}"
                )
            if value not in ("default", "shared", "own"):
                raise ValueError(f"unsupported config.gil {value!r}")
        else:
            if not isinstance(value, bool):
                raise TypeError(
                    f"expected bool for config.{key}, got {value!r}"
                )
        fields[key] = value
    return types.SimpleNamespace(**fields)


def _exists(interp_id):
    return interp_id == 0 or interp_id in set(_xx.list_all())


def _require(interp_id, restrict=False):
    # CPython's `_PyInterpreterID_LookUp` argument contract: a non-index
    # is TypeError, a negative id is ValueError, an unknown id is
    # InterpreterNotFoundError (test__interpreters test_bad_id /
    # test_error_id / test_does_not_exist across the API surface).
    if not isinstance(interp_id, int):
        try:
            interp_id = type(interp_id).__index__(interp_id)
        except (AttributeError, TypeError):
            raise TypeError(
                f"interpreter ID must be an int, got {interp_id!r}"
            ) from None
    if interp_id < 0:
        raise ValueError(
            f"interpreter ID must be a non-negative int, got {interp_id!r}"
        )
    if not _exists(interp_id):
        raise InterpreterNotFoundError(
            "unrecognized interpreter ID %r" % (interp_id,)
        )
    # `restrict=True` narrows the visible set to interpreters created by
    # this module (whence STDLIB). CPython raises the *base*
    # InterpreterError here, not InterpreterNotFoundError — the
    # "from C-API" subtests regex-match 'InterpreterError.*unrecognized'
    # against the captured excinfo's formatted text.
    if restrict and whence(interp_id) != WHENCE_STDLIB:
        raise InterpreterError(
            "unrecognized interpreter ID %r" % (interp_id,)
        )
    return interp_id


def create(config="isolated", *, reqrefs=False):
    if config is None:
        # None selects the default (isolated) preset, like no argument
        # (LowLevelTests.test_create "config: None").
        config = new_config()
    elif isinstance(config, str):
        # GH-126221: an unencodable preset name is a UnicodeEncodeError,
        # not "unsupported config name" (the C module encodes the
        # argument before the lookup).
        config.encode("utf-8")
        config = new_config(config)
    else:
        # Accept any namespace-like object; snapshot the fields so a
        # later mutation of the caller's object doesn't alter what
        # `get_config()` reports. All fields are required
        # (`_PyInterpreterConfig_InitFromDict` — "missing fields" is a
        # ValueError, extras too).
        fields = dict(vars(config))
        for key in _CONFIG_PRESETS["empty"]:
            if key not in fields:
                raise ValueError(f"missing config field {key!r}")
        config = new_config("empty", **fields)
    # `Py_NewInterpreterFromConfig` viability (CPython
    # `init_interp_settings`): the GIL mode must be resolved, and a
    # per-interpreter allocator cannot host single-phase-init extension
    # modules (LowLevelTests.test_create "config: 'empty'").
    if config.gil not in ("shared", "own"):
        raise InterpreterError(
            "interpreter creation failed: unresolved config.gil"
        )
    if not config.use_main_obmalloc and not config.check_multi_interp_extensions:
        raise InterpreterError(
            "interpreter creation failed: per-interpreter obmalloc does not "
            "support single-phase init extension modules"
        )
    # Thread the PEP 684 config fields the native side consults:
    # the extension gate pair (`gil="own"` is the only own-GIL spelling;
    # "default" means shared in a with-GIL build) plus the
    # `Py_RTFLAGS_*` process-control bits (fork/exec/threads/daemon).
    try:
        interp_id = _xx.create(
            bool(config.check_multi_interp_extensions),
            config.gil == "own",
            bool(config.allow_fork),
            bool(config.allow_exec),
            bool(config.allow_threads),
            bool(config.allow_daemon_threads),
            bool(config.use_main_obmalloc),
        )
    except MemoryError:
        # `Py_NewInterpreterFromConfig` allocation failure — CPython's
        # `interp_create` retypes into InterpreterError
        # (test_interpreters test_stress.test_create_interpreter_no_memory
        # under `_testcapi.set_nomemory`).
        raise InterpreterError("interpreter creation failed") from None
    if reqrefs:
        # CPython's `reqrefs=True` links the interpreter's lifetime to
        # its id refcount: the last `decref` destroys it.
        _xx._link(interp_id, True)
    return interp_id


def destroy(interp_id, *, restrict=False):
    interp_id = _require(interp_id, restrict)
    if interp_id == 0:
        raise InterpreterError("cannot destroy the main interpreter")
    if interp_id == _xx.get_current():
        raise InterpreterError("cannot destroy the current interpreter")
    # Py_EndInterpreter refuses while a thread still runs the
    # interpreter (test__interpreters DestroyTests.test_still_running).
    if _xx.is_running(interp_id):
        raise InterpreterError(f"interpreter {interp_id} is running")
    _finalize_threads(interp_id)
    try:
        _xx.destroy(interp_id)
    except RuntimeError as e:
        # Raced with a run_string starting on another thread.
        raise InterpreterError(str(e)) from None


def _finalize_threads(interp_id):
    # Py_EndInterpreter semantics: run `threading._register_atexit`
    # callbacks, then join the interpreter's remaining non-daemon
    # threads (test_threading test_threads_join_with_no_main).
    code = """if True:
        import sys as _weave_sys
        if 'threading' in _weave_sys.modules:
            import threading as _weave_threading
            _weave_shutdown = getattr(
                _weave_threading, '_SHUTTING_DOWN', None)
            for _weave_fn in list(
                    getattr(_weave_threading, '_threading_atexits', ())):
                try:
                    _weave_fn()
                except Exception:
                    pass
            for _weave_t in _weave_threading.enumerate():
                if (_weave_t is not _weave_threading.current_thread()
                        and not _weave_t.daemon):
                    _weave_t.join()
        """
    try:
        _xx.run_string(interp_id, code)
    except Exception:
        pass


def list_all(*, require_ready=False):
    ids = [0] + list(_xx.list_all())
    return [(i, whence(i)) for i in ids]


def get_current():
    interp_id = _xx.get_current()
    return (interp_id, whence(interp_id))


def get_main():
    return (0, WHENCE_RUNTIME)


def is_running(interp_id, *, restrict=False):
    interp_id = _require(interp_id, restrict)
    if interp_id == 0:
        return True
    return _xx.is_running(interp_id)


def whence(interp_id):
    interp_id = _require(interp_id)
    if interp_id == 0:
        return WHENCE_RUNTIME
    return _xx.whence(interp_id)


def get_config(interp_id, *, restrict=False):
    """The PyInterpreterConfig the interpreter was created with
    (CPython's `_interpreters.get_config`)."""
    import types

    interp_id = _require(interp_id, restrict)
    if interp_id == 0:
        # The main interpreter runs under the legacy config, but owns
        # the (sole) GIL.
        return new_config("legacy", gil="own")
    return types.SimpleNamespace(**_xx.get_config(interp_id))


def incref(interp_id, *, implieslink=False):
    interp_id = _require(interp_id)
    if interp_id == 0:
        return
    _xx._incref(interp_id)
    if implieslink:
        _xx._link(interp_id, True)


def decref(interp_id):
    interp_id = _require(interp_id)
    if interp_id == 0:
        return
    count, linked = _xx._decref(interp_id)
    if linked and count <= 0:
        destroy(interp_id)


def _capture_excinfo(exc):
    import traceback
    import types

    # CPython's `_PyXI_excinfo` snapshot. `type` is a *summary
    # namespace*, not the live class — the exception object itself never
    # crosses the interpreter boundary (test_interpreters
    # CaptureExceptionTests reads __name__/__qualname__/__module__).
    exctype = type(exc)
    typens = types.SimpleNamespace(
        __name__=exctype.__name__,
        __qualname__=exctype.__qualname__,
        __module__=exctype.__module__,
    )
    # `formatted` is the bare "TypeName: message" pair (not
    # `format_exception_only`, whose SyntaxError rendering prepends the
    # source-context lines) — always with the colon, even for an empty
    # message ("ValueError: ", CaptureExceptionTests).
    msg = str(exc)
    formatted = f"{exctype.__name__}: {msg}"
    info = types.SimpleNamespace(type=typens, msg=msg, formatted=formatted)
    # Trim this shim's own frames off the traceback head so `errdisplay`
    # starts at the user's script, like CPython's in-subinterpreter
    # rendering (TestInterpreterExec.test_display_preserved_exception).
    here = globals().get("__file__") or "_interpreters.py"
    tb = exc.__traceback__
    while tb is not None and (
        tb.tb_frame.f_code.co_filename == here
        or tb.tb_frame.f_code.co_filename.endswith("_interpreters.py")
    ):
        tb = tb.tb_next
    try:
        # Instantiated (rather than format_exception) so a patched
        # `TracebackException.format` is honoured — gh-143377.
        tbexc = traceback.TracebackException(exctype, exc, tb)
        errdisplay = "".join(tbexc.format())
        if errdisplay.endswith("\n"):
            errdisplay = errdisplay[:-1]
    except BaseException:
        # A broken formatter leaves the snapshot without `errdisplay`
        # (CPython additionally reports the failure as unraisable in
        # debug builds only).
        pass
    else:
        info.errdisplay = errdisplay
    return info


def capture_exception(exc=None):
    """Return a snapshot of an exception (`_PyXI_excinfo` as a
    SimpleNamespace) — the same shape `exec()` returns on failure."""
    import sys

    if exc is None:
        exc = sys.exception()
        if exc is None:
            raise ValueError("no exception is being handled")
    elif not isinstance(exc, BaseException):
        raise TypeError(f"expected an exception instance, got {exc!r}")
    return _capture_excinfo(exc)


# Distinct from None: `shared=None` must be rejected like any other
# non-dict (test__interpreters CommonTests.test_invalid_shared_none).
_UNSET = object()


def _validate_shared(shared):
    """CPython's `_interpreters` argument contract for `shared`: it must
    be a dict (gh-126654), and str keys must be encodable — a lone
    surrogate raises UnicodeEncodeError before anything runs
    (gh-127196, CommonTests.test_invalid_shared_encoding)."""
    if not isinstance(shared, dict):
        raise TypeError("expected 'shared' to be a dict")
    for key in shared:
        if isinstance(key, str):
            key.encode("utf-8")


def _retype_running(interp_id, exc):
    """The native core reports a busy interpreter with a distinctive
    RuntimeError; the public surface raises InterpreterError
    (RunStringTests.test_already_running)."""
    if str(exc) == f"interpreter {interp_id} is already running":
        raise InterpreterError("interpreter already running") from None


def exec(interp_id, code, shared=_UNSET, *, restrict=False):
    interp_id = _require(interp_id, restrict)
    if shared is not _UNSET:
        _validate_shared(shared)
    import types

    if isinstance(code, str):
        # A str subclass runs through its underlying buffer, not a
        # (possibly overridden) __str__ (RunStringTests
        # test_str_subclass_string).
        if type(code) is not str:
            code = str.__str__(code)
    elif isinstance(code, (types.FunctionType, types.CodeType)):
        return run_func(interp_id, code, shared, restrict=restrict)
    else:
        raise TypeError(
            f"expected a str, function, or code object, got {type(code).__name__}"
        )
    if shared is not _UNSET and shared:
        _xx.set_main_attrs(interp_id, dict(shared))
    try:
        _xx.run_string(interp_id, code)
    except RuntimeError as e:
        _retype_running(interp_id, e)
        return _capture_excinfo(e)
    except BaseException as e:
        return _capture_excinfo(e)
    return None


def run_string(interp_id, script, shared=_UNSET, *, restrict=False):
    interp_id = _require(interp_id, restrict)
    if shared is not _UNSET:
        _validate_shared(shared)
    # `run_string` is stricter than `exec`: str only (RunStringTests
    # test_bad_script / test_bytes_for_script expect TypeError raised,
    # not captured).
    if not isinstance(script, str):
        raise TypeError(f"expected str, got {type(script).__name__}")
    if type(script) is not str:
        script = str.__str__(script)
    if shared is not _UNSET and shared:
        _xx.set_main_attrs(interp_id, dict(shared))
    try:
        _xx.run_string(interp_id, script)
    except RuntimeError as e:
        _retype_running(interp_id, e)
        return _capture_excinfo(e)
    except BaseException as e:
        return _capture_excinfo(e)
    return None


def _stateless_code(func, fname):
    """Validate CPython's `convert_code_arg` contract: `run_func` only
    takes plain stateless functions (or their code objects) — no
    parameters of any kind, no closure (test__interpreters
    RunFuncTests.test_args / test_closure expect ValueError)."""
    import types

    if isinstance(func, types.CodeType):
        code = func
    elif isinstance(func, types.FunctionType):
        if func.__closure__:
            raise ValueError(f"{fname}(): closures not supported")
        code = func.__code__
    else:
        raise TypeError(
            f"{fname}(): expected a function or code object, got {func!r}"
        )
    import inspect

    if (
        code.co_argcount
        or code.co_posonlyargcount
        or code.co_kwonlyargcount
        or code.co_flags & (inspect.CO_VARARGS | inspect.CO_VARKEYWORDS)
    ):
        raise ValueError(f"{fname}(): functions with arguments not supported")
    if code.co_freevars:
        raise ValueError(f"{fname}(): closures not supported")
    if code.co_cellvars:
        raise ValueError(f"{fname}(): functions with cell variables not supported")
    return code


def run_func(interp_id, func, shared=_UNSET, *, restrict=False):
    interp_id = _require(interp_id, restrict)
    if shared is not _UNSET:
        _validate_shared(shared)
    code = _stateless_code(func, "run_func")
    if shared is not _UNSET and shared:
        _xx.set_main_attrs(interp_id, dict(shared))
    try:
        _xx.run_func(interp_id, code)
    except RuntimeError as e:
        _retype_running(interp_id, e)
        return _capture_excinfo(e)
    except BaseException as e:
        return _capture_excinfo(e)
    return None


def call(interp_id, callable, args=None, kwargs=None, *, restrict=False):
    if args or kwargs:
        raise NotImplementedError(
            "arguments not supported by the in-process _interpreters port"
        )
    return run_func(interp_id, callable, restrict=restrict)


def set___main___attrs(interp_id, updates, *, restrict=False):
    interp_id = _require(interp_id, restrict)
    # gh-135855: the exact argument-clinic message shape.
    if not isinstance(updates, dict):
        raise TypeError(
            "_interpreters.set___main___attrs() argument 2 must be dict, not "
            + ("None" if updates is None else type(updates).__name__)
        )
    if not updates:
        raise ValueError("arg 2 must be a non-empty dict")
    for key in updates:
        if isinstance(key, str):
            # GH-127165: embedded NULs broke the C-level lookup; the
            # contract is a plain ValueError.
            if "\x00" in key:
                raise ValueError("embedded null character")
            key.encode("utf-8")
    try:
        _xx.set_main_attrs(interp_id, dict(updates))
    except TypeError as e:
        if "not shareable" in str(e):
            raise NotShareableError(str(e)) from None
        raise
    except RuntimeError as e:
        # `_PyXI_Enter` refuses while the interpreter runs
        # (TestInterpreterPrepareMain.test_running).
        _retype_running(interp_id, e)
        raise


def is_shareable(obj):
    return _xx.is_shareable(obj)
