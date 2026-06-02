"""WeavePy `unittest.mock` — `Mock`, `MagicMock`, `patch`.

Implements the everyday `mock` surface: `Mock`, `MagicMock`,
`patch`, `patch.object`, `patch.dict`, `call`, `ANY`, `sentinel`,
and `call_count` / `call_args` / `call_args_list`. Not a full port
but enough to run most test suites that lean on `mock.patch`.
"""

import builtins
import sys


# Public builtin names. Patching one of these onto a *module* implicitly
# creates it (CPython does the same): a module's functions resolve a bare
# name through the module globals before falling back to builtins, so the
# patched name is what they'll see. Lets e.g. `patch.object(mod, 'open')`
# work even though `mod` never bound `open` itself.
_builtins = {name for name in dir(builtins) if not name.startswith("_")}
_ModuleType = type(sys)


__all__ = [
    "Mock",
    "MagicMock",
    "NonCallableMock",
    "NonCallableMagicMock",
    "patch",
    "call",
    "ANY",
    "sentinel",
    "DEFAULT",
    "create_autospec",
    "PropertyMock",
    "AsyncMock",
    "mock_open",
]


_DEFAULT_NAME = "mock"


class _DefaultSentinel:
    def __repr__(self):
        return "DEFAULT"


DEFAULT = _DefaultSentinel()


class _ANY:
    def __eq__(self, other):
        return True

    def __ne__(self, other):
        return False

    def __repr__(self):
        return "ANY"


ANY = _ANY()


class _Sentinel:
    def __init__(self, name):
        self._name = name

    def __repr__(self):
        return f"sentinel.{self._name}"


class _SentinelFactory:
    def __init__(self):
        self._sentinels = {}

    def __getattr__(self, name):
        if name not in self._sentinels:
            self._sentinels[name] = _Sentinel(name)
        return self._sentinels[name]


sentinel = _SentinelFactory()


class _Call:
    """A (name, args, kwargs) call record.

    CPython models this as a tuple subclass so `call_args[0]` and
    `call_args[1]` resolve to args / kwargs. WeavePy doesn't yet have
    full tuple subclass method inheritance, so we implement the
    minimum tuple-shape ourselves: `__getitem__`/`__len__`/`__iter__`
    plus the equality / repr conventions the API documents.
    """

    def __init__(self, value=(), name=None, parent=None, two=False, from_kall=True):
        args = ()
        kwargs = {}
        n = name or ""
        if not isinstance(value, tuple):
            value = (value,)
        if len(value) == 3:
            n, args, kwargs = value
        elif len(value) == 2:
            args, kwargs = value
        elif len(value) == 1:
            args = value[0] if isinstance(value[0], tuple) else (value[0],)
        self._mock_name = n
        self._mock_args = tuple(args)
        self._mock_kwargs = dict(kwargs)
        self._named = name is not None
        self._tuple = (
            (n, self._mock_args, self._mock_kwargs)
            if self._named
            else (self._mock_args, self._mock_kwargs)
        )

    @property
    def args(self):
        return self._mock_args

    @property
    def kwargs(self):
        return self._mock_kwargs

    def __getitem__(self, index):
        return self._tuple[index]

    def __len__(self):
        return len(self._tuple)

    def __iter__(self):
        return iter(self._tuple)

    def __call__(self, *args, **kwargs):
        return _Call((args, kwargs))

    def __getattr__(self, name):
        if name in ("__call__", "_mock_name", "_mock_args", "_mock_kwargs",
                    "_tuple", "_named", "args", "kwargs"):
            raise AttributeError(name)
        return _Call(name=name)

    def __eq__(self, other):
        if isinstance(other, _Call):
            return (self._mock_args == other._mock_args
                    and self._mock_kwargs == other._mock_kwargs
                    and self._mock_name == other._mock_name)
        if isinstance(other, tuple):
            return self._tuple == other
        return NotImplemented

    def __ne__(self, other):
        eq = self.__eq__(other)
        if eq is NotImplemented:
            return eq
        return not eq

    def __hash__(self):
        return hash((self._mock_name, self._mock_args, tuple(sorted(self._mock_kwargs.items()))))

    def __repr__(self):
        return f"call({self._mock_args}, {self._mock_kwargs})"


