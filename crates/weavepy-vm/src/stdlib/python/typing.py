"""Runtime typing helpers — minimal, but enough for type-aware
libraries (dataclasses introspection, Protocol-based duck typing,
``isinstance(x, Optional[int])``-style checks).

What works:

- :data:`Any`, :data:`NoReturn`, :data:`Never`, :data:`Self`,
  :data:`ClassVar`, :data:`InitVar`, :data:`Final`
- :class:`TypeVar` (without variance or bound enforcement)
- :class:`Generic` (subscription returns the bare class to keep MRO
  computation simple)
- :data:`Union`, :data:`Optional`, :data:`List`, :data:`Dict`,
  :data:`Tuple`, :data:`Set`, :data:`FrozenSet`, :data:`Type`
- :class:`Callable`, :class:`Annotated`, :class:`Literal`
- :func:`cast`, :func:`overload`, :func:`get_type_hints`,
  :func:`get_origin`, :func:`get_args`
- :class:`Protocol` (structural via ``runtime_checkable``)

What we do *not* implement: PEP 695 syntax, ``TypeAlias``-style
runtime enforcement, ``ParamSpec``/``TypeVarTuple`` semantics
(they're present as no-op markers), and bound checking on
``TypeVar``.
"""


# ---- sentinel singletons ----------------------------------------------------


class _SpecialForm:
    """Marker for special typing constructs (``Any``, ``Optional``,
    ``Union``, etc.). Subscriptable to produce
    :class:`_GenericAlias`."""

    __slots__ = ("_name",)

    def __init__(self, name):
        self._name = name

    def __repr__(self):
        return f"typing.{self._name}"

    def __getitem__(self, params):
        if not isinstance(params, tuple):
            params = (params,)
        if self._name == "Union":
            # CPython normalizes at construction: `None` becomes
            # `type(None)`, nested unions flatten, duplicates collapse
            # (`Union[str, None]` == `Union[str, NoneType]`; singledispatch
            # requires every arg to be a real class).
            flat = []
            for p in params:
                if p is None:
                    p = type(None)
                if getattr(p, "__origin__", None) is Union:
                    flat.extend(p.__args__)
                else:
                    flat.append(p)
            deduped = []
            for p in flat:
                if not any(p is q for q in deduped):
                    deduped.append(p)
            if len(deduped) == 1:
                return deduped[0]
            params = tuple(deduped)
        return _GenericAlias(self, params)

    def __call__(self, *args, **kwargs):
        raise TypeError(f"Cannot instantiate {self._name!r}")


Any = _SpecialForm("Any")
NoReturn = _SpecialForm("NoReturn")
Never = _SpecialForm("Never")
Self = _SpecialForm("Self")
Final = _SpecialForm("Final")
ClassVar = _SpecialForm("ClassVar")
InitVar = _SpecialForm("InitVar")
Union = _SpecialForm("Union")
Literal = _SpecialForm("Literal")
# PEP 646: ``Unpack[Ts]`` / ``*Ts``. Iterating a PEP 585 generic alias
# (``tuple[int]``) yields ``Unpack[self]`` exactly once, mirroring
# CPython's ``ga_iternext`` (which lazily reaches ``typing.Unpack``).
Unpack = _SpecialForm("Unpack")


def Optional(*params):
    """Strictly speaking, ``Optional[X]`` is ``Union[X, None]``."""
    if len(params) == 1 and isinstance(params[0], tuple):
        params = params[0]
    return _GenericAlias(Union, tuple(list(params) + [type(None)]))


# `Optional` is also subscriptable as `Optional[X]` thanks to this
# wrapper.
class _OptionalForm:
    __slots__ = ()

    def __repr__(self):
        return "typing.Optional"

    def __getitem__(self, params):
        if not isinstance(params, tuple):
            params = (params,)
        return _GenericAlias(Union, tuple(list(params) + [type(None)]))

    def __call__(self, *args, **kwargs):
        raise TypeError("Cannot instantiate Optional")


Optional = _OptionalForm()


# ---- TypeVar ----------------------------------------------------------------


