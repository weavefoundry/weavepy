"""WeavePy ``runpy`` — locate and run Python modules / scripts.

A faithful port of CPython 3.13's ``runpy`` (PEP 338), specialised only
where WeavePy's import system differs from CPython's:

* Module *introspection* goes through ``importlib.util.find_spec`` and the
  loader's ``get_code`` — the target module is **never imported** to locate
  it, so a ``__main__.py``'s unconditional "main only" body and a module's
  ``if __name__ == '__main__'`` guard do not fire during lookup (only the
  *parent package* is imported, exactly as CPython does, so relative imports
  in the target resolve).
* WeavePy's stdlib ships as *frozen* modules with no on-disk source; their
  code is recovered from ``sys._get_frozen_source`` and compiled under a
  synthetic ``<name>.py`` filename so ``-m <frozenmod>`` reads naturally for
  tracebacks / ``argparse`` ``prog`` (``sys.argv[0]``).

Public surface matches CPython: ``run_module``, ``run_path``,
``_run_module_as_main`` (used by the CLI and ``multiprocessing`` spawn).
"""

import sys
import os


__all__ = ["run_module", "run_path", "_run_module_as_main"]


# avoid 'import types' just for ModuleType
ModuleType = type(sys)


def _import_module(mod_name):
    # Mirror ``importlib.import_module`` using the builtin ``__import__``
    # interface; used only to import a *parent package* (never the run target).
    mod = __import__(mod_name)
    if "." in mod_name:
        for part in mod_name.split(".")[1:]:
            mod = getattr(mod, part)
    return mod


class _TempModule(object):
    """Temporarily replace a module in sys.modules with an empty namespace."""

    def __init__(self, mod_name):
        self.mod_name = mod_name
        self.module = ModuleType(mod_name)
        self._saved_module = []

    def __enter__(self):
        mod_name = self.mod_name
        try:
            self._saved_module.append(sys.modules[mod_name])
        except KeyError:
            pass
        sys.modules[mod_name] = self.module
        return self

    def __exit__(self, *args):
        if self._saved_module:
            sys.modules[self.mod_name] = self._saved_module[0]
        else:
            del sys.modules[self.mod_name]
        self._saved_module = []


class _ModifiedArgv0(object):
    def __init__(self, value):
        self.value = value
        self._saved_value = self._sentinel = object()

    def __enter__(self):
        if self._saved_value is not self._sentinel:
            raise RuntimeError("Already preserving saved value")
        self._saved_value = sys.argv[0] if sys.argv else None
        if sys.argv:
            sys.argv[0] = self.value

    def __exit__(self, *args):
        self.value = self._sentinel
        if sys.argv:
            sys.argv[0] = self._saved_value


def _run_code(code, run_globals, init_globals=None, mod_name=None, mod_spec=None,
              pkg_name=None, script_name=None):
    """Helper to run ``code`` in the nominated namespace."""
    if init_globals is not None:
        run_globals.update(init_globals)
    if mod_spec is None:
        loader = None
        fname = script_name
        cached = None
    else:
        loader = mod_spec.loader
        fname = script_name if script_name is not None else mod_spec.origin
        cached = getattr(mod_spec, "cached", None)
        if pkg_name is None:
            pkg_name = mod_spec.parent
    # WeavePy's `dict.update` doesn't accept kwargs; assign each
    # synthetic dunder explicitly.
    run_globals["__name__"] = mod_name
    run_globals["__file__"] = fname
    run_globals["__cached__"] = cached
    run_globals["__doc__"] = None
    run_globals["__loader__"] = loader
    run_globals["__package__"] = pkg_name
    run_globals["__spec__"] = mod_spec
    exec(code, run_globals)
    return run_globals


def _make_globals(mod_name, file, spec, loader, pkg):
    return {
        "__name__": mod_name,
        "__file__": file,
        "__cached__": getattr(spec, "cached", None) if spec is not None else None,
        "__doc__": None,
        "__loader__": loader,
        "__package__": pkg,
        "__spec__": spec,
        "__builtins__": __builtins__,
    }


def _frozen_source(name):
    getter = getattr(sys, "_get_frozen_source", None)
    if getter is None:
        return None
    try:
        return getter(name)
    except (AttributeError, TypeError):
        return None


