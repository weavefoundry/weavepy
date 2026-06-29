"""``_pytest`` — small but real pytest-compatible runner.

A WeavePy-native test runner that implements enough of pytest's
surface to drive most testing workflows that don't reach for plugins:

* ``pytest path/`` test discovery — collects ``test_*.py`` /
  ``*_test.py`` under the path, then ``test_*`` / ``Test*`` symbols
  inside each module.
* ``pytest.fixture`` (basic, no parametrise / no per-scope yet —
  fixtures take an optional ``scope`` kwarg and produce request-time
  values).
* ``pytest.raises`` / ``pytest.warns`` / ``pytest.skip`` /
  ``pytest.fail`` / ``pytest.xfail`` / ``pytest.mark.{skip,xfail}``.
* ``pytest.approx`` for float comparison.
* ``conftest.py`` discovery up the directory tree.
* ``-v`` / ``-q`` / ``-x`` / ``--lf`` / ``-k`` selectors.
* Exit codes match pytest: 0=success, 1=failed, 2=interrupted,
  3=internal error, 4=usage, 5=no tests.

The bundled module exposes itself under both ``_pytest`` and
``pytest`` so user code that imports either spelling works.
"""

import importlib
import importlib.util
import inspect
import os
import re
import sys
import time
import traceback


__all__ = [
    'main', 'fixture', 'raises', 'warns', 'skip', 'fail', 'xfail',
    'importorskip',
    'approx', 'mark', 'param', 'Session', 'Item', 'Collector', 'ExitCode',
    'Module', 'Function', 'Class',
    'UsageError', 'CollectionError',
    'PytestWarning', 'PytestConfigWarning', 'PytestCollectionWarning',
    'PytestDeprecationWarning', 'PytestUnknownMarkWarning',
    'PytestUnraisableExceptionWarning', 'PytestUnhandledThreadExceptionWarning',
]


# ============================================================ exceptions

class UsageError(Exception):
    """Raised on bad CLI input."""


class CollectionError(Exception):
    """Raised when test collection fails for a node."""


class _Skipped(Exception):
    pass


class _Failed(AssertionError):
    pass


class _XFailed(Exception):
    pass


class _XPassed(Exception):
    pass


class ExitCode:
    OK = 0
    TESTS_FAILED = 1
    INTERRUPTED = 2
    INTERNAL_ERROR = 3
    USAGE_ERROR = 4
    NO_TESTS_COLLECTED = 5


# ============================================================ pytest warning types
#
# Projects reference these in ``filterwarnings`` ini entries (pandas:
# ``"error::pytest.PytestUnraisableExceptionWarning"``), so the category
# resolver must find them on the ``pytest`` module.


class PytestWarning(UserWarning):
    """Base for all pytest-emitted warnings."""


class PytestConfigWarning(PytestWarning):
    pass


class PytestCollectionWarning(PytestWarning):
    pass


class PytestDeprecationWarning(PytestWarning, DeprecationWarning):
    pass


class PytestUnknownMarkWarning(PytestWarning):
    pass


class PytestUnraisableExceptionWarning(PytestWarning):
    pass


class PytestUnhandledThreadExceptionWarning(PytestWarning):
    pass


# ============================================================ filterwarnings
#
# pytest applies warning filters in three layers, later layers taking
# precedence: the ``filterwarnings`` ini option (pyproject.toml /
# pytest.ini), then ``@pytest.mark.filterwarnings`` on the item. Python's
# ``warnings.filterwarnings`` *prepends* to the filter list and the list
# is first-match, so installing ini filters first and mark filters after
# yields pytest's precedence exactly. pandas leans on this: its
# pyproject declares ``"error:::pandas"`` (warnings attributed to any
# ``pandas.*`` module become errors), which is what makes
# ``pytest.raises(FutureWarning)`` idioms in its suite work at all.


def _resolve_warning_category(name):
    """Dotted-path (or builtin) warning category; None if unresolvable."""
    name = name.strip()
    if not name:
        return Warning
    if '.' not in name:
        import builtins
        cat = getattr(builtins, name, None)
    else:
        modname, _, klass = name.rpartition('.')
        try:
            mod = importlib.import_module(modname)
        except Exception:
            return None
        cat = getattr(mod, klass, None)
    if isinstance(cat, type) and issubclass(cat, Warning):
        return cat
    return None


def _install_warning_filter(spec):
    """Install one ``action:message:category:module:lineno`` filter spec.

    Mirrors pytest's ``parse_warning_filter``: message and module are
    regexes (unlike ``-W``'s literal prefixes). Unresolvable categories
    (e.g. a module that isn't installed) drop the filter instead of
    erroring — the lenient reading a shim wants.
    """
    import warnings as _warnings
    parts = str(spec).split(':')
    while len(parts) < 5:
        parts.append('')
    action, message, category, module, lineno = (p.strip() for p in parts[:5])
    cat = _resolve_warning_category(category)
    if cat is None:
        return
    try:
        lineno_int = int(lineno) if lineno else 0
    except ValueError:
        lineno_int = 0
    try:
        _warnings.filterwarnings(action or 'always', message=message,
                                 category=cat, module=module,
                                 lineno=lineno_int)
    except Exception:
        pass


_INI_FILTER_CACHE = {}


def _config_filterwarnings(start_path):
    """``filterwarnings`` ini entries governing ``start_path``.

    Walks up from the test file looking for a ``pyproject.toml`` with
    ``[tool.pytest.ini_options]`` (pandas ships one inside the package,
    so its tests find their own config wherever the venv lives).
    """
    d = os.path.dirname(os.path.abspath(start_path))
    seen = []
    result = []
    while d:
        if d in _INI_FILTER_CACHE:
            result = _INI_FILTER_CACHE[d]
            break
        seen.append(d)
        pp = os.path.join(d, 'pyproject.toml')
        if os.path.isfile(pp):
            filters = []
            try:
                import tomllib
                with open(pp, 'rb') as fh:
                    data = tomllib.load(fh)
                ini = data.get('tool', {}).get('pytest', {}).get('ini_options', {})
                filters = [str(f) for f in (ini.get('filterwarnings') or [])]
                found_config = 'filterwarnings' in ini or bool(ini)
            except Exception:
                found_config = False
            if found_config:
                result = filters
                break
        parent = os.path.dirname(d)
        if parent == d:
            break
        d = parent
    for s in seen:
        _INI_FILTER_CACHE[s] = result
    return result


def _item_source_path(item):
    """The test file an item was collected from (walks up to Module)."""
    node = item
    while node is not None:
        path = getattr(node, 'path', None)
        if path:
            return path
        node = getattr(node, 'parent', None)
    return None


# ============================================================ skip/fail/xfail


def skip(reason: str = ''):
    raise _Skipped(reason or 'skipped')


def fail(msg: str = '', pytrace: bool = True):  # noqa: ARG001
    raise _Failed(msg)


def xfail(reason: str = ''):
    raise _XFailed(reason or 'xfail')


def importorskip(modname, minversion=None, reason=None):
    """Import ``modname`` or skip the test if it is unavailable.

    Mirrors ``pytest.importorskip``: pandas guards optional-dependency tests
    (scipy, pyarrow, numexpr, …) with it, so a missing dependency must skip
    rather than error. Returns the imported (sub)module on success.
    """
    try:
        __import__(modname)
    except ImportError as exc:
        raise _Skipped(
            reason or "could not import {!r}: {}".format(modname, exc))
    mod = sys.modules[modname]
    if minversion is not None:
        have = getattr(mod, '__version__', None)
        if have is not None and _version_tuple(have) < _version_tuple(minversion):
            raise _Skipped(
                reason or "module {!r} has __version__ {!r}, required {!r}".format(
                    modname, have, minversion))
    return mod


def _version_tuple(v):
    """Best-effort dotted-version parse for ``importorskip(minversion=...)``."""
    out = []
    for part in str(v).split('.'):
        num = ''
        for ch in part:
            if ch.isdigit():
                num += ch
            else:
                break
        out.append(int(num) if num else 0)
    return tuple(out)


# ============================================================ marker module

class _MarkerDecorator:
    def __init__(self, name, args=(), kwargs=None):
        self.name = name
        self.args = args
        self.kwargs = kwargs or {}

    def __call__(self, *args, **kwargs):
        # Called either as `@mark.skip("reason")` (returns decorated fn) or
        # `mark.skip(reason="...")(fn)` (also decorated). Support both.
        if len(args) == 1 and callable(args[0]) and not kwargs:
            fn = args[0]
            # Store under real pytest's attribute name (``pytestmark``), not
            # a private one: third-party wrappers propagate it by that name.
            # hypothesis's ``@given`` copies only *public* attributes onto
            # its wrapper, so marks stashed as ``_pytest_marks`` vanished —
            # every parametrized hypothesis test (pandas'
            # ``test_hypothesis_delimited_date``) then ran without its
            # parametrize arguments. Like real pytest's ``store_mark``, only
            # the object's *own* marks are extended (a class must not copy —
            # and thus double-apply — marks inherited from its bases).
            own = fn.__dict__.get('pytestmark', [])
            fn.pytestmark = list(own) + [self]
            return fn
        return _MarkerDecorator(self.name, args, kwargs)

    def __repr__(self):
        return '<Mark {}({}{}{})>'.format(
            self.name, self.args,
            ', ' if self.args and self.kwargs else '',
            self.kwargs)


class _MarkModule:
    def __init__(self):
        self.skip = _MarkerDecorator('skip')
        self.skipif = _MarkerDecorator('skipif')
        self.xfail = _MarkerDecorator('xfail')
        self.parametrize = _MarkerDecorator('parametrize')
        self.usefixtures = _MarkerDecorator('usefixtures')
        self.tryfirst = _MarkerDecorator('tryfirst')
        self.trylast = _MarkerDecorator('trylast')

    def __getattr__(self, name):
        # Allow arbitrary custom marks: `@mark.slow`.
        m = _MarkerDecorator(name)
        setattr(self, name, m)
        return m


mark = _MarkModule()


# ============================================================ fixture system


# `name -> _FixtureDef` registry. RFC 0031: extended to support
# scopes, params (parametrized fixtures), autouse, and yield-style
# fixtures with `request.addfinalizer` teardown.
_FIXTURE_REGISTRY = {}

