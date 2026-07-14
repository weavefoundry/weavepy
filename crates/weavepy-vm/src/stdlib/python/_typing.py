"""CPython's ``_typing`` accelerator surface, re-implemented in Python.

CPython 3.13's ``typing.py`` imports its type-parameter objects
(``TypeVar``, ``ParamSpec``, ``TypeVarTuple``, ``ParamSpecArgs``,
``ParamSpecKwargs``, ``TypeAliasType``, ``Generic``, ``NoDefault``)
from the C module ``_typing`` (``Modules/_typingmodule.c`` over
``Objects/typevarobject.c``). The C implementation exists purely for
speed — the semantics are fully observable from Python and are graded
by ``test_typing`` / ``test_type_params`` / ``test_type_aliases``.

This module is a faithful Python port of ``Objects/typevarobject.c``
(v3.13). Where the C code delegates back into ``typing.py`` helpers
(``_type_check``, ``_make_union``, ``_typevar_subst``, …) we lazily
import ``typing`` exactly like ``call_typing_func_object`` does.

WeavePy's PEP 695 compiler lowering constructs these objects through
the ``__weavepy_typevar__`` / ``__weavepy_paramspec__`` /
``__weavepy_typevartuple__`` / ``__weavepy_type_alias__`` VM
intrinsics, mirroring CPython's ``CALL_INTRINSIC_*`` opcodes; the
``_weavepy_*`` constructors at the bottom of this file are their
entry points (they correspond to ``_Py_make_typevar``,
``_Py_make_paramspec``, ``_Py_make_typevartuple``,
``_Py_make_typealias`` and ``_Py_set_typeparam_default``).
"""

import sys

__all__ = [
    "_idfunc",
    "TypeVar",
    "ParamSpec",
    "TypeVarTuple",
    "ParamSpecArgs",
    "ParamSpecKwargs",
    "TypeAliasType",
    "Generic",
    "NoDefault",
]


def _idfunc(*args):
    # The C `_idfunc` is METH_O (identity). typing.py assigns it as
    # `NewType.__call__`; a PyCFunction is not a descriptor so CPython
    # calls it with one argument, while this Python function gets bound
    # and receives (self, x). Return the last argument to serve both.
    return args[-1]


class _ImmutableTypeMeta(type):
    """CPython's NoDefaultType is an immutable C static type: assigning
    or deleting *class* attributes raises TypeError (test_no_attributes
    checks `type(NoDefault).foo = 3`)."""

    def __setattr__(cls, name, value):
        raise TypeError(
            f"cannot set {name!r} attribute of immutable type {cls.__name__!r}"
        )

    def __delattr__(cls, name):
        raise TypeError(
            f"cannot delete {name!r} attribute of immutable type {cls.__name__!r}"
        )


_no_default_singleton = None


class NoDefaultType(metaclass=_ImmutableTypeMeta):
    """The type of the NoDefault singleton."""

    def __new__(cls, *args, **kwargs):
        if args or kwargs:
            raise TypeError("NoDefaultType takes no arguments")
        global _no_default_singleton
        if _no_default_singleton is None:
            _no_default_singleton = super().__new__(cls)
        return _no_default_singleton

    def __repr__(self):
        return "typing.NoDefault"

    def __reduce__(self):
        return "NoDefault"

    def __init_subclass__(cls, /, *args, **kwargs):
        raise TypeError("type 'NoDefaultType' is not an acceptable base type")

    # The C singleton has no `__dict__`; instance attribute writes raise
    # AttributeError (`NoDefault.foo = 3`, test_no_attributes).
    def __setattr__(self, name, value):
        raise AttributeError(f"'NoDefaultType' object has no attribute {name!r}")

    def __delattr__(self, name):
        raise AttributeError(f"'NoDefaultType' object has no attribute {name!r}")


NoDefault = NoDefaultType()


def _caller(depth=2):
    """The `__name__` of the module whose frame is `depth` levels up.

    Mirrors `caller()` in typevarobject.c (the module of the calling
    function), used to stamp `__module__` on manually-constructed type
    parameters. Like the C `_PyEval_GetFrameModuleName`, a namespace
    without `__name__` (e.g. a plain-dict `exec`) yields None — and the
    instance records it (`T.__module__ is None`, test_basic_with_exec).
    """
    try:
        return sys._getframe(depth).f_globals.get("__name__", None)
    except (AttributeError, ValueError):
        return None