def _resolve_filename(name, spec):
    """The ``__file__`` / ``sys.argv[0]`` value for the run.

    A real on-disk source/bytecode path when ``spec`` has one; otherwise a
    synthetic ``<name>.py`` for frozen modules (so ``-m`` reads naturally)."""
    origin = getattr(spec, "origin", None)
    if origin and origin not in ("frozen", "built-in") and not origin.startswith("<"):
        return origin
    return name.replace(".", os.sep) + ".py"


def _code_from_spec(name, spec):
    """Compile the code object for ``name`` *without executing* it.

    Tries, in order: the loader's ``get_code`` (real ``.py``/``.pyc`` on
    ``sys.path``), the on-disk source at ``spec.origin``, and finally the
    frozen-module source table. Returns ``None`` when no Python code exists
    (e.g. an extension module)."""
    loader = getattr(spec, "loader", None)
    origin = getattr(spec, "origin", None)
    is_real_path = (origin and origin not in ("frozen", "built-in")
                    and not origin.startswith("<"))
    if is_real_path and loader is not None and hasattr(loader, "get_code"):
        try:
            code = loader.get_code(name)
        except (ImportError, OSError):
            code = None
        if code is not None:
            return code
    if is_real_path and os.path.exists(origin):
        try:
            with open(origin, "r") as f:
                return compile(f.read(), origin, "exec")
        except OSError:
            pass
    frozen = _frozen_source(name)
    if frozen is not None:
        return compile(frozen, _resolve_filename(name, spec), "exec")
    if loader is not None and hasattr(loader, "get_code"):
        try:
            return loader.get_code(name)
        except (ImportError, OSError):
            return None
    return None


def _get_module_details(mod_name, error=ImportError):
    """Return ``(name, spec, code, filename)`` for ``mod_name``.

    Locates the module via ``importlib.util.find_spec`` (no execution of the
    target) and recovers its code object. A package redirects to its
    ``__main__`` submodule (``python -m pkg`` semantics)."""
    if mod_name.startswith("."):
        raise error("Relative module names not supported")
    import importlib.util
    pkg_name = mod_name.rpartition(".")[0]
    if pkg_name:
        # Importing the parent package (its ``__init__``) is correct and
        # required for the target's relative imports — but we never import
        # the target module itself.
        try:
            __import__(pkg_name)
        except ImportError as e:
            if getattr(e, "name", None) is None or (
                    e.name != pkg_name
                    and not pkg_name.startswith(e.name + ".")):
                raise
        existing = sys.modules.get(mod_name)
        if existing is not None and not hasattr(existing, "__path__"):
            from warnings import warn
            warn(RuntimeWarning(
                "%r found in sys.modules after import of package %r, but "
                "prior to execution of %r; this may result in unpredictable "
                "behaviour" % (mod_name, pkg_name, mod_name)))
    try:
        spec = importlib.util.find_spec(mod_name)
    except (ImportError, AttributeError, TypeError, ValueError) as ex:
        msg = "Error while finding module specification for {!r} ({}: {})"
        if mod_name.endswith(".py"):
            msg += (". Try using '{}' instead of '{}' as the module name."
                    .format(mod_name[:-3], mod_name))
        raise error(msg.format(mod_name, type(ex).__name__, ex)) from ex
    if spec is None:
        raise error("No module named %s" % mod_name)
    if spec.submodule_search_locations is not None:
        if mod_name == "__main__" or mod_name.endswith(".__main__"):
            raise error("Cannot use package as __main__ module")
        try:
            return _get_module_details(mod_name + ".__main__", error)
        except error as e:
            if mod_name not in sys.modules:
                raise
            raise error("%s; %r is a package and cannot be directly executed"
                        % (e, mod_name))
    code = _code_from_spec(mod_name, spec)
    if code is None:
        raise error("No code object available for %s" % mod_name)
    filename = _resolve_filename(mod_name, spec)
    return mod_name, spec, code, filename


class _Error(Exception):
    """Error that _run_module_as_main() should report without a traceback."""


def _get_main_module_details(error=ImportError):
    # Nicer error when executing a zipfile or directory via its __main__.py;
    # also moves the standard __main__ out of the way so its preexisting
    # __loader__/__spec__ doesn't shadow the new module being located.
    main_name = "__main__"
    saved_main = sys.modules.get(main_name)
    if main_name in sys.modules:
        del sys.modules[main_name]
    try:
        return _get_module_details(main_name)
    except ImportError as exc:
        if main_name in str(exc):
            path0 = sys.path[0] if sys.path else None
            raise error("can't find %r module in %r" % (main_name, path0)) from exc
        raise
    finally:
        if saved_main is not None:
            sys.modules[main_name] = saved_main


