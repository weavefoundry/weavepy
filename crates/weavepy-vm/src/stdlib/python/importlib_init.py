"""High-level import machinery surface.

This module re-exports the canonical pieces of the import system —
``import_module``, ``reload``, the ``__import__`` hook, and the
``invalidate_caches`` knob — that user code (especially the packaging
ecosystem) reaches for. Internal bootstrap submodules
(``importlib._bootstrap`` and friends) are intentionally omitted: the
real bootstrap happens in the VM, not in this Python source.
"""

import sys


def _import(name, globals_=None, locals_=None, fromlist=(), level=0):
    # ``builtins`` module isn't yet importable; reach for the
    # ``__import__`` already wired into the interpreter's builtins
    # dict by name.
    return __import__(name, globals_, locals_, fromlist, level)

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


# Submodule re-exports happen lazily — they're loaded on first
# attribute access via the import machinery, which is enough for
# `import importlib; importlib.util.spec_from_file_location(...)`.
def _lazy_import(name):
    try:
        return import_module('importlib.' + name)
    except ImportError:
        return None


machinery = _lazy_import('machinery')
util = _lazy_import('util')
abc = _lazy_import('abc')