def _call_typing_func(name, /, *args):
    # C `call_typing_func_object`: lazy `import typing` then call the
    # named helper.
    import typing

    return getattr(typing, name)(*args)


def _type_check(arg, msg):
    # C `type_check`: None short-circuits to type(None) to avoid
    # bootstrapping problems, everything else goes through
    # typing._type_check.
    if arg is None:
        return type(None)
    return _call_typing_func("_type_check", arg, msg)


def _make_union(self, other):
    # C `make_union` — the nb_or slot for TypeVar/ParamSpec. Produces a
    # typing.Union (not types.UnionType) to preserve string-forward-ref
    # support.
    return _call_typing_func("_make_union", self, other)


class TypeVar:
    """Type variable.

    The preferred way to construct a type variable is via the
    dedicated syntax for generic functions, classes, and type aliases,
    e.g. ``class Sequence[T]: ...``. See PEP 484, PEP 695 and PEP 696.
    """

    __module__ = "typing"

    def __new__(
        cls,
        name,
        *constraints,
        bound=None,
        default=NoDefault,
        covariant=False,
        contravariant=False,
        infer_variance=False,
    ):
        if cls is not TypeVar:
            raise TypeError("type 'typing.TypeVar' is not an acceptable base type")
        if not isinstance(name, str):
            raise TypeError(
                f"TypeVar() argument 'name' must be str, not {type(name).__name__}"
            )
        covariant = bool(covariant)
        contravariant = bool(contravariant)
        infer_variance = bool(infer_variance)
        if covariant and contravariant:
            raise ValueError("Bivariant types are not supported.")
        if infer_variance and (covariant or contravariant):
            raise ValueError("Variance cannot be specified with infer_variance.")
        if bound is not None:
            bound = _type_check(bound, "Bound must be a type.")
        n_constraints = len(constraints)
        if n_constraints == 1:
            raise TypeError("A single constraint is not allowed")
        elif n_constraints == 0:
            constraints = None
        elif bound is not None:
            raise TypeError("Constraints cannot be combined with bound=...")
        self = object.__new__(cls)
        self._name = name
        self._bound = bound
        self._evaluate_bound = None
        self._constraints = tuple(constraints) if constraints else None
        self._evaluate_constraints = None
        self._default_value = default
        self._evaluate_default = None
        self._covariant = covariant
        self._contravariant = contravariant
        self._infer_variance = infer_variance
        # Stored even when None (plain-dict exec): CPython's C
        # constructor records the NULL module and `__module__` reads
        # back as None rather than the class default.
        self.__module__ = _caller()
        return self

    # `typevar_alloc` for the PEP 695 lowering (`_Py_make_typevar`):
    # lazy bound/constraints, infer_variance=True, no module stamp.
    @classmethod
    def _weavepy_make(cls, name, evaluate_bound=None, evaluate_constraints=None):
        self = object.__new__(cls)
        self._name = name
        self._bound = None
        self._evaluate_bound = evaluate_bound
        self._constraints = None
        self._evaluate_constraints = evaluate_constraints
        self._default_value = NoDefault
        self._evaluate_default = None
        self._covariant = False
        self._contravariant = False
        self._infer_variance = True
        return self

    def __init_subclass__(cls, /, *args, **kwargs):
        raise TypeError("type 'typing.TypeVar' is not an acceptable base type")

    @property
    def __name__(self):
        return self._name

    @property
    def __covariant__(self):
        return self._covariant

    @property
    def __contravariant__(self):
        return self._contravariant

    @property
    def __infer_variance__(self):
        return self._infer_variance

    @property
    def __bound__(self):
        if self._bound is not None:
            return self._bound
        if self._evaluate_bound is None:
            return None
        self._bound = self._evaluate_bound()
        return self._bound

    @property
    def __constraints__(self):
        if self._constraints is not None:
            return self._constraints
        if self._evaluate_constraints is None:
            return ()
        self._constraints = tuple(self._evaluate_constraints())
        return self._constraints

    @property
    def __default__(self):
        if self._evaluate_default is not None and self._default_value is NoDefault:
            self._default_value = self._evaluate_default()
        return self._default_value

    def has_default(self):
        if self._evaluate_default is not None:
            return True
        return self._default_value is not NoDefault

    def __repr__(self):
        if self._infer_variance:
            return self._name
        variance = "+" if self._covariant else "-" if self._contravariant else "~"
        return variance + self._name

    def __typing_subst__(self, arg, /):
        return _call_typing_func("_typevar_subst", self, arg)

    def __typing_prepare_subst__(self, alias, args, /):
        # Ported from `typevar_typing_prepare_subst_impl`.
        params = alias.__parameters__
        i = params.index(self)
        args_len = len(args)
        if i < args_len:
            return args
        elif i == args_len:
            dflt = self.__default__
            if dflt is not NoDefault:
                return tuple(args) + (dflt,)
        raise TypeError(
            f"Too few arguments for {alias}; actual {args_len}, expected at least {i + 1}"
        )

    def __reduce__(self):
        return self._name

    def __mro_entries__(self, bases):
        raise TypeError("Cannot subclass an instance of TypeVar")

    def __or__(self, right):
        return _make_union(self, right)

    def __ror__(self, left):
        return _make_union(left, self)