# Distinguishes "fixture not found / not cached" from a fixture that
# legitimately produced ``None`` (e.g. pandas' ``tz_naive_fixture`` yields
# ``None`` for the naive timezone). Using ``None`` as the sentinel would drop
# that value and raise a spurious "missing argument" error.
_NOTSET = object()

# Set by `_run` for the duration of a session so `request.config` (and any
# other config-consulting shim surface) can reach the active `_Config`.
_ACTIVE_CONFIG = None
# The live ``Session`` for the current run; ``Collector.session`` falls back
# to this when a node's parent chain does not reach the session (e.g. items
# whose Class parent was built before session wiring).
_ACTIVE_SESSION = None


class _FixtureDef:
    __slots__ = ('fn', 'scope', 'params', 'ids', 'autouse', 'name', 'generator')

    def __init__(self, fn, scope, params, ids, autouse, name):
        self.fn = fn
        self.scope = scope
        self.params = params
        self.ids = ids
        self.autouse = autouse
        self.name = name
        # `True` if the fixture is a generator function (yield-style
        # fixture). Detected up-front so request execution can drive
        # the teardown side.
        self.generator = inspect.isgeneratorfunction(fn)

    # Backward-compatible dict-style access — older code reads
    # `fn._pytest_fixture['scope']`.
    def __getitem__(self, key):
        return getattr(self, key)

    def get(self, key, default=None):
        return getattr(self, key, default)


def fixture(callable_=None, *, scope='function', params=None, autouse=False,
            ids=None, name=None):
    """Mark a callable as a fixture provider.

    Supports ``scope`` (``'function'`` / ``'class'`` / ``'module'`` /
    ``'session'``), ``params`` (list of values; one fixture-arg
    binding per test), ``autouse`` (request the fixture by default
    on every test that's reachable from the scope), and yield-style
    teardown (use ``yield`` inside the body instead of ``return``).
    """
    if scope not in ('function', 'class', 'module', 'session'):
        raise ValueError("invalid fixture scope: {!r}".format(scope))

    def deco(fn):
        fname = name or fn.__name__
        defn = _FixtureDef(fn, scope, params, ids, autouse, fname)
        fn._pytest_fixture = defn
        # A fixture defined as a *method* (first parameter `self`) belongs
        # to its class, not the global namespace — registering it globally
        # would (a) leak an unbound fn that can't be called without `self`
        # and (b) let one class's fixture shadow an unrelated module
        # fixture of the same name. Class collection rediscovers these via
        # the `_pytest_fixture` attribute and binds them to the instance.
        try:
            first = next(iter(inspect.signature(fn).parameters), None)
        except (TypeError, ValueError):
            first = None
        if first != 'self':
            _FIXTURE_REGISTRY[fname] = defn
        return fn
    if callable_ is not None and callable(callable_):
        return deco(callable_)
    return deco


def _register_fixture_aliases(mod):
    """Register a module's fixtures under *every* attribute name bound to them.

    pandas binds extra module-level names to an existing fixture to get a
    second, independently-parametrised copy for cartesian-product tests::

        nulls_fixture2 = nulls_fixture        # pandas/conftest.py
        tz_aware_fixture2 = tz_aware_fixture

    The `fixture` decorator only keys ``fn.__name__`` in `_FIXTURE_REGISTRY`, so
    requesting ``nulls_fixture2`` failed with a bogus "missing positional
    argument". Real pytest registers a fixture under each attribute name that
    references it; mirror that by scanning the module post-import and adding any
    alias whose name isn't already claimed by another fixture. Imported fixtures
    (``from …conftest import somefix``) are picked up the same way, matching
    pytest's "import to make available" behaviour.
    """
    try:
        names = dir(mod)
    except Exception:
        return
    for attr in names:
        if attr.startswith('__'):
            continue
        try:
            obj = getattr(mod, attr)
        except Exception:
            continue
        defn = getattr(obj, '_pytest_fixture', None)
        if defn is None:
            continue
        # A `self`-method fixture belongs to its class (bound during class
        # collection); never expose it as a module-global.
        try:
            first = next(iter(inspect.signature(obj).parameters), None)
        except (TypeError, ValueError):
            first = None
        if first == 'self':
            continue
        if attr not in _FIXTURE_REGISTRY:
            _FIXTURE_REGISTRY[attr] = defn


# Per-scope caches, refreshed by `_FixtureManager.enter_scope`.
class _FixtureManager:
    """Tracks fixture instances and teardowns across scopes."""

    def __init__(self):
        self._caches = {
            'session': {},
            'module': {},
            'class': {},
            'function': {},
        }
        # Finalizer stacks per scope. LIFO — last-in-first-out.
        self._finalizers = {
            'session': [],
            'module': [],
            'class': [],
            'function': [],
        }

    def reset_scope(self, scope):
        # Run finalizers in reverse order, then clear the cache.
        for fin in reversed(self._finalizers[scope]):
            try:
                fin()
            except Exception:
                traceback.print_exc()
        self._finalizers[scope].clear()
        self._caches[scope].clear()

    def _cache_key(self, name, param):
        # ``param`` is a fixture/parametrize value and may be *unhashable*
        # (a pandas ``Index``/``Series``, a numpy ``ndarray``, a ``list``) —
        # real pytest keys its fixture cache by the param *index*, never the
        # value, so it never hits this. Fall back to object identity for
        # unhashable params: the same param object is reused across requests
        # within a scope and the cache is cleared per scope, so ``id`` is
        # stable and safe. Without this, every parametrization over an Index
        # (huge in ``arithmetic/``) blew up with ``unhashable type`` on *both*
        # interpreters, masking the real WeavePy deltas.
        try:
            hash(param)
        except TypeError:
            return (name, id(param))
        return (name, param)

    def get_cached(self, name, scope, param):
        return self._caches[scope].get(self._cache_key(name, param), _NOTSET)

    def set_cached(self, name, scope, param, value):
        self._caches[scope][self._cache_key(name, param)] = value

    def add_finalizer(self, scope, fn):
        self._finalizers[scope].append(fn)


def _builtin_fixture_tmp_path(request):  # noqa: ARG001
    import tempfile
    import pathlib
    return pathlib.Path(tempfile.mkdtemp(prefix='pytest-'))


class _LocalPath:
    """Minimal stand-in for ``py.path.local`` (real pytest's ``tmpdir``).

    A plain ``str`` is *not* a valid substitute: ``tmpdir.join("f.html")``
    resolves to ``str.join`` and interleaves the directory between the
    characters of the filename (pandas' ``test_to_html_filename`` then
    fails with a garbled "non-existent directory" path).
    """

    def __init__(self, path):
        self._path = str(path)

    def __str__(self):
        return self._path

    def __fspath__(self):
        return self._path

    def __repr__(self):
        return 'local({!r})'.format(self._path)

    @property
    def strpath(self):
        return self._path

    def join(self, *parts):
        import os as _os
        return _LocalPath(_os.path.join(self._path, *[str(p) for p in parts]))

    def mkdir(self, *parts):
        import os as _os
        p = _os.path.join(self._path, *[str(x) for x in parts])
        _os.makedirs(p, exist_ok=True)
        return _LocalPath(p)

    def ensure(self, *parts, **kwargs):
        import os as _os
        p = _os.path.join(self._path, *[str(x) for x in parts])
        if kwargs.get('dir'):
            _os.makedirs(p, exist_ok=True)
        else:
            _os.makedirs(_os.path.dirname(p), exist_ok=True)
            if not _os.path.exists(p):
                with open(p, 'w'):
                    pass
        return _LocalPath(p)

    def exists(self):
        import os as _os
        return _os.path.exists(self._path)

    def read(self):
        with open(self._path) as f:
            return f.read()

    def read_text(self, encoding='utf-8'):
        with open(self._path, encoding=encoding) as f:
            return f.read()

    def write(self, data):
        mode = 'wb' if isinstance(data, bytes) else 'w'
        with open(self._path, mode) as f:
            f.write(data)

    def remove(self):
        import os as _os
        import shutil as _shutil
        if _os.path.isdir(self._path):
            _shutil.rmtree(self._path)
        elif _os.path.exists(self._path):
            _os.unlink(self._path)

    @property
    def basename(self):
        import os as _os
        return _os.path.basename(self._path)

    @property
    def dirname(self):
        import os as _os
        return _os.path.dirname(self._path)


def _builtin_fixture_tmpdir(request):  # noqa: ARG001
    import tempfile
    return _LocalPath(tempfile.mkdtemp(prefix='pytest-'))


def _builtin_fixture_capsys(request):  # noqa: ARG001
    import io as _io
    return _CapsysHandle(_io.StringIO(), _io.StringIO())


def _builtin_fixture_monkeypatch(request):  # noqa: ARG001
    return _MonkeyPatchHandle()


def _builtin_fixture_doctest_namespace(request):  # noqa: ARG001
    # Real pytest injects this (session-scoped) from its doctest plugin;
    # pandas' autouse ``add_doctest_imports`` populates it with ``np``/``pd``.
    # We don't collect doctests, so a plain dict the fixture can scribble on
    # is sufficient — without it the autouse fixture fails with a missing
    # ``doctest_namespace`` argument on *every* test.
    return {}