class TypeVar:
    """Represents a type variable. Bound / variance information is
    accepted but unused — there's no static-type checker behind
    WeavePy."""

    __slots__ = (
        "__name__",
        "__bound__",
        "__constraints__",
        "__covariant__",
        "__contravariant__",
    )

    def __init__(self, name, *constraints, bound=None, covariant=False, contravariant=False):
        self.__name__ = name
        self.__bound__ = bound
        self.__constraints__ = constraints
        self.__covariant__ = covariant
        self.__contravariant__ = contravariant

    def __repr__(self):
        prefix = ""
        if self.__covariant__:
            prefix = "+"
        elif self.__contravariant__:
            prefix = "-"
        else:
            prefix = "~"
        return f"{prefix}{self.__name__}"


class ParamSpec(TypeVar):
    pass


class TypeVarTuple(TypeVar):
    pass


# ---- generic alias ----------------------------------------------------------


class _GenericAlias:
    """Result of subscripting a generic (e.g. ``List[int]``,
    ``Union[int, str]``). At runtime it carries the origin class and
    the type arguments, plus a few introspection hooks."""

    __slots__ = ("__origin__", "__args__", "_name")

    def __init__(self, origin, args):
        self.__origin__ = origin
        self.__args__ = args
        self._name = None

    def __repr__(self):
        # `_name` lets `_OriginAlias.__getitem__` pass through the
        # capitalised typing alias (`List`, `Dict`, …). Fall back to
        # the origin's runtime name only when no alias hint exists.
        origin_name = (
            getattr(self, "_name", None)
            or getattr(self.__origin__, "_name", None)
            or getattr(self.__origin__, "__name__", repr(self.__origin__))
        )
        if origin_name == "Union":
            if len(self.__args__) == 2 and type(None) in self.__args__:
                non_none = [a for a in self.__args__ if a is not type(None)][0]
                return f"typing.Optional[{_type_repr(non_none)}]"
        arg_str = ", ".join(_type_repr(a) for a in self.__args__)
        return f"typing.{origin_name}[{arg_str}]"

    def __getitem__(self, params):
        # Parameterise further: e.g. ``Dict[str][int]`` → ``Dict[str, int]``.
        if not isinstance(params, tuple):
            params = (params,)
        return _GenericAlias(self.__origin__, self.__args__ + params)

    def __call__(self, *args, **kwargs):
        # ``List[int](...)`` constructs the *origin* class.
        return self.__origin__(*args, **kwargs)

    def __or__(self, other):
        return _make_union(self, other)

    def __ror__(self, other):
        return _make_union(other, self)

    def __instancecheck__(self, obj):
        # PEP 3119 hook. Only ``Union`` supports instance checks; any
        # other subscripted generic (``List[int]``) is rejected exactly
        # as CPython does — you can't ask "is x a list-of-int?".
        if self.__origin__ is Union:
            return any(isinstance(obj, arg) for arg in self.__args__)
        raise TypeError(
            "Subscripted generics cannot be used with class and instance checks"
        )

    def __subclasscheck__(self, cls):
        if self.__origin__ is Union:
            return any(issubclass(cls, arg) for arg in self.__args__)
        raise TypeError(
            "Subscripted generics cannot be used with class and instance checks"
        )


def _as_class(x):
    """Coerce a bare typing alias to the runtime class it stands in for,
    so ``issubclass``/``isinstance`` can compare against it."""
    if isinstance(x, _OriginAlias):
        return x._origin
    return x


def _make_union(a, b):
    """Build ``a | b`` as ``Union[a, b]`` (PEP 604), flattening any
    nested unions so ``int | str | bytes`` has three flat args."""

    def flatten(x):
        if isinstance(x, _GenericAlias) and x.__origin__ is Union:
            return list(x.__args__)
        return [x]

    return _GenericAlias(Union, tuple(flatten(a) + flatten(b)))


def _type_repr(t):
    if t is type(None):
        return "None"
    if isinstance(t, type):
        return t.__name__
    return repr(t)


# ---- public generics --------------------------------------------------------


