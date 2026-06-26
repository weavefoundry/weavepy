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


def make_legacy_pyc(source):
    """Move a PEP 3147/488 pyc file to its legacy pyc location.

    :param source: The file system path to the source file.  The source file
        does not need to exist, however the PEP 3147/488 pyc file must exist.
    :return: The file system path to the legacy pyc file.
    """
    import importlib.util
    import shutil
    pyc_file = importlib.util.cache_from_source(source)
    assert source.endswith('.py')
    legacy_pyc = source + 'c'
    shutil.move(pyc_file, legacy_pyc)
    return legacy_pyc


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

        # Block requested names exactly like CPython's real
        # ``_save_and_block_module``: a ``None`` entry in ``sys.modules`` is
        # the import machinery's "halted" sentinel, so any attempt to import
        # the name raises ``ModuleNotFoundError``. WeavePy honours this in its
        # builtin/frozen importer, so blocking a C accelerator (e.g. ``_heapq``
        # / ``_bisect``) forces the pure-Python fallback inside the wrapper
        # module — which is precisely how ``test_heapq``/``test_bisect`` build
        # their C-vs-Python test pairs. (A ``sys.meta_path`` finder can't do
        # this: builtin modules are resolved before ``meta_path`` is consulted.)
        for modname in blocked:
            sys.modules[modname] = None

        try:
            # CPython contract: if any *fresh* module can't be imported the
            # whole call answers None — `import_fresh_module('functools',
            # fresh=['_functools'])` is how test files probe for a missing
            # C accelerator (the C-variant test classes then skip).
            try:
                for modname in fresh:
                    importlib.import_module(modname)
                mod = importlib.import_module(name)
            except ImportError:
                return None
            # A statically-embedded built-in (native) module is a process
            # singleton: re-importing always returns the same object, so
            # it can never be garbage collected. CPython re-creates
            # extension modules on a fresh import; honour that contract
            # (and let callers that drop the result observe its
            # collection — see test_struct's reference-cycle test) by
            # handing back an independent module object populated from the
            # live namespace. Native modules are distinguished by having
            # no source file, whereas frozen modules report '<frozen>'
            # and on-disk modules report a path.
            if getattr(mod, "__file__", None) is None:
                import types
                fresh_mod = types.ModuleType(getattr(mod, "__name__", name))
                fresh_mod.__dict__.update(mod.__dict__)
                return fresh_mod
            return mod
        finally:
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
