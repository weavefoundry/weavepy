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
            frozen_source = getter(mod_name)
        if frozen_source is not None:
            source = frozen_source
            filename = filename or f"<frozen:{mod_name}>"
        else:
            # Truly no source — fall back to the existing module's
            # __dict__ so callers at least get the imported names.
            run_globals = dict(mod.__dict__)
            if init_globals:
                run_globals.update(init_globals)
            run_globals["__name__"] = run_name
            return run_globals
    if alter_sys:
        # ``argv[0]`` becomes the module filename (the Python target);
        # the rest of argv is preserved.
        if filename:
            sys.argv[0] = filename
    code = compile(source, filename or f"<{name}>", "exec")
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