class _MonkeyPatchHandle:
    """Minimal monkeypatch fixture for swapping attrs / env vars."""

    def __init__(self):
        self._undo = []

    _NOTSET = object()

    def setattr(self, target, name=_NOTSET, value=_NOTSET, raising=True):
        if isinstance(target, str):
            # Real pytest: ``setattr("mod.attr", value)`` — with a dotted
            # string target the second positional argument is the *value*.
            if value is _MonkeyPatchHandle._NOTSET:
                if name is _MonkeyPatchHandle._NOTSET:
                    raise TypeError(
                        'monkeypatch.setattr with dotted-string target needs a value'
                    )
                value = name
            mod_name, _, attr = target.rpartition('.')
            mod = importlib.import_module(mod_name)
            target = mod
            name_for_attr = attr
            value_for_attr = value
        else:
            if name is _MonkeyPatchHandle._NOTSET or value is _MonkeyPatchHandle._NOTSET:
                raise TypeError(
                    'monkeypatch.setattr with an object target needs name and value'
                )
            name_for_attr = name
            value_for_attr = value
        if raising and not hasattr(target, name_for_attr):
            raise AttributeError(
                'object {!r} has no attribute {!r}'.format(target, name_for_attr)
            )
        old = getattr(target, name_for_attr, None)
        had = hasattr(target, name_for_attr)
        setattr(target, name_for_attr, value_for_attr)
        self._undo.append(('attr', target, name_for_attr, old, had))

    def setenv(self, name, value):
        old = os.environ.get(name)
        os.environ[name] = str(value)
        self._undo.append(('env', name, old))

    def delenv(self, name, raising=True):
        old = os.environ.pop(name, None)
        if old is None and raising:
            raise KeyError(name)
        self._undo.append(('env', name, old))

    def syspath_prepend(self, path):
        sys.path.insert(0, path)
        self._undo.append(('syspath', path))

    def chdir(self, path):
        old = os.getcwd()
        os.chdir(path)
        self._undo.append(('cwd', old))

    def delattr(self, target, name=None, raising=True):
        if isinstance(target, str):
            mod_name, _, attr = target.rpartition('.')
            target = importlib.import_module(mod_name)
            name = attr
        if not hasattr(target, name):
            if raising:
                raise AttributeError(name)
            return
        old = getattr(target, name, None)
        delattr(target, name)
        self._undo.append(('attr', target, name, old, True))

    def setitem(self, dic, name, value):
        had = name in dic
        old = dic.get(name)
        dic[name] = value
        self._undo.append(('item', dic, name, old, had))

    def delitem(self, dic, name, raising=True):
        if name not in dic:
            if raising:
                raise KeyError(name)
            return
        old = dic[name]
        del dic[name]
        self._undo.append(('item', dic, name, old, True))

    def context(self):
        # ``with monkeypatch.context() as m:`` — a nested handle whose changes
        # are undone when the block exits (real pytest yields a fresh
        # MonkeyPatch and calls ``.undo()`` in a ``finally``).
        return _MonkeyPatchContext()

    def undo(self):
        for entry in reversed(self._undo):
            kind = entry[0]
            if kind == 'attr':
                _, target, name, old, had = entry
                if had:
                    setattr(target, name, old)
                else:
                    try:
                        delattr(target, name)
                    except Exception:
                        pass
            elif kind == 'item':
                _, dic, name, old, had = entry
                if had:
                    dic[name] = old
                else:
                    dic.pop(name, None)
            elif kind == 'env':
                _, name, old = entry
                if old is None:
                    os.environ.pop(name, None)
                else:
                    os.environ[name] = old
            elif kind == 'syspath':
                _, path = entry
                try:
                    sys.path.remove(path)
                except ValueError:
                    pass
            elif kind == 'cwd':
                _, old = entry
                os.chdir(old)
        self._undo.clear()


class _MonkeyPatchContext:
    """Context-manager returned by ``monkeypatch.context()`` — a scoped handle
    whose patches are rolled back on block exit."""

    def __enter__(self):
        self._m = _MonkeyPatchHandle()
        return self._m

    def __exit__(self, *exc):
        self._m.undo()
        return False


class _CapsysHandle:
    def __init__(self, out, err):
        self._out = out
        self._err = err
        self._orig_stdout = sys.stdout
        self._orig_stderr = sys.stderr
        sys.stdout = self._out
        sys.stderr = self._err

    def readouterr(self):
        out = self._out.getvalue()
        err = self._err.getvalue()
        self._out.seek(0)
        self._out.truncate()
        self._err.seek(0)
        self._err.truncate()
        return _CapturedIO(out, err)

    def disabled(self):
        # Real pytest: a context manager that suspends capturing for the
        # duration of the block (`with capsys.disabled(): ...`), restoring
        # it afterwards.
        return _CapsysDisabled(self)

    def _restore(self):
        sys.stdout = self._orig_stdout
        sys.stderr = self._orig_stderr

    def __del__(self):
        try:
            self._restore()
        except Exception:  # pragma: no cover
            pass


class _CapsysDisabled:
    """``capsys.disabled()`` — capture is suspended inside the block."""

    def __init__(self, handle):
        self._handle = handle

    def __enter__(self):
        sys.stdout = self._handle._orig_stdout
        sys.stderr = self._handle._orig_stderr
        return None

    def __exit__(self, *exc):
        sys.stdout = self._handle._out
        sys.stderr = self._handle._err
        return False


class _CapturedIO:
    __slots__ = ('out', 'err')

    def __init__(self, out, err):
        self.out = out
        self.err = err


def _builtin_fixture_pytestconfig(request):
    """The active ``Config`` — pytest's built-in ``pytestconfig`` fixture.

    pandas' ``strict_data_files(pytestconfig)`` (and hence the widely-used
    ``datapath`` fixture) depends on it; without it every ``datapath``-based
    test errors with a missing-argument ``TypeError``.
    """
    return _ACTIVE_CONFIG


class _WarningsRecorder:
    """pytest's ``recwarn`` fixture value — records every warning raised
    during the test (``warnings.catch_warnings(record=True)`` with an
    ``"always"`` filter), exposing pytest's ``WarningsRecorder`` API."""

    def __init__(self):
        import warnings
        self._catcher = warnings.catch_warnings(record=True)
        self.list = self._catcher.__enter__()
        warnings.simplefilter('always')

    def __len__(self):
        return len(self.list)

    def __getitem__(self, i):
        return self.list[i]

    def __iter__(self):
        return iter(self.list)

    def pop(self, cls=Warning):
        # Exact category match wins; fall back to the first subclass match
        # (pytest's semantics).
        best = None
        for i, w in enumerate(self.list):
            if w.category is cls:
                return self.list.pop(i)
            if best is None and issubclass(w.category, cls):
                best = i
        if best is None:
            raise AssertionError('{!r} not found in warning list'.format(cls))
        return self.list.pop(best)

    def clear(self):
        self.list[:] = []

    def _finish(self):
        try:
            self._catcher.__exit__(None, None, None)
        except Exception:  # pragma: no cover - already exited
            pass


def _builtin_fixture_recwarn(request):  # noqa: ARG001
    return _WarningsRecorder()


def _builtin_fixture_worker_id(request):  # noqa: ARG001
    """pytest-xdist's ``worker_id`` — always ``"master"`` here since the
    shim never distributes tests (matches xdist's non-distributed value)."""
    return 'master'


_BUILTIN_FIXTURES = {
    'tmp_path': _builtin_fixture_tmp_path,
    'tmpdir': _builtin_fixture_tmpdir,
    'capsys': _builtin_fixture_capsys,
    'monkeypatch': _builtin_fixture_monkeypatch,
    'doctest_namespace': _builtin_fixture_doctest_namespace,
    'pytestconfig': _builtin_fixture_pytestconfig,
    'recwarn': _builtin_fixture_recwarn,
    'worker_id': _builtin_fixture_worker_id,
}


class _Request:
    """Drop-in for ``pytest.FixtureRequest``.

    Exposes ``node`` / ``item`` (the test being run), ``param`` (the
    indirect-fixture parameter), ``fixturename``, and
    ``addfinalizer``. Finalisers are queued at the fixture's scope.
    """
    __slots__ = ('node', 'item', 'param', 'fixturename', '_manager', '_scope')

    def __init__(self, node, item, manager, scope, fixturename=None, param=None):
        # Real pytest gives a function-scoped request the *item* as its
        # node (class/module/session scopes get the enclosing collector).
        # pandas' dim2 autouse fixture reads ``request.node._obj`` and
        # expects the test function, not the Class collector.
        if scope == 'function' and item is not None:
            self.node = item
        else:
            self.node = node
        self.item = item
        self.param = param
        self.fixturename = fixturename
        self._manager = manager
        self._scope = scope

    def addfinalizer(self, fn):
        self._manager.add_finalizer(self._scope, fn)

    def getfixturevalue(self, name):
        val = _resolve_fixture(name, self._manager, self.item, self.node)
        if val is _NOTSET:
            raise LookupError('no fixture named {!r}'.format(name))
        return val

    # `request.applymarker(pytest.mark.xfail(...))` (and the `request.node.
    # add_marker` spelling) attach a marker to the running test at call time.
    # pandas uses this pervasively for data-dependent xfail/skip. The marker
    # lands on the item's `marks`, which `_run_one_item` re-scans *after* the
    # test body runs, so a runtime-applied xfail/skip is honoured.
    def applymarker(self, marker):
        if self.item is not None:
            self.item.marks.append(marker)

    add_marker = applymarker

    @property
    def config(self):
        return _ACTIVE_CONFIG

    @property
    def function(self):
        return getattr(self.item, 'callable', None)

    @property
    def fixturenames(self):
        """The fixture-name closure of the item (pytest fills this with
        every fixture that will be set up, parametrize argnames included).
        pandas' io/parser conftest probes ``"all_parsers" in
        request.fixturenames`` from its autouse pyarrow-xfail fixtures."""
        item = self.item
        names = []
        seen = set()
        fmap = getattr(item, '_fixture_map', None) or {}

        def add(n):
            if n in seen:
                return
            seen.add(n)
            names.append(n)
            defn = fmap.get(n) or _FIXTURE_REGISTRY.get(n)
            if defn is None:
                return
            try:
                params = inspect.signature(defn.fn).parameters
            except (TypeError, ValueError):
                return
            for p in params:
                if p != 'request':
                    add(p)

        if item is not None:
            for fname, defn in list(fmap.items()) + list(_FIXTURE_REGISTRY.items()):
                if getattr(defn, 'autouse', False):
                    add(fname)
            for m in getattr(item, 'marks', []):
                if getattr(m, 'name', None) == 'usefixtures':
                    for fname in m.args:
                        add(fname)
            try:
                sig_params = inspect.signature(item.callable).parameters
            except (TypeError, ValueError):
                sig_params = {}
            for p in sig_params:
                if p != 'request':
                    add(p)
            for p in getattr(item, '_fixture_params', {}):
                add(p)
            for p in getattr(item, '_direct_params', {}):
                if p not in seen:
                    seen.add(p)
                    names.append(p)
        names.append('request')
        return names

    @property
    def cls(self):
        """The test class of the underlying node, or None (pytest's
        ``FixtureRequest.cls``)."""
        for node in (self.node, self.item):
            while node is not None:
                found = getattr(node, 'cls', None)
                if found is not None:
                    return found
                node = getattr(node, 'parent', None)
        return None

    @property
    def instance(self):
        for node in (self.node, self.item):
            while node is not None:
                inst = getattr(node, '_instance', None)
                if inst is not None:
                    return inst
                node = getattr(node, 'parent', None)
        return None

    @property
    def module(self):
        for node in (self.node, self.item):
            while node is not None:
                mod = getattr(node, 'module', None)
                if mod is not None:
                    return mod
                node = getattr(node, 'parent', None)
        return None

    @property
    def keywords(self):
        # A name→marker mapping is a good-enough stand-in for pytest's
        # keyword set; tests probe it with `"<mark>" in request.keywords`.
        kw = {getattr(m, 'name', None): m for m in getattr(self.item, 'marks', [])}
        name = getattr(self.item, 'name', None)
        if name is not None:
            kw[name] = True
        return kw


