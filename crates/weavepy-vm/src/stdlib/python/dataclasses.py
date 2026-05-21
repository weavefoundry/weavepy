"""Dataclasses, the WeavePy edition.

Implements the surface most code reaches for:

- ``@dataclass`` (with ``init``, ``repr``, ``eq``, ``order``, ``frozen``,
  ``slots``, ``kw_only``)
- ``field(default=..., default_factory=..., repr=..., compare=...,
  init=..., kw_only=...)``
- ``fields(cls_or_instance)``
- ``asdict(obj)`` / ``astuple(obj)``
- ``replace(obj, **changes)``
- ``make_dataclass(name, fields, ...)``
- ``is_dataclass(obj)``

Notable omissions (deferred):

- ``__set_name__`` integration with descriptor fields (we still
  honour ``__set_name__`` on user descriptors, just don't *route*
  field defaults through them).
- ``InitVar`` / ``ClassVar`` annotation introspection — both are
  recognised as marker objects and excluded from the generated
  ``__init__``, but the runtime doesn't enforce ``ClassVar`` access
  control beyond the dataclass machinery.
- ``__match_args__`` synthesis (the AST already supports structural
  patterns; we leave the field list to user code).
"""


MISSING = object()
_HAS_DEFAULT_FACTORY = object()


class Field:
    """Descriptor-friendly record carrying a single dataclass field's
    metadata. Created by :func:`field`. Mirrors CPython's name and
    attribute list closely so introspecting tools (`dataclasses.fields`
    plus user code) keep working."""

    __slots__ = (
        "name",
        "type",
        "default",
        "default_factory",
        "repr",
        "hash",
        "init",
        "compare",
        "metadata",
        "kw_only",
        "_field_type",
    )

    def __init__(
        self,
        default=MISSING,
        default_factory=MISSING,
        repr=True,
        hash=None,
        init=True,
        compare=True,
        metadata=None,
        kw_only=False,
    ):
        self.name = None
        self.type = None
        self.default = default
        self.default_factory = default_factory
        self.repr = repr
        self.hash = hash
        self.init = init
        self.compare = compare
        self.metadata = metadata or {}
        self.kw_only = kw_only
        self._field_type = "_FIELD"

    def __repr__(self):
        return (
            f"Field(name={self.name!r},type={self.type!r},"
            f"default={self.default!r},default_factory={self.default_factory!r},"
            f"repr={self.repr!r},compare={self.compare!r},init={self.init!r},"
            f"kw_only={self.kw_only!r})"
        )

    def __set_name__(self, owner, name):
        self.name = name


def field(
    *,
    default=MISSING,
    default_factory=MISSING,
    repr=True,
    hash=None,
    init=True,
    compare=True,
    metadata=None,
    kw_only=False,
):
    """Marker used inside a dataclass body to control a field's
    behaviour. Mirrors :func:`dataclasses.field` from CPython."""
    if default is not MISSING and default_factory is not MISSING:
        raise ValueError("cannot specify both default and default_factory")
    return Field(
        default=default,
        default_factory=default_factory,
        repr=repr,
        hash=hash,
        init=init,
        compare=compare,
        metadata=metadata,
        kw_only=kw_only,
    )


def _is_classvar(annotation):
    # We accept either the stringly-typed `typing.ClassVar` marker or
    # a runtime ClassVar instance (typing.py exposes the latter).
    if annotation is None:
        return False
    if isinstance(annotation, str):
        return annotation.startswith("ClassVar")
    name = getattr(annotation, "__name__", "")
    return name == "ClassVar"


def _is_initvar(annotation):
    if annotation is None:
        return False
    if isinstance(annotation, str):
        return annotation.startswith("InitVar")
    name = getattr(annotation, "__name__", "")
    return name == "InitVar"


def _collect_fields(cls):
    """Walk the MRO bottom-up and gather every declared field in
    declaration order, with subclass fields overriding base ones."""
    fields_seen = {}
    for base in reversed(cls.__mro__):
        annotations = getattr(base, "__annotations__", {}) or {}
        for name, annotation in annotations.items():
            if _is_classvar(annotation):
                continue
            init_only = _is_initvar(annotation)
            default = getattr(base, name, MISSING)
            if isinstance(default, Field):
                f = default
                f.name = name
                f.type = annotation
            else:
                f = Field(default=default)
                f.name = name
                f.type = annotation
            if init_only:
                f._field_type = "_FIELD_INITVAR"
            fields_seen[name] = f
    return list(fields_seen.values())