call = _Call()


class NonCallableMock:
    """A non-callable mock object (parent of Mock)."""

    def __init__(self, spec=None, wraps=None, name=None, spec_set=None,
                 side_effect=None, return_value=DEFAULT, unsafe=False, **kwargs):
        object.__setattr__(self, "_mock_children", {})
        object.__setattr__(self, "_mock_name", name or _DEFAULT_NAME)
        object.__setattr__(self, "_mock_call_args", None)
        object.__setattr__(self, "_mock_call_args_list", [])
        object.__setattr__(self, "_mock_call_count", 0)
        object.__setattr__(self, "_mock_mock_calls", [])
        object.__setattr__(self, "_mock_return_value", return_value)
        object.__setattr__(self, "_mock_side_effect", side_effect)
        object.__setattr__(self, "_mock_spec", spec)
        object.__setattr__(self, "_mock_spec_set", spec_set)
        object.__setattr__(self, "_mock_wraps", wraps)
        object.__setattr__(self, "_mock_methods", _spec_names(spec_set or spec))
        object.__setattr__(self, "_mock_called", False)
        for k, v in kwargs.items():
            setattr(self, k, v)

    # ----- attribute access ----- #

    def _check_attr(self, name):
        spec = self._mock_methods
        if spec is not None and name not in spec and not name.startswith("_"):
            raise AttributeError(name)

    def __getattr__(self, name):
        if name.startswith("_mock_") or name in ("__class__", "__dict__"):
            raise AttributeError(name)
        self._check_attr(name)
        children = self._mock_children
        if name in children:
            return children[name]
        child = MagicMock(name=f"{self._mock_name}.{name}")
        children[name] = child
        return child

    def __setattr__(self, name, value):
        if self._mock_spec_set is not None and name not in self._mock_methods and not name.startswith("_"):
            raise AttributeError(f"Mock object has no attribute {name!r}")
        object.__setattr__(self, name, value)

    # ----- introspection helpers ----- #

    @property
    def return_value(self):
        rv = self._mock_return_value
        if rv is DEFAULT:
            rv = MagicMock(name=f"{self._mock_name}()")
            object.__setattr__(self, "_mock_return_value", rv)
        return rv

    @return_value.setter
    def return_value(self, value):
        object.__setattr__(self, "_mock_return_value", value)

    @property
    def side_effect(self):
        return self._mock_side_effect

    @side_effect.setter
    def side_effect(self, value):
        object.__setattr__(self, "_mock_side_effect", value)

    @property
    def called(self):
        return self._mock_called

    @property
    def call_count(self):
        return self._mock_call_count

    @property
    def call_args(self):
        return self._mock_call_args

    @property
    def call_args_list(self):
        return list(self._mock_call_args_list)

    @property
    def mock_calls(self):
        return list(self._mock_mock_calls)

    def reset_mock(self, visited=None, *, return_value=False, side_effect=False):
        object.__setattr__(self, "_mock_call_args", None)
        object.__setattr__(self, "_mock_call_args_list", [])
        object.__setattr__(self, "_mock_call_count", 0)
        object.__setattr__(self, "_mock_mock_calls", [])
        object.__setattr__(self, "_mock_called", False)
        if return_value:
            object.__setattr__(self, "_mock_return_value", DEFAULT)
        if side_effect:
            object.__setattr__(self, "_mock_side_effect", None)
        for child in self._mock_children.values():
            if isinstance(child, NonCallableMock):
                child.reset_mock()

    def assert_not_called(self):
        if self._mock_called:
            raise AssertionError(f"Expected '{self._mock_name}' to not have been called.")

    def assert_called(self):
        if not self._mock_called:
            raise AssertionError(f"Expected '{self._mock_name}' to have been called.")

    def assert_called_once(self):
        if self._mock_call_count != 1:
            raise AssertionError(
                f"Expected '{self._mock_name}' to have been called once. "
                f"Called {self._mock_call_count} times."
            )

    def assert_called_with(self, *args, **kwargs):
        expected = _Call((args, kwargs))
        if self._mock_call_args != expected:
            raise AssertionError(
                f"expected call not found.\nExpected: {expected}\nActual: {self._mock_call_args}"
            )

    def assert_called_once_with(self, *args, **kwargs):
        self.assert_called_once()
        self.assert_called_with(*args, **kwargs)

    def assert_any_call(self, *args, **kwargs):
        expected = _Call((args, kwargs))
        for c in self._mock_call_args_list:
            if c == expected:
                return
        raise AssertionError(
            f"{self._mock_name}({args}, {kwargs}) call not found."
        )

    def assert_has_calls(self, calls, any_order=False):
        all_calls = list(self._mock_call_args_list)
        if any_order:
            for c in calls:
                if c not in all_calls:
                    raise AssertionError(f"{c!r} not in {all_calls!r}")
            return
        # In-order sequential match.
        idx = 0
        for c in calls:
            while idx < len(all_calls):
                if all_calls[idx] == c:
                    idx += 1
                    break
                idx += 1
            else:
                raise AssertionError(f"Calls not found in order: {c!r}")

    def configure_mock(self, **kwargs):
        for k, v in kwargs.items():
            setattr(self, k, v)