def _resolve_fixture(name, manager=None, item=None, node=None, param=None,
                     parent_scope='function'):
    """Resolve a fixture by name.

    Honours scope caching, generator-style teardown, and
    parametrised fixtures (the active ``param`` is read from the
    item's `_params` dict if present).
    """
    if manager is None:
        manager = _FIXTURE_MANAGER
    # Class-scoped method fixtures (bound to the test instance) shadow
    # module/global fixtures of the same name; fall back to the global
    # registry for module-level fixtures.
    defn = None
    if item is not None:
        defn = getattr(item, '_fixture_map', {}).get(name)
    if defn is None:
        defn = _FIXTURE_REGISTRY.get(name)
    if defn is not None:
        # Parametrised fixture: pick the active parameter for this
        # item if `parametrize` filled it in.
        active_param = param
        if active_param is None and item is not None:
            active_param = getattr(item, '_fixture_params', {}).get(name)
        cache_key = active_param
        cached = manager.get_cached(name, defn.scope, cache_key)
        if cached is not _NOTSET:
            return cached
        req = _Request(node=node, item=item, manager=manager,
                       scope=defn.scope, fixturename=name, param=active_param)
        # Build the argument bindings — recurse for any fixture deps.
        sig = inspect.signature(defn.fn)
        kwargs = {}
        # `@pytest.mark.parametrize` argnames (supplied *directly* to the test)
        # are also visible to every fixture in the dependency closure — real
        # pytest injects parametrized argnames into the whole graph, not just the
        # test function. pandas' setitem/coercion matrix depends on this: the
        # class is parametrized on e.g. ``val,exp_dtype,warn`` while a base-class
        # fixture ``is_inplace(self, obj, expected)`` or a subclass
        # ``expected(self, obj, key, val, exp_dtype)`` requests those same names.
        # Without this the fixture is called with the arg missing → "missing N
        # required positional arguments" (>800 failures in test_setitem.py alone).
        direct = getattr(item, '_direct_params', {}) if item is not None else {}
        for pname in sig.parameters:
            if pname == 'request':
                kwargs[pname] = req
            elif pname in direct:
                kwargs[pname] = direct[pname]
            else:
                sub = _resolve_fixture(pname, manager, item, node)
                if sub is not _NOTSET:
                    kwargs[pname] = sub
        if defn.generator:
            it = defn.fn(**kwargs)
            value = next(it)
            def _teardown(it=it):
                try:
                    next(it)
                except StopIteration:
                    pass
            manager.add_finalizer(defn.scope, _teardown)
        else:
            value = defn.fn(**kwargs)
        manager.set_cached(name, defn.scope, cache_key, value)
        return value
    builtin = _BUILTIN_FIXTURES.get(name)
    if builtin is not None:
        req = _Request(node=node, item=item, manager=manager,
                       scope='function', fixturename=name)
        # monkeypatch / recwarn need an automatic teardown.
        val = builtin(req)
        if name == 'monkeypatch':
            manager.add_finalizer('function', val.undo)
        elif name == 'recwarn':
            manager.add_finalizer('function', val._finish)
        elif name == 'capsys':
            # Restore the real stdout/stderr even when the test raises —
            # relying on `__del__` leaves the whole run writing into the
            # capture buffer (the handle stays alive via the traceback).
            manager.add_finalizer('function', val._restore)
        return val
    return _NOTSET


_FIXTURE_MANAGER = _FixtureManager()


# ============================================================ raises / warns


class _RaisesContext:
    def __init__(self, expected, match=None):
        self.expected = expected
        self.match = match
        self.value = None
        self.type = None

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_val, tb):
        if exc_type is None:
            raise _Failed('DID NOT RAISE {}'.format(self.expected))
        if not issubclass(exc_type, self.expected):
            return False
        if self.match and not re.search(self.match, str(exc_val)):
            raise _Failed('Pattern {!r} did not match {!r}'.format(
                self.match, str(exc_val)))
        self.type = exc_type
        self.value = exc_val
        return True


def raises(expected, *args, match=None, **kwargs):
    """Like pytest.raises."""
    if args:
        ctx = _RaisesContext(expected, match=match)
        with ctx:
            args[0](*args[1:], **kwargs)
        return ctx
    return _RaisesContext(expected, match=match)


class _WarnsContext:
    def __init__(self, expected=Warning, match=None):
        # ``pytest.warns()`` with no arguments means "at least one
        # warning of any category" (pytest's WarningsChecker default).
        self.expected = Warning if expected is None else expected
        self.match = match

    def __enter__(self):
        import warnings as _warnings
        self._catcher = _warnings.catch_warnings(record=True)
        self.warnings = self._catcher.__enter__()
        _warnings.simplefilter('always')
        return self

    def __exit__(self, exc_type, exc_val, tb):
        import re as _re
        self._catcher.__exit__(exc_type, exc_val, tb)
        if exc_type is not None:
            return False
        matching = [w for w in self.warnings
                    if issubclass(w.category, self.expected)]
        if not matching:
            raise _Failed('Expected warning {} not raised'.format(self.expected))
        if self.match is not None:
            if not any(_re.search(self.match, str(w.message)) for w in matching):
                raise _Failed(
                    'No warning of {} matched pattern {!r}; messages were {}'.format(
                        self.expected, self.match,
                        [str(w.message) for w in matching]))
        return False

    # ``with pytest.warns(...) as record:`` exposes the recorded
    # warnings (pytest's WarningsChecker is list-like).
    def __len__(self):
        return len(self.warnings)

    def __getitem__(self, i):
        return self.warnings[i]

    def __iter__(self):
        return iter(self.warnings)

    @property
    def list(self):
        return list(self.warnings)

    def pop(self, cls=Warning):
        for i, w in enumerate(self.warnings):
            if issubclass(w.category, cls):
                return self.warnings.pop(i)
        raise AssertionError('{} not found in warning list'.format(cls))

    def clear(self):
        del self.warnings[:]


def warns(expected=Warning, *args, match=None, **kwargs):
    if args:
        ctx = _WarnsContext(expected, match=match)
        with ctx:
            args[0](*args[1:], **kwargs)
        return ctx
    return _WarnsContext(expected, match=match)


# ============================================================ approx


class _Approx:
    def __init__(self, expected, rel=None, abs_=None):
        self.expected = expected
        self.rel = rel if rel is not None else 1e-6
        self.abs = abs_ if abs_ is not None else 1e-12

    def __eq__(self, actual):
        if isinstance(self.expected, (list, tuple)):
            if not isinstance(actual, (list, tuple)) or len(actual) != len(self.expected):
                return False
            return all(_isclose(a, b, self.rel, self.abs)
                       for a, b in zip(actual, self.expected))
        return _isclose(actual, self.expected, self.rel, self.abs)

    def __ne__(self, actual):
        eq = self.__eq__(actual)
        if eq is NotImplemented:
            return NotImplemented
        return not eq

    def __repr__(self):
        return 'approx({!r}, rel={}, abs={})'.format(self.expected, self.rel, self.abs)


def _isclose(a, b, rel, abs_):
    try:
        return abs(float(a) - float(b)) <= abs_ + rel * abs(float(b))
    except Exception:
        return False


def approx(expected, rel=None, abs=None):  # noqa: A002
    return _Approx(expected, rel=rel, abs_=abs)


# ============================================================ node hierarchy


class Collector:
    def __init__(self, name, parent=None):
        self.name = name
        self.parent = parent
        self.path = None

    def collect(self):
        raise NotImplementedError

    @property
    def session(self):
        """The enclosing ``Session`` node (pytest exposes this on every
        node; pandas' ``check_comprehensiveness`` fixture reads
        ``request.node.session.items``)."""
        node = self
        while node is not None:
            if isinstance(node, Session):
                return node
            node = getattr(node, 'parent', None)
        return _ACTIVE_SESSION


class _CallSpec:
    """Mirror of pytest's ``CallSpec2`` — the parametrization record
    exposed as ``item.callspec`` (``.id`` is the bracketed id string,
    ``.params`` maps param/fixture names to their active values)."""

    def __init__(self, id, params):
        self.id = id
        self.params = params

    def getparam(self, name):
        try:
            return self.params[name]
        except KeyError:
            raise ValueError(name)