def _make_init(fields, frozen, kw_only_all):
    """Build the synthesised ``__init__`` as a closure over the
    field list — no source-string compilation, so it works in the
    WeavePy runtime which does not implement :func:`exec`."""

    # Split into positional and keyword-only fields, preserving order.
    init_fields = [f for f in fields if f.init]
    pos_fields = [f for f in init_fields if not (kw_only_all or f.kw_only)]
    kw_fields = [f for f in init_fields if (kw_only_all or f.kw_only)]
    non_init_fields = [f for f in fields if not f.init]

    def __init__(self, *args, **kwargs):
        # Bind positional args first.
        if len(args) > len(pos_fields):
            raise TypeError(
                f"__init__() takes {len(pos_fields) + 1} positional arguments "
                f"but {len(args) + 1} were given"
            )
        provided = {}
        for f, value in zip(pos_fields, args):
            provided[f.name] = value
        # Then kwargs (allowed for both positional fields and kw-only).
        for key, value in kwargs.items():
            if key in provided:
                raise TypeError(
                    f"__init__() got multiple values for argument {key!r}"
                )
            provided[key] = value
        # Fill in defaults for missing fields and validate required.
        for f in init_fields:
            if f.name in provided:
                continue
            if f.default is not MISSING:
                provided[f.name] = f.default
            elif f.default_factory is not MISSING:
                provided[f.name] = f.default_factory()
            else:
                raise TypeError(
                    f"__init__() missing required argument: {f.name!r}"
                )
        # Apply attribute writes; honour `frozen` by bypassing the
        # class's __setattr__ via `object.__setattr__`.
        for f in init_fields:
            if f._field_type == "_FIELD_INITVAR":
                continue
            value = provided[f.name]
            if frozen:
                object.__setattr__(self, f.name, value)
            else:
                setattr(self, f.name, value)
        # Non-init fields with defaults/factories.
        for f in non_init_fields:
            if f.default is not MISSING:
                value = f.default
            elif f.default_factory is not MISSING:
                value = f.default_factory()
            else:
                continue
            if frozen:
                object.__setattr__(self, f.name, value)
            else:
                setattr(self, f.name, value)
        # Post-init hook.
        post = getattr(self, "__post_init__", None)
        if post is not None:
            post()

    return __init__


def _make_repr(fields, cls_name):
    repr_fields = [f for f in fields if f.repr]

    def __repr__(self):
        parts = [f"{f.name}={getattr(self, f.name)!r}" for f in repr_fields]
        return f"{cls_name}({', '.join(parts)})"

    return __repr__


def _make_eq(fields):
    cmp_fields = [f for f in fields if f.compare]

    def __eq__(self, other):
        if type(self) is not type(other):
            return NotImplemented
        for f in cmp_fields:
            if getattr(self, f.name) != getattr(other, f.name):
                return False
        return True

    return __eq__


def _make_order(fields, op_name, op):
    cmp_fields = [f for f in fields if f.compare]

    def __cmp__(self, other):
        if type(self) is not type(other):
            return NotImplemented
        self_tuple = tuple(getattr(self, f.name) for f in cmp_fields)
        other_tuple = tuple(getattr(other, f.name) for f in cmp_fields)
        return op(self_tuple, other_tuple)

    __cmp__.__name__ = op_name
    return __cmp__


def _make_hash(fields):
    cmp_fields = [f for f in fields if f.compare]

    def __hash__(self):
        return hash(tuple(getattr(self, f.name) for f in cmp_fields))

    return __hash__


def _process_class(cls, init, repr, eq, order, frozen, slots, kw_only):
    fields = _collect_fields(cls)
    setattr(cls, "__dataclass_fields__", {f.name: f for f in fields})
    setattr(cls, "__dataclass_params__", _DataclassParams(init, repr, eq, order, frozen))

    if init and "__init__" not in cls.__dict__:
        cls.__init__ = _make_init(fields, frozen=frozen, kw_only_all=kw_only)

    if repr and "__repr__" not in cls.__dict__:
        cls.__repr__ = _make_repr(fields, cls.__name__)

    if eq and "__eq__" not in cls.__dict__:
        cls.__eq__ = _make_eq(fields)
        if "__hash__" not in cls.__dict__:
            if frozen:
                cls.__hash__ = _make_hash(fields)
            else:
                cls.__hash__ = None

    if order:
        ops = [
            ("__lt__", lambda a, b: a < b),
            ("__le__", lambda a, b: a <= b),
            ("__gt__", lambda a, b: a > b),
            ("__ge__", lambda a, b: a >= b),
        ]
        for op_name, op in ops:
            if op_name not in cls.__dict__:
                setattr(cls, op_name, _make_order(fields, op_name, op))

    if frozen and "__setattr__" not in cls.__dict__:
        def _frozen_setattr(self, key, value):
            raise FrozenInstanceError(f"cannot assign to field {key!r}")

        def _frozen_delattr(self, key):
            raise FrozenInstanceError(f"cannot delete field {key!r}")

        cls.__setattr__ = _frozen_setattr
        cls.__delattr__ = _frozen_delattr

    if slots:
        cls.__slots__ = tuple(f.name for f in fields)

    return cls


