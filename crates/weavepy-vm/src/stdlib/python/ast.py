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
import re
import _ast
from contextlib import contextmanager, nullcontext
from enum import IntEnum, auto, _simple_enum

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
Invert._field_types = {}
Is._field_types = {}
IsNot._field_types = {}
JoinedStr._field_types = {'values': list[expr]}
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
Invert.__doc__ = 'Invert'
Is.__doc__ = 'Is'
IsNot.__doc__ = 'IsNot'
JoinedStr.__doc__ = 'JoinedStr(expr* values)'
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
        if isinstance(value, AST):
            _obj2ast_check(value)
        elif isinstance(value, list):
            for item in value:
                if isinstance(item, AST):
                    _obj2ast_check(item)


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


_line_pattern = re.compile(r"(.*?(?:\r\n|\n|\r|$))")
def _splitlines_no_ff(source, maxlines=None):
    """Split a string into lines ignoring form feed and other chars.

    This mimics how the Python parser splits source code.
    """
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

    def visit_Constant(self, node):
        value = node.value
        type_name = _const_node_type_names.get(type(value))
        if type_name is None:
            for cls, name in _const_node_type_names.items():
                if isinstance(value, cls):
                    type_name = name
                    break
        if type_name is not None:
            method = 'visit_' + type_name
            try:
                visitor = getattr(self, method)
            except AttributeError:
                pass
            else:
                import warnings
                warnings.warn(f"{method} is deprecated; add visit_Constant",
                              DeprecationWarning, 2)
                return visitor(node)
        return self.generic_visit(node)


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


_DEPRECATED_VALUE_ALIAS_MESSAGE = (
    "{name} is deprecated and will be removed in Python {remove}; use value instead"
)
_DEPRECATED_CLASS_MESSAGE = (
    "{name} is deprecated and will be removed in Python {remove}; "
    "use ast.Constant instead"
)


# If the ast module is loaded more than once, only add deprecated methods once
if not hasattr(Constant, 'n'):
    # The following code is for backward compatibility.
    # It will be removed in future.

    def _n_getter(self):
        """Deprecated. Use value instead."""
        import warnings
        warnings._deprecated(
            "Attribute n", message=_DEPRECATED_VALUE_ALIAS_MESSAGE, remove=(3, 14)
        )
        return self.value

    def _n_setter(self, value):
        import warnings
        warnings._deprecated(
            "Attribute n", message=_DEPRECATED_VALUE_ALIAS_MESSAGE, remove=(3, 14)
        )
        self.value = value

    def _s_getter(self):
        """Deprecated. Use value instead."""
        import warnings
        warnings._deprecated(
            "Attribute s", message=_DEPRECATED_VALUE_ALIAS_MESSAGE, remove=(3, 14)
        )
        return self.value

    def _s_setter(self, value):
        import warnings
        warnings._deprecated(
            "Attribute s", message=_DEPRECATED_VALUE_ALIAS_MESSAGE, remove=(3, 14)
        )
        self.value = value

    Constant.n = property(_n_getter, _n_setter)
    Constant.s = property(_s_getter, _s_setter)

class _ABC(type):

    def __init__(cls, *args):
        cls.__doc__ = """Deprecated AST node class. Use ast.Constant instead"""

    def __instancecheck__(cls, inst):
        if cls in _const_types:
            import warnings
            warnings._deprecated(
                f"ast.{cls.__qualname__}",
                message=_DEPRECATED_CLASS_MESSAGE,
                remove=(3, 14)
            )
        if not isinstance(inst, Constant):
            return False
        if cls in _const_types:
            try:
                value = inst.value
            except AttributeError:
                return False
            else:
                return (
                    isinstance(value, _const_types[cls]) and
                    not isinstance(value, _const_types_not.get(cls, ()))
                )
        return type.__instancecheck__(cls, inst)

def _new(cls, *args, **kwargs):
    for key in kwargs:
        if key not in cls._fields:
            # arbitrary keyword arguments are accepted
            continue
        pos = cls._fields.index(key)
        if pos < len(args):
            raise TypeError(f"{cls.__name__} got multiple values for argument {key!r}")
    if cls in _const_types:
        import warnings
        warnings._deprecated(
            f"ast.{cls.__qualname__}", message=_DEPRECATED_CLASS_MESSAGE, remove=(3, 14)
        )
        return Constant(*args, **kwargs)
    return Constant.__new__(cls, *args, **kwargs)

class Num(Constant, metaclass=_ABC):
    _fields = ('n',)
    __new__ = _new

class Str(Constant, metaclass=_ABC):
    _fields = ('s',)
    __new__ = _new

class Bytes(Constant, metaclass=_ABC):
    _fields = ('s',)
    __new__ = _new

class NameConstant(Constant, metaclass=_ABC):
    __new__ = _new

class Ellipsis(Constant, metaclass=_ABC):
    _fields = ()

    def __new__(cls, *args, **kwargs):
        if cls is _ast_Ellipsis:
            import warnings
            warnings._deprecated(
                "ast.Ellipsis", message=_DEPRECATED_CLASS_MESSAGE, remove=(3, 14)
            )
            return Constant(..., *args, **kwargs)
        return Constant.__new__(cls, *args, **kwargs)