class Item(Collector):
    """A single test item (callable)."""

    def __init__(self, name, parent, callable_, marks=None, params=None,
                 param_id=None, fixture_params=None, fixture_map=None):
        super().__init__(name, parent)
        self.callable = callable_
        self.marks = marks or []
        # `@pytest.mark.parametrize` injects argument values passed
        # *directly* to the test (these win over fixtures of the same
        # name).
        self._direct_params = params or {}
        # Parametrised *fixtures* in the test's dependency closure bind
        # one `request.param` value per fixture here; `_resolve_fixture`
        # reads them so a `@pytest.fixture(params=[...])` multiplies the
        # test and threads the active parameter through dependents.
        self._fixture_params = fixture_params or {}
        # Fixtures visible to this test: class-scoped method fixtures
        # (bound to the test instance) layered over the module/global
        # registry. Empty for the plain module-function case.
        self._fixture_map = fixture_map or {}
        self._param_id = param_id

    @property
    def name(self):
        # pytest's ``item.name`` includes the parametrize id suffix
        # (``test_foo[int64-series]``); pandas' comprehensiveness fixture
        # in test_coercion.py substring-matches dtypes/klasses against it.
        pid = getattr(self, '_param_id', None)
        if pid:
            return '{}[{}]'.format(self._name, pid)
        return self._name

    @name.setter
    def name(self, value):
        self._name = value

    @property
    def nodeid(self):
        base = self.name
        if self.parent and hasattr(self.parent, 'nodeid'):
            return '{}::{}'.format(self.parent.nodeid, base)
        return base

    @property
    def _obj(self):
        """pytest's ``Function._obj`` — the underlying test callable.
        pandas' extension-array dim2 autouse fixture inspects
        ``request.node._obj.__qualname__`` to decide skips."""
        return self.callable

    @property
    def function(self):
        """pytest's ``Function.function`` — same callable as ``_obj``
        (pandas' ``skip_if_immutable`` reads ``node.function.__qualname__``)."""
        return self.callable

    @property
    def callspec(self):
        """pytest's ``CallSpec2`` for parametrized items.

        Only parametrized tests carry one — accessing it on a plain
        test raises AttributeError, exactly like pytest (pandas guards
        with ``request.node.callspec.id == ...`` only in parametrized
        tests).
        """
        if not (self._param_id or self._direct_params or self._fixture_params):
            raise AttributeError('callspec')
        params = dict(self._fixture_params)
        params.update(self._direct_params)
        return _CallSpec(self._param_id or '', params)

    def add_marker(self, marker, append=True):
        """`request.node.add_marker(...)` — attach a marker at call time."""
        if append:
            self.marks.append(marker)
        else:
            self.marks.insert(0, marker)

    def get_closest_marker(self, name, default=None):
        for m in reversed(self.marks):
            if getattr(m, 'name', None) == name:
                return m
        return default

    def runtest(self):
        # pytest instantiates the test class *freshly for every test*, so
        # state one test sets on ``self`` never leaks into the next
        # (pandas' extension ops tests set ``self.divmod_exc`` per test
        # and rely on the class default being restored). Collection bound
        # every method to one shared instance; rebind this test — and any
        # class fixtures — to a fresh instance now.
        shared = getattr(self.callable, '__self__', None)
        if shared is not None and isinstance(self.parent, Class):
            fresh = type(shared)()
            self.callable = self.callable.__func__.__get__(fresh, type(fresh))
            self._instance = fresh
            rebound = {}
            for fname, defn in self._fixture_map.items():
                fn = defn.fn
                if getattr(fn, '__self__', None) is shared:
                    fn = fn.__func__.__get__(fresh, type(fresh))
                    defn = _FixtureDef(fn, defn.scope, defn.params,
                                       defn.ids, defn.autouse, defn.name)
                rebound[fname] = defn
            # A fresh dict: the original map object is shared by every
            # item collected from the class.
            self._fixture_map = rebound
        sig = inspect.signature(self.callable)
        kwargs = {}
        # xunit-style per-test hooks: pytest calls
        # ``instance.setup_method(method)`` before each test method and
        # ``teardown_method(method)`` after (even on failure). pandas'
        # ``TestnanopsDataFrame`` builds all its arrays there.
        inst = getattr(self.callable, '__self__', None)
        func = getattr(self.callable, '__func__', self.callable)
        setup_m = getattr(inst, 'setup_method', None) if inst is not None else None
        teardown_m = getattr(inst, 'teardown_method', None) if inst is not None else None

        def _call_hook(hook):
            # pytest's ``_call_with_optional_argument``: pass the test
            # function only if the hook's signature accepts an argument.
            try:
                nparams = len(inspect.signature(hook).parameters)
            except (TypeError, ValueError):
                nparams = 0
            if nparams:
                hook(func)
            else:
                hook()

        if setup_m is not None:
            _call_hook(setup_m)
        # Eagerly resolve any autouse fixtures so their teardowns
        # get queued (matches pytest's ordering: autouse fires for
        # every test in scope even if not requested by name). Both the
        # global registry and this test's class-scoped fixtures count.
        for fname, defn in list(self._fixture_map.items()) + list(_FIXTURE_REGISTRY.items()):
            if defn.autouse:
                _resolve_fixture(fname, _FIXTURE_MANAGER, self, self.parent)
        # ``@pytest.mark.usefixtures("name", ...)`` requests fixtures without
        # naming them as parameters — they still run (pandas uses this to
        # apply data-dependent xfail marks via ``request.applymarker``).
        for m in self.marks:
            if getattr(m, 'name', None) == 'usefixtures':
                for fname in m.args:
                    _resolve_fixture(fname, _FIXTURE_MANAGER, self, self.parent)
        for pname in sig.parameters:
            # Parametrize injects directly-passed values that aren't
            # fixtures — those win over the resolver.
            if pname in self._direct_params:
                kwargs[pname] = self._direct_params[pname]
                continue
            # The built-in `request` fixture: pandas takes it directly as a
            # test argument (for `request.applymarker`, `request.node`,
            # `request.getfixturevalue`, …), so it must be supplied here just
            # as `_resolve_fixture` supplies it to dependent fixtures.
            if pname == 'request':
                kwargs[pname] = _Request(node=self, item=self,
                                         manager=_FIXTURE_MANAGER,
                                         scope='function')
                continue
            val = _resolve_fixture(pname, _FIXTURE_MANAGER, self, self.parent)
            if val is not _NOTSET:
                kwargs[pname] = val
        try:
            return self.callable(**kwargs)
        finally:
            if teardown_m is not None:
                _call_hook(teardown_m)
            _FIXTURE_MANAGER.reset_scope('function')


# Alias matching CPython's pytest naming convention.
Function = Item


def _get_marks(obj):
    """The ``pytestmark`` marks attached to *obj* (function, class, or
    module), normalised to a list. Decorator marks and an explicit
    ``pytestmark = pytest.mark.…`` / ``pytestmark = [...]`` assignment both
    live under this one attribute, exactly like real pytest."""
    pm = getattr(obj, 'pytestmark', None)
    if not pm:
        return []
    return list(pm) if isinstance(pm, (list, tuple)) else [pm]


def _collect_owner_marks(obj):
    """Marks that live on a *container* (class or module): decorator marks
    (a class is callable, so ``@pytest.mark.parametrize`` on it lands on
    ``pytestmark``) plus an explicit ``pytestmark`` assignment."""
    return _get_marks(obj)


class Class(Collector):
    def __init__(self, name, parent, cls):
        super().__init__(name, parent)
        self.cls = cls

    @property
    def nodeid(self):
        return '{}::{}'.format(self.parent.nodeid, self.name)

    @property
    def _obj(self):
        return self.cls

    def collect(self):
        items = []
        instance = self.cls()
        self._instance = instance
        class_fixtures = _bound_class_fixtures(self.cls, instance)
        # Marks applied to the *class* — `@pytest.mark.parametrize(...)` /
        # `@pytest.mark.skip(...)` decorating the class (stored on the class by
        # `_MarkerDecorator.__call__`, since a class is callable) and a
        # `pytestmark = [...]` list attribute — apply to *every* test method,
        # exactly like real pytest. Dropping them left class-parametrized
        # methods (e.g. `TestHashTable`, parametrized on ``table_type, dtype``)
        # missing their positional arguments for the whole class.
        class_marks = _collect_owner_marks(self.cls) + getattr(
            self.parent, '_module_marks', [])
        for attr in dir(self.cls):
            if not attr.startswith('test_'):
                continue
            method = getattr(instance, attr)
            if not callable(method):
                continue
            # method marks are "closest" and come first; class marks apply on
            # top (their parametrize crosses the method's as a product).
            marks = _get_marks(method) + class_marks
            items.extend(_expand_parametrize(attr, self, method, marks,
                                             fixture_map=class_fixtures))
        return items


class Module(Collector):
    def __init__(self, path, parent=None):
        super().__init__(os.path.basename(path), parent)
        self.path = path
        self.module = None

    @property
    def nodeid(self):
        return self.path

    def collect(self):
        spec = importlib.util.spec_from_file_location(self._mod_name(), self.path)
        if spec is None or spec.loader is None:
            raise CollectionError('cannot load module: {}'.format(self.path))
        mod = importlib.util.module_from_spec(spec)
        sys.modules[self._mod_name()] = mod
        _tr = os.environ.get('WEAVEPY_SHIM_TRACE')
        if _tr:
            sys.stderr.write('>>> IMPORT-START ' + self.path + '\n'); sys.stderr.flush()
        try:
            spec.loader.exec_module(mod)
        except _Skipped:
            # A module-level ``pytest.importorskip(...)`` (or ``pytest.skip``
            # with allow_module_level) skips the whole file, exactly like
            # real pytest — it must not count as a collection error.
            raise
        except Exception as exc:
            raise CollectionError('error importing {}: {}'.format(self.path, exc)) from None
        if _tr:
            sys.stderr.write('>>> IMPORT-DONE ' + self.path + '\n'); sys.stderr.flush()
        self.module = mod
        _register_fixture_aliases(mod)
        # Module-level ``pytestmark`` applies to every test in the module (both
        # module functions and class methods); stash it so `Class.collect` can
        # pick it up via its parent.
        self._module_marks = _collect_owner_marks(mod)
        out = []
        for name in dir(mod):
            obj = getattr(mod, name)
            # Real pytest only collects test_* *functions* here; a class
            # merely named ``test_*`` (pandas' module-level
            # ``test_tuple = collections.namedtuple(...)``) is not a test
            # even though it is callable. Test classes are matched by the
            # ``Test*`` prefix below.
            if name.startswith('test_') and callable(obj) and not inspect.isclass(obj):
                if _tr:
                    sys.stderr.write('>>> COLLECT-FN ' + name + '\n'); sys.stderr.flush()
                marks = _get_marks(obj) + self._module_marks
                out.extend(_expand_parametrize(name, self, obj, marks))
            elif name.startswith('Test') and inspect.isclass(obj):
                out.append(Class(name, self, obj))
        return out

    def _mod_name(self):
        """Import name for the test module.

        When the file sits inside a package (``__init__.py`` chain), use
        the full dotted path — ``pandas.tests.copy_view.test_array`` —
        exactly like pytest's rootdir-based naming. This is load-bearing
        for warning filters: ``warnings`` matches a filter's ``module``
        regex against the *attributed frame's* ``__name__``, so pandas'
        ``"error:::pandas"`` ini entry only escalates warnings raised
        from its own test modules when they're named as part of the
        ``pandas`` package.
        """
        base = os.path.basename(self.path)
        if base.endswith('.py'):
            base = base[:-3]
        parts = [base]
        d = os.path.dirname(os.path.abspath(self.path))
        while os.path.isfile(os.path.join(d, '__init__.py')):
            parts.append(os.path.basename(d))
            parent = os.path.dirname(d)
            if parent == d:
                break
            d = parent
        return '.'.join(reversed(parts))