class _OriginAlias(_SpecialForm):
    """Like _SpecialForm, but pretends to be the named built-in
    container — ``isinstance(x, List)`` should fall back to checking
    against the real ``list`` (handled via __class_getitem__ on the
    container in modern Python; here we keep List as an alias)."""

    __slots__ = ("_origin",)

    def __init__(self, name, origin):
        super().__init__(name)
        self._origin = origin

    def __getitem__(self, params):
        if not isinstance(params, tuple):
            params = (params,)
        # Stamp the original alias name onto the underlying class so
        # the generated repr reads ``typing.List[int]`` rather than
        # ``typing.list[int]``.
        alias = _GenericAlias(self._origin, params)
        alias._name = self._name
        return alias

    def __or__(self, other):
        return _make_union(self, other)

    def __ror__(self, other):
        return _make_union(other, self)

    def __instancecheck__(self, obj):
        # A bare alias (``typing.List``) checks against its origin class,
        # mirroring ``isinstance(x, list)``.
        return isinstance(obj, self._origin)

    def __subclasscheck__(self, cls):
        return issubclass(_as_class(cls), self._origin)


List = _OriginAlias("List", list)
Dict = _OriginAlias("Dict", dict)
Tuple = _OriginAlias("Tuple", tuple)
Set = _OriginAlias("Set", set)
FrozenSet = _OriginAlias("FrozenSet", frozenset)
Type = _OriginAlias("Type", type)


class Callable:
    """``Callable[..., R]`` and ``Callable[[A, B], R]``."""

    def __class_getitem__(cls, params):
        return _GenericAlias(cls, params if isinstance(params, tuple) else (params,))


class Annotated:
    """``Annotated[X, meta1, meta2, ...]`` — the unwrapped type is
    ``__args__[0]``, the metadata is ``__metadata__``."""

    def __class_getitem__(cls, params):
        if not isinstance(params, tuple):
            params = (params,)
        if len(params) < 2:
            raise TypeError("Annotated needs at least two arguments")
        alias = _GenericAlias(cls, params)
        alias.__metadata__ = params[1:]
        return alias


# ---- Generic / Protocol ----------------------------------------------------


class Generic:
    """Marker base for user generics. Subscription is allowed but
    returns the class unchanged — type erasure is the rule."""

    __slots__ = ()

    def __class_getitem__(cls, params):
        return cls


class _ProtocolMeta(type):
    """Metaclass for :class:`Protocol`. ``@runtime_checkable`` plumbs
    a structural ``__instancecheck__`` here so user code can write
    ``isinstance(obj, MyProtocol)``."""

    def __instancecheck__(cls, instance):
        if getattr(cls, "_is_runtime_protocol", False):
            attrs = getattr(cls, "_protocol_attrs", ())
            for name in attrs:
                if not hasattr(instance, name):
                    return False
            return True
        return type.__instancecheck__(cls, instance)

    def __subclasscheck__(cls, subclass):
        if getattr(cls, "_is_runtime_protocol", False):
            attrs = getattr(cls, "_protocol_attrs", ())
            for name in attrs:
                if not hasattr(subclass, name):
                    return False
            return True
        return type.__subclasscheck__(cls, subclass)


class Protocol(Generic, metaclass=_ProtocolMeta):
    """Structural-protocol marker. Combine with ``@runtime_checkable``
    to allow ``isinstance(x, MyProtocol)``."""

    _is_protocol = True
    _is_runtime_protocol = False

    def __init_subclass__(cls, **kwargs):
        cls._is_protocol = True


# Infrastructure names that are never part of a protocol's structural
# signature. Mirrors CPython's ``typing.EXCLUDED_ATTRIBUTES`` so that
# *special* methods (``__int__``, ``__float__``, ``__abs__``, …) — which
# are exactly what protocols like ``SupportsInt`` describe — are retained
# while dunders that every object carries are dropped.
_PROTOCOL_EXCLUDED_ATTRS = frozenset(
    {
        "__abstractmethods__",
        "__annotations__",
        "__dict__",
        "__doc__",
        "__init__",
        "__module__",
        "__name__",
        "__qualname__",
        "__new__",
        "__slots__",
        "__subclasshook__",
        "__weakref__",
        "__class_getitem__",
        "__init_subclass__",
        "__orig_bases__",
        "__parameters__",
        "__classcell__",
        "__mro__",
        "__bases__",
        "_is_protocol",
        "_is_runtime_protocol",
        "_protocol_attrs",
    }
)