class ParamSpecArgs:
    """The args for a ParamSpec object.

    Given a ParamSpec object P, P.args is an instance of ParamSpecArgs.
    """

    __module__ = "typing"

    def __new__(cls, origin):
        if cls is not ParamSpecArgs:
            raise TypeError(
                "type 'typing.ParamSpecArgs' is not an acceptable base type"
            )
        self = object.__new__(cls)
        self._origin = origin
        return self

    def __init_subclass__(cls, /, *args, **kwargs):
        raise TypeError("type 'typing.ParamSpecArgs' is not an acceptable base type")

    @property
    def __origin__(self):
        return self._origin

    def __repr__(self):
        if isinstance(self._origin, ParamSpec):
            return f"{self._origin._name}.args"
        return f"{self._origin!r}.args"

    def __eq__(self, other):
        if not isinstance(other, ParamSpecArgs):
            return NotImplemented
        return self._origin == other._origin

    def __mro_entries__(self, bases):
        raise TypeError("Cannot subclass an instance of ParamSpecArgs")

    __hash__ = None


class ParamSpecKwargs:
    """The kwargs for a ParamSpec object.

    Given a ParamSpec object P, P.kwargs is an instance of ParamSpecKwargs.
    """

    __module__ = "typing"

    def __new__(cls, origin):
        if cls is not ParamSpecKwargs:
            raise TypeError(
                "type 'typing.ParamSpecKwargs' is not an acceptable base type"
            )
        self = object.__new__(cls)
        self._origin = origin
        return self

    def __init_subclass__(cls, /, *args, **kwargs):
        raise TypeError("type 'typing.ParamSpecKwargs' is not an acceptable base type")

    @property
    def __origin__(self):
        return self._origin

    def __repr__(self):
        if isinstance(self._origin, ParamSpec):
            return f"{self._origin._name}.kwargs"
        return f"{self._origin!r}.kwargs"

    def __eq__(self, other):
        if not isinstance(other, ParamSpecKwargs):
            return NotImplemented
        return self._origin == other._origin

    def __mro_entries__(self, bases):
        raise TypeError("Cannot subclass an instance of ParamSpecKwargs")

    __hash__ = None