def _bound_class_fixtures(cls, instance):
    """Discover fixtures defined as methods on ``cls`` (or its bases) and
    bind them to ``instance``.

    pandas leans heavily on class-scoped fixtures (e.g. ``TestNonNano``'s
    ``unit``/``val``/``td``), defined as ``@pytest.fixture`` methods that
    take ``self``. Binding to the instance supplies ``self`` and drops it
    from the fixture's visible signature, so the ordinary dependency
    resolver can inject the fixture's *own* fixture arguments.
    """
    fixtures = {}
    for name in dir(cls):
        try:
            attr = getattr(cls, name)
        except Exception:
            continue
        defn = getattr(attr, '_pytest_fixture', None)
        if defn is None:
            continue
        bound = getattr(instance, name)
        fixtures[defn.name] = _FixtureDef(
            bound, defn.scope, defn.params, defn.ids, defn.autouse, defn.name)
    return fixtures


def _fixture_deps(defn):
    """Fixture-argument names a fixture def requests (minus ``request``)."""
    try:
        return [p for p in inspect.signature(defn.fn).parameters if p != 'request']
    except (TypeError, ValueError):
        return []


def _closure_param_fixtures(requested, fixture_map):
    """Ordered list of *parametrised* fixture defs reachable from the
    ``requested`` fixture names (following dependencies). A dependency is
    emitted before the fixture that requests it, matching pytest's id
    ordering. Both the class-scoped ``fixture_map`` and the module/global
    registry are consulted."""
    order = []
    seen = set()

    def lookup(fname):
        return fixture_map.get(fname) or _FIXTURE_REGISTRY.get(fname)

    def visit(fname):
        if fname in seen:
            return
        seen.add(fname)
        defn = lookup(fname)
        if defn is None:
            return
        for dep in _fixture_deps(defn):
            visit(dep)
        if defn.params is not None:
            # Key by the *requested* name, not ``defn.name``: pandas aliases a
            # parametrised fixture under a second module name for
            # cartesian-product tests (``string_dtype_arguments2 =
            # string_dtype_arguments``). Both names share one ``defn`` object,
            # so using ``defn.name`` collapsed the two into one binding and left
            # the alias' ``request.param`` unset (→ ``None``).
            order.append((fname, defn))

    for r in requested:
        visit(r)
    return order


def _fixture_param_matrix(fn, fixture_map, skip_names):
    """Cartesian product of the ``params`` of every parametrised fixture in
    ``fn``'s fixture dependency closure. Returns a list of
    ``(fixture_params, id_frags)`` — one entry per test instance the
    parametrised fixtures multiply the test into."""
    try:
        requested = [p for p in inspect.signature(fn).parameters
                     if p != 'request' and p not in skip_names]
    except (TypeError, ValueError):
        requested = []
    # Autouse fixtures join every test's closure without being named in the
    # signature — a *parametrised* autouse fixture therefore multiplies every
    # test in scope (pandas' `switch_numexpr_min_elements` doubles all of
    # tests/series/test_arithmetic.py into numexpr/python variants).
    autouse = [fname
               for fname, defn in (list((fixture_map or {}).items())
                                   + list(_FIXTURE_REGISTRY.items()))
               if getattr(defn, 'autouse', False) and fname not in skip_names]
    param_fixtures = _closure_param_fixtures(autouse + requested, fixture_map)
    matrix = [({}, [], [])]
    for fname, defn in param_fixtures:
        rows = []
        # ``ids`` may be None, a sequence indexed by param position, or a
        # *callable* invoked per value (pytest falls back to the auto id when
        # the callable returns None). Treating a callable as a sequence blows
        # up with ``object of type 'function' has no len()`` and aborts the
        # whole file's collection.
        ids = defn.ids
        for i, pv in enumerate(defn.params):
            # Unwrap `pytest.param(value, id=..., marks=...)` entries in a
            # `@pytest.fixture(params=[...])` list — the direct
            # `@pytest.mark.parametrize` path does this but the fixture path
            # historically did not, so `request.param` was the `_ParamSet`
            # wrapper (pandas' `all_parsers` does `request.param()`;
            # `any_string_dtype` does `storage, na = request.param`) and any
            # per-param `marks=` (e.g. `skip_if_no("pyarrow")`) were dropped.
            pv_id = None
            pv_marks = []
            if _is_param_set(pv):
                pv_id = pv.id
                pv_marks = pv.marks
                # One fixture ``params=[...]`` entry is a single ``request.param``
                # value. ``.values`` is always a tuple now, so unwrap the lone
                # value (``pytest.param(x)`` → ``request.param is x``, which
                # pandas' ``all_parsers`` relies on via ``request.param()``);
                # keep the tuple for the rare multi-value fixture param.
                pv = pv.values[0] if len(pv.values) == 1 else pv.values
            frag = None
            if pv_id is not None:
                frag = str(pv_id)
            elif callable(ids):
                got = ids(pv)
                if got is not None:
                    frag = str(got)
            elif ids is not None and i < len(ids) and ids[i] is not None:
                frag = str(ids[i])
            if frag is None:
                frag = _id_for(pv)
            rows.append((pv, frag, pv_marks))
        new_matrix = []
        for fparams, frags, fmarks in matrix:
            for pv, frag, pv_marks in rows:
                merged = dict(fparams)
                merged[fname] = pv
                new_matrix.append(
                    (merged, frags + [frag], fmarks + pv_marks))
        matrix = new_matrix
    return matrix


def _expand_parametrize(name, parent, fn, marks, fixture_map=None):
    """Expand a test callable into concrete :class:`Item`s.

    Two independent sources multiply a test:

    * ``@pytest.mark.parametrize`` markers (stacked → Cartesian product),
      whose values are passed *directly* to the test.
    * parametrised fixtures (``@pytest.fixture(params=[...])``) anywhere in
      the test's fixture dependency closure, whose active ``request.param``
      is threaded through the resolver.

    Supports the canonical parametrize spellings::

      @pytest.mark.parametrize('a,b', [(1, 2), (3, 4)])
      @pytest.mark.parametrize('a', [1, 2, 3], ids=['one', 'two', 'three'])
      @pytest.mark.parametrize('value', [pytest.param(1, id='one'), 2])
      @pytest.mark.parametrize('index', ['string'], indirect=True)

    ``indirect`` routes the value to the *fixture* of that name as its
    ``request.param`` (overriding the fixture's own ``params=[...]``
    list) instead of passing it directly to the test — pandas'
    ``tests/indexes`` relies on this to select one flavour of the big
    parametrised ``index`` fixture per test.
    """
    fixture_map = fixture_map or {}
    param_marks = [m for m in marks if m.name == 'parametrize']
    other_marks = [m for m in marks if m.name != 'parametrize']
    # Rows: (direct-param dict, indirect-param dict, id-fragments, marks).
    matrix = [({}, {}, [], [])]
    for marker in reversed(param_marks):
        args = marker.args
        if len(args) < 2:
            raise UsageError('parametrize: need (argnames, argvalues)')
        argnames = args[0]
        argvalues = args[1]
        explicit_ids = marker.kwargs.get('ids')
        if isinstance(argnames, str):
            names = [n.strip() for n in argnames.split(',') if n.strip()]
        else:
            names = list(argnames)
        indirect = marker.kwargs.get('indirect', False)
        if indirect is True:
            indirect_names = set(names)
        elif indirect:
            indirect_names = set(indirect)
        else:
            indirect_names = set()
        new_matrix = []
        for row_idx, row in enumerate(argvalues):
            # Unwrap `pytest.param(value, id=..., marks=...)` if used.
            row_id = None
            row_marks = []
            if _is_param_set(row):
                row_id = row.id
                row_marks = row.marks
                # ``.values`` is always a tuple of the row's positional values,
                # so it aligns 1:1 with ``names`` (single-name → ``(v,)``,
                # multi-name → ``(v1, v2, ...)``).
                values = list(row.values)
            elif len(names) > 1:
                values = list(row)
            else:
                values = [row]
            if len(names) > 1 and len(values) != len(names):
                raise UsageError(
                    'parametrize: row {} has {} values for {} names'.format(
                        row_idx, len(values), len(names)))
            if row_id is None and explicit_ids is not None:
                # `ids=` is either a sequence indexed by row, or a *callable*
                # invoked per argvalue (pytest calls it once per value and
                # falls back to the auto id when it returns None).
                if callable(explicit_ids):
                    parts = []
                    for v in values:
                        got = explicit_ids(v)
                        parts.append(str(got) if got is not None else _id_for(v))
                    row_id = '-'.join(parts)
                else:
                    row_id = explicit_ids[row_idx]
            if row_id is None:
                row_id = '-'.join(_id_for(v) for v in values)
            for prior_params, prior_indirect, prior_ids, prior_marks in matrix:
                merged = dict(prior_params)
                merged_ind = dict(prior_indirect)
                for nm, val in zip(names, values):
                    if nm in indirect_names:
                        merged_ind[nm] = val
                    else:
                        merged[nm] = val
                # Stacked ``@parametrize`` decorators compose ids with the
                # *bottom* decorator's fragment first (pytest:
                # ``@parametrize("x", …)`` over ``@parametrize("y", …)``
                # yields ``test[y-x]``). We iterate the marks topmost-first
                # (``reversed``), so each newly-processed mark's fragment
                # is *prepended* to keep the bottom-first composite order —
                # pandas matches on it (`callspec.id.startswith("reindex-")`
                # in copy_view/test_methods).
                new_matrix.append(
                    (merged, merged_ind,
                     [row_id] + prior_ids, prior_marks + row_marks))
        matrix = new_matrix
    items = []
    for params, indirect_params, id_frags, row_marks in matrix:
        # Cross the parametrize row with the parametrised-fixture matrix.
        # Names bound by parametrize are excluded from the fixture closure:
        # direct values shadow the fixture entirely, and indirect values
        # override the fixture's own ``params=[...]`` (so it must not
        # multiply the test).
        fmatrix = _fixture_param_matrix(
            fn, fixture_map, set(params.keys()) | set(indirect_params.keys()))
        if os.environ.get('WEAVEPY_SHIM_TRACE') and len(fmatrix) > 8:
            _cl = _closure_param_fixtures(
                [p for p in inspect.signature(fn).parameters
                 if p != 'request' and p not in set(params.keys())],
                fixture_map or {})
            sys.stderr.write('>>> EXPAND {} fmatrix={} closure={}\n'.format(
                name, len(fmatrix),
                [(fname, len(d.params)) for fname, d in _cl]))
            sys.stderr.flush()
        for fparams, fid_frags, fmarks in fmatrix:
            # Real pytest composes the bracketed id with *fixture* param
            # fragments first, then the mark-parametrize fragments
            # (`test[numpy-single-block-column-iloc-slice]` — `numpy` is the
            # `backend` fixture's param). pandas string-matches these via
            # `request.node.callspec.id`, so the order is load-bearing.
            all_frags = fid_frags + id_frags
            pid = '-'.join(all_frags) if all_frags else None
            # Per-param marks (`pytest.param(v, marks=...)`) apply on top of
            # the function-level marks so a single parametrization can be
            # skipped or xfailed independently of its siblings — from both the
            # direct parametrize row (`row_marks`) and any parametrised-fixture
            # `pytest.param` entries in the closure (`fmarks`).
            merged_fparams = dict(fparams)
            # Indirect parametrize values become the named fixture's active
            # ``request.param`` — same channel a parametrised fixture uses.
            merged_fparams.update(indirect_params)
            items.append(Item(name, parent, fn,
                              marks=other_marks + row_marks + fmarks,
                              params=params, param_id=pid,
                              fixture_params=merged_fparams,
                              fixture_map=fixture_map))
    return items