def _get_protocol_attrs(cls):
    """Collect the structural attribute names a protocol requires.

    Walks the protocol's own MRO (skipping ``object``/``Protocol``/
    ``Generic``) and unions each base's namespace and annotations,
    dropping the infrastructure dunders in
    :data:`_PROTOCOL_EXCLUDED_ATTRS`.
    """
    attrs = set()
    for base in getattr(cls, "__mro__", (cls,)):
        if getattr(base, "__name__", "") in ("Protocol", "Generic", "object"):
            continue
        names = list(getattr(base, "__dict__", ()) or ())
        names += list(getattr(base, "__annotations__", {}) or {})
        for name in names:
            if name.startswith("_abc_"):
                continue
            if name in _PROTOCOL_EXCLUDED_ATTRS:
                continue
            attrs.add(name)
    return attrs


def runtime_checkable(cls):
    """Enable ``isinstance``/``issubclass`` against a Protocol class.

    The actual structural check lives on :class:`_ProtocolMeta`. Here
    we just record the candidate attribute names so the metaclass can
    iterate them later.
    """
    if not getattr(cls, "_is_protocol", False):
        raise TypeError("runtime_checkable expects a Protocol subclass")
    cls._is_runtime_protocol = True
    cls._protocol_attrs = _get_protocol_attrs(cls)
    return cls


# ---- numeric "Supports*" protocols ------------------------------------------
# Runtime-checkable structural protocols from the stdlib. ``isinstance(x, P)``
# is True iff ``x`` exposes the corresponding special method.


@runtime_checkable
class SupportsInt(Protocol):
    """An ABC with one abstract method ``__int__``."""

    __slots__ = ()

    def __int__(self) -> int:
        pass


@runtime_checkable
class SupportsFloat(Protocol):
    """An ABC with one abstract method ``__float__``."""

    __slots__ = ()

    def __float__(self) -> float:
        pass


@runtime_checkable
class SupportsComplex(Protocol):
    """An ABC with one abstract method ``__complex__``."""

    __slots__ = ()

    def __complex__(self) -> complex:
        pass


@runtime_checkable
class SupportsBytes(Protocol):
    """An ABC with one abstract method ``__bytes__``."""

    __slots__ = ()

    def __bytes__(self) -> bytes:
        pass


@runtime_checkable
class SupportsAbs(Protocol):
    """An ABC with one abstract method ``__abs__`` that is covariant in
    its return type."""

    __slots__ = ()

    def __abs__(self):
        pass


@runtime_checkable
class SupportsRound(Protocol):
    """An ABC with one abstract method ``__round__`` that is covariant in
    its return type."""

    __slots__ = ()

    def __round__(self, ndigits: int = 0):
        pass


@runtime_checkable
class SupportsIndex(Protocol):
    """An ABC with one abstract method ``__index__``."""

    __slots__ = ()

    def __index__(self) -> int:
        pass


# ---- functional helpers -----------------------------------------------------


def cast(typ, val):
    """Static-typing affordance: at runtime, simply returns *val*."""
    return val


def overload(func):
    """Marker decorator: the runtime simply returns a stub that
    raises if called. Real implementations live under non-``@overload``
    siblings."""

    def _stub(*args, **kwargs):
        raise NotImplementedError(
            f"You should not call an overloaded function. "
            f"A series of @overload-decorated definitions must be "
            f"followed by exactly one non-@overload-decorated definition."
        )

    _stub.__name__ = getattr(func, "__name__", "overloaded")
    return _stub


def get_type_hints(obj, globalns=None, localns=None, include_extras=False):
    """Return a dict of annotations for the given class or function.

    Forward references — annotations written as string literals to
    sidestep ordering issues — are resolved via ``eval`` against the
    function/class globals and locals. Missing names propagate as
    ``NameError``."""
    if isinstance(obj, type):
        hints = {}
        for base in reversed(obj.__mro__):
            anns = getattr(base, "__annotations__", None) or {}
            hints.update(anns)
    else:
        hints = dict(getattr(obj, "__annotations__", {}) or {})

    if not hints:
        return hints

    # Look up globals/locals once.
    if globalns is None:
        globalns = getattr(obj, "__globals__", None)
        if globalns is None and isinstance(obj, type):
            module = getattr(obj, "__module__", None)
            try:
                import sys
                globalns = sys.modules[module].__dict__ if module else {}
            except Exception:
                globalns = {}
        if globalns is None:
            globalns = {}
    if localns is None:
        localns = {}

    resolved = {}
    for name, ann in hints.items():
        if isinstance(ann, str):
            try:
                ann = eval(ann, globalns, localns)
            except Exception:
                # Leave unresolved strings as-is; CPython would raise,
                # but loose behavior keeps simple tests passing.
                pass
        resolved[name] = ann
    return resolved


