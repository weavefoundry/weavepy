"""WeavePy `runpy` — locate and run Python modules / scripts.

Implements the two functions CPython users actually reach for:

* `run_module(mod_name, init_globals=None, run_name="__main__",
              alter_sys=False)`
* `run_path(path, init_globals=None, run_name="__main__")`

Plus helper `_run_code` that drives the actual `exec()`.
"""

import sys
import os


__all__ = ["run_module", "run_path", "_run_module_as_main"]


def _import_module(mod_name):
    # Mirror ``importlib.import_module`` using the builtin ``__import__``
    # interface so this module works in WeavePy without ``importlib``.
    mod = __import__(mod_name)
    if "." in mod_name:
        for part in mod_name.split(".")[1:]:
            mod = getattr(mod, part)
    return mod


def _run_code(code, run_globals, init_globals=None, mod_name=None, mod_spec=None,
              pkg_name=None, script_name=None):
    if init_globals is not None:
        run_globals.update(init_globals)
    # WeavePy's `dict.update` doesn't accept kwargs yet; assign each
    # synthetic dunder explicitly.
    run_globals["__name__"] = mod_name
    run_globals["__file__"] = script_name
    run_globals["__cached__"] = None
    run_globals["__doc__"] = None
    run_globals["__loader__"] = None
    run_globals["__package__"] = pkg_name
    run_globals["__spec__"] = mod_spec
    exec(code, run_globals)
    return run_globals


def _module_exists(name):
    """True if ``name`` is importable as a frozen/builtin/file module."""
    if name in sys.modules:
        return True
    getter = getattr(sys, "_get_frozen_source", None)
    if getter is not None and getter(name) is not None:
        return True
    try:
        __import__(name)
        return True
    except ImportError:
        return False
    except Exception:
        # A module that exists but fails to import still "exists".
        return True


def _get_module_details(mod_name):
    _import_module(mod_name)
    mod = sys.modules.get(mod_name)
    # CPython: running a *package* with ``-m`` runs its ``__main__``
    # submodule (e.g. ``python -m test`` -> ``test.__main__``). Detect a
    # package by ``__path__`` and redirect when a ``__main__`` exists;
    # otherwise fall back to executing the package ``__init__`` so no
    # existing ``-m <module>`` target regresses.
    if mod is not None and hasattr(mod, "__path__"):
        main_name = mod_name + ".__main__"
        if _module_exists(main_name):
            return _get_module_details(main_name)
    filename = getattr(mod, "__file__", None)
    return mod_name, getattr(mod, "__spec__", None), mod, filename


def _make_globals(mod_name, file, spec, loader, pkg):
    return {
        "__name__": mod_name,
        "__file__": file,
        "__cached__": None,
        "__doc__": None,
        "__loader__": loader,
        "__package__": pkg,
        "__spec__": spec,
        "__builtins__": __builtins__,
    }


def _run_module_code(code, init_globals=None, mod_name=None, mod_spec=None,
                     pkg_name=None, script_name=None):
    """Exec ``code`` as ``mod_name`` inside a fresh temporary module that
    is registered in ``sys.modules`` for the duration of the run, then
    removed — CPython's ``runpy._TempModule`` + ``_ModifiedArgv0``.

    Registering the module matters: code that introspects
    ``sys.modules[__name__]`` mid-execution (e.g. ``enum.global_enum``
    hoisting members into the running module's globals, as ``calendar``
    does) must see the *same* namespace it is executing in."""
    import types
    saved_module = sys.modules.get(mod_name)
    saved_argv0 = sys.argv[0] if sys.argv else None
    temp_module = types.ModuleType(mod_name)
    sys.modules[mod_name] = temp_module
    try:
        if script_name is not None and sys.argv:
            sys.argv[0] = script_name
        mod_globals = temp_module.__dict__
        _run_code(code, mod_globals, init_globals, mod_name, mod_spec,
                  pkg_name, script_name)
        # Return a snapshot so callers can't mutate the (now-removed)
        # temporary module's namespace.
        return dict(mod_globals)
    finally:
        if saved_argv0 is not None and sys.argv:
            sys.argv[0] = saved_argv0
        if saved_module is not None:
            sys.modules[mod_name] = saved_module
        elif mod_name in sys.modules:
            del sys.modules[mod_name]


def run_module(mod_name, init_globals=None, run_name=None, alter_sys=False):
    """Locate ``mod_name`` and exec it with ``__name__`` set."""
    if run_name is None:
        run_name = mod_name
    name, spec, mod, filename = _get_module_details(mod_name)
    if mod is None:
        raise ImportError(name)
    pkg = name.rpartition(".")[0] or None
    source = None
    if filename and os.path.exists(filename):
        with open(filename, "r") as f:
            source = f.read()
    if source is None:
        # Frozen module path — pull the original source out of the
        # interpreter's frozen-module table. This is the only way to
        # re-execute a frozen module under a new ``__name__`` (e.g.
        # for ``runpy.run_module('venv')``).
        frozen_source = None
        getter = getattr(sys, "_get_frozen_source", None)
        if getter is not None:
            # Use the *resolved* name, not the original argument: running a
            # frozen package (`-m zipfile`) redirects to `<pkg>.__main__`, so
            # we must fetch that submodule's source rather than re-running the
            # package `__init__`.
            frozen_source = getter(name)
        if frozen_source is not None:
            source = frozen_source
            # A frozen module has no real path; synthesise a CPython-like
            # ``<modpath>.py`` so ``-m``'s ``sys.argv[0]`` / argparse
            # ``prog`` and tracebacks read naturally (``calendar.py``)
            # rather than the opaque ``<frozen>`` placeholder.
            if not filename or filename.startswith("<frozen"):
                filename = name.replace(".", os.sep) + ".py"
        else:
            # Truly no source — fall back to the existing module's
            # __dict__ so callers at least get the imported names.
            run_globals = dict(mod.__dict__)
            if init_globals:
                run_globals.update(init_globals)
            run_globals["__name__"] = run_name
            return run_globals
    code = compile(source, filename or f"<{name}>", "exec")
    if alter_sys:
        return _run_module_code(code, init_globals, run_name, spec, pkg,
                                filename)
    run_globals = _make_globals(run_name, filename, spec, None, pkg)
    return _run_code(code, run_globals, init_globals, run_name, spec, pkg, filename)


def _run_module_as_main(mod_name, alter_argv=True):
    return run_module(mod_name, run_name="__main__", alter_sys=True)


def run_path(path_name, init_globals=None, run_name=None):
    """Read and execute ``path_name`` as a script."""
    if run_name is None:
        run_name = "<run_path>"
    if os.path.isdir(path_name):
        # Allow `python <dir>` to fall through to `__main__.py`.
        main_path = os.path.join(path_name, "__main__.py")
        if os.path.exists(main_path):
            path_name = main_path
        else:
            raise ImportError(f"Cannot find __main__.py in {path_name!r}")
    with open(path_name, "r") as f:
        source = f.read()
    code = compile(source, path_name, "exec")
    run_globals = {
        "__name__": run_name,
        "__file__": path_name,
        "__cached__": None,
        "__doc__": None,
        "__loader__": None,
        "__package__": None,
        "__spec__": None,
        "__builtins__": __builtins__,
    }
    return _run_code(code, run_globals, init_globals, run_name, None, None, path_name)
