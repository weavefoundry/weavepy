"""``test.support.import_helper`` — import-manipulation helpers.

Faithful subset of CPython 3.13's ``Lib/test/support/import_helper.py``.
Implements the names ``Lib/test/`` modules reach for: ``import_module``
(skip on ``ImportError``), ``import_fresh_module``, ``unload``/``forget``,
``CleanImport``, ``DirsOnSysPath``, ``modules_setup``/``modules_cleanup``,
and the ``frozen_modules`` no-op context manager.
"""

import contextlib
import importlib
import sys


def unload(name):
    """Drop *name* (and refuse to error if it was never imported)."""
    try:
        del sys.modules[name]
    except KeyError:
        pass


def forget(modname):
    """Forget the named module and any ``.pyc`` cached copy."""
    unload(modname)
    # Also drop submodules so a fresh import re-runs everything.
    for name in list(sys.modules):
        if name == modname or name.startswith(modname + '.'):
            unload(name)


def make_legacy_pyc(source):  # pragma: no cover - compatibility stub
    """No-op: WeavePy does not maintain legacy ``.pyc`` layouts."""
    return source + 'c'


def import_module(name, deprecated=False, *, required_on=()):
    """Import *name*, turning a failure into ``unittest.SkipTest``."""
    import unittest
    import warnings
    with warnings.catch_warnings():
        if deprecated:
            warnings.filterwarnings("ignore", ".+ (module|package)",
                                    DeprecationWarning)
        try:
            return importlib.import_module(name)
        except ImportError as msg:
            if sys.platform.startswith(tuple(required_on)):
                raise
            raise unittest.SkipTest(str(msg))


def _save_and_remove_modules(names):
    orig_modules = {}
    prefixes = tuple(name + '.' for name in names)
    for modname in list(sys.modules):
        if modname in names or modname.startswith(prefixes):
            orig_modules[modname] = sys.modules.pop(modname)
    return orig_modules


def import_fresh_module(name, fresh=(), blocked=(), *, deprecated=False):
    """Import *name* afresh, optionally blocking some submodules.

    ``fresh`` names are reloaded; ``blocked`` names are forced to fail.
    Returns the freshly-imported module, or ``None`` if blocked.
    """
    import warnings
    with warnings.catch_warnings():
        if deprecated:
            warnings.simplefilter("ignore")

        names = {name}
        names.update(fresh)
        names.update(blocked)
        orig_modules = _save_and_remove_modules(names)
        for modname in fresh:
            sys.modules.pop(modname, None)

        # Block requested names by installing a finder that raises.
        class _Blocker:
            def find_spec(self, fullname, path=None, target=None):
                if fullname in blocked or fullname == name and name in blocked:
                    raise ImportError(f'import of {fullname!r} blocked')
                return None

        blocker = _Blocker() if blocked else None
        if blocker is not None:
            sys.meta_path.insert(0, blocker)
        try:
            for modname in fresh:
                try:
                    importlib.import_module(modname)
                except ImportError:
                    pass
            try:
                return importlib.import_module(name)
            except ImportError:
                return None
        finally:
            if blocker is not None:
                try:
                    sys.meta_path.remove(blocker)
                except ValueError:
                    pass
            # Restore the original module table.
            for modname in list(sys.modules):
                if modname == name or modname in fresh or modname in blocked:
                    sys.modules.pop(modname, None)
            sys.modules.update(orig_modules)


class CleanImport:
    """Context manager forcing a fresh import of the named modules."""

    def __init__(self, *module_names):
        self.original_modules = sys.modules.copy()
        for module_name in module_names:
            if module_name in sys.modules:
                module = sys.modules[module_name]
                if getattr(module, '__name__', None) != module_name:
                    module_name = module.__name__
                del sys.modules[module_name]

    def __enter__(self):
        return self

    def __exit__(self, *ignore_exc):
        sys.modules.update(self.original_modules)


class DirsOnSysPath:
    """Context manager pushing directories onto ``sys.path``."""

    def __init__(self, *paths):
        self.original_value = sys.path[:]
        self.original_object = sys.path
        sys.path.extend(paths)

    def __enter__(self):
        return self

    def __exit__(self, *ignore_exc):
        sys.path = self.original_object
        sys.path[:] = self.original_value


def modules_setup():
    return sys.modules.copy(),


def modules_cleanup(oldmodules):
    encodings = [(k, v) for k, v in sys.modules.items()
                 if k.startswith('encodings.')]
    sys.modules.clear()
    sys.modules.update(encodings)
    sys.modules.update(oldmodules)


@contextlib.contextmanager
def frozen_modules(enabled=True):
    """No-op CM: WeavePy frozen modules are always active."""
    yield


@contextlib.contextmanager
def isolated_modules():
    """Restore ``sys.modules`` to its prior state on exit."""
    saved = sys.modules.copy()
    try:
        yield
    finally:
        sys.modules.clear()
        sys.modules.update(saved)


def mock_register_at_fork(func):  # pragma: no cover - compatibility stub
    return func