class _DataclassParams:
    __slots__ = ("init", "repr", "eq", "order", "frozen")

    def __init__(self, init, repr, eq, order, frozen):
        self.init = init
        self.repr = repr
        self.eq = eq
        self.order = order
        self.frozen = frozen


class FrozenInstanceError(AttributeError):
    pass


def dataclass(
    cls=None,
    /,
    *,
    init=True,
    repr=True,
    eq=True,
    order=False,
    unsafe_hash=False,
    frozen=False,
    match_args=True,
    kw_only=False,
    slots=False,
    weakref_slot=False,
):
    """The ``@dataclass`` class decorator. Accepts the same keyword
    arguments as CPython's dataclass; ``match_args`` and
    ``weakref_slot`` are accepted but ignored (no behaviour
    difference in the current runtime)."""
    _ = unsafe_hash, match_args, weakref_slot

    def wrap(c):
        return _process_class(c, init, repr, eq, order, frozen, slots, kw_only)

    if cls is None:
        return wrap
    return wrap(cls)


def fields(class_or_instance):
    """Return a tuple of the dataclass fields for the given class or
    instance, in declaration order."""
    try:
        flds = class_or_instance.__dataclass_fields__
    except AttributeError:
        raise TypeError("fields() argument must be a dataclass or instance")
    return tuple(flds.values())


def is_dataclass(obj):
    """``True`` if *obj* is a dataclass *or* a dataclass instance."""
    return hasattr(obj, "__dataclass_fields__")


def asdict(obj, *, dict_factory=dict):
    """Recursively convert a dataclass instance to a dict, mirroring
    each dataclass field's value."""
    if not is_dataclass(obj) or isinstance(obj, type):
        raise TypeError("asdict() expects a dataclass instance")
    return _asdict_inner(obj, dict_factory)


def _asdict_inner(obj, dict_factory):
    if is_dataclass(obj) and not isinstance(obj, type):
        result = []
        for f in fields(obj):
            value = _asdict_inner(getattr(obj, f.name), dict_factory)
            result.append((f.name, value))
        return dict_factory(result)
    if isinstance(obj, (list, tuple)):
        kind = type(obj)
        return kind(_asdict_inner(v, dict_factory) for v in obj)
    if isinstance(obj, dict):
        return type(obj)(
            (_asdict_inner(k, dict_factory), _asdict_inner(v, dict_factory))
            for k, v in obj.items()
        )
    return obj


def astuple(obj, *, tuple_factory=tuple):
    """Recursively convert a dataclass instance to a tuple."""
    if not is_dataclass(obj) or isinstance(obj, type):
        raise TypeError("astuple() expects a dataclass instance")
    return _astuple_inner(obj, tuple_factory)


def _astuple_inner(obj, tuple_factory):
    if is_dataclass(obj) and not isinstance(obj, type):
        return tuple_factory(
            _astuple_inner(getattr(obj, f.name), tuple_factory) for f in fields(obj)
        )
    if isinstance(obj, (list, tuple)):
        kind = type(obj)
        return kind(_astuple_inner(v, tuple_factory) for v in obj)
    if isinstance(obj, dict):
        return type(obj)(
            (_astuple_inner(k, tuple_factory), _astuple_inner(v, tuple_factory))
            for k, v in obj.items()
        )
    return obj


def replace(obj, /, **changes):
    """Return a new dataclass instance with `changes` applied, all
    other fields copied from `obj`."""
    if not is_dataclass(obj) or isinstance(obj, type):
        raise TypeError("replace() expects a dataclass instance")
    kwargs = {}
    for f in fields(obj):
        if not f.init:
            if f.name in changes:
                raise ValueError(
                    f"cannot replace non-init field {f.name!r}"
                )
            continue
        if f.name in changes:
            kwargs[f.name] = changes[f.name]
        else:
            kwargs[f.name] = getattr(obj, f.name)
    return type(obj)(**kwargs)


def make_dataclass(cls_name, fields_spec, *, bases=(), namespace=None, **kwargs):
    """Dynamically create a dataclass.

    Each entry in ``fields_spec`` is either ``name``, ``(name, type)``,
    or ``(name, type, field_descriptor)`` — matching CPython.
    """
    ns = dict(namespace or {})
    annotations = ns.setdefault("__annotations__", {})
    for entry in fields_spec:
        if isinstance(entry, str):
            name, type_, default = entry, "typing.Any", MISSING
        elif len(entry) == 2:
            name, type_ = entry
            default = MISSING
        else:
            name, type_, default = entry
        annotations[name] = type_
        if default is not MISSING:
            ns[name] = default
    new_cls = type(cls_name, bases, ns)
    return dataclass(new_cls, **kwargs)


__all__ = [
    "dataclass",
    "field",
    "Field",
    "FrozenInstanceError",
    "MISSING",
    "fields",
    "is_dataclass",
    "asdict",
    "astuple",
    "replace",
    "make_dataclass",
]