# Keep another reference to Ellipsis in the global namespace
# so it can be referenced in Ellipsis.__new__
# (The original "Ellipsis" name is removed from the global namespace later on)
_ast_Ellipsis = Ellipsis

_const_types = {
    Num: (int, float, complex),
    Str: (str,),
    Bytes: (bytes,),
    NameConstant: (type(None), bool),
    Ellipsis: (type(...),),
}
_const_types_not = {
    Num: (bool,),
}

_const_node_type_names = {
    bool: 'NameConstant',  # should be before int
    type(None): 'NameConstant',
    int: 'Num',
    float: 'Num',
    complex: 'Num',
    str: 'Str',
    bytes: 'Bytes',
    type(...): 'Ellipsis',
}

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


# Large float and imaginary literals get turned into infinities in the AST.
# We unparse those infinities to INFSTR.
_INFSTR = "1e" + repr(sys.float_info.max_10_exp + 1)

@_simple_enum(IntEnum)
class _Precedence:
    """Precedence table that originated from python grammar."""

    NAMED_EXPR = auto()      # <target> := <expr1>
    TUPLE = auto()           # <expr1>, <expr2>
    YIELD = auto()           # 'yield', 'yield from'
    TEST = auto()            # 'if'-'else', 'lambda'
    OR = auto()              # 'or'
    AND = auto()             # 'and'
    NOT = auto()             # 'not'
    CMP = auto()             # '<', '>', '==', '>=', '<=', '!=',
                             # 'in', 'not in', 'is', 'is not'
    EXPR = auto()
    BOR = EXPR               # '|'
    BXOR = auto()            # '^'
    BAND = auto()            # '&'
    SHIFT = auto()           # '<<', '>>'
    ARITH = auto()           # '+', '-'
    TERM = auto()            # '*', '@', '/', '%', '//'
    FACTOR = auto()          # unary '+', '-', '~'
    POWER = auto()           # '**'
    AWAIT = auto()           # 'await'
    ATOM = auto()

    def next(self):
        try:
            return self.__class__(self + 1)
        except ValueError:
            return self


_SINGLE_QUOTES = ("'", '"')
_MULTI_QUOTES = ('"""', "'''")
_ALL_QUOTES = (*_SINGLE_QUOTES, *_MULTI_QUOTES)

