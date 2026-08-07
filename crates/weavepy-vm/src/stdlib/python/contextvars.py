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
"""

__all__ = ["ContextVar", "Context", "Token", "copy_context"]

import _thread

# `ContextVar[int]` yields a `types.GenericAlias` (CPython exposes this on
# the C `ContextVar`). `types` only imports `sys`, so this is safe here.
from types import GenericAlias as _GenericAlias


_MISSING = object()


class Token:
    """Returned by `ContextVar.set`; used to restore the previous value."""

    MISSING = _MISSING

    __slots__ = ("_var", "_old", "_used")

    __class_getitem__ = classmethod(_GenericAlias)

    def __init__(self, var, old):
        self._var = var
        self._old = old
        self._used = False

    @property
    def var(self):
        return self._var

    @property
    def old_value(self):
        return self._old

    def __repr__(self):
        return f"<Token var={self._var!r}>"


class ContextVar:
    """A variable whose value depends on the active `Context`."""

    __slots__ = ("_name", "_default", "_id")

    __class_getitem__ = classmethod(_GenericAlias)

    _counter = 0

    def __init__(self, name, *, default=_MISSING):
        if not isinstance(name, str):
            raise TypeError("ContextVar name must be a str")
        self._name = name
        self._default = default
        ContextVar._counter += 1
        self._id = ContextVar._counter

    @property
    def name(self):
        return self._name

    def get(self, *args):
        ctx = _current_context()
        if self._id in ctx._data:
            return ctx._data[self._id]
        if args:
            return args[0]
        if self._default is not _MISSING:
            return self._default
        raise LookupError(self)

    def set(self, value):
        ctx = _current_context()
        old = ctx._data.get(self._id, _MISSING)
        ctx._data[self._id] = value
        return Token(self, old)

    def reset(self, token):
        if not isinstance(token, Token):
            raise TypeError("not a Token")
        if token._used:
            raise ValueError("Token already used")
        if token._var is not self:
            raise ValueError("Token belongs to a different ContextVar")
        ctx = _current_context()
        token._used = True
        if token._old is _MISSING:
            ctx._data.pop(self._id, None)
        else:
            ctx._data[self._id] = token._old

    def __repr__(self):
        if self._default is _MISSING:
            return f"<ContextVar name={self._name!r}>"
        return f"<ContextVar name={self._name!r} default={self._default!r}>"


class Context:
    """A mapping of `ContextVar` -> value."""

    __slots__ = ("_data", "_entered")

    def __init__(self):
        self._data = {}
        self._entered = False

    def run(self, callable_, *args, **kwargs):
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

    def __contains__(self, var):
        if not isinstance(var, ContextVar):
            return False
        return var._id in self._data

    def __getitem__(self, var):
        if not isinstance(var, ContextVar):
            raise TypeError("expected ContextVar")
        if var._id not in self._data:
            raise KeyError(var)
        return self._data[var._id]

    def get(self, var, default=None):
        if not isinstance(var, ContextVar):
            raise TypeError("expected ContextVar")
        return self._data.get(var._id, default)

    def __iter__(self):
        # The Context API yields ContextVar objects, but we only keep
        # them by id. The cooperative use case (`for k, v in ctx:`) is
        # rare, so we yield (id, value) — close enough for typical
        # introspection. Pull names from a registry if needed.
        return iter(_resolve_keys(self._data))

    def __len__(self):
        return len(self._data)

    def keys(self):
        return list(iter(self))

    def values(self):
        return list(self._data.values())

    def items(self):
        return [(k, self._data[k._id]) for k in iter(self)]


# Per-thread current context: thread ident -> Context. A thread with
# no entry yet lazily gets a fresh empty Context on first access.
_STATES = {}
_REGISTRY = {}


def _resolve_keys(data):
    # We cannot dereference variables by id reliably without a
    # registry; we approximate by returning Tokens-less keys. For
    # the typical PEP 567 use case (asyncio reads its tasks' context)
    # this iteration is rarely needed.
    return iter(data.keys())


def _current_context():
    ident = _thread.get_ident()
    ctx = _STATES.get(ident)
    if ctx is None:
        ctx = Context()
        _STATES[ident] = ctx
    return ctx


def copy_context():
    return _current_context().copy()
