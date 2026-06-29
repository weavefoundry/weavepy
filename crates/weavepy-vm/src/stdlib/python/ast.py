"""Abstract Syntax Trees (WeavePy, RFC 0033).

A drop-in subset of CPython's :mod:`ast`. The node classes and the
public helpers (`parse`, `dump`, `walk`, `NodeVisitor`,
`NodeTransformer`, `literal_eval`, `get_docstring`, location helpers)
are pure Python; the one engine-level operation — turning source into a
tree — is delegated to the native :mod:`_ast` core, which runs WeavePy's
real lexer + parser and hands back a value-based spec tree.

The node-class hierarchy, ``_fields``, and ``_attributes`` are generated
from CPython 3.13, so ``ast.dump`` output and field access match.
"""

import _ast
import sys
from enum import IntEnum, auto
from contextlib import contextmanager, nullcontext


# ---------------------------------------------------------------------------
# Base node
# ---------------------------------------------------------------------------


class AST:
    _fields = ()
    _attributes = ()

    def __init__(self, *args, **kwargs):
        cls = type(self)
        if len(args) > len(cls._fields):
            raise TypeError(
                f"{cls.__name__} constructor takes at most "
                f"{len(cls._fields)} positional argument(s)"
            )
        for field, value in zip(cls._fields, args):
            setattr(self, field, value)
        for key, value in kwargs.items():
            setattr(self, key, value)

    def __repr__(self):
        parts = []
        for name in self._fields:
            if hasattr(self, name):
                parts.append(f"{name}={getattr(self, name)!r}")
        return f"{type(self).__name__}({', '.join(parts)})"


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

class slice(AST):
    _fields = ()

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

class BoolOp(expr):
    _fields = ('op', 'values', )
    _attributes = ('lineno', 'col_offset', 'end_lineno', 'end_col_offset', )

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


# ---------------------------------------------------------------------------
# Spec-tree -> node-instance builder
# ---------------------------------------------------------------------------

_NODE_TYPES = {
    name: obj
    for name, obj in list(globals().items())
    if isinstance(obj, type) and issubclass(obj, AST)
}

# PEP 634: AST nodes are matchable by position (`case ast.Expr(value)`).
# CPython generates `__match_args__ = _fields` on every node type.
for _node in _NODE_TYPES.values():
    _node.__match_args__ = _node._fields
del _node


def _build(spec):
    """Rebuild a node tree from the value-based spec produced by ``_ast``."""
    if isinstance(spec, dict):
        cls = _NODE_TYPES[spec["_type"]]
        node = cls()
        for key, value in spec.items():
            if key == "_type":
                continue
            setattr(node, key, _build(value))
        return node
    if isinstance(spec, list):
        return [_build(item) for item in spec]
    return spec


def _set_ctx(node, ctx):
    """Stamp `ctx` onto an expression appearing in a store/del position,
    recursing through tuple/list/starred targets. Attribute/Subscript only
    flip their own `ctx`; their `.value`/`.slice` stay `Load`."""
    kind = type(node)
    if kind in (Name, Attribute, Subscript, Starred, List, Tuple):
        node.ctx = ctx()
        if kind in (List, Tuple):
            for elt in node.elts:
                _set_ctx(elt, ctx)
        elif kind is Starred:
            _set_ctx(node.value, ctx)


def _fix_contexts(tree):
    """The WeavePy parser doesn't track expression contexts; reconstruct
    them from position so `ast.dump` matches CPython for Store/Del targets."""
    for n in walk(tree):
        kind = type(n)
        if kind is Assign:
            for target in n.targets:
                _set_ctx(target, Store)
        elif kind in (AugAssign, AnnAssign, NamedExpr):
            _set_ctx(n.target, Store)
        elif kind in (For, AsyncFor, comprehension):
            _set_ctx(n.target, Store)
        elif kind is Delete:
            for target in n.targets:
                _set_ctx(target, Del)
        elif kind in (With, AsyncWith):
            for item in n.items:
                if item.optional_vars is not None:
                    _set_ctx(item.optional_vars, Store)
    return tree