class ParamSpec:
    """Parameter specification variable.

    The preferred way to construct a parameter specification is via
    the dedicated syntax for generic functions, classes, and type
    aliases, where the use of '**' creates a parameter specification,
    e.g. ``type IntFunc[**P] = Callable[P, int]``. See PEP 612.
    """

    __module__ = "typing"

    def __new__(
        cls,
        name,
        *,
        bound=None,
        default=NoDefault,
        covariant=False,
        contravariant=False,
        infer_variance=False,
    ):
        if cls is not ParamSpec:
            raise TypeError("type 'typing.ParamSpec' is not an acceptable base type")
        if not isinstance(name, str):
            raise TypeError(
                f"ParamSpec() argument 'name' must be str, not {type(name).__name__}"
            )
        covariant = bool(covariant)
        contravariant = bool(contravariant)
        infer_variance = bool(infer_variance)
        if covariant and contravariant:
            raise ValueError("Bivariant types are not supported.")
        if infer_variance and (covariant or contravariant):
            raise ValueError("Variance cannot be specified with infer_variance.")
        if bound is not None:
            bound = _type_check(bound, "Bound must be a type.")
        self = object.__new__(cls)
        self._name = name
        self._bound = bound
        self._default_value = default
        self._evaluate_default = None
        self._covariant = covariant
        self._contravariant = contravariant
        self._infer_variance = infer_variance
        # Stored even when None (plain-dict exec): CPython's C
        # constructor records the NULL module and `__module__` reads
        # back as None rather than the class default.
        self.__module__ = _caller()
        return self

    # `paramspec_alloc` for the PEP 695 lowering (`_Py_make_paramspec`).
    @classmethod
    def _weavepy_make(cls, name):
        self = object.__new__(cls)
        self._name = name
        self._bound = None
        self._default_value = NoDefault
        self._evaluate_default = None
        self._covariant = False
        self._contravariant = False
        self._infer_variance = True
        return self

    def __init_subclass__(cls, /, *args, **kwargs):
        raise TypeError("type 'typing.ParamSpec' is not an acceptable base type")

    @property
    def __name__(self):
        return self._name

    @property
    def __bound__(self):
        return self._bound

    @property
    def __covariant__(self):
        return self._covariant

    @property
    def __contravariant__(self):
        return self._contravariant

    @property
    def __infer_variance__(self):
        return self._infer_variance

    @property
    def args(self):
        """Represents positional arguments."""
        return ParamSpecArgs(self)

    @property
    def kwargs(self):
        """Represents keyword arguments."""
        return ParamSpecKwargs(self)

    @property
    def __default__(self):
        if self._evaluate_default is not None and self._default_value is NoDefault:
            self._default_value = self._evaluate_default()
        return self._default_value

    def has_default(self):
        if self._evaluate_default is not None:
            return True
        return self._default_value is not NoDefault

    def __repr__(self):
        if self._infer_variance:
            return self._name
        variance = "+" if self._covariant else "-" if self._contravariant else "~"
        return variance + self._name

    def __typing_subst__(self, arg, /):
        return _call_typing_func("_paramspec_subst", self, arg)

    def __typing_prepare_subst__(self, alias, args, /):
        return _call_typing_func("_paramspec_prepare_subst", self, alias, args)

    def __reduce__(self):
        return self._name

    def __mro_entries__(self, bases):
        raise TypeError("Cannot subclass an instance of ParamSpec")

    def __or__(self, right):
        return _make_union(self, right)

    def __ror__(self, left):
        return _make_union(left, self)


class TypeVarTuple:
    """Type variable tuple. A specialized form of type variable that
    enables variadic generics.

    The preferred way to construct a type variable tuple is via the
    dedicated syntax for generic functions, classes, and type aliases,
    where a single '*' indicates a type variable tuple. See PEP 646.
    """

    __module__ = "typing"

    def __new__(cls, name, *, default=NoDefault):
        if cls is not TypeVarTuple:
            raise TypeError(
                "type 'typing.TypeVarTuple' is not an acceptable base type"
            )
        if not isinstance(name, str):
            raise TypeError(
                f"TypeVarTuple() argument 'name' must be str, not {type(name).__name__}"
            )
        self = object.__new__(cls)
        self._name = name
        self._default_value = default
        self._evaluate_default = None
        # Stored even when None (plain-dict exec): CPython's C
        # constructor records the NULL module and `__module__` reads
        # back as None rather than the class default.
        self.__module__ = _caller()
        return self

    # `typevartuple_alloc` for the PEP 695 lowering
    # (`_Py_make_typevartuple`).
    @classmethod
    def _weavepy_make(cls, name):
        self = object.__new__(cls)
        self._name = name
        self._default_value = NoDefault
        self._evaluate_default = None
        return self

    def __init_subclass__(cls, /, *args, **kwargs):
        raise TypeError("type 'typing.TypeVarTuple' is not an acceptable base type")

    @property
    def __name__(self):
        return self._name

    @property
    def __default__(self):
        if self._evaluate_default is not None and self._default_value is NoDefault:
            self._default_value = self._evaluate_default()
        return self._default_value

    def has_default(self):
        if self._evaluate_default is not None:
            return True
        return self._default_value is not NoDefault

    def __repr__(self):
        return self._name

    def __iter__(self):
        # `typevartuple_iter`: yields the single element `Unpack[self]`.
        import typing

        yield typing.Unpack[self]

    def __typing_subst__(self, arg, /):
        raise TypeError("Substitution of bare TypeVarTuple is not supported")

    def __typing_prepare_subst__(self, alias, args, /):
        return _call_typing_func("_typevartuple_prepare_subst", self, alias, args)

    def __reduce__(self):
        return self._name

    def __mro_entries__(self, bases):
        raise TypeError("Cannot subclass an instance of TypeVarTuple")