class _ParamSet:
    """``pytest.param(value, id=..., marks=...)`` payload."""
    __slots__ = ('values', 'id', 'marks')

    def __init__(self, values, id=None, marks=()):  # noqa: A002
        # ``.values`` mirrors real pytest's ``ParameterSet.values``: a *tuple*
        # of the row's positional values, always — ``pytest.param(x).values``
        # is ``(x,)``, not ``x``. Accept a ready tuple (from ``param()``) or a
        # lone value (legacy / cross-module "twin" callers) and normalise so
        # ``.values[0]`` / ``len(.values)`` are valid for user code that reads
        # it directly (pandas' io/parser conftest does ``parser.values[0]``).
        self.values = values if isinstance(values, tuple) else (values,)
        self.id = id
        # `marks=` accepts a *single* mark or a collection of them, exactly
        # like real pytest — `pytest.param(x, marks=td.skip_if_no("scipy"))`
        # passes one `_MarkerDecorator`, which is not iterable.
        if not marks:
            self.marks = []
        elif isinstance(marks, _MarkerDecorator):
            self.marks = [marks]
        else:
            self.marks = list(marks)


def param(*values, id=None, marks=()):  # noqa: A002
    """Wrap a parametrize row with an explicit id and/or marks."""
    # Store the *whole* positional tuple, exactly like real pytest — even for a
    # single value (``pytest.param(x).values == (x,)``). Consumers unwrap as
    # needed (a fixture param / single-name parametrize takes ``values[0]``; a
    # multi-name row zips the tuple). The old ``values[0]`` shortcut for the
    # single case left ``.values`` a bare object, so user code doing
    # ``p.values[0]`` (pandas parser conftest) hit "'type' object is not
    # subscriptable" and took out the entire io/parser suite.
    return _ParamSet(tuple(values), id=id, marks=marks)


def _is_param_set(obj):
    """True if ``obj`` is a ``pytest.param(...)`` payload (a ``_ParamSet``).

    The module under collection may ``import pytest`` and get a *different*
    module object than the one running collection — weavepy's frozen
    ``pytest`` vs this re-imported/``__main__`` copy — so its ``pytest.param``
    builds a ``_ParamSet`` from a twin class. A plain ``isinstance`` against
    our ``_ParamSet`` misses that instance, leaving the value wrapped
    (``request.param`` becomes the wrapper and node ids read ``_ParamSet``).
    Match by class name as well so unwrapping survives the module boundary.
    """
    return isinstance(obj, _ParamSet) or type(obj).__name__ == '_ParamSet'


def _id_for(value):
    # Mirror pytest's auto-id rules closely enough that node ids line up
    # with real pytest (bool before int — bool is an int subclass).
    if isinstance(value, bool):
        return str(value)
    if isinstance(value, (int, float)):
        return str(value)
    if isinstance(value, str):
        return value
    if isinstance(value, bytes):
        return value.decode('utf-8', 'replace')
    if value is None:
        return 'None'
    return type(value).__name__


class Session(Collector):
    def __init__(self, config):
        super().__init__('session')
        self.config = config
        self.items = []
        self.failed = []
        self.passed = []
        self.skipped = []
        self.xfailed = []
        self.xpassed = []

    @property
    def nodeid(self):
        return ''


# ============================================================ discovery


def _is_test_file(name):
    return (name.startswith('test_') and name.endswith('.py')) or \
           (name.endswith('_test.py'))


def _discover_files(start):
    if os.path.isfile(start):
        return [start]
    out = []
    for root, dirs, files in os.walk(start):
        # Skip hidden / venv / __pycache__.
        dirs[:] = [d for d in dirs
                   if not d.startswith('.')
                   and d not in ('__pycache__', 'venv', '.venv', 'node_modules')]
        for fn in files:
            if _is_test_file(fn):
                out.append(os.path.join(root, fn))
    out.sort()
    return out


def _match_keyword(item, expr):
    if not expr:
        return True
    return expr in item.name or expr in item.nodeid


def _match_marks(item, expr):
    """Evaluate a ``-m`` mark expression (``not slow and not network``)
    against the item's marks — pytest's markexpr semantics.

    Marks include method-, class- and module-level ``pytestmark`` entries
    (they are merged into ``item.marks`` at collection time). Each bare
    identifier in the expression evaluates to whether the item carries a
    mark of that name."""
    if not expr:
        return True
    names = set()
    for m in getattr(item, 'marks', []):
        n = getattr(m, 'name', None)
        if n:
            names.add(n)
    idents = set(re.findall(r'[A-Za-z_][A-Za-z0-9_]*', expr))
    ns = {i: (i in names) for i in idents
          if i not in ('and', 'or', 'not', 'True', 'False')}
    try:
        return bool(eval(expr, {'__builtins__': {}}, ns))
    except Exception:
        # An unparsable expression selects everything rather than
        # silently hiding tests.
        return True


# ============================================================ runner


def _evaluate_skipif(args, kwargs):
    """Evaluate a `@pytest.mark.skipif(cond, reason=...)` marker.

    Returns (should_skip, reason).
    """
    cond = args[0] if args else kwargs.get('condition')
    reason = kwargs.get('reason', '')
    try:
        return bool(cond), reason
    except Exception:
        return False, reason


def _xfail_from_marks(item):
    """Resolve the item's effective xfail state from its marks.

    Returns ``(expected, reason, raises, run, strict)``. Honours an optional
    condition (first non-str positional arg or ``condition=``). Re-read
    *after* the test body so a ``request.applymarker(pytest.mark.xfail(...))``
    applied at call time is counted, matching pytest.
    """
    for m in item.marks:
        if getattr(m, 'name', None) != 'xfail':
            continue
        kw = m.kwargs
        cond = kw.get('condition', _NOTSET)
        if cond is _NOTSET and m.args and not isinstance(m.args[0], str):
            cond = m.args[0]
        if cond is _NOTSET:
            cond = True
        if not cond:
            continue
        reason = kw.get('reason') or ''
        if not reason and m.args and isinstance(m.args[0], str):
            reason = m.args[0]
        return (True, reason, kw.get('raises'), kw.get('run', True),
                kw.get('strict', False))
    return (False, '', None, True, False)


def _run_one_item(item, config):
    """Run a single :class:`Item`; emit a result tuple."""
    import warnings as _warnings
    start = time.time()
    # Apply marks known before the body runs (skip / skipif).
    for m in item.marks:
        if m.name == 'skip':
            args = m.args
            reason = (m.kwargs.get('reason')
                      or (args[0] if args and isinstance(args[0], str) else 'skipped'))
            return ('skipped', item, reason, time.time() - start)
        if m.name == 'skipif':
            should, reason = _evaluate_skipif(m.args, m.kwargs)
            if should:
                return ('skipped', item, reason or 'skipif', time.time() - start)
    # `xfail(run=False)` means "don't execute the body at all".
    xe, xr, _xraises, xrun, _xstrict = _xfail_from_marks(item)
    if xe and not xrun:
        return ('xfailed', item, xr, time.time() - start)
    # Warning-filter layering (see the filterwarnings section): ini
    # entries first, then `@pytest.mark.filterwarnings` (closest wins by
    # virtue of being installed last → checked first).
    src = _item_source_path(item)
    ini_filters = _config_filterwarnings(src) if src else []
    mark_filters = [spec
                    for m in item.marks if m.name == 'filterwarnings'
                    for spec in m.args]
    try:
        with _warnings.catch_warnings():
            for spec in ini_filters:
                _install_warning_filter(spec)
            for spec in mark_filters:
                _install_warning_filter(spec)
            item.runtest()
    except _Skipped as exc:
        return ('skipped', item, str(exc), time.time() - start)
    except _XFailed as exc:
        return ('xfailed', item, str(exc), time.time() - start)
    except (AssertionError, Exception) as exc:
        tb = traceback.format_exc()
        # Re-read marks: `request.applymarker(xfail)` may have added one
        # inside the body before the failure.
        xe, xr, xraises, _xrun, _xstrict = _xfail_from_marks(item)
        if xe and (xraises is None or isinstance(exc, xraises)):
            return ('xfailed', item, xr or repr(exc), time.time() - start)
        return ('failed', item, tb, time.time() - start)
    # Passed — but a runtime-applied (or decorator) xfail turns this into an
    # xpass (a strict xfail that passes is a failure, as in pytest).
    xe, xr, _xraises, _xrun, xstrict = _xfail_from_marks(item)
    if xe:
        if xstrict:
            return ('failed', item,
                    '[XPASS(strict)] ' + (xr or ''), time.time() - start)
        return ('xpassed', item, xr, time.time() - start)
    return ('passed', item, '', time.time() - start)


