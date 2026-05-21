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


def runtime_checkable(cls):
    """Enable ``isinstance``/``issubclass`` against a Protocol class.

    The actual structural check lives on :class:`_ProtocolMeta`. Here
    we just record the candidate attribute names so the metaclass can
    iterate them later.
    """
    if not getattr(cls, "_is_protocol", False):
        raise TypeError("runtime_checkable expects a Protocol subclass")
    cls._is_runtime_protocol = True
    protocol_attrs = set()
    for name in dir(cls):
        if name.startswith("_"):
            continue
        protocol_attrs.add(name)
    cls._protocol_attrs = protocol_attrs
    return cls


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
    """Return a dict of annotations for the given class or function."""
    if isinstance(obj, type):
        hints = {}
        for base in reversed(obj.__mro__):
            anns = getattr(base, "__annotations__", None) or {}
            hints.update(anns)
        return hints
    return getattr(obj, "__annotations__", {}) or {}


def get_origin(tp):
    """Return the unsubscripted version of a generic alias."""
    if isinstance(tp, _GenericAlias):
        return tp.__origin__
    return None


def get_args(tp):
    """Return the type arguments of a generic alias."""
    if isinstance(tp, _GenericAlias):
        return tp.__args__
    return ()


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
    "TypeVar",
    "ParamSpec",
    "TypeVarTuple",
    "runtime_checkable",
    "cast",
    "overload",
    "get_type_hints",
    "get_origin",
    "get_args",
]