def _unpack_typevartuples(params):
    # `unpack_typevartuples`: TypeVarTuple entries must be unpacked
    # when exposed through `__parameters__`.
    if any(isinstance(p, TypeVarTuple) for p in params):
        import typing

        return tuple(
            typing.Unpack[p] if isinstance(p, TypeVarTuple) else p for p in params
        )
    return params


def _ga_unpacked_tuple_args(x):
    # `__typing_unpacked_tuple_args__` for an argument that may be a
    # typing-level object (which exposes the attribute itself) or a
    # WeavePy native alias namespace (which only carries `__unpacked__`).
    if isinstance(x, type):
        return None
    subargs = getattr(x, "__typing_unpacked_tuple_args__", None)
    if subargs is not None:
        return subargs
    if getattr(x, "__unpacked__", False) and getattr(x, "__origin__", None) is tuple:
        return x.__args__
    return None


def _ga_unpack_args(args):
    # C `unpack_args`: splice fixed-length unpacked tuples
    # (`*tuple[int, str]`) into the argument list; keep variadic ones
    # (`*tuple[int, ...]`) whole.
    newargs = []
    for arg in args:
        subargs = _ga_unpacked_tuple_args(arg)
        if subargs is not None and not (subargs and subargs[-1] is ...):
            newargs.extend(subargs)
        else:
            newargs.append(arg)
    return newargs


def _ga_is_unpacked_typevartuple(x):
    return (not isinstance(x, type)) and getattr(
        x, "__typing_is_unpacked_typevartuple__", False
    )


def _ga_make_substitution(args, new_arg_by_param, is_callable_origin):
    # Mirror of `typing._GenericAlias._make_substitution` (kept in sync
    # with the C `subs_tvars`), operating on plain `__args__` tuples so
    # it works for WeavePy's namespace-shaped native aliases.
    new_args = []
    for old_arg in args:
        if isinstance(old_arg, type):
            new_args.append(old_arg)
            continue

        substfunc = getattr(old_arg, "__typing_subst__", None)
        if substfunc:
            new_arg = substfunc(new_arg_by_param[old_arg])
        else:
            subparams = getattr(old_arg, "__parameters__", ())
            if not subparams:
                new_arg = old_arg
            else:
                subargs = []
                for x in subparams:
                    if isinstance(x, TypeVarTuple):
                        subargs.extend(new_arg_by_param[x])
                    else:
                        subargs.append(new_arg_by_param[x])
                new_arg = old_arg[tuple(subargs)]

        if is_callable_origin and isinstance(new_arg, tuple):
            # Flatten `Callable[P, T][[int, str], float]` to
            # `(int, str, float)` — see typing._GenericAlias.
            new_args.extend(new_arg)
        elif _ga_is_unpacked_typevartuple(old_arg):
            # `A[T, *Ts][float, int, str]`: the `*Ts` replacement is a
            # tuple `(int, str)` that must be spliced in flat. GH-138497:
            # a rogue `__typing_subst__` returning a non-tuple must raise
            # the canonical TypeError, not fail obscurely on extend().
            if not isinstance(new_arg, tuple):
                raise TypeError(
                    f"expected __typing_subst__ of "
                    f"{type(old_arg).__name__} objects to return a tuple, "
                    f"not {type(new_arg).__name__}"
                )
            new_args.extend(new_arg)
        elif isinstance(old_arg, tuple):
            # `Base[[int, T]]` — substitute inside the parameter list.
            new_args.append(
                tuple(_ga_make_substitution(old_arg, new_arg_by_param, is_callable_origin))
            )
        else:
            new_args.append(new_arg)
    return new_args