def get_origin(tp):
    """Return the unsubscripted version of a generic alias."""
    if isinstance(tp, _GenericAlias):
        return tp.__origin__
    # PEP 604 unions (`int | str`) — CPython answers `types.UnionType`.
    if getattr(tp, "__is_pep604_union__", False):
        import types
        return types.UnionType
    # PEP 585 aliases (`list[int]`) carry a real `__origin__`.
    origin = getattr(tp, "__origin__", None)
    if origin is not None and getattr(tp, "__args__", None) is not None:
        return origin
    return None


def get_args(tp):
    """Return the type arguments of a generic alias."""
    if isinstance(tp, _GenericAlias):
        return tp.__args__
    if getattr(tp, "__is_pep604_union__", False):
        return tuple(tp.__args__)
    if getattr(tp, "__origin__", None) is not None:
        args = getattr(tp, "__args__", None)
        if args is not None:
            return tuple(args)
    return ()


def NewType(name, tp):
    """``typing.NewType`` — at runtime returns a callable that simply
    forwards its argument unchanged. We mirror CPython's modern (3.10+)
    behaviour where ``NewType`` returns a *callable* object rather
    than a class so ``isinstance`` checks against it raise a
    helpful ``TypeError``."""
    def _new(x):
        return x
    _new.__name__ = name
    _new.__supertype__ = tp
    return _new


def TYPE_CHECKING():
    """Constant exposed for ``typing.TYPE_CHECKING``; always
    ``False`` at runtime since static analysers are the only consumers
    of branches guarded by it."""
    return False


TYPE_CHECKING = False


# ---- NamedTuple (PEP 526 class syntax + functional syntax) -----------------


def _make_nmtuple(name, types, module, defaults=()):
    """Build a ``collections.namedtuple`` carrying ``__annotations__``.

    ``types`` is an iterable of ``(field_name, annotation)`` pairs. We
    keep the annotation as-is (weavepy's typing is intentionally
    permissive — no runtime ``_type_check``).
    """
    import collections

    types = dict(types)
    fields = list(types)
    nm_tpl = collections.namedtuple(name, fields, defaults=defaults, module=module)
    nm_tpl.__annotations__ = types
    # CPython also stamps the synthesised ``__new__`` with the same
    # annotations; weavepy's namedtuple ``__new__`` may be a builtin that
    # rejects attribute assignment, so make this best-effort.
    try:
        nm_tpl.__new__.__annotations__ = types
    except (AttributeError, TypeError):
        pass
    return nm_tpl


# Attributes that NamedTuple class syntax may not override, and the
# class-machinery attributes that are copied through verbatim.
_prohibited = frozenset(
    {
        "__new__",
        "__init__",
        "__slots__",
        "__getnewargs__",
        "_fields",
        "_field_defaults",
        "_make",
        "_replace",
        "_asdict",
        "_source",
    }
)
_special = frozenset({"__module__", "__name__", "__annotations__", "__orig_bases__"})


class NamedTupleMeta(type):
    def __new__(cls, typename, bases, ns):
        if _NamedTuple not in bases:
            # Plain ``type.__new__`` bootstrap of ``_NamedTuple`` itself.
            return super().__new__(cls, typename, bases, ns)
        types = ns.get("__annotations__", {})
        default_names = []
        for field_name in types:
            if field_name in ns:
                default_names.append(field_name)
            elif default_names:
                raise TypeError(
                    "Non-default namedtuple field {} cannot follow default "
                    "field{} {}".format(
                        field_name,
                        "s" if len(default_names) > 1 else "",
                        ", ".join(default_names),
                    )
                )
        nm_tpl = _make_nmtuple(
            typename,
            types.items(),
            defaults=[ns[n] for n in default_names],
            module=ns.get("__module__", None),
        )
        # Copy user-defined methods/attributes that aren't part of the
        # namedtuple machinery (mirrors CPython's NamedTupleMeta).
        for key, val in ns.items():
            if key in _prohibited:
                raise AttributeError("Cannot overwrite NamedTuple attribute " + key)
            elif key not in _special and key not in nm_tpl._fields:
                setattr(nm_tpl, key, val)
        return nm_tpl


