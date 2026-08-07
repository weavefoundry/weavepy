"""High-level import machinery surface.

This module re-exports the canonical pieces of the import system —
``import_module``, ``reload``, the ``__import__`` hook, and the
``invalidate_caches`` knob — that user code (especially the packaging
ecosystem) reaches for. Internal bootstrap submodules
(``importlib._bootstrap`` and friends) are intentionally omitted: the
real bootstrap happens in the VM, not in this Python source.
"""

import sys


# Capture the interpreter's import hook *before* this module defines its
# own ``__import__`` below (after which the bare name resolves to the
# module-level function instead of the builtin).
_builtin_import = __import__


def _import(name, globals_=None, locals_=None, fromlist=(), level=0):
    # ``builtins`` module isn't yet importable; reach for the
    # ``__import__`` already wired into the interpreter's builtins
    # dict by name.
    return _builtin_import(name, globals_, locals_, fromlist, level)


def __import__(name, globals=None, locals=None, fromlist=(), level=0):
    """Public programmatic mirror of the builtin ``__import__`` (CPython
    exposes it on importlib for code that wants the import machinery
    without touching builtins — test.test_importlib.util builds its
    Frozen/Source variant table from it)."""
    # CPython `_bootstrap._sanity_check`: reject these *before* the
    # machinery runs (test_importlib.import_.test_api).
    if not isinstance(name, str):
        raise TypeError('module name must be str, not {}'.format(
            type(name).__name__))
    if level < 0:
        raise ValueError('level must be >= 0')
    return _builtin_import(name, globals, locals, fromlist, level)

__all__ = [
    'import_module',
    'reload',
    'invalidate_caches',
    'find_loader',
    'machinery',
    'util',
    'abc',
]


def _resolve_name(name, package, level):
    """Resolve a relative module name. Mirrors CPython's
    ``importlib._bootstrap._resolve_name``.
    """
    if level == 0:
        return name
    if not package:
        raise ImportError(
            "attempted relative import with no known parent package")
    bits = package.rsplit('.', level - 1)
    if len(bits) < level:
        raise ImportError("attempted relative import beyond top-level package")
    base = bits[0]
    return '{}.{}'.format(base, name) if name else base


def import_module(name, package=None):
    """``importlib.import_module('pkg.mod')``."""
    level = 0
    if name.startswith('.'):
        if not package:
            raise TypeError(
                "the 'package' argument is required to perform a relative "
                "import for {!r}".format(name))
        for ch in name:
            if ch != '.':
                break
            level += 1
        name = name[level:]
    abs_name = _resolve_name(name, package, level)
    return _import(abs_name, globals(), locals(), ['__name__'], 0)


def reload(module):
    """Re-execute a previously imported module."""
    if not hasattr(module, '__name__'):
        raise TypeError("reload() argument must be a module")
    name = module.__name__
    if name not in sys.modules:
        raise ImportError(
            "module {!r} not in sys.modules".format(name), name=name)
    spec = getattr(module, '__spec__', None)
    if spec is None:
        # Try to discover a spec via the loader chain.
        from . import util
        spec = util.find_spec(name)
    if spec is None:
        raise ImportError(
            "no loader available for {!r}".format(name), name=name)
    loader = spec.loader
    if loader is None or not hasattr(loader, 'exec_module'):
        # Fall back to a fresh __import__.
        del sys.modules[name]
        return _import(name, globals(), locals(), ['__name__'], 0)
    loader.exec_module(module)
    return module


def invalidate_caches():
    """Clear any cached finder state. We don't yet maintain caches
    beyond ``sys.modules`` (which we do not clear here); CPython's
    sibling ``PathFinder`` would walk every entry on ``sys.meta_path``.
    """
    for finder in sys.meta_path:
        if hasattr(finder, 'invalidate_caches'):
            try:
                finder.invalidate_caches()
            except Exception:
                pass


def find_loader(name, path=None):
    """Compat shim: deprecated upstream but still called by some
    packaging code. Falls back to ``find_spec``.
    """
    from . import util
    spec = util.find_spec(name, path)
    return spec.loader if spec else None


# Submodule re-exports are PEP 562 lazy: CPython's importlib/__init__
# binds *no* submodules at import time, and eagerly importing `abc`
# here would drag in `importlib.resources` → `inspect` → `ast` on any
# startup spec synthesis. `test_traceback.test_print_traceback_at_exit`
# depends on `ast` NOT being in sys.modules at finalization (so the
# caret-anchor helper's `import ast` fails and full-range carets are
# printed). A successful `import importlib.abc` still binds the
# attribute directly on this package, bypassing this hook thereafter.
# (`_bootstrap`/`_bootstrap_external` are reachable the same way —
# `test_zipimport` and packaging tools import them directly.)
def __getattr__(name):
    if name in ('machinery', 'util', 'abc', '_bootstrap',
                '_bootstrap_external'):
        module = import_module('importlib.' + name)
        globals()[name] = module
        return module
    if name in ('_pack_uint32', '_unpack_uint32'):
        # CPython's importlib/__init__ re-exports these from
        # `_bootstrap_external` at import time; keep them lazy here for
        # the same startup-weight reason as the submodules above.
        module = import_module('importlib._bootstrap_external')
        value = getattr(module, name)
        globals()[name] = value
        return value
    raise AttributeError(
        "module 'importlib' has no attribute {!r}".format(name))