def _run_module_code(code, init_globals=None, mod_name=None, mod_spec=None,
                     pkg_name=None, script_name=None):
    """Exec ``code`` as ``mod_name`` inside a fresh temporary module that is
    registered in ``sys.modules`` for the duration of the run, then removed
    (CPython's ``_TempModule`` + ``_ModifiedArgv0``).

    Registering the module matters: code that introspects
    ``sys.modules[__name__]`` mid-execution (e.g. ``enum.global_enum``
    hoisting members into the running module, as ``calendar`` does) must see
    the same namespace it is executing in."""
    fname = script_name if mod_spec is None else _resolve_filename(mod_name, mod_spec)
    with _TempModule(mod_name) as temp_module, _ModifiedArgv0(fname):
        mod_globals = temp_module.module.__dict__
        _run_code(code, mod_globals, init_globals, mod_name, mod_spec,
                  pkg_name, fname)
        # Copy out: the temp module's namespace may be cleared on teardown.
        return dict(mod_globals)


def run_module(mod_name, init_globals=None, run_name=None, alter_sys=False):
    """Execute a module's code without importing it. Returns the resulting
    module globals dictionary."""
    name, spec, code, filename = _get_module_details(mod_name)
    if run_name is None:
        run_name = name
    pkg = name.rpartition(".")[0] or None
    if alter_sys:
        return _run_module_code(code, init_globals, run_name, spec, pkg, filename)
    run_globals = _make_globals(run_name, filename, spec, getattr(spec, "loader", None), pkg)
    return _run_code(code, run_globals, init_globals, run_name, spec, pkg, filename)


def _run_module_as_main(mod_name, alter_argv=True):
    """Run the designated module in the ``__main__`` namespace.

    Used by the CLI (directory / zipfile / ``-m`` execution) and by
    ``multiprocessing`` spawn. The executed module has full access to the
    ``__main__`` namespace; ``sys.modules['__main__'].__spec__`` is set to the
    located spec so a child process can reconstruct ``__main__`` faithfully."""
    try:
        if alter_argv or mod_name != "__main__":  # i.e. -m switch
            name, spec, code, filename = _get_module_details(mod_name, _Error)
        else:  # i.e. directory or zipfile execution
            name, spec, code, filename = _get_main_module_details(_Error)
    except _Error as exc:
        msg = "%s: %s" % (sys.executable, exc)
        sys.exit(msg)
    main_globals = sys.modules["__main__"].__dict__
    if alter_argv and sys.argv:
        sys.argv[0] = filename
    return _run_code(code, main_globals, None, "__main__", spec, None, filename)


def _get_code_from_file(fname):
    # Check for a compiled file first.
    from pkgutil import read_code
    import io
    code_path = os.path.abspath(fname)
    with io.open_code(code_path) as f:
        code = read_code(f)
    if code is None:
        # Not a .pyc — compile as source.
        with io.open_code(code_path) as f:
            code = compile(f.read(), fname, "exec")
    return code


def run_path(path_name, init_globals=None, run_name=None):
    """Execute code located at the specified filesystem location: a Python
    script, a ``.pyc`` file, a zipfile, or a directory with a top-level
    ``__main__.py``. Returns the resulting module globals dictionary."""
    if run_name is None:
        run_name = "<run_path>"
    pkg_name = run_name.rpartition(".")[0]
    from pkgutil import get_importer
    importer = get_importer(path_name)
    path_name = os.fsdecode(path_name)
    if importer is None:
        # Not a valid sys.path entry, so run the code directly (this also
        # handles compiled files, which execfile() would not).
        code = _get_code_from_file(path_name)
        return _run_module_code(code, init_globals, run_name,
                                pkg_name=pkg_name, script_name=path_name)
    # A finder is defined for the path (directory / zipfile): add it to the
    # front of sys.path and locate its __main__.
    sys.path.insert(0, path_name)
    try:
        name, spec, code, filename = _get_main_module_details()
        with _TempModule(run_name) as temp_module, _ModifiedArgv0(path_name):
            mod_globals = temp_module.module.__dict__
            return dict(_run_code(code, mod_globals, init_globals,
                                  run_name, spec, pkg_name, filename))
    finally:
        try:
            sys.path.remove(path_name)
        except ValueError:
            pass