def _weavepy_ga_subs_parameters(alias, item):
    """CPython's `_Py_subs_parameters` for native generic aliases.

    Called by the VM when `alias[item]` is evaluated on a
    namespace-shaped `types.GenericAlias` whose `__parameters__`
    contain typing-level type variables (TypeVar / ParamSpec /
    TypeVarTuple), which carry `__typing_prepare_subst__` /
    `__typing_subst__` hooks the Rust fast path cannot honour.
    Returns the new `__args__` tuple.
    """
    parameters = alias.__parameters__
    if not parameters:
        raise TypeError(f"{alias!r} is not a generic class")
    if not isinstance(item, tuple):
        item = (item,)
    item = tuple(_ga_unpack_args(item))
    for param in parameters:
        prepare = getattr(param, "__typing_prepare_subst__", None)
        if prepare is not None:
            item = prepare(alias, item)
    # C `_Py_subs_parameters`: a non-tuple prepare result is treated as a
    # single argument, not iterated (GH-138497's evil hooks return None).
    # typing's pure-Python `_paramspec_prepare_subst` may hand back a
    # list, which keeps its element count.
    if isinstance(item, list):
        item = tuple(item)
    elif not isinstance(item, tuple):
        item = (item,)
    alen = len(item)
    plen = len(parameters)
    if alen != plen:
        raise TypeError(
            f"Too {'many' if alen > plen else 'few'} arguments for {alias!r};"
            f" actual {alen}, expected {plen}"
        )
    new_arg_by_param = dict(zip(parameters, item))
    import collections.abc

    # A PEP 604 union goes through the same substitution (CPython's
    # `union_getitem` also calls `_Py_subs_parameters`) but has no
    # `__origin__`.
    is_callable_origin = (
        getattr(alias, "__origin__", None) is collections.abc.Callable
    )
    return tuple(
        _ga_make_substitution(alias.__args__, new_arg_by_param, is_callable_origin)
    )


class _TypeAliasModuleDescriptor:
    """`__module__` for TypeAliasType.

    CPython's `typealias` exposes `__module__` as an instance getset;
    on the *class* the metatype's `type.__module__` data descriptor
    wins and reports "typing". WeavePy resolves class-dict entries
    first, so this descriptor answers both accesses itself.
    """

    def __get__(self, obj, objtype=None):
        if obj is None:
            return "typing"
        return obj._module

    def __set__(self, obj, value):
        raise AttributeError("readonly attribute")


class TypeAliasType(metaclass=_ImmutableTypeMeta):
    """Type alias.

    Type aliases are created through the type statement::

        type Alias = int

    In this example, Alias and int will be treated equivalently by
    static type checkers.

    At runtime, Alias is an instance of TypeAliasType. The __name__
    attribute holds the name of the type alias. The value of the type
    alias is stored in the __value__ attribute. It is evaluated
    lazily, so the value is computed only if the attribute is
    accessed.

    Type aliases can also be generic::

        type ListOrSet[T] = list[T] | set[T]

    In this case, the type parameters of the alias are stored in the
    __type_params__ attribute.

    See PEP 695 for more information.
    """

    __module__ = "typing"

    def __new__(cls, name, value, *, type_params=None):
        if cls is not TypeAliasType:
            raise TypeError(
                "type 'typing.TypeAliasType' is not an acceptable base type"
            )
        if not isinstance(name, str):
            raise TypeError(
                f"TypeAliasType() argument 'name' must be str, not {type(name).__name__}"
            )
        if type_params is not None and not isinstance(type_params, tuple):
            raise TypeError("type_params must be a tuple")
        self = object.__new__(cls)
        self._name = name
        if not type_params:
            self._type_params = None
        else:
            self._type_params = type_params
        self._compute_value = None
        self._value = value
        self._module = _caller()
        return self

    # `typealias_alloc` with a compute_value thunk — the PEP 695
    # `type X[T] = ...` path (`_Py_make_typealias`).
    @classmethod
    def _weavepy_lazy(cls, name, type_params, compute_value):
        self = object.__new__(cls)
        self._name = name
        if not type_params:
            self._type_params = None
        else:
            self._type_params = tuple(type_params)
        self._compute_value = compute_value
        self._value = _SENTINEL
        self._module = getattr(compute_value, "__module__", None)
        return self

    def __init_subclass__(cls, /, *args, **kwargs):
        # The C type lacks Py_TPFLAGS_BASETYPE, so subclassing is
        # rejected by `type_new` with this exact message.
        raise TypeError("type 'typing.TypeAliasType' is not an acceptable base type")

    @property
    def __name__(self):
        return self._name

    @property
    def __value__(self):
        if self._value is _SENTINEL:
            self._value = self._compute_value()
        return self._value

    @property
    def __type_params__(self):
        if self._type_params is None:
            return ()
        return self._type_params

    @property
    def __parameters__(self):
        if self._type_params is None:
            return ()
        return _unpack_typevartuples(self._type_params)

    __module__ = _TypeAliasModuleDescriptor()

    def __repr__(self):
        return self._name

    def __reduce__(self):
        return self._name

    def __getitem__(self, args):
        if self._type_params is None:
            raise TypeError("Only generic type aliases are subscriptable")
        import types

        if not isinstance(args, tuple):
            args = (args,)
        return types.GenericAlias(self, args)

    def __or__(self, right):
        # C uses `_Py_union_type_or` (a types.UnionType union); the
        # VM builtin builds the same native PEP 604 union.
        return __weavepy_pep604_union__(self, right)

    def __ror__(self, left):
        return __weavepy_pep604_union__(left, self)


