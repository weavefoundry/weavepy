"""WeavePy `contextvars` — PEP 567 context variables.

The classes are defined here with `__slots__`; their hot methods
(`ContextVar.get`/`set`/`reset`, `Context.run`/`copy`, and
`copy_context`) are native (`stdlib/contextvars_native.rs`), which also
keeps each OS thread's current context. A freshly started thread begins
with an empty context; it doesn't inherit the spawning thread's values.

A context's `_data` slot holds its mapping (ContextVar -> value), a
dict shared between a context and its copies and copied on the first
write while shared, so `copy_context()` is O(1) like CPython's HAMT-based
copy. Python code here only reads it. Tokens record the Context they
were minted in; `reset` enforces CPython's full error taxonomy
(RuntimeError for a reused token, ValueError for a foreign variable or
Context).
"""

__all__ = ["ContextVar", "Context", "Token", "copy_context"]

import _weave_contextvars as _native

# `ContextVar[int]` yields a `types.GenericAlias` (CPython exposes this on
# the C `ContextVar`). Spelled like `_collections_abc` does rather than
# `from types import GenericAlias`: this module loads at startup (via
# `_warnings`), and `types` must not be in `sys.modules` after
# `python -I -c pass` (test_site.test_startup_imports).
_GenericAlias = type(list[int])


class _TokenMissing:
    """`Token.MISSING` (CPython's `_token_missing` singleton)."""

    __slots__ = ()

    def __repr__(self):
        return '<Token.MISSING>'

    def __reduce__(self):
        return '_MISSING'


_MISSING = _TokenMissing()

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

    # The native `ContextVar.set` builds tokens over this slot order.
    __slots__ = ("_var", "_old", "_used", "_ctx", "__weakref__")

    __class_getitem__ = classmethod(_GenericAlias)

    def __init_subclass__(cls, **kwargs):
        _no_subclassing(cls)

    def __new__(cls, *args, **kwargs):
        raise RuntimeError("Tokens can only be created by ContextVars")

    @property
    def var(self):
        return self._var

    @property
    def old_value(self):
        return self._old

    def __repr__(self):
        used = " used" if self._used else ""
        return f"<Token{used} var={self._var!r} at {id(self):#x}>"

    # 3.14 (gh-129889): Token is a context manager; leaving the block
    # resets the variable to the value it held before ``set()``.
    def __enter__(self):
        return self

    def __exit__(self, *exc_info):
        self._var.reset(self)
        return None


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

    get = _native.get
    set = _native.set
    reset = _native.reset

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

    # The native `copy_context` builds contexts over this slot order.
    __slots__ = ("_data", "_entered", "_prev", "__weakref__")

    def __init_subclass__(cls, **kwargs):
        _no_subclassing(cls)

    def __init__(self, *args, **kwargs):
        if args or kwargs:
            raise TypeError("Context() does not accept any arguments")
        self._data = _native._new_mapping()
        self._entered = False
        self._prev = None

    run = _native.run
    copy = _native.copy

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
        # Snapshot: iteration stays valid if a nested `run` writes a new
        # mapping (CPython iterates an immutable HAMT).
        return iter(list(self._data))

    def __len__(self):
        return len(self._data)

    def keys(self):
        return list(self._data)

    def values(self):
        return list(self._data.values())

    def items(self):
        return list(self._data.items())


copy_context = _native.copy_context

_native.install(Context, ContextVar, Token, _MISSING)


# --- C-API bridge (RFC 0072 WS3) -------------------------------------------
#
# `PyContext_Enter`/`PyContext_Exit` (uvloop runs every callback under a
# copied context this way) are `Context.run` split in half: enter makes
# `ctx` current and remembers the previous context in its `_prev` slot,
# exit restores it.

_enter_context = _native._enter_context
_exit_context = _native._exit_context

# --- context watchers (PyContext_AddWatcher, 3.14) -------------------------
#
# `Py_CONTEXT_SWITCHED` fires after every enter and exit with the context
# that just became current (`None` when the stack unwinds to empty).

_set_switch_hook = _native._set_switch_hook
_clear_context_stack = _native._clear_context_stack


import _collections_abc  # noqa: E402  (3.14: Context is a virtual Mapping)
_collections_abc.Mapping.register(Context)