def parse(source, filename="<unknown>", mode="exec",
          type_comments=False, feature_version=None, optimize=-1):
    """Parse source into a CPython-shaped AST (RFC 0033)."""
    if isinstance(source, (bytes, bytearray)):
        source = bytes(source).decode("utf-8")
    spec = _ast.parse(source, filename, mode)
    tree = _fix_contexts(_build(spec))
    # Remember the original text so `compile(tree, ...)` can recompile it
    # (WeavePy compiles from source; an unmodified `ast.parse` round-trip
    # is by far the common case).
    try:
        tree._weavepy_source = source
    except Exception:
        pass
    return tree


# ---------------------------------------------------------------------------
# Traversal + rendering helpers
# ---------------------------------------------------------------------------


def iter_fields(node):
    for field in node._fields:
        if hasattr(node, field):
            yield field, getattr(node, field)


def iter_child_nodes(node):
    for _name, field in iter_fields(node):
        if isinstance(field, AST):
            yield field
        elif isinstance(field, list):
            for item in field:
                if isinstance(item, AST):
                    yield item


def walk(node):
    todo = [node]
    i = 0
    while i < len(todo):
        cur = todo[i]
        i += 1
        todo.extend(iter_child_nodes(cur))
        yield cur


_OMITTED = object()


def dump(node, annotate_fields=True, include_attributes=False, *,
         indent=None, show_empty=False):
    """Return a formatted dump of `node` (CPython 3.13 semantics).

    With ``show_empty=False`` (the default) empty lists and ``None`` fields
    are omitted. CPython consults ``cls._field_types`` to confirm an empty
    ``[]`` belongs to a list-typed field; in the AST schema an empty list
    value is *always* such a field, so the simplified check below matches.
    """
    if indent is not None and not isinstance(indent, str):
        indent = " " * indent

    def fmt(node, level=0):
        if indent is not None:
            level += 1
            prefix = "\n" + indent * level
            sep = ",\n" + indent * level
        else:
            prefix = ""
            sep = ", "
        if isinstance(node, AST):
            cls = type(node)
            args = []
            args_buffer = []
            allsimple = True
            keywords = annotate_fields
            for name in node._fields:
                if not hasattr(node, name):
                    keywords = True
                    continue
                value = getattr(node, name)
                if value is None and getattr(cls, name, _OMITTED) is None:
                    keywords = True
                    continue
                if not show_empty:
                    if value == []:
                        if not keywords:
                            args_buffer.append(repr(value))
                        continue
                    if not keywords:
                        args.extend(args_buffer)
                        args_buffer = []
                value, simple = fmt(value, level)
                allsimple = allsimple and simple
                if keywords:
                    args.append("%s=%s" % (name, value))
                else:
                    args.append(value)
            if include_attributes and node._attributes:
                for name in node._attributes:
                    if not hasattr(node, name):
                        continue
                    value = getattr(node, name)
                    if value is None and getattr(cls, name, _OMITTED) is None:
                        continue
                    value, simple = fmt(value, level)
                    allsimple = allsimple and simple
                    args.append("%s=%s" % (name, value))
            if allsimple and len(args) <= 3:
                return "%s(%s)" % (cls.__name__, ", ".join(args)), not args
            return "%s(%s%s)" % (cls.__name__, prefix, sep.join(args)), False
        elif isinstance(node, list):
            if not node:
                return "[]", True
            return "[%s%s]" % (prefix, sep.join(fmt(x, level)[0] for x in node)), False
        return repr(node), True

    if not isinstance(node, AST):
        raise TypeError("expected AST, got %r" % type(node).__name__)
    return fmt(node)[0]


def copy_location(new_node, old_node):
    for attr in ("lineno", "col_offset", "end_lineno", "end_col_offset"):
        if hasattr(old_node, attr):
            setattr(new_node, attr, getattr(old_node, attr))
    return new_node


def fix_missing_locations(node):
    def fix(node, lineno, col_offset, end_lineno, end_col_offset):
        if "lineno" in node._attributes:
            if not hasattr(node, "lineno"):
                node.lineno = lineno
            else:
                lineno = node.lineno
            if not hasattr(node, "col_offset"):
                node.col_offset = col_offset
            else:
                col_offset = node.col_offset
            if not hasattr(node, "end_lineno"):
                node.end_lineno = end_lineno
            else:
                end_lineno = node.end_lineno
            if not hasattr(node, "end_col_offset"):
                node.end_col_offset = end_col_offset
            else:
                end_col_offset = node.end_col_offset
        for child in iter_child_nodes(node):
            fix(child, lineno, col_offset, end_lineno, end_col_offset)

    fix(node, 1, 0, 1, 0)
    return node