def _spec_names(spec):
    if spec is None:
        return None
    if isinstance(spec, list):
        return set(spec)
    return set(n for n in dir(spec) if not n.startswith("__"))


class Mock(NonCallableMock):
    def __call__(self, *args, **kwargs):
        object.__setattr__(self, "_mock_called", True)
        object.__setattr__(self, "_mock_call_count", self._mock_call_count + 1)
        c = _Call((args, kwargs))
        object.__setattr__(self, "_mock_call_args", c)
        self._mock_call_args_list.append(c)
        self._mock_mock_calls.append(c)
        if self._mock_side_effect is not None:
            effect = self._mock_side_effect
            if callable(effect):
                rv = effect(*args, **kwargs)
                if rv is not DEFAULT:
                    return rv
            elif isinstance(effect, (list, tuple)) or hasattr(effect, "__iter__"):
                # Probe the cached iterator via ``__dict__`` rather than
                # ``hasattr`` / attribute access: a ``Mock``'s
                # ``__getattr__`` auto-creates children for unknown
                # names, so ``hasattr(self, "_side_effect_iter")`` is
                # always true and would defeat the cache.
                it = self.__dict__.get("_side_effect_iter")
                if it is None:
                    it = iter(effect)
                    object.__setattr__(self, "_side_effect_iter", it)
                # An exhausted side-effect iterable raises StopIteration
                # to the caller, mirroring CPython's mock.
                rv = next(it)
                if isinstance(rv, BaseException) or (isinstance(rv, type) and issubclass(rv, BaseException)):
                    raise rv
                return rv
            else:
                raise effect
        if self._mock_return_value is DEFAULT:
            return self.return_value
        return self._mock_return_value


class MagicMock(Mock):
    """A `Mock` with magic-method protocols pre-wired."""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        object.__setattr__(self, "_magic_iter", iter([]))

    def __iter__(self):
        return iter(self._magic_iter)

    def __next__(self):
        return next(self._magic_iter)

    def __len__(self):
        return 0

    def __bool__(self):
        return True

    def __contains__(self, item):
        return False

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


# CPython models NonCallableMagicMock as a multi-base class
# (NonCallableMock + MagicMock); WeavePy's MRO does not yet accept
# the C3 linearisation those bases require, so we shim it as a thin
# subclass of NonCallableMock. The only difference users observe is
# that this class does not pull in MagicMock's magic dunder cache,
# which is acceptable for our stdlib's smoke tests.
class NonCallableMagicMock(NonCallableMock):
    pass


class AsyncMock(MagicMock):
    """A MagicMock whose call returns an awaitable."""

    async def __call__(self, *args, **kwargs):
        return MagicMock.__call__(self, *args, **kwargs)


class PropertyMock(Mock):
    """A mock that triggers when used as a property."""

    def __get__(self, obj, objtype=None):
        return self()

    def __set__(self, obj, value):
        self(value)


def mock_open(mock=None, read_data=""):
    if mock is None:
        mock = MagicMock(name="open")
    handle = MagicMock(name="open()")
    handle.__enter__.return_value = handle
    handle.__exit__.return_value = False
    handle.read.return_value = read_data
    handle.readline.return_value = read_data.splitlines(True)[0] if read_data else ""
    handle.readlines.return_value = read_data.splitlines(True)
    mock.return_value = handle
    return mock