_SENTINEL = object()


class Generic:
    """Abstract base class for generic types.

    On Python 3.12 and newer, generic classes implicitly inherit from
    Generic when they declare a parameter list after the class's name::

        class Mapping[KT, VT]:
            def __getitem__(self, key: KT) -> VT:
                ...
            # Etc.
    """

    __module__ = "typing"
    __slots__ = ()

    def __class_getitem__(cls, args):
        """Parameterizes a generic class.

        At least, parameterizing a generic class is the *main* thing
        this method does. For example, for some generic class `Foo`,
        this is called when we do `Foo[int]` - there, with `cls=Foo`
        and `params=int`.
        """
        return _call_typing_func("_generic_class_getitem", cls, args)

    def __init_subclass__(cls, /, *args, **kwargs):
        """Function to initialize subclasses."""
        _call_typing_func("_generic_init_subclass", cls, *args, **kwargs)


# ---------------------------------------------------------------------
# PEP 695 intrinsic entry points (the VM's `__weavepy_*__` lowering
# calls these; CPython equivalents in pycore_typevarobject.h).
# ---------------------------------------------------------------------


def _weavepy_make_typevar(name, evaluate_bound=None, evaluate_constraints=None):
    return TypeVar._weavepy_make(name, evaluate_bound, evaluate_constraints)


def _weavepy_make_paramspec(name):
    return ParamSpec._weavepy_make(name)


def _weavepy_make_typevartuple(name):
    return TypeVarTuple._weavepy_make(name)


def _weavepy_make_typealias(name, type_params, compute_value):
    # The parser's lowering passes the alias body as a zero-argument
    # lambda that *closes over* the type parameters (each parameter is
    # bound by an immediately-invoked lambda standing in for CPython's
    # hidden PEP 695 scope), so it already has the shape CPython's
    # `typealias_alloc` stores.
    return TypeAliasType._weavepy_lazy(name, tuple(type_params), compute_value)


def _weavepy_make_starred_alias(origin, args):
    """Rebuild a PEP 646 *unpacked* alias (``*tuple[int, ...]``).

    CPython's ``ga_reduce`` pickles a starred alias as
    ``next(iter(plain_alias))`` (the alias iterator yields the starred
    form once). WeavePy reduces to this named helper instead so the
    callable pickles by module+qualname at every protocol.
    """
    import types

    return next(iter(types.GenericAlias(origin, args)))


def _weavepy_subscript_generic(*params):
    # `_Py_subscript_generic`: `Generic[params]` for the PEP 695 class
    # prologue.
    params = _unpack_typevartuples(params)
    return _call_typing_func("_GenericAlias", Generic, params)


def _weavepy_set_typeparam_default(typeparam, evaluate_default):
    # `_Py_set_typeparam_default` (PEP 696).
    if isinstance(typeparam, (TypeVar, ParamSpec, TypeVarTuple)):
        object.__setattr__(typeparam, "_evaluate_default", evaluate_default)
        return typeparam
    raise TypeError(f"Expected a type param, got {typeparam!r}")


def _weavepy_set_typeparam_default_starred(typeparam, evaluate_default):
    # `*Ts = *tuple[int, str]` — the default is the *unpacked* form of
    # the evaluated operand, matching CPython's compiled
    # `<operand>; UNPACK / next(iter(...))` shape (test_type_params
    # asserts `Ts.__default__ == next(iter(operand))`).
    def evaluate_starred():
        return next(iter(evaluate_default()))

    return _weavepy_set_typeparam_default(typeparam, evaluate_starred)
