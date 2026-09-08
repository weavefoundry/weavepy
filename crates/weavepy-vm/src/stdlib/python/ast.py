"""Abstract Syntax Trees (WeavePy, RFC 0033/0057).

A drop-in replacement for CPython's :mod:`ast`. The node classes are
pure Python (generated from CPython 3.13's ASDL surface, including
``_fields`` / ``_attributes`` / ``_field_types`` and the 3.13
constructor semantics); every public helper from ``parse`` through
``unparse`` and the deprecated ``Num``/``Str``/``slice`` shims is
CPython's own code, running unchanged on top of those classes. The one
engine-level operation — turning source into a tree — is delegated to
the native :mod:`_ast` core via ``compile(..., PyCF_ONLY_AST)``.
"""

import sys
import _ast

# `compile()` control flags (CPython exposes these on `_ast`; values
# from Include/cpython/compile.h).
PyCF_ONLY_AST = 0x0400
PyCF_TYPE_COMMENTS = 0x1000
PyCF_ALLOW_TOP_LEVEL_AWAIT = 0x2000
PyCF_OPTIMIZED_AST = 0x8000 | PyCF_ONLY_AST


# ---------------------------------------------------------------------------
# Base node (CPython 3.13 `ast_type_init` semantics)
# ---------------------------------------------------------------------------


def _is_list_field(field_type):
    return getattr(field_type, "__origin__", None) is list


def _is_optional_field(field_type):
    return type(None) in getattr(field_type, "__args__", ())


_MISSING_FIELD_TYPE = object()


class AST:
    _fields = ()
    _attributes = ()

    # CPython's ast.AST tp_new is PyType_GenericNew, which ignores
    # excess arguments (test_constant_subclasses_deprecated relies on
    # `Constant.__new__(cls, *args, **kwargs)` not raising).
    # `cls` must be positional-only: MatchClass has a field literally
    # named `cls`, passed by keyword (test_field_attr_existence).
    def __new__(cls, /, *args, **kwargs):
        return object.__new__(cls)

    def __init__(self, *args, **kwargs):
        _ast_init(self, args, kwargs)

    # CPython's C-level `ast_type_init` receives the CALL_FUNCTION_EX
    # `**mapping` dict raw — non-str keys included — and matches field
    # names by equality (test_ast test_non_str_kwarg). The VM routes
    # class calls whose kwargs dict holds a non-str key here instead of
    # rejecting them with "keywords must be strings".
    __weave_raw_kwargs__ = True

    def __weave_construct__(cls, args, kwargs):
        self = cls.__new__(cls, *args)
        _ast_init(self, args, kwargs)
        return self

    # 3.14 `ast_repr`: fields recursively to depth 3, sequences shown as
    # first/last element with `...` between, a cycle or exhausted depth
    # rendering as `Name(...)`.
    def __repr__(self):
        return _ast_repr_max_depth(self, 3)

    # 3.14 `ast_type_replace` (PEP 8 `copy.replace` protocol).
    def __replace__(self, /, *args, **kwargs):
        return _ast_replace(self, args, kwargs)


def _ast_replace(self, args, kwargs):
        if args:
            raise TypeError("__replace__() takes no positional arguments")
        cls = type(self)
        tp_name = _ast_tp_name(cls)
        d = getattr(self, "__dict__", None)
        fields = getattr(cls, "_fields", None)
        attributes = getattr(cls, "_attributes", None)
        expecting = set(fields) if fields is not None else set()
        if attributes is not None:
            expecting.update(attributes)
        # Any keyword that is neither a field nor an attribute is rejected.
        for key in kwargs:
            if key not in expecting:
                raise TypeError(
                    f"{tp_name}.__replace__ got an unexpected keyword "
                    f"argument {key!r}."
                )
            expecting.discard(key)
        if d is not None:
            # Fields or attributes present on the instance are copied
            # over; any attribute may be missing at runtime.
            expecting.difference_update(d)
            if attributes is not None:
                expecting.difference_update(attributes)
        # Fields typed `X | None` default to None.
        field_types = getattr(cls, "_field_types", None)
        if field_types is not None:
            for name, field_type in field_types.items():
                if _is_optional_field(field_type):
                    expecting.discard(name)
        if expecting:
            names = sorted(repr(name) for name in expecting)
            m = len(names)
            raise TypeError(
                f"{tp_name}.__replace__ missing {m} keyword "
                f"argument{'' if m == 1 else 's'}: {', '.join(names)}."
            )
        payload = {}
        if d is not None:
            for names in (fields, attributes):
                for key in names or ():
                    if key in d:
                        payload[key] = d[key]
        payload.update(kwargs)
        return cls(**payload)


# `node.__replace__(**{object(): 1})`: the C method sees the raw dict and
# reports the key by repr (test_replace_non_str_kwarg). The VM routes a
# `**mapping` call holding non-str keys here with the dict intact.
AST.__replace__.__weave_raw_call__ = _ast_replace


def _ast_tp_name(cls):
    # The C node classes' `tp_name` is the bare class name; only the
    # base type spells itself `ast.AST`.
    if cls is AST:
        return "ast.AST"
    return cls.__name__


_repr_active = []


def _ast_repr_list(seq, depth):
    length = len(seq)
    if length == 0:
        return repr(seq)
    is_list = isinstance(seq, list)
    items = [seq[0]]
    if length > 1:
        items.append(seq[-1])
    out = ["[" if is_list else "("]
    for i, item in enumerate(items):
        if i > 0:
            out.append(", ")
        if isinstance(item, AST):
            out.append(_ast_repr_max_depth(item, depth - 1))
        else:
            out.append(repr(item))
        if i == 0 and length > 2:
            out.append(", ...")
    out.append("]" if is_list else ")")
    return "".join(out)


def _ast_repr_max_depth(self, depth):
    tp_name = _ast_tp_name(type(self))
    if depth <= 0:
        return f"{tp_name}(...)"
    key = id(self)
    if key in _repr_active:
        return f"{tp_name}(...)"
    _repr_active.append(key)
    try:
        fields = getattr(type(self), "_fields", None)
        if fields is None:
            fields = ()
        if len(fields) == 0:
            return f"{tp_name}()"
        parts = []
        for name in fields:
            value = getattr(self, name)
            if isinstance(value, (list, tuple)):
                value_repr = _ast_repr_list(value, depth)
            elif isinstance(value, AST):
                value_repr = _ast_repr_max_depth(value, depth - 1)
            else:
                value_repr = repr(value)
            parts.append(f"{name}={value_repr}")
        return f"{tp_name}({', '.join(parts)})"
    finally:
        _repr_active.pop()


def _ast_init(self, args, kwargs):
        cls = type(self)
        try:
            fields = cls._fields
        except AttributeError:
            # ast_type_init reports the C tp_name ("ast.AST") for the
            # module's own node classes, not the bare class __name__
            # (test_AST_fields_NULL_check, gh-126105).
            name = cls.__name__
            if cls.__module__ in ("ast", "_ast"):
                name = f"ast.{name}"
            raise AttributeError(
                f"type object '{name}' has no attribute '_fields'"
            ) from None
        if len(args) > len(fields):
            raise TypeError(
                f"{cls.__name__} constructor takes at most "
                f"{len(fields)} positional argument{'' if len(fields) == 1 else 's'}"
            )
        given = set()
        for name, value in zip(fields, args):
            if name in kwargs:
                raise TypeError(
                    f"{cls.__name__} got multiple values for argument {name!r}"
                )
            given.add(name)
            setattr(self, name, value)
        for key, value in kwargs.items():
            if key not in fields and key not in cls._attributes:
                import warnings
                warnings.warn(
                    f"{cls.__name__}.__init__ got an unexpected keyword "
                    f"argument {key!r}. Support for arbitrary keyword "
                    "arguments is deprecated and will be removed in Python "
                    "3.15.",
                    DeprecationWarning, stacklevel=2)
            given.add(key)
            setattr(self, key, value)
        # Unassigned fields default per their ASDL kind: sequences get a
        # fresh list, a `ctx` slot gets the Load singleton, optionals
        # keep their class-level None; a missing required field warns
        # (one DeprecationWarning per field, reverse field order —
        # matching Python-ast.c).
        field_types = getattr(cls, "_field_types", None)
        if field_types:
            missing = []
            for name in fields:
                if name in given:
                    continue
                field_type = field_types.get(name, _MISSING_FIELD_TYPE)
                if field_type is _MISSING_FIELD_TYPE:
                    # ast_type_init: a field absent from _field_types warns
                    # (test_incomplete_field_types).
                    import warnings
                    warnings.warn(
                        f"Field {name!r} is missing from {cls.__name__}."
                        "_field_types. This will become an error in Python "
                        "3.15.",
                        DeprecationWarning, stacklevel=2)
                    continue
                if field_type is None:
                    continue
                if _is_list_field(field_type):
                    setattr(self, name, [])
                elif _is_optional_field(field_type):
                    pass  # class-level None default
                elif field_type is expr_context:
                    setattr(self, name, _load_singleton)
                else:
                    missing.append(name)
            if missing:
                import warnings
                for name in reversed(missing):
                    warnings.warn(
                        f"{cls.__name__}.__init__ missing 1 required "
                        f"positional argument: {name!r}. This will become "
                        "an error in Python 3.15.",
                        DeprecationWarning, stacklevel=2)


# ---------------------------------------------------------------------------
# Node classes (generated from CPython 3.13)
# ---------------------------------------------------------------------------