class _Unparser(NodeVisitor):
    """Methods in this class recursively traverse an AST and
    output source code for the abstract syntax; original formatting
    is disregarded."""

    def __init__(self):
        self._source = []
        self._precedences = {}
        self._type_ignores = {}
        self._indent = 0
        self._in_try_star = False

    def interleave(self, inter, f, seq):
        """Call f on each item in seq, calling inter() in between."""
        seq = iter(seq)
        try:
            f(next(seq))
        except StopIteration:
            pass
        else:
            for x in seq:
                inter()
                f(x)

    def items_view(self, traverser, items):
        """Traverse and separate the given *items* with a comma and append it to
        the buffer. If *items* is a single item sequence, a trailing comma
        will be added."""
        if len(items) == 1:
            traverser(items[0])
            self.write(",")
        else:
            self.interleave(lambda: self.write(", "), traverser, items)

    def maybe_newline(self):
        """Adds a newline if it isn't the start of generated source"""
        if self._source:
            self.write("\n")

    def fill(self, text=""):
        """Indent a piece of text and append it, according to the current
        indentation level"""
        self.maybe_newline()
        self.write("    " * self._indent + text)

    def write(self, *text):
        """Add new source parts"""
        self._source.extend(text)

    @contextmanager
    def buffered(self, buffer = None):
        if buffer is None:
            buffer = []

        original_source = self._source
        self._source = buffer
        yield buffer
        self._source = original_source

    @contextmanager
    def block(self, *, extra = None):
        """A context manager for preparing the source for blocks. It adds
        the character':', increases the indentation on enter and decreases
        the indentation on exit. If *extra* is given, it will be directly
        appended after the colon character.
        """
        self.write(":")
        if extra:
            self.write(extra)
        self._indent += 1
        yield
        self._indent -= 1

    @contextmanager
    def delimit(self, start, end):
        """A context manager for preparing the source for expressions. It adds
        *start* to the buffer and enters, after exit it adds *end*."""

        self.write(start)
        yield
        self.write(end)

    def delimit_if(self, start, end, condition):
        if condition:
            return self.delimit(start, end)
        else:
            return nullcontext()

    def require_parens(self, precedence, node):
        """Shortcut to adding precedence related parens"""
        return self.delimit_if("(", ")", self.get_precedence(node) > precedence)

    def get_precedence(self, node):
        return self._precedences.get(node, _Precedence.TEST)

    def set_precedence(self, precedence, *nodes):
        for node in nodes:
            self._precedences[node] = precedence

    def get_raw_docstring(self, node):
        """If a docstring node is found in the body of the *node* parameter,
        return that docstring node, None otherwise.

        Logic mirrored from ``_PyAST_GetDocString``."""
        if not isinstance(
            node, (AsyncFunctionDef, FunctionDef, ClassDef, Module)
        ) or len(node.body) < 1:
            return None
        node = node.body[0]
        if not isinstance(node, Expr):
            return None
        node = node.value
        if isinstance(node, Constant) and isinstance(node.value, str):
            return node

    def get_type_comment(self, node):
        comment = self._type_ignores.get(node.lineno) or node.type_comment
        if comment is not None:
            return f" # type: {comment}"

    def traverse(self, node):
        if isinstance(node, list):
            for item in node:
                self.traverse(item)
        else:
            super().visit(node)

    # Note: as visit() resets the output text, do NOT rely on
    # NodeVisitor.generic_visit to handle any nodes (as it calls back in to
    # the subclass visit() method, which resets self._source to an empty list)
    def visit(self, node):
        """Outputs a source code string that, if converted back to an ast
        (using ast.parse) will generate an AST equivalent to *node*"""
        self._source = []
        self.traverse(node)
        return "".join(self._source)

    def _write_docstring_and_traverse_body(self, node):
        if (docstring := self.get_raw_docstring(node)):
            self._write_docstring(docstring)
            self.traverse(node.body[1:])
        else:
            self.traverse(node.body)

    def visit_Module(self, node):
        self._type_ignores = {
            ignore.lineno: f"ignore{ignore.tag}"
            for ignore in node.type_ignores
        }
        self._write_docstring_and_traverse_body(node)
        self._type_ignores.clear()

    def visit_FunctionType(self, node):
        with self.delimit("(", ")"):
            self.interleave(
                lambda: self.write(", "), self.traverse, node.argtypes
            )

        self.write(" -> ")
        self.traverse(node.returns)

    def visit_Expr(self, node):
        self.fill()
        self.set_precedence(_Precedence.YIELD, node.value)
        self.traverse(node.value)

    def visit_NamedExpr(self, node):
        with self.require_parens(_Precedence.NAMED_EXPR, node):
            self.set_precedence(_Precedence.ATOM, node.target, node.value)
            self.traverse(node.target)
            self.write(" := ")
            self.traverse(node.value)

    def visit_Import(self, node):
        self.fill("import ")
        self.interleave(lambda: self.write(", "), self.traverse, node.names)

    def visit_ImportFrom(self, node):
        self.fill("from ")
        self.write("." * (node.level or 0))
        if node.module:
            self.write(node.module)
        self.write(" import ")
        self.interleave(lambda: self.write(", "), self.traverse, node.names)

    def visit_Assign(self, node):
        self.fill()
        for target in node.targets:
            self.set_precedence(_Precedence.TUPLE, target)
            self.traverse(target)
            self.write(" = ")
        self.traverse(node.value)
        if type_comment := self.get_type_comment(node):
            self.write(type_comment)

    def visit_AugAssign(self, node):
        self.fill()
        self.traverse(node.target)
        self.write(" " + self.binop[node.op.__class__.__name__] + "= ")
        self.traverse(node.value)

    def visit_AnnAssign(self, node):
        self.fill()
        with self.delimit_if("(", ")", not node.simple and isinstance(node.target, Name)):
            self.traverse(node.target)
        self.write(": ")
        self.traverse(node.annotation)
        if node.value:
            self.write(" = ")
            self.traverse(node.value)

    def visit_Return(self, node):
        self.fill("return")
        if node.value:
            self.write(" ")
            self.traverse(node.value)

    def visit_Pass(self, node):
        self.fill("pass")

    def visit_Break(self, node):
        self.fill("break")

    def visit_Continue(self, node):
        self.fill("continue")

    def visit_Delete(self, node):
        self.fill("del ")
        self.interleave(lambda: self.write(", "), self.traverse, node.targets)

    def visit_Assert(self, node):
        self.fill("assert ")
        self.traverse(node.test)
        if node.msg:
            self.write(", ")
            self.traverse(node.msg)

    def visit_Global(self, node):
        self.fill("global ")
        self.interleave(lambda: self.write(", "), self.write, node.names)

    def visit_Nonlocal(self, node):
        self.fill("nonlocal ")
        self.interleave(lambda: self.write(", "), self.write, node.names)

    def visit_Await(self, node):
        with self.require_parens(_Precedence.AWAIT, node):
            self.write("await")
            if node.value:
                self.write(" ")
                self.set_precedence(_Precedence.ATOM, node.value)
                self.traverse(node.value)

    def visit_Yield(self, node):
        with self.require_parens(_Precedence.YIELD, node):
            self.write("yield")
            if node.value:
                self.write(" ")
                self.set_precedence(_Precedence.ATOM, node.value)
                self.traverse(node.value)

    def visit_YieldFrom(self, node):
        with self.require_parens(_Precedence.YIELD, node):
            self.write("yield from ")
            if not node.value:
                raise ValueError("Node can't be used without a value attribute.")
            self.set_precedence(_Precedence.ATOM, node.value)
            self.traverse(node.value)

    def visit_Raise(self, node):
        self.fill("raise")
        if not node.exc:
            if node.cause:
                raise ValueError(f"Node can't use cause without an exception.")
            return
        self.write(" ")
        self.traverse(node.exc)
        if node.cause:
            self.write(" from ")
            self.traverse(node.cause)

    def do_visit_try(self, node):
        self.fill("try")
        with self.block():
            self.traverse(node.body)
        for ex in node.handlers:
            self.traverse(ex)
        if node.orelse:
            self.fill("else")
            with self.block():
                self.traverse(node.orelse)
        if node.finalbody:
            self.fill("finally")
            with self.block():
                self.traverse(node.finalbody)

    def visit_Try(self, node):
        prev_in_try_star = self._in_try_star
        try:
            self._in_try_star = False
            self.do_visit_try(node)
        finally:
            self._in_try_star = prev_in_try_star

    def visit_TryStar(self, node):
        prev_in_try_star = self._in_try_star
        try:
            self._in_try_star = True
            self.do_visit_try(node)
        finally:
            self._in_try_star = prev_in_try_star

    def visit_ExceptHandler(self, node):
        self.fill("except*" if self._in_try_star else "except")
        if node.type:
            self.write(" ")
            self.traverse(node.type)
        if node.name:
            self.write(" as ")
            self.write(node.name)
        with self.block():
            self.traverse(node.body)

    def visit_ClassDef(self, node):
        self.maybe_newline()
        for deco in node.decorator_list:
            self.fill("@")
            self.traverse(deco)
        self.fill("class " + node.name)
        if hasattr(node, "type_params"):
            self._type_params_helper(node.type_params)
        with self.delimit_if("(", ")", condition = node.bases or node.keywords):
            comma = False
            for e in node.bases:
                if comma:
                    self.write(", ")
                else:
                    comma = True
                self.traverse(e)
            for e in node.keywords:
                if comma:
                    self.write(", ")
                else:
                    comma = True
                self.traverse(e)

        with self.block():
            self._write_docstring_and_traverse_body(node)

    def visit_FunctionDef(self, node):
        self._function_helper(node, "def")

    def visit_AsyncFunctionDef(self, node):
        self._function_helper(node, "async def")

    def _function_helper(self, node, fill_suffix):
        self.maybe_newline()
        for deco in node.decorator_list:
            self.fill("@")
            self.traverse(deco)
        def_str = fill_suffix + " " + node.name
        self.fill(def_str)
        if hasattr(node, "type_params"):
            self._type_params_helper(node.type_params)
        with self.delimit("(", ")"):
            self.traverse(node.args)
        if node.returns:
            self.write(" -> ")
            self.traverse(node.returns)
        with self.block(extra=self.get_type_comment(node)):
            self._write_docstring_and_traverse_body(node)

    def _type_params_helper(self, type_params):
        if type_params is not None and len(type_params) > 0:
            with self.delimit("[", "]"):
                self.interleave(lambda: self.write(", "), self.traverse, type_params)

    def visit_TypeVar(self, node):
        self.write(node.name)
        if node.bound:
            self.write(": ")
            self.traverse(node.bound)
        if node.default_value:
            self.write(" = ")
            self.traverse(node.default_value)

    def visit_TypeVarTuple(self, node):
        self.write("*" + node.name)
        if node.default_value:
            self.write(" = ")
            self.traverse(node.default_value)

    def visit_ParamSpec(self, node):
        self.write("**" + node.name)
        if node.default_value:
            self.write(" = ")
            self.traverse(node.default_value)

    def visit_TypeAlias(self, node):
        self.fill("type ")
        self.traverse(node.name)
        self._type_params_helper(node.type_params)
        self.write(" = ")
        self.traverse(node.value)

    def visit_For(self, node):
        self._for_helper("for ", node)

    def visit_AsyncFor(self, node):
        self._for_helper("async for ", node)

    def _for_helper(self, fill, node):
        self.fill(fill)
        self.set_precedence(_Precedence.TUPLE, node.target)
        self.traverse(node.target)
        self.write(" in ")
        self.traverse(node.iter)
        with self.block(extra=self.get_type_comment(node)):
            self.traverse(node.body)
        if node.orelse:
            self.fill("else")
            with self.block():
                self.traverse(node.orelse)

    def visit_If(self, node):
        self.fill("if ")
        self.traverse(node.test)
        with self.block():
            self.traverse(node.body)
        # collapse nested ifs into equivalent elifs.
        while node.orelse and len(node.orelse) == 1 and isinstance(node.orelse[0], If):
            node = node.orelse[0]
            self.fill("elif ")
            self.traverse(node.test)
            with self.block():
                self.traverse(node.body)
        # final else
        if node.orelse:
            self.fill("else")
            with self.block():
                self.traverse(node.orelse)

    def visit_While(self, node):
        self.fill("while ")
        self.traverse(node.test)
        with self.block():
            self.traverse(node.body)
        if node.orelse:
            self.fill("else")
            with self.block():
                self.traverse(node.orelse)

    def visit_With(self, node):
        self.fill("with ")
        self.interleave(lambda: self.write(", "), self.traverse, node.items)
        with self.block(extra=self.get_type_comment(node)):
            self.traverse(node.body)

    def visit_AsyncWith(self, node):
        self.fill("async with ")
        self.interleave(lambda: self.write(", "), self.traverse, node.items)
        with self.block(extra=self.get_type_comment(node)):
            self.traverse(node.body)

    def _str_literal_helper(
        self, string, *, quote_types=_ALL_QUOTES, escape_special_whitespace=False
    ):
        """Helper for writing string literals, minimizing escapes.
        Returns the tuple (string literal to write, possible quote types).
        """
        def escape_char(c):
            # \n and \t are non-printable, but we only escape them if
            # escape_special_whitespace is True
            if not escape_special_whitespace and c in "\n\t":
                return c
            # Always escape backslashes and other non-printable characters
            if c == "\\" or not c.isprintable():
                return c.encode("unicode_escape").decode("ascii")
            return c

        escaped_string = "".join(map(escape_char, string))
        possible_quotes = quote_types
        if "\n" in escaped_string:
            possible_quotes = [q for q in possible_quotes if q in _MULTI_QUOTES]
        possible_quotes = [q for q in possible_quotes if q not in escaped_string]
        if not possible_quotes:
            # If there aren't any possible_quotes, fallback to using repr
            # on the original string. Try to use a quote from quote_types,
            # e.g., so that we use triple quotes for docstrings.
            string = repr(string)
            quote = next((q for q in quote_types if string[0] in q), string[0])
            return string[1:-1], [quote]
        if escaped_string:
            # Sort so that we prefer '''"''' over """\""""
            possible_quotes.sort(key=lambda q: q[0] == escaped_string[-1])
            # If we're using triple quotes and we'd need to escape a final
            # quote, escape it
            if possible_quotes[0][0] == escaped_string[-1]:
                assert len(possible_quotes[0]) == 3
                escaped_string = escaped_string[:-1] + "\\" + escaped_string[-1]
        return escaped_string, possible_quotes

    def _write_str_avoiding_backslashes(self, string, *, quote_types=_ALL_QUOTES):
        """Write string literal value with a best effort attempt to avoid backslashes."""
        string, quote_types = self._str_literal_helper(string, quote_types=quote_types)
        quote_type = quote_types[0]
        self.write(f"{quote_type}{string}{quote_type}")

    def visit_JoinedStr(self, node):
        self.write("f")

        fstring_parts = []
        for value in node.values:
            with self.buffered() as buffer:
                self._write_fstring_inner(value)
            fstring_parts.append(
                ("".join(buffer), isinstance(value, Constant))
            )

        new_fstring_parts = []
        quote_types = list(_ALL_QUOTES)
        fallback_to_repr = False
        for value, is_constant in fstring_parts:
            if is_constant:
                value, new_quote_types = self._str_literal_helper(
                    value,
                    quote_types=quote_types,
                    escape_special_whitespace=True,
                )
                if set(new_quote_types).isdisjoint(quote_types):
                    fallback_to_repr = True
                    break
                quote_types = new_quote_types
            else:
                if "\n" in value:
                    quote_types = [q for q in quote_types if q in _MULTI_QUOTES]
                    assert quote_types

                new_quote_types = [q for q in quote_types if q not in value]
                if new_quote_types:
                    quote_types = new_quote_types
            new_fstring_parts.append(value)

        if fallback_to_repr:
            # If we weren't able to find a quote type that works for all parts
            # of the JoinedStr, fallback to using repr and triple single quotes.
            quote_types = ["'''"]
            new_fstring_parts.clear()
            for value, is_constant in fstring_parts:
                if is_constant:
                    value = repr('"' + value)  # force repr to use single quotes
                    expected_prefix = "'\""
                    assert value.startswith(expected_prefix), repr(value)
                    value = value[len(expected_prefix):-1]
                new_fstring_parts.append(value)

        value = "".join(new_fstring_parts)
        quote_type = quote_types[0]
        self.write(f"{quote_type}{value}{quote_type}")

    def _write_fstring_inner(self, node, is_format_spec=False):
        if isinstance(node, JoinedStr):
            # for both the f-string itself, and format_spec
            for value in node.values:
                self._write_fstring_inner(value, is_format_spec=is_format_spec)
        elif isinstance(node, Constant) and isinstance(node.value, str):
            value = node.value.replace("{", "{{").replace("}", "}}")

            if is_format_spec:
                value = value.replace("\\", "\\\\")
                value = value.replace("'", "\\'")
                value = value.replace('"', '\\"')
                value = value.replace("\n", "\\n")
            self.write(value)
        elif isinstance(node, FormattedValue):
            self.visit_FormattedValue(node)
        else:
            raise ValueError(f"Unexpected node inside JoinedStr, {node!r}")

    def visit_FormattedValue(self, node):
        def unparse_inner(inner):
            unparser = type(self)()
            unparser.set_precedence(_Precedence.TEST.next(), inner)
            return unparser.visit(inner)

        with self.delimit("{", "}"):
            expr = unparse_inner(node.value)
            if expr.startswith("{"):
                # Separate pair of opening brackets as "{ {"
                self.write(" ")
            self.write(expr)
            if node.conversion != -1:
                self.write(f"!{chr(node.conversion)}")
            if node.format_spec:
                self.write(":")
                self._write_fstring_inner(node.format_spec, is_format_spec=True)

    def visit_Name(self, node):
        self.write(node.id)

    def _write_docstring(self, node):
        self.fill()
        if node.kind == "u":
            self.write("u")
        self._write_str_avoiding_backslashes(node.value, quote_types=_MULTI_QUOTES)

    def _write_constant(self, value):
        if isinstance(value, (float, complex)):
            # Substitute overflowing decimal literal for AST infinities,
            # and inf - inf for NaNs.
            self.write(
                repr(value)
                .replace("inf", _INFSTR)
                .replace("nan", f"({_INFSTR}-{_INFSTR})")
            )
        else:
            self.write(repr(value))

    def visit_Constant(self, node):
        value = node.value
        if isinstance(value, tuple):
            with self.delimit("(", ")"):
                self.items_view(self._write_constant, value)
        elif value is ...:
            self.write("...")
        else:
            if node.kind == "u":
                self.write("u")
            self._write_constant(node.value)

    def visit_List(self, node):
        with self.delimit("[", "]"):
            self.interleave(lambda: self.write(", "), self.traverse, node.elts)

    def visit_ListComp(self, node):
        with self.delimit("[", "]"):
            self.traverse(node.elt)
            for gen in node.generators:
                self.traverse(gen)

    def visit_GeneratorExp(self, node):
        with self.delimit("(", ")"):
            self.traverse(node.elt)
            for gen in node.generators:
                self.traverse(gen)

    def visit_SetComp(self, node):
        with self.delimit("{", "}"):
            self.traverse(node.elt)
            for gen in node.generators:
                self.traverse(gen)

    def visit_DictComp(self, node):
        with self.delimit("{", "}"):
            self.traverse(node.key)
            self.write(": ")
            self.traverse(node.value)
            for gen in node.generators:
                self.traverse(gen)

    def visit_comprehension(self, node):
        if node.is_async:
            self.write(" async for ")
        else:
            self.write(" for ")
        self.set_precedence(_Precedence.TUPLE, node.target)
        self.traverse(node.target)
        self.write(" in ")
        self.set_precedence(_Precedence.TEST.next(), node.iter, *node.ifs)
        self.traverse(node.iter)
        for if_clause in node.ifs:
            self.write(" if ")
            self.traverse(if_clause)

    def visit_IfExp(self, node):
        with self.require_parens(_Precedence.TEST, node):
            self.set_precedence(_Precedence.TEST.next(), node.body, node.test)
            self.traverse(node.body)
            self.write(" if ")
            self.traverse(node.test)
            self.write(" else ")
            self.set_precedence(_Precedence.TEST, node.orelse)
            self.traverse(node.orelse)

    def visit_Set(self, node):
        if node.elts:
            with self.delimit("{", "}"):
                self.interleave(lambda: self.write(", "), self.traverse, node.elts)
        else:
            # `{}` would be interpreted as a dictionary literal, and
            # `set` might be shadowed. Thus:
            self.write('{*()}')

    def visit_Dict(self, node):
        def write_key_value_pair(k, v):
            self.traverse(k)
            self.write(": ")
            self.traverse(v)

        def write_item(item):
            k, v = item
            if k is None:
                # for dictionary unpacking operator in dicts {**{'y': 2}}
                # see PEP 448 for details
                self.write("**")
                self.set_precedence(_Precedence.EXPR, v)
                self.traverse(v)
            else:
                write_key_value_pair(k, v)

        with self.delimit("{", "}"):
            self.interleave(
                lambda: self.write(", "), write_item, zip(node.keys, node.values)
            )

    def visit_Tuple(self, node):
        with self.delimit_if(
            "(",
            ")",
            len(node.elts) == 0 or self.get_precedence(node) > _Precedence.TUPLE
        ):
            self.items_view(self.traverse, node.elts)

    unop = {"Invert": "~", "Not": "not", "UAdd": "+", "USub": "-"}
    unop_precedence = {
        "not": _Precedence.NOT,
        "~": _Precedence.FACTOR,
        "+": _Precedence.FACTOR,
        "-": _Precedence.FACTOR,
    }

    def visit_UnaryOp(self, node):
        operator = self.unop[node.op.__class__.__name__]
        operator_precedence = self.unop_precedence[operator]
        with self.require_parens(operator_precedence, node):
            self.write(operator)
            # factor prefixes (+, -, ~) shouldn't be separated
            # from the value they belong, (e.g: +1 instead of + 1)
            if operator_precedence is not _Precedence.FACTOR:
                self.write(" ")
            self.set_precedence(operator_precedence, node.operand)
            self.traverse(node.operand)

    binop = {
        "Add": "+",
        "Sub": "-",
        "Mult": "*",
        "MatMult": "@",
        "Div": "/",
        "Mod": "%",
        "LShift": "<<",
        "RShift": ">>",
        "BitOr": "|",
        "BitXor": "^",
        "BitAnd": "&",
        "FloorDiv": "//",
        "Pow": "**",
    }

    binop_precedence = {
        "+": _Precedence.ARITH,
        "-": _Precedence.ARITH,
        "*": _Precedence.TERM,
        "@": _Precedence.TERM,
        "/": _Precedence.TERM,
        "%": _Precedence.TERM,
        "<<": _Precedence.SHIFT,
        ">>": _Precedence.SHIFT,
        "|": _Precedence.BOR,
        "^": _Precedence.BXOR,
        "&": _Precedence.BAND,
        "//": _Precedence.TERM,
        "**": _Precedence.POWER,
    }

    binop_rassoc = frozenset(("**",))
    def visit_BinOp(self, node):
        operator = self.binop[node.op.__class__.__name__]
        operator_precedence = self.binop_precedence[operator]
        with self.require_parens(operator_precedence, node):
            if operator in self.binop_rassoc:
                left_precedence = operator_precedence.next()
                right_precedence = operator_precedence
            else:
                left_precedence = operator_precedence
                right_precedence = operator_precedence.next()

            self.set_precedence(left_precedence, node.left)
            self.traverse(node.left)
            self.write(f" {operator} ")
            self.set_precedence(right_precedence, node.right)
            self.traverse(node.right)

    cmpops = {
        "Eq": "==",
        "NotEq": "!=",
        "Lt": "<",
        "LtE": "<=",
        "Gt": ">",
        "GtE": ">=",
        "Is": "is",
        "IsNot": "is not",
        "In": "in",
        "NotIn": "not in",
    }

    def visit_Compare(self, node):
        with self.require_parens(_Precedence.CMP, node):
            self.set_precedence(_Precedence.CMP.next(), node.left, *node.comparators)
            self.traverse(node.left)
            for o, e in zip(node.ops, node.comparators):
                self.write(" " + self.cmpops[o.__class__.__name__] + " ")
                self.traverse(e)

    boolops = {"And": "and", "Or": "or"}
    boolop_precedence = {"and": _Precedence.AND, "or": _Precedence.OR}

    def visit_BoolOp(self, node):
        operator = self.boolops[node.op.__class__.__name__]
        operator_precedence = self.boolop_precedence[operator]

        def increasing_level_traverse(node):
            nonlocal operator_precedence
            operator_precedence = operator_precedence.next()
            self.set_precedence(operator_precedence, node)
            self.traverse(node)

        with self.require_parens(operator_precedence, node):
            s = f" {operator} "
            self.interleave(lambda: self.write(s), increasing_level_traverse, node.values)

    def visit_Attribute(self, node):
        self.set_precedence(_Precedence.ATOM, node.value)
        self.traverse(node.value)
        # Special case: 3.__abs__() is a syntax error, so if node.value
        # is an integer literal then we need to either parenthesize
        # it or add an extra space to get 3 .__abs__().
        if isinstance(node.value, Constant) and isinstance(node.value.value, int):
            self.write(" ")
        self.write(".")
        self.write(node.attr)

    def visit_Call(self, node):
        self.set_precedence(_Precedence.ATOM, node.func)
        self.traverse(node.func)
        with self.delimit("(", ")"):
            comma = False
            for e in node.args:
                if comma:
                    self.write(", ")
                else:
                    comma = True
                self.traverse(e)
            for e in node.keywords:
                if comma:
                    self.write(", ")
                else:
                    comma = True
                self.traverse(e)

    def visit_Subscript(self, node):
        def is_non_empty_tuple(slice_value):
            return (
                isinstance(slice_value, Tuple)
                and slice_value.elts
            )

        self.set_precedence(_Precedence.ATOM, node.value)
        self.traverse(node.value)
        with self.delimit("[", "]"):
            if is_non_empty_tuple(node.slice):
                # parentheses can be omitted if the tuple isn't empty
                self.items_view(self.traverse, node.slice.elts)
            else:
                self.traverse(node.slice)

    def visit_Starred(self, node):
        self.write("*")
        self.set_precedence(_Precedence.EXPR, node.value)
        self.traverse(node.value)

    def visit_Ellipsis(self, node):
        self.write("...")

    def visit_Slice(self, node):
        if node.lower:
            self.traverse(node.lower)
        self.write(":")
        if node.upper:
            self.traverse(node.upper)
        if node.step:
            self.write(":")
            self.traverse(node.step)

    def visit_Match(self, node):
        self.fill("match ")
        self.traverse(node.subject)
        with self.block():
            for case in node.cases:
                self.traverse(case)

    def visit_arg(self, node):
        self.write(node.arg)
        if node.annotation:
            self.write(": ")
            self.traverse(node.annotation)

    def visit_arguments(self, node):
        first = True
        # normal arguments
        all_args = node.posonlyargs + node.args
        defaults = [None] * (len(all_args) - len(node.defaults)) + node.defaults
        for index, elements in enumerate(zip(all_args, defaults), 1):
            a, d = elements
            if first:
                first = False
            else:
                self.write(", ")
            self.traverse(a)
            if d:
                self.write("=")
                self.traverse(d)
            if index == len(node.posonlyargs):
                self.write(", /")

        # varargs, or bare '*' if no varargs but keyword-only arguments present
        if node.vararg or node.kwonlyargs:
            if first:
                first = False
            else:
                self.write(", ")
            self.write("*")
            if node.vararg:
                self.write(node.vararg.arg)
                if node.vararg.annotation:
                    self.write(": ")
                    self.traverse(node.vararg.annotation)

        # keyword-only arguments
        if node.kwonlyargs:
            for a, d in zip(node.kwonlyargs, node.kw_defaults):
                self.write(", ")
                self.traverse(a)
                if d:
                    self.write("=")
                    self.traverse(d)

        # kwargs
        if node.kwarg:
            if first:
                first = False
            else:
                self.write(", ")
            self.write("**" + node.kwarg.arg)
            if node.kwarg.annotation:
                self.write(": ")
                self.traverse(node.kwarg.annotation)

    def visit_keyword(self, node):
        if node.arg is None:
            self.write("**")
        else:
            self.write(node.arg)
            self.write("=")
        self.traverse(node.value)

    def visit_Lambda(self, node):
        with self.require_parens(_Precedence.TEST, node):
            self.write("lambda")
            with self.buffered() as buffer:
                self.traverse(node.args)
            if buffer:
                self.write(" ", *buffer)
            self.write(": ")
            self.set_precedence(_Precedence.TEST, node.body)
            self.traverse(node.body)

    def visit_alias(self, node):
        self.write(node.name)
        if node.asname:
            self.write(" as " + node.asname)

    def visit_withitem(self, node):
        self.traverse(node.context_expr)
        if node.optional_vars:
            self.write(" as ")
            self.traverse(node.optional_vars)

    def visit_match_case(self, node):
        self.fill("case ")
        self.traverse(node.pattern)
        if node.guard:
            self.write(" if ")
            self.traverse(node.guard)
        with self.block():
            self.traverse(node.body)

    def visit_MatchValue(self, node):
        self.traverse(node.value)

    def visit_MatchSingleton(self, node):
        self._write_constant(node.value)

    def visit_MatchSequence(self, node):
        with self.delimit("[", "]"):
            self.interleave(
                lambda: self.write(", "), self.traverse, node.patterns
            )

    def visit_MatchStar(self, node):
        name = node.name
        if name is None:
            name = "_"
        self.write(f"*{name}")

    def visit_MatchMapping(self, node):
        def write_key_pattern_pair(pair):
            k, p = pair
            self.traverse(k)
            self.write(": ")
            self.traverse(p)

        with self.delimit("{", "}"):
            keys = node.keys
            self.interleave(
                lambda: self.write(", "),
                write_key_pattern_pair,
                zip(keys, node.patterns, strict=True),
            )
            rest = node.rest
            if rest is not None:
                if keys:
                    self.write(", ")
                self.write(f"**{rest}")

    def visit_MatchClass(self, node):
        self.set_precedence(_Precedence.ATOM, node.cls)
        self.traverse(node.cls)
        with self.delimit("(", ")"):
            patterns = node.patterns
            self.interleave(
                lambda: self.write(", "), self.traverse, patterns
            )
            attrs = node.kwd_attrs
            if attrs:
                def write_attr_pattern(pair):
                    attr, pattern = pair
                    self.write(f"{attr}=")
                    self.traverse(pattern)

                if patterns:
                    self.write(", ")
                self.interleave(
                    lambda: self.write(", "),
                    write_attr_pattern,
                    zip(attrs, node.kwd_patterns, strict=True),
                )

    def visit_MatchAs(self, node):
        name = node.name
        pattern = node.pattern
        if name is None:
            self.write("_")
        elif pattern is None:
            self.write(node.name)
        else:
            with self.require_parens(_Precedence.TEST, node):
                self.set_precedence(_Precedence.BOR, node.pattern)
                self.traverse(node.pattern)
                self.write(f" as {node.name}")

    def visit_MatchOr(self, node):
        with self.require_parens(_Precedence.BOR, node):
            self.set_precedence(_Precedence.BOR.next(), *node.patterns)
            self.interleave(lambda: self.write(" | "), self.traverse, node.patterns)