def increment_lineno(node, n=1):
    for child in walk(node):
        if "lineno" in child._attributes and hasattr(child, "lineno"):
            child.lineno = child.lineno + n
        if "end_lineno" in child._attributes and getattr(child, "end_lineno", None) is not None:
            child.end_lineno = child.end_lineno + n
    return node


def get_docstring(node, clean=True):
    if not isinstance(node, (AsyncFunctionDef, FunctionDef, ClassDef, Module)):
        raise TypeError("%r can't have docstrings" % type(node).__name__)
    if not (node.body and isinstance(node.body[0], Expr)):
        return None
    value = node.body[0].value
    if isinstance(value, Constant) and isinstance(value.value, str):
        text = value.value
    else:
        return None
    if clean:
        text = _cleandoc(text)
    return text


def _cleandoc(doc):
    lines = doc.expandtabs().split("\n")
    margin = None
    for line in lines[1:]:
        stripped = line.lstrip()
        if stripped:
            indent_len = len(line) - len(stripped)
            margin = indent_len if margin is None else min(margin, indent_len)
    if lines:
        lines[0] = lines[0].lstrip()
    if margin is not None:
        for i in range(1, len(lines)):
            lines[i] = lines[i][margin:]
    while lines and not lines[-1]:
        lines.pop()
    while lines and not lines[0]:
        lines.pop(0)
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Visitors
# ---------------------------------------------------------------------------


class NodeVisitor:
    def visit(self, node):
        method = "visit_" + type(node).__name__
        visitor = getattr(self, method, self.generic_visit)
        return visitor(node)

    def generic_visit(self, node):
        for _field, value in iter_fields(node):
            if isinstance(value, list):
                for item in value:
                    if isinstance(item, AST):
                        self.visit(item)
            elif isinstance(value, AST):
                self.visit(value)


class NodeTransformer(NodeVisitor):
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


# ---------------------------------------------------------------------------
# literal_eval
# ---------------------------------------------------------------------------


def literal_eval(node_or_string):
    if isinstance(node_or_string, str):
        node_or_string = parse(node_or_string.lstrip(" \t"), mode="eval")
    if isinstance(node_or_string, Expression):
        node_or_string = node_or_string.body

    def _raise(node):
        raise ValueError("malformed node or string: " + repr(node))

    def _convert_num(node):
        if not isinstance(node, Constant) or type(node.value) not in (int, float, complex):
            _raise(node)
        return node.value

    def _convert_signed_num(node):
        if isinstance(node, UnaryOp) and isinstance(node.op, (UAdd, USub)):
            operand = _convert_num(node.operand)
            if isinstance(node.op, UAdd):
                return +operand
            return -operand
        return _convert_num(node)

    def _convert(node):
        if isinstance(node, Constant):
            return node.value
        elif isinstance(node, Tuple):
            return tuple(_convert(x) for x in node.elts)
        elif isinstance(node, List):
            return [_convert(x) for x in node.elts]
        elif isinstance(node, Set):
            return set(_convert(x) for x in node.elts)
        elif (isinstance(node, Call) and isinstance(node.func, Name)
              and node.func.id == "set" and not node.args and not node.keywords):
            return set()
        elif isinstance(node, Dict):
            if len(node.keys) != len(node.values):
                _raise(node)
            return {_convert(k): _convert(v) for k, v in zip(node.keys, node.values)}
        elif isinstance(node, BinOp) and isinstance(node.op, (Add, Sub)):
            left = _convert_signed_num(node.left)
            right = _convert_num(node.right)
            if isinstance(left, (int, float)) and isinstance(right, complex):
                if isinstance(node.op, Add):
                    return left + right
                return left - right
        return _convert_signed_num(node)

    return _convert(node_or_string)


# ---------------------------------------------------------------------------
# unparse() — AST -> source. Verbatim port of CPython 3.13's `_Unparser`
# (the `@_simple_enum(IntEnum)` optimization on `_Precedence` is expanded to
# a plain `IntEnum` subclass, which WeavePy's `enum` supports).
# ---------------------------------------------------------------------------


# Large float and imaginary literals get turned into infinities in the AST.
# We unparse those infinities to INFSTR.
_INFSTR = "1e" + repr(sys.float_info.max_10_exp + 1)

class _Precedence(IntEnum):
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
