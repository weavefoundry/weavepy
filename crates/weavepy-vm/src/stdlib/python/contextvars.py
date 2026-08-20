"""WeavePy `contextvars` — PEP 567 context variables.

A pure-Python implementation that matches the CPython API surface
for asyncio and library code: `ContextVar`, `Context`, `Token`,
`copy_context`. The runtime keeps a per-thread current context
pointer; `Context.run(fn, ...)` swaps it on entry and restores it on
exit.

PEP 567 semantics: each OS thread has its own independent "current
context"; a freshly started thread begins with an empty context (it
does NOT inherit the spawning thread's values). We keep the per-thread
state in a dict keyed by `_thread.get_ident()` — all mutations happen
under the GIL, so plain dict operations are safe.

Contexts map ContextVar objects (identity-hashed) to values, so
iteration/keys/items yield the real variables like CPython's
HAMT-backed Context. Tokens record the Context they were minted in;
`reset` enforces CPython's full error taxonomy (RuntimeError for a
reused token, ValueError for a foreign variable or Context).
"""

__all__ = ["ContextVar", "Context", "Token", "copy_context"]

import _thread

# `ContextVar[int]` yields a `types.GenericAlias` (CPython exposes this on
# the C `ContextVar`). `types` only imports `sys`, so this is safe here.
from types import GenericAlias as _GenericAlias


_MISSING = object()

# Py_ReprEnter stand-in for ContextVar.__repr__: a var whose *default*
# reprs back to the var (e.g. a list containing it) renders as `...`
# instead of recursing (test_context test_context_var_repr_1).
_repr_running = set()


def _no_subclassing(cls):
    # The C types are final (no Py_TPFLAGS_BASETYPE); class statements
    # deriving from them fail at creation (test_context
    # test_context_subclassing_1).
    raise TypeError(
        f"type 'contextvars.{cls.__mro__[1].__name__}' "
        "is not an acceptable base type")


class Token:
    """Returned by `ContextVar.set`; used to restore the previous value."""

    MISSING = _MISSING

    __slots__ = ("_var", "_old", "_used", "_ctx", "__weakref__")

    __class_getitem__ = classmethod(_GenericAlias)

    def __init_subclass__(cls, **kwargs):
        _no_subclassing(cls)

    def __init__(self, var, old, ctx):
        self._var = var
        self._old = old
        self._used = False
        self._ctx = ctx

    @property
    def var(self):
        return self._var

    @property
    def old_value(self):
        return self._old

    def __repr__(self):
        used = " used" if self._used else ""
        return f"<Token{used} var={self._var!r} at {id(self):#x}>"


class ContextVar:
    """A variable whose value depends on the active `Context`."""

    __slots__ = ("_name", "_default", "__weakref__")

    __class_getitem__ = classmethod(_GenericAlias)

    def __init_subclass__(cls, **kwargs):
        _no_subclassing(cls)

    def __init__(self, *args, default=_MISSING):
        if len(args) != 1:
            raise TypeError(
                "ContextVar() takes exactly 1 positional argument "
                f"({len(args)} given)")
        name = args[0]
        if not isinstance(name, str):
            raise TypeError("context variable name must be a str")
        # The C type interns the name in a hash-keyed cache slot; an
        # unhashable str subclass fails *here*, not later (gh-132002).
        hash(name)
        self._name = name
        self._default = default

    @property
    def name(self):
        return self._name

    def get(self, *args):
        if len(args) > 1:
            raise TypeError(
                f"get() takes at most 1 argument ({len(args)} given)")
        data = _current_context()._data
        if self in data:
            return data[self]
        if args:
            return args[0]
        if self._default is not _MISSING:
            return self._default
        raise LookupError(self)

    def set(self, value):
        ctx = _current_context()
        old = ctx._data.get(self, _MISSING)
        ctx._data[self] = value
        return Token(self, old, ctx)

    def reset(self, token):
        if not isinstance(token, Token):
            raise TypeError("expected an instance of Token")
        if token._used:
            raise RuntimeError(f"{token!r} has already been used once")
        if token._var is not self:
            raise ValueError(f"{token!r} was created by a different ContextVar")
        ctx = _current_context()
        if token._ctx is not ctx:
            raise ValueError(f"{token!r} was created in a different Context")
        token._used = True
        if token._old is _MISSING:
            ctx._data.pop(self, None)
        else:
            ctx._data[self] = token._old

    def __repr__(self):
        key = id(self)
        if key in _repr_running:
            return "..."
        r = f"<ContextVar name={self._name!r}"
        if self._default is not _MISSING:
            _repr_running.add(key)
            try:
                r += f" default={self._default!r}"
            finally:
                _repr_running.discard(key)
        return r + f" at {id(self):#x}>"


class Context:
    """A mapping of `ContextVar` -> value."""

    __slots__ = ("_data", "_entered")

    def __init_subclass__(cls, **kwargs):
        _no_subclassing(cls)

    def __init__(self, *args, **kwargs):
        if args or kwargs:
            raise TypeError("Context() does not accept any arguments")
        self._data = {}
        self._entered = False

    def run(self, callable_, /, *args, **kwargs):
        if self._entered:
            raise RuntimeError(
                f"cannot enter context: {self!r} is already entered")
        ident = _thread.get_ident()
        prev = _STATES.get(ident)
        self._entered = True
        _STATES[ident] = self
        try:
            return callable_(*args, **kwargs)
        finally:
            self._entered = False
            if prev is None:
                _STATES.pop(ident, None)
            else:
                _STATES[ident] = prev

    def copy(self):
        new = Context()
        new._data = dict(self._data)
        return new

    @staticmethod
    def _check_key(var):
        if not isinstance(var, ContextVar):
            raise TypeError(f"a ContextVar key was expected, got {var!r}")

    def __contains__(self, var):
        self._check_key(var)
        return var in self._data

    def __getitem__(self, var):
        self._check_key(var)
        return self._data[var]

    def get(self, var, default=None):
        self._check_key(var)
        return self._data.get(var, default)

    def __eq__(self, other):
        if not isinstance(other, Context):
            return NotImplemented
        return self._data == other._data

    # Unhashable, like the C type (it defines tp_richcompare without
    # tp_hash inheritance).
    __hash__ = None

    def __iter__(self):
        # Snapshot: iteration stays valid if a nested `run` mutates
        # the live mapping (CPython iterates an immutable HAMT).
        return iter(list(self._data))

    def __len__(self):
        return len(self._data)

    def keys(self):
        return list(self._data)

    def values(self):
        return list(self._data.values())

    def items(self):
        return list(self._data.items())


# Per-thread current context: thread ident -> Context. A thread with
# no entry yet lazily gets a fresh empty Context on first access.
_STATES = {}


def _current_context():
    ident = _thread.get_ident()
    ctx = _STATES.get(ident)
    if ctx is None:
        ctx = Context()
        _STATES[ident] = ctx
    return ctx


def copy_context():
    return _current_context().copy()