def unparse(ast_obj):
    unparser = _Unparser()
    return unparser.visit(ast_obj)


_deprecated_globals = {
    name: globals().pop(name)
    for name in ('Num', 'Str', 'Bytes', 'NameConstant', 'Ellipsis')
}

def __getattr__(name):
    if name in _deprecated_globals:
        globals()[name] = value = _deprecated_globals[name]
        import warnings
        warnings._deprecated(
            f"ast.{name}", message=_DEPRECATED_CLASS_MESSAGE, remove=(3, 14)
        )
        return value
    raise AttributeError(f"module 'ast' has no attribute '{name}'")



def main():
    import argparse

    parser = argparse.ArgumentParser(prog='python -m ast')
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
    args = parser.parse_args()

    if args.infile == '-':
        name = '<stdin>'
        source = sys.stdin.buffer.read()
    else:
        name = args.infile
        with open(args.infile, 'rb') as infile:
            source = infile.read()
    tree = parse(source, name, args.mode, type_comments=args.no_type_comments)
    print(dump(tree, include_attributes=args.include_attributes, indent=args.indent))

# NOTE: the `if __name__ == '__main__'` runner lives at the very end of
# this file — main() must not run before `_export_node_classes_to_native`
# publishes the node classes (the internal `ast.parse` import relies on
# it to share classes with a `weavepy -m ast` __main__ copy).

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


class _ConstantFolder(NodeTransformer):
    # astfold_expr replaces a Load of `__debug__` with `not optimize`
    # (test_optimization_levels__debug__).
    _optimize = 1

    def visit_Name(self, node):
        if isinstance(node.ctx, Load) and node.id == "__debug__":
            return copy_location(Constant(not self._optimize), node)
        return node

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