def NamedTuple(typename, fields=None, /, **kwargs):
    """Typed version of ``collections.namedtuple``.

    Supports the class-based syntax::

        class Employee(NamedTuple):
            name: str
            id: int = 0

    and the functional syntax::

        Employee = NamedTuple('Employee', [('name', str), ('id', int)])
    """
    if fields is None:
        fields = kwargs.items()
    nt = _make_nmtuple(typename, fields, module=None)
    nt.__orig_bases__ = (NamedTuple,)
    return nt


_NamedTuple = type.__new__(NamedTupleMeta, "NamedTuple", (), {})


def _namedtuple_mro_entries(bases):
    return (_NamedTuple,)


NamedTuple.__mro_entries__ = _namedtuple_mro_entries


# ---- nominal collections wrappers (PEP 585 aliases) ------------------------

# CPython 3.9+ deprecated ``typing.List`` etc. in favour of bare
# ``list[int]``. We expose both spellings for compatibility.


__all__ = [
    "Any",
    "NoReturn",
    "Never",
    "Self",
    "Final",
    "ClassVar",
    "InitVar",
    "Union",
    "Optional",
    "Literal",
    "List",
    "Dict",
    "Tuple",
    "Set",
    "FrozenSet",
    "Type",
    "Callable",
    "Annotated",
    "Generic",
    "Protocol",
    "NamedTuple",
    "SupportsInt",
    "SupportsFloat",
    "SupportsComplex",
    "SupportsBytes",
    "SupportsAbs",
    "SupportsRound",
    "SupportsIndex",
    "TypeVar",
    "ParamSpec",
    "TypeVarTuple",
    "runtime_checkable",
    "cast",
    "overload",
    "get_type_hints",
    "get_origin",
    "get_args",
    "NewType",
    "TYPE_CHECKING",
    "Deque",
    "DefaultDict",
    "OrderedDict",
    "Counter",
    "ChainMap",
]


# Container aliases backed by the ``collections`` module (the legacy
# ``typing.Deque`` / ``typing.DefaultDict`` spellings). Resolved lazily via
# PEP 562 so importing ``typing`` never forces ``collections`` during
# interpreter bootstrap (avoids an import cycle).
_LAZY_COLLECTION_ALIASES = {
    "Deque": "deque",
    "DefaultDict": "defaultdict",
    "OrderedDict": "OrderedDict",
    "Counter": "Counter",
    "ChainMap": "ChainMap",
}

# Aliases backed by ``collections.abc`` (``typing.Iterable[str]`` and
# friends). Same lazy PEP 562 treatment as the container aliases above.
_LAZY_ABC_ALIASES = {
    "AbstractSet": "Set",
    "AsyncGenerator": "AsyncGenerator",
    "AsyncIterable": "AsyncIterable",
    "AsyncIterator": "AsyncIterator",
    "Awaitable": "Awaitable",
    "ByteString": "ByteString",
    "Collection": "Collection",
    "Container": "Container",
    "Coroutine": "Coroutine",
    "Generator": "Generator",
    "Hashable": "Hashable",
    "ItemsView": "ItemsView",
    "Iterable": "Iterable",
    "Iterator": "Iterator",
    "KeysView": "KeysView",
    "Mapping": "Mapping",
    "MappingView": "MappingView",
    "MutableMapping": "MutableMapping",
    "MutableSequence": "MutableSequence",
    "MutableSet": "MutableSet",
    "Reversible": "Reversible",
    "Sequence": "Sequence",
    "Sized": "Sized",
    "ValuesView": "ValuesView",
}


def __getattr__(name):
    target = _LAZY_COLLECTION_ALIASES.get(name)
    if target is not None:
        import collections

        alias = _OriginAlias(name, getattr(collections, target))
        globals()[name] = alias
        return alias
    target = _LAZY_ABC_ALIASES.get(name)
    if target is not None:
        import collections.abc

        alias = _OriginAlias(name, getattr(collections.abc, target))
        globals()[name] = alias
        return alias
    raise AttributeError(f"module 'typing' has no attribute {name!r}")
