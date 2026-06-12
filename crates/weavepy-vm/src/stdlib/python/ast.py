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
