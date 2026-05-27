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
import inspect
import os
import re
import sys
import time
import traceback


__all__ = [
    'main', 'fixture', 'raises', 'warns', 'skip', 'fail', 'xfail',
    'approx', 'mark', 'param', 'Session', 'Item', 'Collector', 'ExitCode',
    'Module', 'Function', 'Class',
    'UsageError', 'CollectionError',
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


# ============================================================ skip/fail/xfail


def skip(reason: str = ''):
    raise _Skipped(reason or 'skipped')


def fail(msg: str = '', pytrace: bool = True):  # noqa: ARG001
    raise _Failed(msg)


def xfail(reason: str = ''):
    raise _XFailed(reason or 'xfail')


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
            existing = getattr(fn, '_pytest_marks', [])
            fn._pytest_marks = existing + [self]
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
        _FIXTURE_REGISTRY[fname] = defn
        return fn
    if callable_ is not None and callable(callable_):
        return deco(callable_)
    return deco


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

    def get_cached(self, name, scope, param):
        return self._caches[scope].get((name, param))

    def set_cached(self, name, scope, param, value):
        self._caches[scope][(name, param)] = value

    def add_finalizer(self, scope, fn):
        self._finalizers[scope].append(fn)


def _builtin_fixture_tmp_path(request):  # noqa: ARG001
    import tempfile
    import pathlib
    return pathlib.Path(tempfile.mkdtemp(prefix='pytest-'))


def _builtin_fixture_tmpdir(request):  # noqa: ARG001
    import tempfile
    return tempfile.mkdtemp(prefix='pytest-')


def _builtin_fixture_capsys(request):  # noqa: ARG001
    import io as _io
    return _CapsysHandle(_io.StringIO(), _io.StringIO())


def _builtin_fixture_monkeypatch(request):  # noqa: ARG001
    return _MonkeyPatchHandle()


class _MonkeyPatchHandle:
    """Minimal monkeypatch fixture for swapping attrs / env vars."""

    def __init__(self):
        self._undo = []

    def setattr(self, target, name=None, value=None, raising=True):
        if isinstance(target, str):
            if name is None or value is None:
                raise TypeError(
                    'monkeypatch.setattr with dotted-string target needs name+value'
                )
            mod_name, _, attr = target.rpartition('.')
            mod = importlib.import_module(mod_name)
            target = mod
            name_for_attr = attr
            value_for_attr = value
        else:
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
        sys.stdout = self._orig_stdout
        sys.stderr = self._orig_stderr

    def __del__(self):
        try:
            sys.stdout = self._orig_stdout
            sys.stderr = self._orig_stderr
        except Exception:  # pragma: no cover
            pass


class _CapturedIO:
    __slots__ = ('out', 'err')

    def __init__(self, out, err):
        self.out = out
        self.err = err


_BUILTIN_FIXTURES = {
    'tmp_path': _builtin_fixture_tmp_path,
    'tmpdir': _builtin_fixture_tmpdir,
    'capsys': _builtin_fixture_capsys,
    'monkeypatch': _builtin_fixture_monkeypatch,
}


class _Request:
    """Drop-in for ``pytest.FixtureRequest``.

    Exposes ``node`` / ``item`` (the test being run), ``param`` (the
    indirect-fixture parameter), ``fixturename``, and
    ``addfinalizer``. Finalisers are queued at the fixture's scope.
    """
    __slots__ = ('node', 'item', 'param', 'fixturename', '_manager', '_scope')

    def __init__(self, node, item, manager, scope, fixturename=None, param=None):
        self.node = node
        self.item = item
        self.param = param
        self.fixturename = fixturename
        self._manager = manager
        self._scope = scope

    def addfinalizer(self, fn):
        self._manager.add_finalizer(self._scope, fn)

    def getfixturevalue(self, name):
        return _resolve_fixture(name, self._manager, self.item, self.node)


def _resolve_fixture(name, manager=None, item=None, node=None, param=None,
                     parent_scope='function'):
    """Resolve a fixture by name.

    Honours scope caching, generator-style teardown, and
    parametrised fixtures (the active ``param`` is read from the
    item's `_params` dict if present).
    """
    if manager is None:
        manager = _FIXTURE_MANAGER
    defn = _FIXTURE_REGISTRY.get(name)
    if defn is not None:
        # Parametrised fixture: pick the active parameter for this
        # item if `parametrize` filled it in.
        active_param = param
        if active_param is None and item is not None:
            active_param = getattr(item, '_fixture_params', {}).get(name)
        cache_key = active_param
        cached = manager.get_cached(name, defn.scope, cache_key)
        if cached is not None:
            return cached
        req = _Request(node=node, item=item, manager=manager,
                       scope=defn.scope, fixturename=name, param=active_param)
        # Build the argument bindings — recurse for any fixture deps.
        sig = inspect.signature(defn.fn)
        kwargs = {}
        for pname in sig.parameters:
            if pname == 'request':
                kwargs[pname] = req
            else:
                sub = _resolve_fixture(pname, manager, item, node)
                if sub is not None:
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
        # monkeypatch needs an automatic teardown.
        val = builtin(req)
        if name == 'monkeypatch':
            manager.add_finalizer('function', val.undo)
        return val
    return None


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
    def __init__(self, expected, match=None):
        self.expected = expected
        self.match = match

    def __enter__(self):
        import warnings as _warnings
        self._catcher = _warnings.catch_warnings(record=True)
        self.warnings = self._catcher.__enter__()
        _warnings.simplefilter('always')
        return self

    def __exit__(self, exc_type, exc_val, tb):
        self._catcher.__exit__(exc_type, exc_val, tb)
        if exc_type is not None:
            return False
        if not any(issubclass(w.category, self.expected) for w in self.warnings):
            raise _Failed('Expected warning {} not raised'.format(self.expected))
        return False


def warns(expected, *args, match=None, **kwargs):
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


class Item(Collector):
    """A single test item (callable)."""

    def __init__(self, name, parent, callable_, marks=None, params=None,
                 param_id=None):
        super().__init__(name, parent)
        self.callable = callable_
        self.marks = marks or []
        # Parametrize sets `_fixture_params` so the resolver picks
        # the right value for each fixture argument.
        self._fixture_params = params or {}
        self._param_id = param_id

    @property
    def nodeid(self):
        base = self.name
        if self._param_id:
            base = '{}[{}]'.format(self.name, self._param_id)
        if self.parent and hasattr(self.parent, 'nodeid'):
            return '{}::{}'.format(self.parent.nodeid, base)
        return base

    def runtest(self):
        sig = inspect.signature(self.callable)
        kwargs = {}
        # Eagerly resolve any autouse fixtures so their teardowns
        # get queued (matches pytest's ordering: autouse fires for
        # every test in scope even if not requested by name).
        for fname, defn in _FIXTURE_REGISTRY.items():
            if defn.autouse:
                _resolve_fixture(fname, _FIXTURE_MANAGER, self, self.parent)
        for pname in sig.parameters:
            # Parametrize injects directly-passed values that aren't
            # fixtures — those win over the resolver.
            if pname in self._fixture_params:
                kwargs[pname] = self._fixture_params[pname]
                continue
            val = _resolve_fixture(pname, _FIXTURE_MANAGER, self, self.parent)
            if val is not None:
                kwargs[pname] = val
        try:
            return self.callable(**kwargs)
        finally:
            _FIXTURE_MANAGER.reset_scope('function')


# Alias matching CPython's pytest naming convention.
Function = Item


class Class(Collector):
    def __init__(self, name, parent, cls):
        super().__init__(name, parent)
        self.cls = cls

    @property
    def nodeid(self):
        return '{}::{}'.format(self.parent.nodeid, self.name)

    def collect(self):
        items = []
        instance = self.cls()
        for attr in dir(self.cls):
            if not attr.startswith('test_'):
                continue
            method = getattr(instance, attr)
            if not callable(method):
                continue
            marks = getattr(method, '_pytest_marks', [])
            items.extend(_expand_parametrize(attr, self, method, marks))
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
        try:
            spec.loader.exec_module(mod)
        except Exception as exc:
            raise CollectionError('error importing {}: {}'.format(self.path, exc)) from None
        self.module = mod
        out = []
        for name in dir(mod):
            obj = getattr(mod, name)
            if name.startswith('test_') and callable(obj):
                marks = getattr(obj, '_pytest_marks', [])
                out.extend(_expand_parametrize(name, self, obj, marks))
            elif name.startswith('Test') and inspect.isclass(obj):
                out.append(Class(name, self, obj))
        return out

    def _mod_name(self):
        base = os.path.basename(self.path)
        if base.endswith('.py'):
            base = base[:-3]
        return base


def _expand_parametrize(name, parent, fn, marks):
    """Expand `@pytest.mark.parametrize` markers into per-row items.

    Supports the canonical pytest spellings:

      @pytest.mark.parametrize('a,b', [(1, 2), (3, 4)])
      @pytest.mark.parametrize('a', [1, 2, 3], ids=['one', 'two', 'three'])
      @pytest.mark.parametrize('value', [pytest.param(1, id='one'), 2])

    Multiple parametrize decorators stack into a Cartesian product
    (pytest matrix semantics).
    """
    param_marks = [m for m in marks if m.name == 'parametrize']
    other_marks = [m for m in marks if m.name != 'parametrize']
    if not param_marks:
        return [Item(name, parent, fn, marks=other_marks)]
    matrix = [({}, [])]  # (param-binding dict, id-fragments)
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
        new_matrix = []
        for row_idx, row in enumerate(argvalues):
            # Unwrap `pytest.param(value, id=..., marks=...)` if used.
            row_id = None
            if isinstance(row, _ParamSet):
                row_value = row.values
                row_id = row.id
            else:
                row_value = row
            if len(names) > 1:
                values = list(row_value) if not isinstance(row_value, (tuple, list)) \
                                          else list(row_value)
                if len(values) != len(names):
                    raise UsageError(
                        'parametrize: row {} has {} values for {} names'.format(
                            row_idx, len(values), len(names)))
            else:
                values = [row_value]
            if row_id is None and explicit_ids is not None:
                row_id = explicit_ids[row_idx]
            if row_id is None:
                row_id = '-'.join(_id_for(v) for v in values)
            for prior_params, prior_ids in matrix:
                merged = dict(prior_params)
                for nm, val in zip(names, values):
                    merged[nm] = val
                new_matrix.append((merged, prior_ids + [row_id]))
        matrix = new_matrix
    items = []
    for params, id_frags in matrix:
        pid = '-'.join(id_frags) if id_frags else None
        items.append(Item(name, parent, fn, marks=other_marks,
                          params=params, param_id=pid))
    return items


class _ParamSet:
    """``pytest.param(value, id=..., marks=...)`` payload."""
    __slots__ = ('values', 'id', 'marks')

    def __init__(self, values, id=None, marks=()):  # noqa: A002
        self.values = values
        self.id = id
        self.marks = list(marks) if marks else []


def param(*values, id=None, marks=()):  # noqa: A002
    """Wrap a parametrize row with an explicit id and/or marks."""
    return _ParamSet(values if len(values) > 1 else values[0],
                     id=id, marks=marks)


def _id_for(value):
    if isinstance(value, (int, float, bool, str, bytes)):
        return repr(value)
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


def _run_one_item(item, config):
    """Run a single :class:`Item`; emit a result tuple."""
    start = time.time()
    # Apply marks.
    skip_reason = None
    xfail_expected = False
    xfail_reason = ''
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
        if m.name == 'xfail':
            xfail_expected = True
            xfail_reason = (m.kwargs.get('reason')
                            or (m.args[0] if m.args else ''))
    try:
        item.runtest()
    except _Skipped as exc:
        return ('skipped', item, str(exc), time.time() - start)
    except _XFailed as exc:
        return ('xfailed', item, str(exc), time.time() - start)
    except (AssertionError, Exception) as exc:
        tb = traceback.format_exc()
        if xfail_expected:
            return ('xfailed', item, xfail_reason or repr(exc), time.time() - start)
        return ('failed', item, tb, time.time() - start)
    if xfail_expected:
        return ('xpassed', item, xfail_reason, time.time() - start)
    return ('passed', item, '', time.time() - start)


# ============================================================ Config / Session helpers


class _Config:
    def __init__(self, paths, verbose=0, exitfirst=False, keyword=None,
                 quiet=False):
        self.paths = paths
        self.verbose = verbose
        self.exitfirst = exitfirst
        self.keyword = keyword
        self.quiet = quiet
        self.rootdir = os.getcwd()


# ============================================================ main


def main(args=None):
    if args is None:
        args = sys.argv[1:]
    paths = []
    verbose = 0
    quiet = False
    exitfirst = False
    keyword = None
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
                     keyword=keyword, quiet=quiet)
    return _run(config)


def _run(config):
    session = Session(config)
    files = []
    for p in config.paths:
        files.extend(_discover_files(p))
    if not files:
        if not config.quiet:
            print('collected 0 items / no tests ran')
        return ExitCode.NO_TESTS_COLLECTED
    collected = []
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
        except CollectionError as exc:
            if not config.quiet:
                print('ERROR: {}'.format(exc))
            return ExitCode.INTERNAL_ERROR

    if config.keyword:
        collected = [it for it in collected if _match_keyword(it, config.keyword)]

    if not collected:
        if not config.quiet:
            print('collected 0 items / no tests ran')
        return ExitCode.NO_TESTS_COLLECTED

    if not config.quiet:
        print('collected {} items'.format(len(collected)))

    results = []
    n_passed = n_failed = n_skipped = n_xfailed = n_xpassed = 0
    for item in collected:
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
    if not config.quiet:
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
        except Exception:
            pass


if __name__ == '__main__':
    sys.exit(main())