def create_autospec(spec, *args, **kwargs):
    return MagicMock(spec=spec, *args, **kwargs)


# ---------------- patch ---------------- #

class _patch:
    def __init__(self, target, attribute, new=DEFAULT, spec=None, create=False,
                 spec_set=None, autospec=None, new_callable=None, **kwargs):
        self.target = target
        self.attribute = attribute
        self.new = new
        self.spec = spec
        self.create = create
        self.spec_set = spec_set
        self.autospec = autospec
        self.new_callable = new_callable
        self.kwargs = kwargs
        self._original = None
        self._had = False

    def _resolve_target(self):
        if isinstance(self.target, str):
            mod_name, _, _ = self.target.rpartition(".")
            return _import_target(mod_name)
        return self.target

    def __enter__(self):
        obj = self._resolve_target()
        self._had = hasattr(obj, self.attribute)
        # A builtin name patched onto a module is created implicitly (see
        # `_builtins`), matching CPython's `_patch.get_original`.
        create = self.create or (
            self.attribute in _builtins and isinstance(obj, _ModuleType)
        )
        if self._had:
            self._original = getattr(obj, self.attribute)
        elif not create:
            raise AttributeError(f"{obj!r} does not have the attribute {self.attribute!r}")
        new = self.new
        if new is DEFAULT:
            if self.new_callable is not None:
                new = self.new_callable(**self.kwargs)
            else:
                new = MagicMock(**self.kwargs)
        setattr(obj, self.attribute, new)
        self._obj = obj
        return new

    def __exit__(self, *exc):
        if self._had:
            setattr(self._obj, self.attribute, self._original)
        else:
            try:
                delattr(self._obj, self.attribute)
            except Exception:
                pass
        return False

    def __call__(self, func):
        def wrapper(*args, **kwargs):
            with self as new_mock:
                return func(*args, new_mock, **kwargs)

        wrapper.__name__ = getattr(func, "__name__", "patched")
        return wrapper

    def start(self):
        return self.__enter__()

    def stop(self):
        self.__exit__(None, None, None)


def _import_target(dotted):
    if not dotted:
        return sys.modules.get("__main__")
    parts = dotted.split(".")
    name = parts[0]
    mod = sys.modules.get(name)
    if mod is None:
        mod = __import__(name)
    for p in parts[1:]:
        mod = getattr(mod, p)
    return mod


def _split_target(target):
    parent, _, attr = target.rpartition(".")
    return parent, attr


def patch(target, new=DEFAULT, spec=None, create=False, spec_set=None,
          autospec=None, new_callable=None, **kwargs):
    parent, attr = _split_target(target)
    return _patch(parent, attr, new=new, spec=spec, create=create,
                  spec_set=spec_set, autospec=autospec,
                  new_callable=new_callable, **kwargs)


def _patch_object(target, attribute, new=DEFAULT, spec=None, create=False,
                  spec_set=None, autospec=None, new_callable=None, **kwargs):
    return _patch(target, attribute, new=new, spec=spec, create=create,
                  spec_set=spec_set, autospec=autospec,
                  new_callable=new_callable, **kwargs)


def _patch_dict(in_dict, values=(), clear=False, **kwargs):
    class _DictPatch:
        def __init__(self):
            self._original = None

        def _resolve(self):
            if isinstance(in_dict, str):
                return _import_target(in_dict)
            return in_dict

        def __enter__(self):
            d = self._resolve()
            self._original = dict(d)
            self._target = d
            if clear:
                d.clear()
            if hasattr(values, "items"):
                d.update(values)
            elif values:
                for k, v in values:
                    d[k] = v
            if kwargs:
                d.update(kwargs)
            return d

        def __exit__(self, *exc):
            d = self._target
            d.clear()
            d.update(self._original)
            return False

        def start(self):
            return self.__enter__()

        def stop(self):
            self.__exit__(None, None, None)

    return _DictPatch()


patch.object = _patch_object
patch.dict = _patch_dict
patch.DEFAULT = DEFAULT