class alias(AST):
    _fields = ('name', 'asname', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class arg(AST):
    _fields = ('arg', 'annotation', 'type_comment', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class arguments(AST):
    _fields = ('posonlyargs', 'args', 'vararg', 'kwonlyargs', 'kw_defaults', 'kwarg', 'defaults', )

class boolop(AST):
    _fields = ()

class cmpop(AST):
    _fields = ()

class comprehension(AST):
    _fields = ('target', 'iter', 'ifs', 'is_async', )

class excepthandler(AST):
    _fields = ()
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class expr(AST):
    _fields = ()
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class expr_context(AST):
    _fields = ()

class keyword(AST):
    _fields = ('arg', 'value', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class match_case(AST):
    _fields = ('pattern', 'guard', 'body', )

class mod(AST):
    _fields = ()

class operator(AST):
    _fields = ()

class pattern(AST):
    _fields = ()
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class stmt(AST):
    _fields = ()
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class type_ignore(AST):
    _fields = ()

class type_param(AST):
    _fields = ()
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class unaryop(AST):
    _fields = ()

class withitem(AST):
    _fields = ('context_expr', 'optional_vars', )

# Defined before the alphabetical run: test_ast_asdl_signature formats
# `expr.__subclasses__()[0]` as the `expr = …` head line, and in CPython's
# ASDL order that first subclass is BoolOp.
class BoolOp(expr):
    _fields = ('op', 'values', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Add(operator):
    _fields = ()

class And(boolop):
    _fields = ()

class AnnAssign(stmt):
    _fields = ('target', 'annotation', 'value', 'simple', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Assert(stmt):
    _fields = ('test', 'msg', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Assign(stmt):
    _fields = ('targets', 'value', 'type_comment', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class AsyncFor(stmt):
    _fields = ('target', 'iter', 'body', 'orelse', 'type_comment', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class AsyncFunctionDef(stmt):
    _fields = ('name', 'args', 'body', 'decorator_list', 'returns', 'type_comment', 'type_params', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class AsyncWith(stmt):
    _fields = ('items', 'body', 'type_comment', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Attribute(expr):
    _fields = ('value', 'attr', 'ctx', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class AugAssign(stmt):
    _fields = ('target', 'op', 'value', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Await(expr):
    _fields = ('value', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class BinOp(expr):
    _fields = ('left', 'op', 'right', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class BitAnd(operator):
    _fields = ()

class BitOr(operator):
    _fields = ()

class BitXor(operator):
    _fields = ()

class Break(stmt):
    _fields = ()
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Call(expr):
    _fields = ('func', 'args', 'keywords', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class ClassDef(stmt):
    _fields = ('name', 'bases', 'keywords', 'body', 'decorator_list', 'type_params', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Compare(expr):
    _fields = ('left', 'ops', 'comparators', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Constant(expr):
    _fields = ('value', 'kind', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Continue(stmt):
    _fields = ()
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Del(expr_context):
    _fields = ()

class Delete(stmt):
    _fields = ('targets', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Dict(expr):
    _fields = ('keys', 'values', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class DictComp(expr):
    _fields = ('key', 'value', 'generators', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Div(operator):
    _fields = ()

class Eq(cmpop):
    _fields = ()

class ExceptHandler(excepthandler):
    _fields = ('type', 'name', 'body', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Expr(stmt):
    _fields = ('value', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Expression(mod):
    _fields = ('body', )

class FloorDiv(operator):
    _fields = ()

class For(stmt):
    _fields = ('target', 'iter', 'body', 'orelse', 'type_comment', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class FormattedValue(expr):
    _fields = ('value', 'conversion', 'format_spec', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class FunctionDef(stmt):
    _fields = ('name', 'args', 'body', 'decorator_list', 'returns', 'type_comment', 'type_params', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class FunctionType(mod):
    _fields = ('argtypes', 'returns', )

class GeneratorExp(expr):
    _fields = ('elt', 'generators', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Global(stmt):
    _fields = ('names', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Gt(cmpop):
    _fields = ()

class GtE(cmpop):
    _fields = ()

class If(stmt):
    _fields = ('test', 'body', 'orelse', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class IfExp(expr):
    _fields = ('test', 'body', 'orelse', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Import(stmt):
    _fields = ('names', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class ImportFrom(stmt):
    _fields = ('module', 'names', 'level', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class In(cmpop):
    _fields = ()

class Interpolation(expr):
    _fields = ('value', 'str', 'conversion', 'format_spec', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Interactive(mod):
    _fields = ('body', )

class Invert(unaryop):
    _fields = ()

class Is(cmpop):
    _fields = ()

class IsNot(cmpop):
    _fields = ()

class JoinedStr(expr):
    _fields = ('values', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class LShift(operator):
    _fields = ()

class Lambda(expr):
    _fields = ('args', 'body', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class List(expr):
    _fields = ('elts', 'ctx', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class ListComp(expr):
    _fields = ('elt', 'generators', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Load(expr_context):
    _fields = ()

class Lt(cmpop):
    _fields = ()

class LtE(cmpop):
    _fields = ()

class MatMult(operator):
    _fields = ()

class Match(stmt):
    _fields = ('subject', 'cases', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class MatchAs(pattern):
    _fields = ('pattern', 'name', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class MatchClass(pattern):
    _fields = ('cls', 'patterns', 'kwd_attrs', 'kwd_patterns', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class MatchMapping(pattern):
    _fields = ('keys', 'patterns', 'rest', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class MatchOr(pattern):
    _fields = ('patterns', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class MatchSequence(pattern):
    _fields = ('patterns', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class MatchSingleton(pattern):
    _fields = ('value', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class MatchStar(pattern):
    _fields = ('name', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class MatchValue(pattern):
    _fields = ('value', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Mod(operator):
    _fields = ()

class Module(mod):
    _fields = ('body', 'type_ignores', )

class Mult(operator):
    _fields = ()

class Name(expr):
    _fields = ('id', 'ctx', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class NamedExpr(expr):
    _fields = ('target', 'value', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Nonlocal(stmt):
    _fields = ('names', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Not(unaryop):
    _fields = ()

class NotEq(cmpop):
    _fields = ()

class NotIn(cmpop):
    _fields = ()

class Or(boolop):
    _fields = ()

class ParamSpec(type_param):
    _fields = ('name', 'default_value', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Pass(stmt):
    _fields = ()
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Pow(operator):
    _fields = ()

class RShift(operator):
    _fields = ()

class Raise(stmt):
    _fields = ('exc', 'cause', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Return(stmt):
    _fields = ('value', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Set(expr):
    _fields = ('elts', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class SetComp(expr):
    _fields = ('elt', 'generators', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Slice(expr):
    _fields = ('lower', 'upper', 'step', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Starred(expr):
    _fields = ('value', 'ctx', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Store(expr_context):
    _fields = ()

class Sub(operator):
    _fields = ()

class Subscript(expr):
    _fields = ('value', 'slice', 'ctx', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class TemplateStr(expr):
    _fields = ('values', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Try(stmt):
    _fields = ('body', 'handlers', 'orelse', 'finalbody', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class TryStar(stmt):
    _fields = ('body', 'handlers', 'orelse', 'finalbody', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Tuple(expr):
    _fields = ('elts', 'ctx', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class TypeAlias(stmt):
    _fields = ('name', 'type_params', 'value', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class TypeIgnore(type_ignore):
    _fields = ('lineno', 'tag', )

class TypeVar(type_param):
    _fields = ('name', 'bound', 'default_value', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class TypeVarTuple(type_param):
    _fields = ('name', 'default_value', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class UAdd(unaryop):
    _fields = ()

class USub(unaryop):
    _fields = ()

class UnaryOp(expr):
    _fields = ('op', 'operand', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class While(stmt):
    _fields = ('test', 'body', 'orelse', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class With(stmt):
    _fields = ('items', 'body', 'type_comment', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class Yield(expr):
    _fields = ('value', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

class YieldFrom(expr):
    _fields = ('value', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )


# Optional (ASDL ``?``) fields carry a class-level ``None`` default so
# ``dump`` omits them when unset — matching CPython 3.13.
AnnAssign.value = None
Assert.msg = None
Assign.type_comment = None
AsyncFor.type_comment = None
AsyncFunctionDef.returns = None
AsyncFunctionDef.type_comment = None
AsyncWith.type_comment = None
Constant.kind = None
ExceptHandler.type = None
ExceptHandler.name = None
For.type_comment = None
FormattedValue.format_spec = None
Interpolation.format_spec = None
FunctionDef.returns = None
FunctionDef.type_comment = None
ImportFrom.module = None
ImportFrom.level = None
MatchAs.pattern = None
MatchAs.name = None
MatchMapping.rest = None
MatchStar.name = None
ParamSpec.default_value = None
Raise.exc = None
Raise.cause = None
Return.value = None
Slice.lower = None
Slice.upper = None
Slice.step = None
TypeVar.bound = None
TypeVar.default_value = None
TypeVarTuple.default_value = None
With.type_comment = None
Yield.value = None
alias.asname = None
arg.annotation = None
arg.type_comment = None
arguments.vararg = None
arguments.kwarg = None
keyword.arg = None
match_case.guard = None
withitem.optional_vars = None


# When another instance of this module already published the node classes
# onto `_ast` (e.g. `weavepy -m ast` runs ast.py as `__main__` before the
# importable copy loads for `ast.parse`), adopt those classes: isinstance
# checks must agree across both copies (in CPython they live in the shared
# C `_ast` module, so this situation can't arise there).
def _adopt_existing_node_classes():
    existing = getattr(_ast, "AST", None)
    if existing is None or existing is AST:
        return
    g = globals()
    base = AST  # capture: the loop rebinds the `AST` global itself
    for name, obj in list(g.items()):
        if (isinstance(obj, type) and issubclass(obj, base)
                and hasattr(_ast, name)):
            g[name] = getattr(_ast, name)
    g["AST"] = existing


_adopt_existing_node_classes()
del _adopt_existing_node_classes

# ---------------------------------------------------------------------------
# Field-type tables (generated from CPython 3.13's `_field_types`; the
# constructor derives sequence/optional/`ctx` defaulting from these).
# ---------------------------------------------------------------------------

Add._field_types = {}
And._field_types = {}
AnnAssign._field_types = {'target': expr, 'annotation': expr, 'value': expr | None, 'simple': int}
Assert._field_types = {'test': expr, 'msg': expr | None}
Assign._field_types = {'targets': list[expr], 'value': expr, 'type_comment': str | None}
AsyncFor._field_types = {'target': expr, 'iter': expr, 'body': list[stmt], 'orelse': list[stmt], 'type_comment': str | None}
AsyncFunctionDef._field_types = {'name': str, 'args': arguments, 'body': list[stmt], 'decorator_list': list[expr], 'returns': expr | None, 'type_comment': str | None, 'type_params': list[type_param]}
AsyncWith._field_types = {'items': list[withitem], 'body': list[stmt], 'type_comment': str | None}
Attribute._field_types = {'value': expr, 'attr': str, 'ctx': expr_context}
AugAssign._field_types = {'target': expr, 'op': operator, 'value': expr}
Await._field_types = {'value': expr}
BinOp._field_types = {'left': expr, 'op': operator, 'right': expr}
BitAnd._field_types = {}
BitOr._field_types = {}
BitXor._field_types = {}
BoolOp._field_types = {'op': boolop, 'values': list[expr]}
Break._field_types = {}
Call._field_types = {'func': expr, 'args': list[expr], 'keywords': list[keyword]}
ClassDef._field_types = {'name': str, 'bases': list[expr], 'keywords': list[keyword], 'body': list[stmt], 'decorator_list': list[expr], 'type_params': list[type_param]}
Compare._field_types = {'left': expr, 'ops': list[cmpop], 'comparators': list[expr]}
Constant._field_types = {'value': object, 'kind': str | None}
Continue._field_types = {}
Del._field_types = {}
Delete._field_types = {'targets': list[expr]}
Dict._field_types = {'keys': list[expr], 'values': list[expr]}
DictComp._field_types = {'key': expr, 'value': expr, 'generators': list[comprehension]}
Div._field_types = {}
Eq._field_types = {}
ExceptHandler._field_types = {'type': expr | None, 'name': str | None, 'body': list[stmt]}
Expr._field_types = {'value': expr}
Expression._field_types = {'body': expr}
FloorDiv._field_types = {}
For._field_types = {'target': expr, 'iter': expr, 'body': list[stmt], 'orelse': list[stmt], 'type_comment': str | None}
FormattedValue._field_types = {'value': expr, 'conversion': int, 'format_spec': expr | None}
FunctionDef._field_types = {'name': str, 'args': arguments, 'body': list[stmt], 'decorator_list': list[expr], 'returns': expr | None, 'type_comment': str | None, 'type_params': list[type_param]}
FunctionType._field_types = {'argtypes': list[expr], 'returns': expr}
GeneratorExp._field_types = {'elt': expr, 'generators': list[comprehension]}
Global._field_types = {'names': list[str]}
Gt._field_types = {}
GtE._field_types = {}
If._field_types = {'test': expr, 'body': list[stmt], 'orelse': list[stmt]}
IfExp._field_types = {'test': expr, 'body': expr, 'orelse': expr}
Import._field_types = {'names': list[alias]}
ImportFrom._field_types = {'module': str | None, 'names': list[alias], 'level': int | None}
In._field_types = {}
Interactive._field_types = {'body': list[stmt]}
Interpolation._field_types = {'value': expr, 'str': object, 'conversion': int, 'format_spec': expr | None}
Invert._field_types = {}
Is._field_types = {}
IsNot._field_types = {}
JoinedStr._field_types = {'values': list[expr]}
TemplateStr._field_types = {'values': list[expr]}
LShift._field_types = {}
Lambda._field_types = {'args': arguments, 'body': expr}
List._field_types = {'elts': list[expr], 'ctx': expr_context}
ListComp._field_types = {'elt': expr, 'generators': list[comprehension]}
Load._field_types = {}
Lt._field_types = {}
LtE._field_types = {}
MatMult._field_types = {}
Match._field_types = {'subject': expr, 'cases': list[match_case]}
MatchAs._field_types = {'pattern': pattern | None, 'name': str | None}
MatchClass._field_types = {'cls': expr, 'patterns': list[pattern], 'kwd_attrs': list[str], 'kwd_patterns': list[pattern]}
MatchMapping._field_types = {'keys': list[expr], 'patterns': list[pattern], 'rest': str | None}
MatchOr._field_types = {'patterns': list[pattern]}
MatchSequence._field_types = {'patterns': list[pattern]}
MatchSingleton._field_types = {'value': object}
MatchStar._field_types = {'name': str | None}
MatchValue._field_types = {'value': expr}
Mod._field_types = {}
Module._field_types = {'body': list[stmt], 'type_ignores': list[type_ignore]}
Mult._field_types = {}
Name._field_types = {'id': str, 'ctx': expr_context}
NamedExpr._field_types = {'target': expr, 'value': expr}
Nonlocal._field_types = {'names': list[str]}
Not._field_types = {}
NotEq._field_types = {}
NotIn._field_types = {}
Or._field_types = {}
ParamSpec._field_types = {'name': str, 'default_value': expr | None}
Pass._field_types = {}
Pow._field_types = {}
RShift._field_types = {}
Raise._field_types = {'exc': expr | None, 'cause': expr | None}
Return._field_types = {'value': expr | None}
Set._field_types = {'elts': list[expr]}
SetComp._field_types = {'elt': expr, 'generators': list[comprehension]}
Slice._field_types = {'lower': expr | None, 'upper': expr | None, 'step': expr | None}
Starred._field_types = {'value': expr, 'ctx': expr_context}
Store._field_types = {}
Sub._field_types = {}
Subscript._field_types = {'value': expr, 'slice': expr, 'ctx': expr_context}
Try._field_types = {'body': list[stmt], 'handlers': list[excepthandler], 'orelse': list[stmt], 'finalbody': list[stmt]}
TryStar._field_types = {'body': list[stmt], 'handlers': list[excepthandler], 'orelse': list[stmt], 'finalbody': list[stmt]}
Tuple._field_types = {'elts': list[expr], 'ctx': expr_context}
TypeAlias._field_types = {'name': expr, 'type_params': list[type_param], 'value': expr}
TypeIgnore._field_types = {'lineno': int, 'tag': str}
TypeVar._field_types = {'name': str, 'bound': expr | None, 'default_value': expr | None}
TypeVarTuple._field_types = {'name': str, 'default_value': expr | None}
UAdd._field_types = {}
USub._field_types = {}
UnaryOp._field_types = {'op': unaryop, 'operand': expr}
While._field_types = {'test': expr, 'body': list[stmt], 'orelse': list[stmt]}
With._field_types = {'items': list[withitem], 'body': list[stmt], 'type_comment': str | None}
Yield._field_types = {'value': expr | None}
YieldFrom._field_types = {'value': expr}
alias._field_types = {'name': str, 'asname': str | None}
arg._field_types = {'arg': str, 'annotation': expr | None, 'type_comment': str | None}
arguments._field_types = {'posonlyargs': list[arg], 'args': list[arg], 'vararg': arg | None, 'kwonlyargs': list[arg], 'kw_defaults': list[expr], 'kwarg': arg | None, 'defaults': list[expr]}
comprehension._field_types = {'target': expr, 'iter': expr, 'ifs': list[expr], 'is_async': int}
keyword._field_types = {'arg': str | None, 'value': expr}
match_case._field_types = {'pattern': pattern, 'guard': expr | None, 'body': list[stmt]}
withitem._field_types = {'context_expr': expr, 'optional_vars': expr | None}

# ASDL signature docstrings (generated from CPython 3.13's Python-ast.c;
# test_ast_asdl_signature checks these verbatim).
Add.__doc__ = 'Add'
And.__doc__ = 'And'
AnnAssign.__doc__ = 'AnnAssign(expr target, expr annotation, expr? value, int simple)'
Assert.__doc__ = 'Assert(expr test, expr? msg)'
Assign.__doc__ = 'Assign(expr* targets, expr value, string? type_comment)'
AsyncFor.__doc__ = 'AsyncFor(expr target, expr iter, stmt* body, stmt* orelse, string? type_comment)'
AsyncFunctionDef.__doc__ = 'AsyncFunctionDef(identifier name, arguments args, stmt* body, expr* decorator_list, expr? returns, string? type_comment, type_param* type_params)'
AsyncWith.__doc__ = 'AsyncWith(withitem* items, stmt* body, string? type_comment)'
Attribute.__doc__ = 'Attribute(expr value, identifier attr, expr_context ctx)'
AugAssign.__doc__ = 'AugAssign(expr target, operator op, expr value)'
Await.__doc__ = 'Await(expr value)'
BinOp.__doc__ = 'BinOp(expr left, operator op, expr right)'
BitAnd.__doc__ = 'BitAnd'
BitOr.__doc__ = 'BitOr'
BitXor.__doc__ = 'BitXor'
BoolOp.__doc__ = 'BoolOp(boolop op, expr* values)'
Break.__doc__ = 'Break'
Call.__doc__ = 'Call(expr func, expr* args, keyword* keywords)'
ClassDef.__doc__ = 'ClassDef(identifier name, expr* bases, keyword* keywords, stmt* body, expr* decorator_list, type_param* type_params)'
Compare.__doc__ = 'Compare(expr left, cmpop* ops, expr* comparators)'
Constant.__doc__ = 'Constant(constant value, string? kind)'
Continue.__doc__ = 'Continue'
Del.__doc__ = 'Del'
Delete.__doc__ = 'Delete(expr* targets)'
Dict.__doc__ = 'Dict(expr* keys, expr* values)'
DictComp.__doc__ = 'DictComp(expr key, expr value, comprehension* generators)'
Div.__doc__ = 'Div'
Eq.__doc__ = 'Eq'
ExceptHandler.__doc__ = 'ExceptHandler(expr? type, identifier? name, stmt* body)'
Expr.__doc__ = 'Expr(expr value)'
Expression.__doc__ = 'Expression(expr body)'
FloorDiv.__doc__ = 'FloorDiv'
For.__doc__ = 'For(expr target, expr iter, stmt* body, stmt* orelse, string? type_comment)'
FormattedValue.__doc__ = 'FormattedValue(expr value, int conversion, expr? format_spec)'
FunctionDef.__doc__ = 'FunctionDef(identifier name, arguments args, stmt* body, expr* decorator_list, expr? returns, string? type_comment, type_param* type_params)'
FunctionType.__doc__ = 'FunctionType(expr* argtypes, expr returns)'
GeneratorExp.__doc__ = 'GeneratorExp(expr elt, comprehension* generators)'
Global.__doc__ = 'Global(identifier* names)'
Gt.__doc__ = 'Gt'
GtE.__doc__ = 'GtE'
If.__doc__ = 'If(expr test, stmt* body, stmt* orelse)'
IfExp.__doc__ = 'IfExp(expr test, expr body, expr orelse)'
Import.__doc__ = 'Import(alias* names)'
ImportFrom.__doc__ = 'ImportFrom(identifier? module, alias* names, int? level)'
In.__doc__ = 'In'
Interactive.__doc__ = 'Interactive(stmt* body)'
Interpolation.__doc__ = 'Interpolation(expr value, constant str, int conversion, expr? format_spec)'
Invert.__doc__ = 'Invert'
Is.__doc__ = 'Is'
IsNot.__doc__ = 'IsNot'
JoinedStr.__doc__ = 'JoinedStr(expr* values)'
TemplateStr.__doc__ = 'TemplateStr(expr* values)'
LShift.__doc__ = 'LShift'
Lambda.__doc__ = 'Lambda(arguments args, expr body)'
List.__doc__ = 'List(expr* elts, expr_context ctx)'
ListComp.__doc__ = 'ListComp(expr elt, comprehension* generators)'
Load.__doc__ = 'Load'
Lt.__doc__ = 'Lt'
LtE.__doc__ = 'LtE'
MatMult.__doc__ = 'MatMult'
Match.__doc__ = 'Match(expr subject, match_case* cases)'
MatchAs.__doc__ = 'MatchAs(pattern? pattern, identifier? name)'
MatchClass.__doc__ = 'MatchClass(expr cls, pattern* patterns, identifier* kwd_attrs, pattern* kwd_patterns)'
MatchMapping.__doc__ = 'MatchMapping(expr* keys, pattern* patterns, identifier? rest)'
MatchOr.__doc__ = 'MatchOr(pattern* patterns)'
MatchSequence.__doc__ = 'MatchSequence(pattern* patterns)'
MatchSingleton.__doc__ = 'MatchSingleton(constant value)'
MatchStar.__doc__ = 'MatchStar(identifier? name)'
MatchValue.__doc__ = 'MatchValue(expr value)'
Mod.__doc__ = 'Mod'
Module.__doc__ = 'Module(stmt* body, type_ignore* type_ignores)'
Mult.__doc__ = 'Mult'
Name.__doc__ = 'Name(identifier id, expr_context ctx)'
NamedExpr.__doc__ = 'NamedExpr(expr target, expr value)'
Nonlocal.__doc__ = 'Nonlocal(identifier* names)'
Not.__doc__ = 'Not'
NotEq.__doc__ = 'NotEq'
NotIn.__doc__ = 'NotIn'
Or.__doc__ = 'Or'
ParamSpec.__doc__ = 'ParamSpec(identifier name, expr? default_value)'
Pass.__doc__ = 'Pass'
Pow.__doc__ = 'Pow'
RShift.__doc__ = 'RShift'
Raise.__doc__ = 'Raise(expr? exc, expr? cause)'
Return.__doc__ = 'Return(expr? value)'
Set.__doc__ = 'Set(expr* elts)'
SetComp.__doc__ = 'SetComp(expr elt, comprehension* generators)'
Slice.__doc__ = 'Slice(expr? lower, expr? upper, expr? step)'
Starred.__doc__ = 'Starred(expr value, expr_context ctx)'
Store.__doc__ = 'Store'
Sub.__doc__ = 'Sub'
Subscript.__doc__ = 'Subscript(expr value, expr slice, expr_context ctx)'
Try.__doc__ = 'Try(stmt* body, excepthandler* handlers, stmt* orelse, stmt* finalbody)'
TryStar.__doc__ = 'TryStar(stmt* body, excepthandler* handlers, stmt* orelse, stmt* finalbody)'
Tuple.__doc__ = 'Tuple(expr* elts, expr_context ctx)'
TypeAlias.__doc__ = 'TypeAlias(expr name, type_param* type_params, expr value)'
TypeIgnore.__doc__ = 'TypeIgnore(int lineno, string tag)'
TypeVar.__doc__ = 'TypeVar(identifier name, expr? bound, expr? default_value)'
TypeVarTuple.__doc__ = 'TypeVarTuple(identifier name, expr? default_value)'
UAdd.__doc__ = 'UAdd'
USub.__doc__ = 'USub'
UnaryOp.__doc__ = 'UnaryOp(unaryop op, expr operand)'
While.__doc__ = 'While(expr test, stmt* body, stmt* orelse)'
With.__doc__ = 'With(withitem* items, stmt* body, string? type_comment)'
Yield.__doc__ = 'Yield(expr? value)'
YieldFrom.__doc__ = 'YieldFrom(expr value)'
alias.__doc__ = 'alias(identifier name, identifier? asname)'
arg.__doc__ = 'arg(identifier arg, expr? annotation, string? type_comment)'
arguments.__doc__ = 'arguments(arg* posonlyargs, arg* args, arg? vararg, arg* kwonlyargs, expr* kw_defaults, arg? kwarg, expr* defaults)'
boolop.__doc__ = 'boolop = And | Or'
cmpop.__doc__ = 'cmpop = Eq | NotEq | Lt | LtE | Gt | GtE | Is | IsNot | In | NotIn'
comprehension.__doc__ = 'comprehension(expr target, expr iter, expr* ifs, int is_async)'
excepthandler.__doc__ = 'excepthandler = ExceptHandler(expr? type, identifier? name, stmt* body)'
expr.__doc__ = ('expr = BoolOp(boolop op, expr* values)\n'
                '     | NamedExpr(expr target, expr value)\n'
                '     | BinOp(expr left, operator op, expr right)\n'
                '     | UnaryOp(unaryop op, expr operand)\n'
                '     | Lambda(arguments args, expr body)\n'
                '     | IfExp(expr test, expr body, expr orelse)\n'
                '     | Dict(expr* keys, expr* values)\n'
                '     | Set(expr* elts)\n'
                '     | ListComp(expr elt, comprehension* generators)\n'
                '     | SetComp(expr elt, comprehension* generators)\n'
                '     | DictComp(expr key, expr value, comprehension* generators)\n'
                '     | GeneratorExp(expr elt, comprehension* generators)\n'
                '     | Await(expr value)\n'
                '     | Yield(expr? value)\n'
                '     | YieldFrom(expr value)\n'
                '     | Compare(expr left, cmpop* ops, expr* comparators)\n'
                '     | Call(expr func, expr* args, keyword* keywords)\n'
                '     | FormattedValue(expr value, int conversion, expr? format_spec)\n'
                '     | JoinedStr(expr* values)\n'
                '     | Interpolation(expr value, constant str, int conversion, expr? format_spec)\n'
                '     | TemplateStr(expr* values)\n'
                '     | Constant(constant value, string? kind)\n'
                '     | Attribute(expr value, identifier attr, expr_context ctx)\n'
                '     | Subscript(expr value, expr slice, expr_context ctx)\n'
                '     | Starred(expr value, expr_context ctx)\n'
                '     | Name(identifier id, expr_context ctx)\n'
                '     | List(expr* elts, expr_context ctx)\n'
                '     | Tuple(expr* elts, expr_context ctx)\n'
                '     | Slice(expr? lower, expr? upper, expr? step)')
expr_context.__doc__ = 'expr_context = Load | Store | Del'
keyword.__doc__ = 'keyword(identifier? arg, expr value)'
match_case.__doc__ = 'match_case(pattern pattern, expr? guard, stmt* body)'
mod.__doc__ = ('mod = Module(stmt* body, type_ignore* type_ignores)\n'
               '    | Interactive(stmt* body)\n'
               '    | Expression(expr body)\n'
               '    | FunctionType(expr* argtypes, expr returns)')
operator.__doc__ = 'operator = Add | Sub | Mult | MatMult | Div | Mod | Pow | LShift | RShift | BitOr | BitXor | BitAnd | FloorDiv'
pattern.__doc__ = ('pattern = MatchValue(expr value)\n'
                   '        | MatchSingleton(constant value)\n'
                   '        | MatchSequence(pattern* patterns)\n'
                   '        | MatchMapping(expr* keys, pattern* patterns, identifier? rest)\n'
                   '        | MatchClass(expr cls, pattern* patterns, identifier* kwd_attrs, pattern* kwd_patterns)\n'
                   '        | MatchStar(identifier? name)\n'
                   '        | MatchAs(pattern? pattern, identifier? name)\n'
                   '        | MatchOr(pattern* patterns)')
stmt.__doc__ = ('stmt = FunctionDef(identifier name, arguments args, stmt* body, expr* decorator_list, expr? returns, string? type_comment, type_param* type_params)\n'
                '     | AsyncFunctionDef(identifier name, arguments args, stmt* body, expr* decorator_list, expr? returns, string? type_comment, type_param* type_params)\n'
                '     | ClassDef(identifier name, expr* bases, keyword* keywords, stmt* body, expr* decorator_list, type_param* type_params)\n'
                '     | Return(expr? value)\n'
                '     | Delete(expr* targets)\n'
                '     | Assign(expr* targets, expr value, string? type_comment)\n'
                '     | TypeAlias(expr name, type_param* type_params, expr value)\n'
                '     | AugAssign(expr target, operator op, expr value)\n'
                '     | AnnAssign(expr target, expr annotation, expr? value, int simple)\n'
                '     | For(expr target, expr iter, stmt* body, stmt* orelse, string? type_comment)\n'
                '     | AsyncFor(expr target, expr iter, stmt* body, stmt* orelse, string? type_comment)\n'
                '     | While(expr test, stmt* body, stmt* orelse)\n'
                '     | If(expr test, stmt* body, stmt* orelse)\n'
                '     | With(withitem* items, stmt* body, string? type_comment)\n'
                '     | AsyncWith(withitem* items, stmt* body, string? type_comment)\n'
                '     | Match(expr subject, match_case* cases)\n'
                '     | Raise(expr? exc, expr? cause)\n'
                '     | Try(stmt* body, excepthandler* handlers, stmt* orelse, stmt* finalbody)\n'
                '     | TryStar(stmt* body, excepthandler* handlers, stmt* orelse, stmt* finalbody)\n'
                '     | Assert(expr test, expr? msg)\n'
                '     | Import(alias* names)\n'
                '     | ImportFrom(identifier? module, alias* names, int? level)\n'
                '     | Global(identifier* names)\n'
                '     | Nonlocal(identifier* names)\n'
                '     | Expr(expr value)\n'
                '     | Pass\n'
                '     | Break\n'
                '     | Continue')
type_ignore.__doc__ = 'type_ignore = TypeIgnore(int lineno, string tag)'
type_param.__doc__ = ('type_param = TypeVar(identifier name, expr? bound, expr? default_value)\n'
                      '           | ParamSpec(identifier name, expr? default_value)\n'
                      '           | TypeVarTuple(identifier name, expr? default_value)')
unaryop.__doc__ = 'unaryop = Invert | Not | UAdd | USub'
withitem.__doc__ = 'withitem(expr context_expr, expr? optional_vars)'

# `ctx` defaults share one Load instance, matching CPython's singleton
# (`ast.Name('x').ctx is ast.Name('y').ctx`).
_load_singleton = Load()

_NODE_TYPES = {
    name: obj
    for name, obj in list(globals().items())
    if isinstance(obj, type) and issubclass(obj, AST)
}

# PEP 634: AST nodes are matchable by position (`case ast.Expr(value)`).
# CPython generates `__match_args__ = _fields` on every node type, plus
# per-class `__annotations__` mirroring `_field_types`, and class-level
# ``None`` defaults for the optional end_lineno/end_col_offset attributes.
for _node in _NODE_TYPES.values():
    _node.__match_args__ = _node._fields
    _ft = _node.__dict__.get('_field_types')
    if _ft is not None:
        _node.__annotations__ = dict(_ft)
    if 'end_lineno' in _node._attributes:
        _node.end_lineno = None
        _node.end_col_offset = None
del _node


# ---------------------------------------------------------------------------
# Spec-tree -> node-instance builder
# ---------------------------------------------------------------------------


def _build(spec):
    """Rebuild a node tree from the value-based spec produced by ``_ast``.

    Bypasses ``__init__`` (via ``__new__``) so partially-populated specs
    can't trip the 3.13 missing-required-field DeprecationWarnings.
    """
    if isinstance(spec, dict):
        cls = _NODE_TYPES[spec["_type"]]
        node = cls.__new__(cls)
        for key, value in spec.items():
            if key == "_type":
                continue
            setattr(node, key, _build(value))
        return node
    if isinstance(spec, list):
        return [_build(item) for item in spec]
    return spec


def _from_spec(spec):
    """Build a node tree from an `_ast` spec (used by the native
    `compile(..., PyCF_ONLY_AST)` path — RFC 0052). Store/Del expression
    contexts are already stamped on the spec by the native builder."""
    return _build(spec)


# ---------------------------------------------------------------------------
# AST validation — port of CPython's PyAST_obj2ast checks (Python-ast.c)
# followed by _PyAST_Validate (Python/ast.c). Called by the native
# `compile()` builtin before lowering an AST object (test_ast
# ASTValidatorTests / test_match_validation_pattern / test_none_checks).
# ---------------------------------------------------------------------------

_MISSING_FIELD = object()


def _validate_positions(node):
    # Python-ast.c VALIDATE_POSITIONS macro (3.13).
    cls = type(node)
    lineno = getattr(node, "lineno", _MISSING_FIELD)
    col = getattr(node, "col_offset", _MISSING_FIELD)
    if lineno is _MISSING_FIELD:
        raise TypeError(f'required field "lineno" missing from {cls.__name__}')
    if col is _MISSING_FIELD:
        raise TypeError(f'required field "col_offset" missing from {cls.__name__}')
    # obj2ast_int: a present-but-non-int position is a ValueError
    # (test_bad_integer expects "invalid integer value: None").
    if not isinstance(lineno, int):
        raise ValueError(f"invalid integer value: {lineno!r}")
    if not isinstance(col, int):
        raise ValueError(f"invalid integer value: {col!r}")
    end_lineno = getattr(node, "end_lineno", None)
    if end_lineno is None:
        end_lineno = lineno
    elif not isinstance(end_lineno, int):
        raise ValueError(f"invalid integer value: {end_lineno!r}")
    end_col = getattr(node, "end_col_offset", None)
    if end_col is None:
        end_col = col
    elif not isinstance(end_col, int):
        raise ValueError(f"invalid integer value: {end_col!r}")
    if lineno > end_lineno:
        raise ValueError(
            f"AST node line range ({lineno}, {end_lineno}) is not valid")
    if (lineno < 0 and end_lineno != lineno) or (col < 0 and col != end_col):
        raise ValueError(
            f"AST node column range ({col}, {end_col}) for line range "
            f"({lineno}, {end_lineno}) is not valid")
    if lineno == end_lineno and col > end_col:
        raise ValueError(
            f"line {lineno}, column {col}-{end_col} is not a valid range")


_SUM_TYPES = frozenset({
    mod, stmt, expr, expr_context, boolop, operator, unaryop, cmpop,
    excepthandler, pattern, type_ignore, type_param,
})


def _obj2ast_check(node):
    """Required-field / position checks mimicking PyAST_obj2ast."""
    cls = type(node)
    # obj2ast reads a node's position attributes before converting its
    # child fields (test_bad_integer: ImportFrom(lineno=None) reports
    # "invalid integer value: None", not the alias's missing lineno).
    if "lineno" in cls._attributes:
        _validate_positions(node)
    field_types = getattr(cls, "_field_types", {})
    for name in cls._fields:
        value = getattr(node, name, _MISSING_FIELD)
        if value is _MISSING_FIELD or value is None:
            ft = field_types.get(name)
            # `object`-typed fields are ASDL `constant` — None is a value.
            required = (ft is not None and ft is not object
                        and not _is_list_field(ft) and not _is_optional_field(ft))
            if required:
                if value is _MISSING_FIELD:
                    raise TypeError(
                        f'required field "{name}" missing from {cls.__name__}')
                raise ValueError(f"field '{name}' is required for {cls.__name__}")
            continue
        # obj2ast_<sum> tries each concrete constructor and gives up on
        # a bare abstract node or a non-node ("expected some sort of
        # expr, but got expr()" — test_invalid_sum).
        base = _sum_base(field_types.get(name))
        if base is not None:
            for item in (value if isinstance(value, list) else (value,)):
                if item is not None and (type(item) is base
                                         or not isinstance(item, base)):
                    raise TypeError(
                        f"expected some sort of {base.__name__}, but got {item!r}")
        if isinstance(value, AST):
            _obj2ast_check(value)
        elif isinstance(value, list):
            for item in value:
                if isinstance(item, AST):
                    _obj2ast_check(item)


def _sum_base(field_type):
    """The ASDL sum class a field type names (`expr`, `expr | None`,
    `list[expr]`), or None for products and builtins."""
    if field_type is None:
        return None
    if _is_list_field(field_type):
        args = getattr(field_type, "__args__", ())
        field_type = args[0] if args else None
    elif _is_optional_field(field_type):
        args = [a for a in field_type.__args__ if a is not type(None)]
        field_type = args[0] if len(args) == 1 else None
    if field_type in _SUM_TYPES:
        return field_type
    return None


def _validate_name(name):
    if name in ("None", "True", "False"):
        raise ValueError(f"identifier field can't represent '{name}' constant")


def _validate_constant(value):
    # `...` literal, not the `Ellipsis` name: this module shadows it with
    # the deprecated ast.Ellipsis node class.
    if value is None or value is ...:
        return
    tp = type(value)
    if tp in (int, float, complex, bool, str, bytes):
        return
    if tp in (tuple, frozenset):
        for item in value:
            _validate_constant(item)
        return
    raise TypeError(f"got an invalid type in Constant: {tp.__name__}")


def _validate_exprs(exprs, ctx, null_ok):
    for e in exprs:
        if e is None:
            if null_ok:
                continue
            raise ValueError("None disallowed in expression list")
        _validate_expr(e, ctx)


def _validate_stmts(stmts):
    for s in stmts:
        if s is None:
            raise ValueError("None disallowed in statement list")
        _validate_stmt(s)


def _validate_body(body, owner):
    if not body:
        raise ValueError(f"empty body on {owner}")
    _validate_stmts(body)


def _validate_keywords(keywords):
    for k in keywords:
        _validate_expr(k.value, Load)


def _validate_arguments(args):
    for group in (args.posonlyargs, args.args, args.kwonlyargs):
        for a in group:
            if a.annotation is not None:
                _validate_expr(a.annotation, Load)
    for a in (args.vararg, args.kwarg):
        if a is not None and a.annotation is not None:
            _validate_expr(a.annotation, Load)
    if len(args.defaults) > len(args.posonlyargs) + len(args.args):
        raise ValueError("more positional defaults than args on arguments")
    if len(args.kw_defaults) != len(args.kwonlyargs):
        raise ValueError(
            "length of kwonlyargs is not the same as kw_defaults on arguments")
    _validate_exprs(args.defaults, Load, False)
    _validate_exprs(args.kw_defaults, Load, True)


def _validate_comprehension(gens):
    if not gens:
        raise ValueError("comprehension with no generators")
    for comp in gens:
        _validate_expr(comp.target, Store)
        _validate_expr(comp.iter, Load)
        _validate_exprs(comp.ifs, Load, False)


def _validate_type_params(type_params):
    for tp in type_params:
        if isinstance(tp, TypeVar):
            if tp.bound is not None:
                _validate_expr(tp.bound, Load)
        if getattr(tp, "default_value", None) is not None:
            _validate_expr(tp.default_value, Load)


def _validate_expr(exp, ctx):
    cls = type(exp)
    if cls in (Attribute, Subscript, Starred, Name, List, Tuple):
        actual = type(exp.ctx)
        if actual is not ctx:
            raise ValueError(
                f"expression must have {ctx.__name__} context but has "
                f"{actual.__name__} instead")
    elif ctx is not Load:
        raise ValueError(
            f"expression which can't be assigned to in {ctx.__name__} context")
    if cls is BoolOp:
        if len(exp.values) < 2:
            raise ValueError("BoolOp with less than 2 values")
        _validate_exprs(exp.values, Load, False)
    elif cls is BinOp:
        _validate_expr(exp.left, Load)
        _validate_expr(exp.right, Load)
    elif cls is UnaryOp:
        _validate_expr(exp.operand, Load)
    elif cls is Lambda:
        _validate_arguments(exp.args)
        _validate_expr(exp.body, Load)
    elif cls is IfExp:
        _validate_expr(exp.test, Load)
        _validate_expr(exp.body, Load)
        _validate_expr(exp.orelse, Load)
    elif cls is Dict:
        if len(exp.keys) != len(exp.values):
            raise ValueError(
                "Dict doesn't have the same number of keys as values")
        # None keys are `**` expansions.
        _validate_exprs(exp.keys, Load, True)
        _validate_exprs(exp.values, Load, False)
    elif cls is Set:
        _validate_exprs(exp.elts, Load, False)
    elif cls in (ListComp, SetComp, GeneratorExp):
        _validate_comprehension(exp.generators)
        _validate_expr(exp.elt, Load)
    elif cls is DictComp:
        _validate_comprehension(exp.generators)
        _validate_expr(exp.key, Load)
        _validate_expr(exp.value, Load)
    elif cls is Yield:
        if exp.value is not None:
            _validate_expr(exp.value, Load)
    elif cls in (YieldFrom, Await):
        _validate_expr(exp.value, Load)
    elif cls is Compare:
        if not exp.comparators:
            raise ValueError("Compare with no comparators")
        if len(exp.comparators) != len(exp.ops):
            raise ValueError(
                "Compare has a different number of comparators and operands")
        _validate_expr(exp.left, Load)
        _validate_exprs(exp.comparators, Load, False)
    elif cls is Call:
        _validate_expr(exp.func, Load)
        _validate_exprs(exp.args, Load, False)
        _validate_keywords(exp.keywords)
    elif cls is Constant:
        _validate_constant(exp.value)
    elif cls is JoinedStr:
        _validate_exprs(exp.values, Load, False)
    elif cls is FormattedValue:
        _validate_expr(exp.value, Load)
        if exp.format_spec is not None:
            _validate_expr(exp.format_spec, Load)
    elif cls is TemplateStr:
        _validate_exprs(exp.values, Load, False)
    elif cls is Interpolation:
        _validate_expr(exp.value, Load)
        if exp.format_spec is not None:
            _validate_expr(exp.format_spec, Load)
    elif cls is Attribute:
        _validate_expr(exp.value, Load)
    elif cls is Subscript:
        _validate_expr(exp.slice, Load)
        _validate_expr(exp.value, Load)
    elif cls is Starred:
        _validate_expr(exp.value, ctx)
    elif cls is Slice:
        for part in (exp.lower, exp.upper, exp.step):
            if part is not None:
                _validate_expr(part, Load)
    elif cls in (List, Tuple):
        _validate_exprs(exp.elts, ctx, False)
    elif cls is Name:
        _validate_name(exp.id)
    elif cls is NamedExpr:
        _validate_expr(exp.value, Load)


def _validate_capture(name):
    if name == "_":
        raise ValueError("can't capture name '_' in patterns")
    _validate_name(name)


def _validate_pattern_match_value(exp):
    _validate_expr(exp, Load)
    cls = type(exp)
    if cls is Constant:
        if type(exp.value) in (int, float, complex, str, bytes):
            return
        raise ValueError("unexpected constant inside of a literal pattern")
    if cls is Attribute:
        return
    if cls is UnaryOp and isinstance(exp.op, USub) \
            and type(exp.operand) is Constant \
            and type(exp.operand.value) in (int, float, complex):
        return
    if cls is BinOp and isinstance(exp.op, (Add, Sub)):
        # Complex literals: `case 1 + 2j` / `case -1 - 2j`.
        right = exp.right
        if type(right) is Constant and type(right.value) is complex:
            _validate_pattern_match_value(exp.left)
            return
    raise ValueError("patterns may only match literals and attribute lookups")


def _validate_patterns(patterns, star_ok):
    for p in patterns:
        _validate_pattern(p, star_ok)


def _validate_pattern(p, star_ok):
    cls = type(p)
    if cls is MatchValue:
        _validate_pattern_match_value(p.value)
    elif cls is MatchSingleton:
        if p.value is not True and p.value is not False and p.value is not None:
            raise ValueError(
                "MatchSingleton can only contain True, False and None")
    elif cls is MatchSequence:
        _validate_patterns(p.patterns, True)
    elif cls is MatchMapping:
        if len(p.keys) != len(p.patterns):
            raise ValueError(
                "MatchMapping doesn't have the same number of keys as patterns")
        if p.rest is not None:
            _validate_capture(p.rest)
        for key in p.keys:
            if type(key) is Constant and (key.value is None
                                          or key.value is True
                                          or key.value is False):
                continue
            _validate_pattern_match_value(key)
        _validate_patterns(p.patterns, False)
    elif cls is MatchClass:
        if len(p.kwd_attrs) != len(p.kwd_patterns):
            raise ValueError(
                "MatchClass doesn't have the same number of keyword "
                "attributes as patterns")
        _validate_expr(p.cls, Load)
        node = p.cls
        while type(node) is Attribute:
            node = node.value
        if type(node) is not Name:
            raise ValueError(
                "MatchClass cls field can only contain Name or Attribute "
                "nodes.")
        for ident in p.kwd_attrs:
            _validate_name(ident)
        _validate_patterns(p.patterns, False)
        _validate_patterns(p.kwd_patterns, False)
    elif cls is MatchStar:
        if not star_ok:
            raise ValueError("can't use MatchStar here")
        if p.name is not None:
            _validate_capture(p.name)
    elif cls is MatchAs:
        if p.name is not None:
            _validate_capture(p.name)
        if p.pattern is not None:
            if p.name is None:
                raise ValueError(
                    "MatchAs must specify a target name if a pattern is given")
            _validate_pattern(p.pattern, False)
    elif cls is MatchOr:
        if len(p.patterns) < 2:
            raise ValueError("MatchOr requires at least 2 patterns")
        _validate_patterns(p.patterns, False)


def _validate_stmt(s):
    cls = type(s)
    if cls in (FunctionDef, AsyncFunctionDef):
        _validate_body(s.body, cls.__name__)
        _validate_type_params(s.type_params)
        _validate_arguments(s.args)
        _validate_exprs(s.decorator_list, Load, False)
        if s.returns is not None:
            _validate_expr(s.returns, Load)
    elif cls is ClassDef:
        _validate_body(s.body, "ClassDef")
        _validate_type_params(s.type_params)
        _validate_exprs(s.bases, Load, False)
        _validate_keywords(s.keywords)
        _validate_exprs(s.decorator_list, Load, False)
    elif cls is Return:
        if s.value is not None:
            _validate_expr(s.value, Load)
    elif cls is Delete:
        if not s.targets:
            raise ValueError("empty targets on Delete")
        _validate_exprs(s.targets, Del, False)
    elif cls is Assign:
        if not s.targets:
            raise ValueError("empty targets on Assign")
        _validate_exprs(s.targets, Store, False)
        _validate_expr(s.value, Load)
    elif cls is AugAssign:
        _validate_expr(s.target, Store)
        _validate_expr(s.value, Load)
    elif cls is AnnAssign:
        if s.simple and type(s.target) is not Name:
            raise TypeError("AnnAssign with simple non-Name target")
        _validate_expr(s.target, Store)
        if s.value is not None:
            _validate_expr(s.value, Load)
        _validate_expr(s.annotation, Load)
    elif cls in (For, AsyncFor):
        _validate_expr(s.target, Store)
        _validate_expr(s.iter, Load)
        _validate_body(s.body, cls.__name__)
        _validate_stmts(s.orelse)
    elif cls is While:
        _validate_expr(s.test, Load)
        _validate_body(s.body, "While")
        _validate_stmts(s.orelse)
    elif cls is If:
        _validate_expr(s.test, Load)
        _validate_body(s.body, "If")
        _validate_stmts(s.orelse)
    elif cls in (With, AsyncWith):
        if not s.items:
            raise ValueError(f"empty items on {cls.__name__}")
        for item in s.items:
            _validate_expr(item.context_expr, Load)
            if item.optional_vars is not None:
                _validate_expr(item.optional_vars, Store)
        _validate_body(s.body, cls.__name__)
    elif cls is Match:
        _validate_expr(s.subject, Load)
        if not s.cases:
            raise ValueError("empty cases on Match")
        for case in s.cases:
            _validate_pattern(case.pattern, False)
            if case.guard is not None:
                _validate_expr(case.guard, Load)
            _validate_body(case.body, "match_case")
    elif cls is Raise:
        if s.exc is not None:
            _validate_expr(s.exc, Load)
            if s.cause is not None:
                _validate_expr(s.cause, Load)
        elif s.cause is not None:
            raise ValueError("Raise with cause but no exception")
    elif cls in (Try, TryStar):
        _validate_body(s.body, cls.__name__)
        if not s.handlers and not s.finalbody:
            raise ValueError(
                f"{cls.__name__} has neither except handlers nor finalbody")
        if not s.handlers and s.orelse:
            raise ValueError(
                f"{cls.__name__} has orelse but no except handlers")
        for handler in s.handlers:
            if handler.type is not None:
                _validate_expr(handler.type, Load)
            _validate_body(handler.body, "ExceptHandler")
        _validate_stmts(s.orelse)
        _validate_stmts(s.finalbody)
    elif cls is Assert:
        _validate_expr(s.test, Load)
        if s.msg is not None:
            _validate_expr(s.msg, Load)
    elif cls is Import:
        if not s.names:
            raise ValueError("empty names on Import")
    elif cls is ImportFrom:
        if s.level is not None and s.level < 0:
            raise ValueError("Negative ImportFrom level")
        if not s.names:
            raise ValueError("empty names on ImportFrom")
    elif cls is Global:
        if not s.names:
            raise ValueError("empty names on Global")
    elif cls is Nonlocal:
        if not s.names:
            raise ValueError("empty names on Nonlocal")
    elif cls is Expr:
        _validate_expr(s.value, Load)
    elif cls is TypeAlias:
        _validate_expr(s.name, Store)
        _validate_type_params(s.type_params)
        _validate_expr(s.value, Load)


def _validate(tree):
    """CPython obj2ast + _PyAST_Validate over a user-supplied node tree."""
    _obj2ast_check(tree)
    cls = type(tree)
    if cls in (Module, Interactive):
        _validate_stmts(tree.body)
    elif cls is Expression:
        _validate_expr(tree.body, Load)
    elif cls is FunctionType:
        _validate_exprs(tree.argtypes, Load, False)
        _validate_expr(tree.returns, Load)


def parse(source, filename='<unknown>', mode='exec', *,
          type_comments=False, feature_version=None, optimize=-1):
    """
    Parse the source into an AST node.
    Equivalent to compile(source, filename, mode, PyCF_ONLY_AST).
    Pass type_comments=True to get back type comments where the syntax allows.
    """
    flags = PyCF_ONLY_AST
    if optimize > 0:
        flags |= PyCF_OPTIMIZED_AST
    if type_comments:
        flags |= PyCF_TYPE_COMMENTS
    if feature_version is None:
        feature_version = -1
    elif isinstance(feature_version, tuple):
        major, minor = feature_version  # Should be a 2-tuple.
        if major != 3:
            raise ValueError(f"Unsupported major version: {major}")
        feature_version = minor
    # Else it should be an int giving the minor version for 3.x.
    if mode == 'func_type':
        # PEP 484 signature type comments parse under their own start
        # rule; the native `_ast` core handles it directly (the VM's
        # `compile()` intrinsic only routes exec/eval/single).
        tree = _from_spec(_ast.parse(source, filename, mode))
        if feature_version >= 0:
            _check_feature_version(tree, feature_version, filename, source)
        return tree
    tree = compile(source, filename, mode, flags,
                   _feature_version=feature_version, optimize=optimize)
    if feature_version >= 0 and isinstance(tree, AST):
        _check_feature_version(tree, feature_version, filename, source)
    return tree


def _tuple_parenthesized(node, source):
    """Does the Tuple's source span open with `(`? (Positions are UTF-8
    byte columns.) Unknown source reads as parenthesized."""
    if isinstance(source, str):
        source = source.encode("utf-8", "surrogatepass")
    if not isinstance(source, bytes):
        return True
    lineno = getattr(node, "lineno", None)
    col = getattr(node, "col_offset", None)
    if not isinstance(lineno, int) or not isinstance(col, int) or lineno < 1:
        return True
    lines = source.splitlines()
    if lineno > len(lines):
        return True
    return lines[lineno - 1][col:col + 1] == b"("


def _check_feature_version(tree, minor, filename, source=None):
    """Reject syntax newer than ``(3, minor)`` — the pure-Python analogue
    of pegen's `CHECK_VERSION` gates (only the constructs CPython's
    grammar actually versions)."""
    def bail(node, msg):
        raise SyntaxError(
            msg,
            (filename, getattr(node, "lineno", 1),
             getattr(node, "col_offset", 0) + 1, None),
        )

    for node in walk(tree):
        if minor < 14:
            if isinstance(node, TemplateStr):
                bail(node, "t-strings are only supported in Python 3.14 and greater")
            # PEP 758: `except A, B:` — a Tuple type whose source doesn't
            # open with a paren.
            if (isinstance(node, ExceptHandler) and isinstance(node.type, Tuple)
                    and not _tuple_parenthesized(node.type, source)):
                bail(node, "except expressions without parentheses are only "
                     "supported in Python 3.14 and greater")
        if minor < 12 and isinstance(node, TypeAlias):
            bail(node, "Type statement is only supported in Python 3.12 and greater")
        if isinstance(node, (FunctionDef, AsyncFunctionDef, ClassDef)):
            if minor < 12 and node.type_params:
                bail(node, "Type parameter lists are only supported in Python 3.12 and greater")
        if minor < 13 and isinstance(node, (TypeVar, TypeVarTuple, ParamSpec)):
            if getattr(node, "default_value", None) is not None:
                bail(node, "TypeVar default values are only supported in Python 3.13 and greater")
        if minor < 8:
            if isinstance(node, NamedExpr):
                bail(node, "Assignment expressions are only supported in Python 3.8 and greater")
            if isinstance(node, arguments) and node.posonlyargs:
                bail(node, "Positional-only parameters are only supported in Python 3.8 and greater")
        if minor < 10 and isinstance(node, Match):
            bail(node, "Pattern matching is only supported in Python 3.10 and greater")
        if minor < 11 and isinstance(node, TryStar):
            bail(node, "Exception groups are only supported in Python 3.11 and greater")
        if minor < 5:
            if isinstance(node, AsyncFunctionDef):
                bail(node, "Async functions are only supported in Python 3.5 and greater")
            if isinstance(node, AsyncFor):
                bail(node, "Async for loops are only supported in Python 3.5 and greater")
            if isinstance(node, AsyncWith):
                bail(node, "Async with statements are only supported in Python 3.5 and greater")
            if isinstance(node, Await):
                bail(node, "Await expressions are only supported in Python 3.5 and greater")
            if isinstance(node, BinOp) and isinstance(node.op, MatMult):
                bail(node, "The '@' operator is only supported in Python 3.5 and greater")
            if isinstance(node, AugAssign) and isinstance(node.op, MatMult):
                bail(node, "The '@=' operator is only supported in Python 3.5 and greater")
        if minor < 6 and isinstance(node, comprehension) and node.is_async:
            bail(node, "Async comprehensions are only supported in Python 3.6 and greater")
    if minor < 6:
        # Underscored numeric literals (3.6) are lexical, not structural:
        # rescan the source's NUMBER tokens.
        if isinstance(source, bytes):
            try:
                source = source.decode('utf-8')
            except UnicodeDecodeError:
                source = None
        if isinstance(source, str) and '_' in source:
            import io
            import tokenize as _tokenize
            try:
                for tok in _tokenize.generate_tokens(io.StringIO(source).readline):
                    if tok.type == _tokenize.NUMBER and '_' in tok.string:
                        raise SyntaxError(
                            "Underscores in numeric literals are only "
                            "supported in Python 3.6 and greater",
                            (filename, tok.start[0], tok.start[1] + 1, tok.line))
            except SyntaxError:
                raise
            except Exception:
                pass


def literal_eval(node_or_string):
    """
    Evaluate an expression node or a string containing only a Python
    expression.  The string or node provided may only consist of the following
    Python literal structures: strings, bytes, numbers, tuples, lists, dicts,
    sets, booleans, and None.

    Caution: A complex expression can overflow the C stack and cause a crash.
    """
    if isinstance(node_or_string, str):
        node_or_string = parse(node_or_string.lstrip(" \t"), mode='eval')
    if isinstance(node_or_string, Expression):
        node_or_string = node_or_string.body
    def _raise_malformed_node(node):
        msg = "malformed node or string"
        if lno := getattr(node, 'lineno', None):
            msg += f' on line {lno}'
        raise ValueError(msg + f': {node!r}')
    def _convert_num(node):
        if not isinstance(node, Constant) or type(node.value) not in (int, float, complex):
            _raise_malformed_node(node)
        return node.value
    def _convert_signed_num(node):
        if isinstance(node, UnaryOp) and isinstance(node.op, (UAdd, USub)):
            operand = _convert_num(node.operand)
            if isinstance(node.op, UAdd):
                return + operand
            else:
                return - operand
        return _convert_num(node)
    def _convert(node):
        if isinstance(node, Constant):
            return node.value
        elif isinstance(node, Tuple):
            return tuple(map(_convert, node.elts))
        elif isinstance(node, List):
            return list(map(_convert, node.elts))
        elif isinstance(node, Set):
            return set(map(_convert, node.elts))
        elif (isinstance(node, Call) and isinstance(node.func, Name) and
              node.func.id == 'set' and node.args == node.keywords == []):
            return set()
        elif isinstance(node, Dict):
            if len(node.keys) != len(node.values):
                _raise_malformed_node(node)
            return dict(zip(map(_convert, node.keys),
                            map(_convert, node.values)))
        elif isinstance(node, BinOp) and isinstance(node.op, (Add, Sub)):
            left = _convert_signed_num(node.left)
            right = _convert_num(node.right)
            if isinstance(left, (int, float)) and isinstance(right, complex):
                if isinstance(node.op, Add):
                    return left + right
                else:
                    return left - right
        return _convert_signed_num(node)
    return _convert(node_or_string)


def dump(
    node, annotate_fields=True, include_attributes=False,
    *,
    indent=None, show_empty=False,
):
    """
    Return a formatted dump of the tree in node.  This is mainly useful for
    debugging purposes.  If annotate_fields is true (by default),
    the returned string will show the names and the values for fields.
    If annotate_fields is false, the result string will be more compact by
    omitting unambiguous field names.  Attributes such as line
    numbers and column offsets are not dumped by default.  If this is wanted,
    include_attributes can be set to true.  If indent is a non-negative
    integer or string, then the tree will be pretty-printed with that indent
    level. None (the default) selects the single line representation.
    If show_empty is False, then empty lists and fields that are None
    will be omitted from the output for better readability.
    """
    def _format(node, level=0):
        if indent is not None:
            level += 1
            prefix = '\n' + indent * level
            sep = ',\n' + indent * level
        else:
            prefix = ''
            sep = ', '
        if isinstance(node, AST):
            cls = type(node)
            args = []
            args_buffer = []
            allsimple = True
            keywords = annotate_fields
            for name in node._fields:
                try:
                    value = getattr(node, name)
                except AttributeError:
                    keywords = True
                    continue
                if value is None and getattr(cls, name, ...) is None:
                    keywords = True
                    continue
                if not show_empty:
                    if value == []:
                        field_type = cls._field_types.get(name, object)
                        if getattr(field_type, '__origin__', ...) is list:
                            if not keywords:
                                args_buffer.append(repr(value))
                            continue
                    if not keywords:
                        args.extend(args_buffer)
                        args_buffer = []
                value, simple = _format(value, level)
                allsimple = allsimple and simple
                if keywords:
                    args.append('%s=%s' % (name, value))
                else:
                    args.append(value)
            if include_attributes and node._attributes:
                for name in node._attributes:
                    try:
                        value = getattr(node, name)
                    except AttributeError:
                        continue
                    if value is None and getattr(cls, name, ...) is None:
                        continue
                    value, simple = _format(value, level)
                    allsimple = allsimple and simple
                    args.append('%s=%s' % (name, value))
            if allsimple and len(args) <= 3:
                return '%s(%s)' % (node.__class__.__name__, ', '.join(args)), not args
            return '%s(%s%s)' % (node.__class__.__name__, prefix, sep.join(args)), False
        elif isinstance(node, list):
            if not node:
                return '[]', True
            return '[%s%s]' % (prefix, sep.join(_format(x, level)[0] for x in node)), False
        return repr(node), True

    if not isinstance(node, AST):
        raise TypeError('expected AST, got %r' % node.__class__.__name__)
    if indent is not None and not isinstance(indent, str):
        indent = ' ' * indent
    return _format(node)[0]


def copy_location(new_node, old_node):
    """
    Copy source location (`lineno`, `col_offset`, `end_lineno`, and `end_col_offset`
    attributes) from *old_node* to *new_node* if possible, and return *new_node*.
    """
    for attr in 'lineno', 'col_offset', 'end_lineno', 'end_col_offset':
        if attr in old_node._attributes and attr in new_node._attributes:
            value = getattr(old_node, attr, None)
            # end_lineno and end_col_offset are optional attributes, and they
            # should be copied whether the value is None or not.
            if value is not None or (
                hasattr(old_node, attr) and attr.startswith("end_")
            ):
                setattr(new_node, attr, value)
    return new_node


def fix_missing_locations(node):
    """
    When you compile a node tree with compile(), the compiler expects lineno and
    col_offset attributes for every node that supports them.  This is rather
    tedious to fill in for generated nodes, so this helper adds these attributes
    recursively where not already set, by setting them to the values of the
    parent node.  It works recursively starting at *node*.
    """
    def _fix(node, lineno, col_offset, end_lineno, end_col_offset):
        if 'lineno' in node._attributes:
            if not hasattr(node, 'lineno'):
                node.lineno = lineno
            else:
                lineno = node.lineno
        if 'end_lineno' in node._attributes:
            if getattr(node, 'end_lineno', None) is None:
                node.end_lineno = end_lineno
            else:
                end_lineno = node.end_lineno
        if 'col_offset' in node._attributes:
            if not hasattr(node, 'col_offset'):
                node.col_offset = col_offset
            else:
                col_offset = node.col_offset
        if 'end_col_offset' in node._attributes:
            if getattr(node, 'end_col_offset', None) is None:
                node.end_col_offset = end_col_offset
            else:
                end_col_offset = node.end_col_offset
        for child in iter_child_nodes(node):
            _fix(child, lineno, col_offset, end_lineno, end_col_offset)
    _fix(node, 1, 0, 1, 0)
    return node


def increment_lineno(node, n=1):
    """
    Increment the line number and end line number of each node in the tree
    starting at *node* by *n*. This is useful to "move code" to a different
    location in a file.
    """
    for child in walk(node):
        # TypeIgnore is a special case where lineno is not an attribute
        # but rather a field of the node itself.
        if isinstance(child, TypeIgnore):
            child.lineno = getattr(child, 'lineno', 0) + n
            continue

        if 'lineno' in child._attributes:
            child.lineno = getattr(child, 'lineno', 0) + n
        if (
            "end_lineno" in child._attributes
            and (end_lineno := getattr(child, "end_lineno", 0)) is not None
        ):
            child.end_lineno = end_lineno + n
    return node


def iter_fields(node):
    """
    Yield a tuple of ``(fieldname, value)`` for each field in ``node._fields``
    that is present on *node*.
    """
    for field in node._fields:
        try:
            yield field, getattr(node, field)
        except AttributeError:
            pass


def iter_child_nodes(node):
    """
    Yield all direct child nodes of *node*, that is, all fields that are nodes
    and all items of fields that are lists of nodes.
    """
    for name, field in iter_fields(node):
        if isinstance(field, AST):
            yield field
        elif isinstance(field, list):
            for item in field:
                if isinstance(item, AST):
                    yield item


def get_docstring(node, clean=True):
    """
    Return the docstring for the given node or None if no docstring can
    be found.  If the node provided does not have docstrings a TypeError
    will be raised.

    If *clean* is `True`, all tabs are expanded to spaces and any whitespace
    that can be uniformly removed from the second line onwards is removed.
    """
    if not isinstance(node, (AsyncFunctionDef, FunctionDef, ClassDef, Module)):
        raise TypeError("%r can't have docstrings" % node.__class__.__name__)
    if not(node.body and isinstance(node.body[0], Expr)):
        return None
    node = node.body[0].value
    if isinstance(node, Constant) and isinstance(node.value, str):
        text = node.value
    else:
        return None
    if clean:
        import inspect
        text = inspect.cleandoc(text)
    return text


_line_pattern = None
def _splitlines_no_ff(source, maxlines=None):
    """Split a string into lines ignoring form feed and other chars.

    This mimics how the Python parser splits source code.
    """
    global _line_pattern
    if _line_pattern is None:
        # lazily computed to speedup import time of `ast`
        import re
        _line_pattern = re.compile(r"(.*?(?:\r\n|\n|\r|$))")

    lines = []
    for lineno, match in enumerate(_line_pattern.finditer(source), 1):
        if maxlines is not None and lineno > maxlines:
            break
        lines.append(match[0])
    return lines


def _pad_whitespace(source):
    r"""Replace all chars except '\f\t' in a line with spaces."""
    result = ''
    for c in source:
        if c in '\f\t':
            result += c
        else:
            result += ' '
    return result


def get_source_segment(source, node, *, padded=False):
    """Get source code segment of the *source* that generated *node*.

    If some location information (`lineno`, `end_lineno`, `col_offset`,
    or `end_col_offset`) is missing, return None.

    If *padded* is `True`, the first line of a multi-line statement will
    be padded with spaces to match its original position.
    """
    try:
        if node.end_lineno is None or node.end_col_offset is None:
            return None
        lineno = node.lineno - 1
        end_lineno = node.end_lineno - 1
        col_offset = node.col_offset
        end_col_offset = node.end_col_offset
    except AttributeError:
        return None

    lines = _splitlines_no_ff(source, maxlines=end_lineno+1)
    if end_lineno == lineno:
        return lines[lineno].encode()[col_offset:end_col_offset].decode()

    if padded:
        padding = _pad_whitespace(lines[lineno].encode()[:col_offset].decode())
    else:
        padding = ''

    first = padding + lines[lineno].encode()[col_offset:].decode()
    last = lines[end_lineno].encode()[:end_col_offset].decode()
    lines = lines[lineno+1:end_lineno]

    lines.insert(0, first)
    lines.append(last)
    return ''.join(lines)


def walk(node):
    """
    Recursively yield all descendant nodes in the tree starting at *node*
    (including *node* itself), in no specified order.  This is useful if you
    only want to modify nodes in place and don't care about the context.
    """
    from collections import deque
    todo = deque([node])
    while todo:
        node = todo.popleft()
        todo.extend(iter_child_nodes(node))
        yield node


def compare(
    a,
    b,
    /,
    *,
    compare_attributes=False,
):
    """Recursively compares two ASTs.

    compare_attributes affects whether AST attributes are considered
    in the comparison. If compare_attributes is False (default), then
    attributes are ignored. Otherwise they must all be equal. This
    option is useful to check whether the ASTs are structurally equal but
    might differ in whitespace or similar details.
    """

    sentinel = object()  # handle the possibility of a missing attribute/field

    def _compare(a, b):
        # Compare two fields on an AST object, which may themselves be
        # AST objects, lists of AST objects, or primitive ASDL types
        # like identifiers and constants.
        if isinstance(a, AST):
            return compare(
                a,
                b,
                compare_attributes=compare_attributes,
            )
        elif isinstance(a, list):
            # If a field is repeated, then both objects will represent
            # the value as a list.
            if len(a) != len(b):
                return False
            for a_item, b_item in zip(a, b):
                if not _compare(a_item, b_item):
                    return False
            else:
                return True
        else:
            return type(a) is type(b) and a == b

    def _compare_fields(a, b):
        if a._fields != b._fields:
            return False
        for field in a._fields:
            a_field = getattr(a, field, sentinel)
            b_field = getattr(b, field, sentinel)
            if a_field is sentinel and b_field is sentinel:
                # both nodes are missing a field at runtime
                continue
            if a_field is sentinel or b_field is sentinel:
                # one of the node is missing a field
                return False
            if not _compare(a_field, b_field):
                return False
        else:
            return True

    def _compare_attributes(a, b):
        if a._attributes != b._attributes:
            return False
        # Attributes are always ints.
        for attr in a._attributes:
            a_attr = getattr(a, attr, sentinel)
            b_attr = getattr(b, attr, sentinel)
            if a_attr is sentinel and b_attr is sentinel:
                # both nodes are missing an attribute at runtime
                continue
            if a_attr != b_attr:
                return False
        else:
            return True

    if type(a) is not type(b):
        return False
    if not _compare_fields(a, b):
        return False
    if compare_attributes and not _compare_attributes(a, b):
        return False
    return True


class NodeVisitor(object):
    """
    A node visitor base class that walks the abstract syntax tree and calls a
    visitor function for every node found.  This function may return a value
    which is forwarded by the `visit` method.

    This class is meant to be subclassed, with the subclass adding visitor
    methods.

    Per default the visitor functions for the nodes are ``'visit_'`` +
    class name of the node.  So a `TryFinally` node visit function would
    be `visit_TryFinally`.  This behavior can be changed by overriding
    the `visit` method.  If no visitor function exists for a node
    (return value `None`) the `generic_visit` visitor is used instead.

    Don't use the `NodeVisitor` if you want to apply changes to nodes during
    traversing.  For this a special visitor exists (`NodeTransformer`) that
    allows modifications.
    """

    def visit(self, node):
        """Visit a node."""
        method = 'visit_' + node.__class__.__name__
        visitor = getattr(self, method, self.generic_visit)
        return visitor(node)

    def generic_visit(self, node):
        """Called if no explicit visitor function exists for a node."""
        for field, value in iter_fields(node):
            if isinstance(value, list):
                for item in value:
                    if isinstance(item, AST):
                        self.visit(item)
            elif isinstance(value, AST):
                self.visit(value)


class NodeTransformer(NodeVisitor):
    """
    A :class:`NodeVisitor` subclass that walks the abstract syntax tree and
    allows modification of nodes.

    The `NodeTransformer` will walk the AST and use the return value of the
    visitor methods to replace or remove the old node.  If the return value of
    the visitor method is ``None``, the node will be removed from its location,
    otherwise it is replaced with the return value.  The return value may be the
    original node in which case no replacement takes place.

    Here is an example transformer that rewrites all occurrences of name lookups
    (``foo``) to ``data['foo']``::

       class RewriteName(NodeTransformer):

           def visit_Name(self, node):
               return Subscript(
                   value=Name(id='data', ctx=Load()),
                   slice=Constant(value=node.id),
                   ctx=node.ctx
               )

    Keep in mind that if the node you're operating on has child nodes you must
    either transform the child nodes yourself or call the :meth:`generic_visit`
    method for the node first.

    For nodes that were part of a collection of statements (that applies to all
    statement nodes), the visitor may also return a list of nodes rather than
    just a single node.

    Usually you use the transformer like this::

       node = YourTransformer().visit(node)
    """

    def generic_visit(self, node):
        for field, old_value in iter_fields(node):
            if isinstance(old_value, list):
                new_values = []
                for value in old_value:
                    if isinstance(value, AST):
                        value = self.visit(value)
                        if value is None:
                            continue
                        elif not isinstance(value, AST):
                            new_values.extend(value)
                            continue
                    new_values.append(value)
                old_value[:] = new_values
            elif isinstance(old_value, AST):
                new_node = self.visit(old_value)
                if new_node is None:
                    delattr(node, field)
                else:
                    setattr(node, field, new_node)
        return node

class slice(AST):
    """Deprecated AST node class."""

class Index(slice):
    """Deprecated AST node class. Use the index value directly instead."""
    def __new__(cls, value, **kwargs):
        return value

class ExtSlice(slice):
    """Deprecated AST node class. Use ast.Tuple instead."""
    def __new__(cls, dims=(), **kwargs):
        return Tuple(list(dims), Load(), **kwargs)

# If the ast module is loaded more than once, only add deprecated methods once
if not hasattr(Tuple, 'dims'):
    # The following code is for backward compatibility.
    # It will be removed in future.

    def _dims_getter(self):
        """Deprecated. Use elts instead."""
        return self.elts

    def _dims_setter(self, value):
        self.elts = value

    Tuple.dims = property(_dims_getter, _dims_setter)

class Suite(mod):
    """Deprecated AST node class.  Unused in Python 3."""

class AugLoad(expr_context):
    """Deprecated AST node class.  Unused in Python 3."""

class AugStore(expr_context):
    """Deprecated AST node class.  Unused in Python 3."""

class Param(expr_context):
    """Deprecated AST node class.  Unused in Python 3."""


def unparse(ast_obj):
    global _Unparser
    try:
        unparser = _Unparser()
    except NameError:
        from _ast_unparse import Unparser as _Unparser
        unparser = _Unparser()
    return unparser.visit(ast_obj)


def main(args=None):
    import argparse
    import sys

    parser = argparse.ArgumentParser(color=True)
    parser.add_argument('infile', nargs='?', default='-',
                        help='the file to parse; defaults to stdin')
    parser.add_argument('-m', '--mode', default='exec',
                        choices=('exec', 'single', 'eval', 'func_type'),
                        help='specify what kind of code must be parsed')
    parser.add_argument('--no-type-comments', default=True, action='store_false',
                        help="don't add information about type comments")
    parser.add_argument('-a', '--include-attributes', action='store_true',
                        help='include attributes such as line numbers and '
                             'column offsets')
    parser.add_argument('-i', '--indent', type=int, default=3,
                        help='indentation of nodes (number of spaces)')
    parser.add_argument('--feature-version',
                        type=str, default=None, metavar='VERSION',
                        help='Python version in the format 3.x '
                             '(for example, 3.10)')
    parser.add_argument('-O', '--optimize',
                        type=int, default=-1, metavar='LEVEL',
                        help='optimization level for parser (default -1)')
    parser.add_argument('--show-empty', default=False, action='store_true',
                        help='show empty lists and fields in dump output')
    args = parser.parse_args(args)

    if args.infile == '-':
        name = '<stdin>'
        source = sys.stdin.buffer.read()
    else:
        name = args.infile
        with open(args.infile, 'rb') as infile:
            source = infile.read()

    # Process feature_version
    feature_version = None
    if args.feature_version:
        try:
            major, minor = map(int, args.feature_version.split('.', 1))
        except ValueError:
            parser.error('Invalid format for --feature-version; '
                         'expected format 3.x (for example, 3.10)')

        feature_version = (major, minor)

    tree = parse(source, name, args.mode, type_comments=args.no_type_comments,
                 feature_version=feature_version, optimize=args.optimize)
    print(dump(tree, include_attributes=args.include_attributes,
               indent=args.indent, show_empty=args.show_empty))


# ---------------------------------------------------------------------------
# PyCF_OPTIMIZED_AST (RFC 0052/0057) — the pure-Python analogue of
# CPython's AST-level constant folder (Python/ast_opt.c), covering the
# folds `test_ast.ASTOptimizationTests` asserts: constant binary/unary
# operations, `not (x in y)` fusions, all-constant Load tuples,
# list/set literals in iteration or `in`-comparison position,
# constant subscripts, and `'%s' % (...)`-to-fstring rewriting.
# ---------------------------------------------------------------------------

_FOLD_BINOP = {
    "Add": lambda a, b: a + b,
    "Sub": lambda a, b: a - b,
    "Mult": lambda a, b: a * b,
    "Div": lambda a, b: a / b,
    "FloorDiv": lambda a, b: a // b,
    "Mod": lambda a, b: a % b,
    "Pow": lambda a, b: a ** b,
    "LShift": lambda a, b: a << b,
    "RShift": lambda a, b: a >> b,
    "BitOr": lambda a, b: a | b,
    "BitXor": lambda a, b: a ^ b,
    "BitAnd": lambda a, b: a & b,
}

_FOLD_UNARYOP = {
    "Invert": lambda v: ~v,
    "Not": lambda v: not v,
    "UAdd": lambda v: +v,
    "USub": lambda v: -v,
}

# fold_unaryop: `not (a in b)` -> `a not in b` (and the is/==/!= family).
_FOLD_NOT_COMPARE = {
    "Is": IsNot,
    "IsNot": Is,
    "In": NotIn,
    "NotIn": In,
}


def _fold_result_ok(v):
    # Mirror ast_opt.c's "don't grow the code object" guards: cap folded
    # int/str/bytes sizes; allow the other constant-able types as-is.
    if isinstance(v, int):
        return v.bit_length() <= 256
    if isinstance(v, (str, bytes)):
        return len(v) <= 4096
    if isinstance(v, (tuple, frozenset)):
        return len(v) <= 256
    return v is None or isinstance(v, (bool, float, complex))


def _fold_args_ok(op_name, a, b):
    # Pre-guards so folding can't be tricked into huge computation
    # (10 ** 10**6, 1 << 10**6, 'x' * 10**6 …).
    if op_name in ("Pow", "LShift"):
        return isinstance(b, (int, bool)) and abs(b) <= 512 or isinstance(b, float)
    if op_name == "Mult":
        if isinstance(a, (str, bytes, tuple)) and isinstance(b, int):
            return len(a) * max(b, 0) <= 4096
        if isinstance(b, (str, bytes, tuple)) and isinstance(a, int):
            return len(b) * max(a, 0) <= 4096
    return True


def _fold_format_values(node):
    """`'%(fmt)s' % (a, b)` -> the JoinedStr value list, or None when the
    format string uses anything beyond %s/%r/%a/%% (ast_opt.c
    optimize_format)."""
    fmt = node.left.value
    elts = node.right.elts
    parts = []
    literal = []
    i = 0
    arg_i = 0
    while i < len(fmt):
        ch = fmt[i]
        i += 1
        if ch != "%":
            literal.append(ch)
            continue
        if i >= len(fmt):
            return None
        spec = fmt[i]
        i += 1
        if spec == "%":
            literal.append("%")
            continue
        if spec not in "sra":
            return None
        if arg_i >= len(elts):
            return None
        elt = elts[arg_i]
        arg_i += 1
        if isinstance(elt, Starred):
            return None
        if literal:
            parts.append(copy_location(Constant("".join(literal)), node))
            literal = []
        parts.append(copy_location(
            FormattedValue(value=elt, conversion=ord(spec), format_spec=None),
            node))
    if arg_i != len(elts):
        return None
    if literal:
        parts.append(copy_location(Constant("".join(literal)), node))
    return parts


def _fold_iterable(node):
    """A `List`/`Set` literal of constants in iteration / `in` position
    folds to a constant tuple / frozenset (ast_opt.c fold_iter)."""
    if isinstance(node, (List, Set)) and all(
            type(e) is Constant for e in node.elts):
        value = tuple(e.value for e in node.elts)
        if isinstance(node, Set):
            value = frozenset(value)
        if _fold_result_ok(value):
            return copy_location(Constant(value), node)
    return node


def _body_docstring(body):
    """`_PyAST_GetDocString`: the leading `Expr(Constant(str))`, if any."""
    if not body:
        return None
    st = body[0]
    if type(st) is Expr and type(st.value) is Constant \
            and type(st.value.value) is str:
        return st.value.value
    return None


class _ConstantFolder(NodeTransformer):
    # astfold_expr replaces a Load of `__debug__` with `not optimize`
    # (test_optimization_levels__debug__).
    _optimize = 1

    def visit_Name(self, node):
        if isinstance(node.ctx, Load) and node.id == "__debug__":
            return copy_location(Constant(not self._optimize), node)
        return node

    # astfold_body (3.14 ast_preprocess.c): at -OO the docstring goes
    # away — a lone docstring becomes `pass` (so the body stays
    # non-empty), otherwise the statement is dropped (gh-137308). A
    # body whose first statement only *became* a str constant through
    # folding is wrapped in a JoinedStr so codegen doesn't take it for
    # a docstring.
    def _fold_body(self, node):
        body = node.body
        docstring = _body_docstring(body) is not None
        if docstring and self._optimize >= 2:
            st = body[0]
            if len(body) == 1:
                body[0] = Pass(
                    lineno=st.lineno, col_offset=st.col_offset,
                    end_lineno=st.lineno, end_col_offset=st.col_offset + 4)
            else:
                del body[0]
            docstring = False
        self.generic_visit(node)
        body = node.body
        if not docstring and _body_docstring(body) is not None:
            st = body[0]
            st.value = copy_location(JoinedStr(values=[st.value]), st)
        return node

    visit_Module = _fold_body
    visit_FunctionDef = _fold_body
    visit_AsyncFunctionDef = _fold_body
    visit_ClassDef = _fold_body

    def visit_BinOp(self, node):
        self.generic_visit(node)
        left, right = node.left, node.right
        # `'%s' % (a,)` -> f-string (before the two-constant fold so a
        # non-constant tuple still rewrites).
        if (type(node.op) is Mod and type(left) is Constant
                and isinstance(left.value, str) and type(right) is Tuple):
            values = _fold_format_values(node)
            if values is not None:
                return copy_location(JoinedStr(values=values), node)
        if type(left) is Constant and type(right) is Constant:
            func = _FOLD_BINOP.get(type(node.op).__name__)
            if func is not None and _fold_args_ok(
                    type(node.op).__name__, left.value, right.value):
                try:
                    value = func(left.value, right.value)
                except Exception:
                    return node
                if _fold_result_ok(value):
                    return copy_location(Constant(value), node)
        return node

    def visit_UnaryOp(self, node):
        self.generic_visit(node)
        operand = node.operand
        if (type(node.op) is Not and type(operand) is Compare
                and len(operand.ops) == 1):
            inverted = _FOLD_NOT_COMPARE.get(type(operand.ops[0]).__name__)
            if inverted is not None:
                operand.ops = [copy_location(inverted(), operand.ops[0])]
                return operand
        if type(operand) is Constant:
            func = _FOLD_UNARYOP.get(type(node.op).__name__)
            if func is not None:
                try:
                    value = func(operand.value)
                except Exception:
                    return node
                if _fold_result_ok(value):
                    return copy_location(Constant(value), node)
        return node

    def visit_Tuple(self, node):
        self.generic_visit(node)
        if isinstance(node.ctx, Load) and all(
                type(e) is Constant for e in node.elts):
            value = tuple(e.value for e in node.elts)
            if _fold_result_ok(value):
                return copy_location(Constant(value), node)
        return node

    def visit_Compare(self, node):
        self.generic_visit(node)
        if node.ops and type(node.ops[-1]).__name__ in ("In", "NotIn"):
            node.comparators[-1] = _fold_iterable(node.comparators[-1])
        return node

    def visit_For(self, node):
        self.generic_visit(node)
        node.iter = _fold_iterable(node.iter)
        return node

    def visit_comprehension(self, node):
        self.generic_visit(node)
        node.iter = _fold_iterable(node.iter)
        return node

    def visit_Subscript(self, node):
        self.generic_visit(node)
        if (isinstance(node.ctx, Load) and type(node.value) is Constant
                and type(node.slice) is Constant):
            try:
                value = node.value.value[node.slice.value]
            except Exception:
                return node
            if _fold_result_ok(value):
                return copy_location(Constant(value), node)
        return node


def _fold_constants(tree, optimize=1):
    """Apply PyCF_OPTIMIZED_AST constant folding in place; returns the tree."""
    folder = _ConstantFolder()
    folder._optimize = optimize
    return folder.visit(tree)


def _export_node_classes_to_native():
    # In CPython the node classes are *defined* in the C `_ast` module and
    # `ast.py` star-imports them. WeavePy defines them here instead, so we
    # push them back onto `_ast` — code that does `import _ast` after `ast`
    # (e.g. `type(tree) == _ast.Module` in test_compile) sees the same
    # class objects. The `ast.py`-level deprecation shims (slice/Index/…)
    # stay out: CPython's `_ast` never had them.
    _shims = {"slice", "Index", "ExtSlice", "Suite", "AugLoad", "AugStore",
              "Param", "Num", "Str", "Bytes", "NameConstant", "Ellipsis"}
    for _name, _obj in list(globals().items()):
        if (isinstance(_obj, type) and issubclass(_obj, AST)
                and _name not in _shims):
            setattr(_ast, _name, _obj)
    _ast.AST = AST
    _ast.PyCF_ONLY_AST = PyCF_ONLY_AST
    _ast.PyCF_TYPE_COMMENTS = PyCF_TYPE_COMMENTS
    _ast.PyCF_ALLOW_TOP_LEVEL_AWAIT = PyCF_ALLOW_TOP_LEVEL_AWAIT
    _ast.PyCF_OPTIMIZED_AST = PyCF_OPTIMIZED_AST


_export_node_classes_to_native()
del _export_node_classes_to_native


if __name__ == '__main__':
    main()