# ============================================================ Config / Session helpers


class _OptionNamespace:
    """Lenient stand-in for pytest's parsed-CLI-options namespace."""

    def __init__(self, config):
        self._config = config

    def __getattr__(self, name):
        cfg = object.__getattribute__(self, '_config')
        if name == 'keyword':
            return cfg.keyword
        if name == 'markexpr':
            return cfg.markexpr
        if name == 'verbose':
            return cfg.verbose
        if name in ('lf', 'last_failed', 'ff', 'failedfirst'):
            return False
        return None


class _Config:
    def __init__(self, paths, verbose=0, exitfirst=False, keyword=None,
                 quiet=False, markexpr=None):
        self.paths = paths
        self.verbose = verbose
        self.exitfirst = exitfirst
        self.keyword = keyword
        self.quiet = quiet
        self.markexpr = markexpr
        self.rootdir = os.getcwd()

    @property
    def option(self):
        """pytest's argparse-namespace of CLI options. Only the handful of
        flags the shim parses are meaningful; everything else reads as
        None/falsy (``check_comprehensiveness`` probes ``.lf`` and
        ``.keyword``)."""
        return _OptionNamespace(self)

    def getoption(self, name, default=None, skip=False):
        """Best-effort ``Config.getoption``.

        The shim registers no command-line plugins, so every project option
        (``--no-strict-data-files``, ``--doctest-modules``, …) is absent;
        return the caller's ``default`` (falsy) instead of raising. That is
        the lenient reading pandas' fixtures expect — ``datapath`` then
        *skips* a missing data file rather than erroring.
        """
        return default

    getvalue = getoption


# ============================================================ main


def main(args=None):
    if args is None:
        args = sys.argv[1:]
    paths = []
    verbose = 0
    quiet = False
    exitfirst = False
    keyword = None
    markexpr = None
    i = 0
    while i < len(args):
        a = args[i]
        if a == '-v' or a == '--verbose':
            verbose += 1
        elif a.startswith('-v'):
            verbose += len(a) - 1
        elif a == '-q' or a == '--quiet':
            quiet = True
        elif a == '-x' or a == '--exitfirst':
            exitfirst = True
        elif a == '-k':
            i += 1
            if i >= len(args):
                raise UsageError('-k requires a keyword')
            keyword = args[i]
        elif a.startswith('-k'):
            keyword = a[2:]
        elif a == '-m':
            i += 1
            if i >= len(args):
                raise UsageError('-m requires a mark expression')
            markexpr = args[i]
        elif a.startswith('-m') and len(a) > 2:
            markexpr = a[2:]
        elif a == '--help' or a == '-h':
            print(__doc__)
            return ExitCode.OK
        elif a == '--version':
            print('pytest 8.0.0+weavepy')
            return ExitCode.OK
        elif a.startswith('-'):
            # Accept-and-ignore unknown flags so unsupported options
            # don't crash the harness.
            pass
        else:
            paths.append(a)
        i += 1
    if not paths:
        paths = [os.getcwd()]
    config = _Config(paths=paths, verbose=verbose, exitfirst=exitfirst,
                     keyword=keyword, quiet=quiet, markexpr=markexpr)
    return _run(config)


def _run(config):
    global _ACTIVE_CONFIG, _ACTIVE_SESSION
    _ACTIVE_CONFIG = config
    session = Session(config)
    _ACTIVE_SESSION = session
    files = []
    for p in config.paths:
        files.extend(_discover_files(p))
    if not files:
        if not config.quiet:
            print('collected 0 items / no tests ran')
        return ExitCode.NO_TESTS_COLLECTED
    collected = []
    n_skipped_modules = 0
    for path in files:
        # Run any conftest.py up the chain.
        _load_conftests(path)
        mod = Module(path, parent=session)
        try:
            for item in mod.collect():
                if isinstance(item, Class):
                    collected.extend(item.collect())
                else:
                    collected.append(item)
        except _Skipped as exc:
            # Module-level skip (``pytest.importorskip`` at import time):
            # the whole file is skipped, not an error.
            n_skipped_modules += 1
            if not config.quiet:
                print('SKIPPED module {}: {}'.format(path, exc))
        except CollectionError as exc:
            if not config.quiet:
                print('ERROR: {}'.format(exc))
            return ExitCode.INTERNAL_ERROR

    if config.keyword:
        collected = [it for it in collected if _match_keyword(it, config.keyword)]

    if getattr(config, 'markexpr', None):
        collected = [it for it in collected if _match_marks(it, config.markexpr)]

    if not collected:
        if n_skipped_modules:
            # Everything was skipped (e.g. a missing optional dependency
            # guarding every module given): report success like pytest.
            if not config.quiet:
                print('{} module(s) skipped'.format(n_skipped_modules))
            return ExitCode.OK
        if not config.quiet:
            print('collected 0 items / no tests ran')
        return ExitCode.NO_TESTS_COLLECTED

    if not config.quiet:
        print('collected {} items'.format(len(collected)))

    # ``session.items`` is the post-filter collection list (pytest fills it
    # in ``pytest_collection_modifyitems``); fixtures introspect it.
    session.items = collected

    results = []
    n_passed = n_failed = n_skipped = n_xfailed = n_xpassed = 0
    _trace = os.environ.get('WEAVEPY_SHIM_TRACE')
    for item in collected:
        if _trace:
            sys.stderr.write('>>> RUN ' + str(item.nodeid) + '\n')
            sys.stderr.flush()
        rv = _run_one_item(item, config)
        results.append(rv)
        outcome = rv[0]
        if outcome == 'passed':
            n_passed += 1
            marker = '.'
        elif outcome == 'failed':
            n_failed += 1
            marker = 'F'
        elif outcome == 'skipped':
            n_skipped += 1
            marker = 's'
        elif outcome == 'xfailed':
            n_xfailed += 1
            marker = 'x'
        elif outcome == 'xpassed':
            n_xpassed += 1
            marker = 'X'
        else:
            marker = '?'
        if config.verbose:
            print('{} {}'.format(item.nodeid, outcome.upper()))
        elif not config.quiet:
            sys.stdout.write(marker)
            sys.stdout.flush()
        if config.exitfirst and outcome == 'failed':
            break

    if not config.verbose and not config.quiet:
        print()

    if n_failed:
        print()
        print('=== FAILURES ===')
        for outcome, item, info, _ in results:
            if outcome == 'failed':
                print('--- {} ---'.format(item.nodeid))
                print(info)

    summary_parts = []
    if n_passed:
        summary_parts.append('{} passed'.format(n_passed))
    if n_failed:
        summary_parts.append('{} failed'.format(n_failed))
    if n_skipped:
        summary_parts.append('{} skipped'.format(n_skipped))
    if n_xfailed:
        summary_parts.append('{} xfailed'.format(n_xfailed))
    if n_xpassed:
        summary_parts.append('{} xpassed'.format(n_xpassed))
    # Real pytest's ``-q`` still prints the one-line summary; only the
    # per-test progress dots are suppressed above.
    print('{}'.format(', '.join(summary_parts) or 'no tests'))

    # Tear down session-scoped finalizers so any database
    # connections, temp dirs etc. set up by `scope='session'`
    # fixtures get cleaned before the runner exits.
    _FIXTURE_MANAGER.reset_scope('class')
    _FIXTURE_MANAGER.reset_scope('module')
    _FIXTURE_MANAGER.reset_scope('session')

    if n_failed:
        return ExitCode.TESTS_FAILED
    return ExitCode.OK


def _load_conftests(test_path):
    """Walk up from ``test_path`` loading any ``conftest.py`` files."""
    dirpath = os.path.dirname(os.path.abspath(test_path))
    seen = []
    while dirpath:
        conftest = os.path.join(dirpath, 'conftest.py')
        if os.path.isfile(conftest):
            seen.append(conftest)
        parent = os.path.dirname(dirpath)
        if parent == dirpath:
            break
        dirpath = parent
    for path in reversed(seen):
        modname = '_pytest_conftest_{}'.format(abs(hash(path)))
        if modname in sys.modules:
            continue
        spec = importlib.util.spec_from_file_location(modname, path)
        if spec is None or spec.loader is None:
            continue
        try:
            mod = importlib.util.module_from_spec(spec)
            sys.modules[modname] = mod
            spec.loader.exec_module(mod)
            _register_fixture_aliases(mod)
        except Exception:
            # A conftest that fails to import silently drops *all* of its
            # fixtures, which resurfaces later as a baffling "missing positional
            # argument" on every test that requests one (a missing stdlib module
            # like `shlex` once took out pandas' entire `io` conftest). Real
            # pytest hard-errors here; we keep loading the rest, but drop the
            # half-initialized module and surface the cause under the shim trace
            # flag so the silent fixture loss is at least discoverable.
            sys.modules.pop(modname, None)
            if os.environ.get('WEAVEPY_SHIM_TRACE'):
                import traceback
                sys.stderr.write(
                    'weavepy-pytest: conftest failed to import: {}\n'.format(path))
                traceback.print_exc()


if __name__ == '__main__':
    # When launched as a script (`weavepy shim/pytest.py ...` or
    # `python shim/pytest.py ...`), any `import pytest` performed by a
    # conftest or test binds to the *module* named ``pytest`` — the frozen
    # shim under WeavePy, or this same file re-imported off ``sys.path``
    # under CPython — which is a DIFFERENT module object than ``__main__``.
    # Fixtures registered by those conftests land in *that* module's
    # ``_FIXTURE_REGISTRY``; a runner executing here in ``__main__`` would
    # consult an empty registry and silently drop every conftest fixture
    # (e.g. pandas' parametrised ``tz_naive_fixture``/``tz_aware_fixture``).
    # Delegate to the canonical module so collection, fixture resolution and
    # the registry all share one namespace — the same reason real pytest's
    # ``__main__`` trampolines through ``pytest.console_main``.
    import pytest as _canonical
    if _canonical is not sys.modules.get(__name__):
        sys.exit(_canonical.main())
    sys.exit(main())
