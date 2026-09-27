"""Native AST field checks preserve validation, mutations, and callbacks."""
import _ast
import ast
import marshal
import sys
from types import SimpleNamespace


sources = [
    "value = 1 + 2\n",
    "def outer(x: int = 4) -> int:\n    def inner():\n        return x\n    return inner\n",
    "from __future__ import annotations\ndef f(x: Missing):\n    return x\n",
    "values = [x * 2 for x in range(5) if x]\n",
    "try:\n    1 / 0\nexcept ZeroDivisionError as error:\n    pass\nfinally:\n    pass\n",
    "class C:\n    @property\n    def value(self):\n        return 3\n",
    "async def f(xs):\n    async for x in xs:\n        await x\n",
    "match value:\n    case {'x': x, **rest}:\n        result = x\n",
    "type Pair[T] = tuple[T, T]\n",
    "text = f'{value!r:>8}'\n",
]
native = getattr(_ast, "_validate_fields", None)
events = []


def outcome(tree, mode="exec", optimize=0):
    try:
        code = compile(tree, "native_ast.py", mode, optimize=optimize)
    except Exception as error:
        return type(error).__name__, str(error), getattr(error, "lineno", None)
    return marshal.dumps(code)


def compare(factory, mode="exec", optimize=0):
    tree = factory()
    events.clear()
    actual = outcome(tree, mode, optimize)
    actual_events = events[:]
    if native is not None:
        tree = factory()
        events.clear()
        _ast._validate_fields = lambda *args: False
        try:
            expected = outcome(tree, mode, optimize)
            expected_events = events[:]
        finally:
            _ast._validate_fields = native
        assert actual == expected, (actual, expected)
        assert actual_events == expected_events, (actual_events, expected_events)
    return actual


for source in sources:
    for optimize in range(3):
        result = compare(lambda: ast.parse(source), optimize=optimize)
        assert isinstance(result, bytes), result
for mode in ("eval", "single"):
    assert isinstance(compare(lambda: ast.parse("1 + 2", mode=mode), mode), bytes)

simple = ast.parse("def f(x):\n    return x + 1\n")
if native is not None and sys._is_gil_enabled():
    assert native(simple, ast.AST, ast._SUM_TYPES), "ordinary nodes missed native validation"


def invalid(position, value):
    tree = ast.parse("result = 3")
    setattr(tree.body[0], position, value)
    return tree


for name, value in [("lineno", None), ("col_offset", None), ("end_lineno", 0),
                    ("end_col_offset", -1), ("lineno", "bad")]:
    assert not isinstance(compare(lambda: invalid(name, value)), bytes)


def missing():
    tree = ast.parse("result = 3")
    del tree.body[0].lineno
    return tree


assert not isinstance(compare(missing), bytes)


def wrong_expression():
    tree = ast.parse("result = 3")
    tree.body[0].value = 4
    return tree


assert not isinstance(compare(wrong_expression), bytes)


# Ordinary AST classes can also carry user attribute hooks. Preserve those
# calls without depending on the separate AST-subclass lowering path.
def watched_getattribute(self, name):
    events.append(name)
    return object.__getattribute__(self, name)


old_getattribute = ast.Constant.__dict__.get("__getattribute__")
try:
    ast.Constant.__getattribute__ = watched_getattribute
    assert isinstance(compare(lambda: ast.parse("3", mode="eval"), "eval"), bytes)
finally:
    if old_getattribute is None:
        del ast.Constant.__getattribute__
    else:
        ast.Constant.__getattribute__ = old_getattribute


def get_line(self):
    events.append("position")
    return self.__dict__["lineno"]


def set_line(self, value):
    self.__dict__["lineno"] = value


old_lineno = ast.Constant.__dict__.get("lineno")
try:
    ast.Constant.lineno = property(get_line, set_line)
    assert isinstance(compare(lambda: ast.parse("3", mode="eval"), "eval"), bytes)
finally:
    if old_lineno is None:
        del ast.Constant.lineno
    else:
        ast.Constant.lineno = old_lineno

# A shared node is valid, and mutation after an earlier check is visible.
shared = ast.Constant(value=2, lineno=1, col_offset=0, end_lineno=1, end_col_offset=1)
tree = ast.Expression(body=ast.BinOp(left=shared, op=ast.Add(), right=shared,
                                    lineno=1, col_offset=0, end_lineno=1, end_col_offset=3))
assert eval(compile(tree, "shared.py", "eval")) == 4
shared.value = 7
assert eval(compile(tree, "shared.py", "eval")) == 14

if native is not None:
    # These are checks of WeavePy's existing private validator behavior.
    # CPython's compiler doesn't consult Python _field_types dictionaries.
    old_types = ast.Constant._field_types
    try:
        for replacement in (ast.expr, list[ast.expr], ast.expr | None,
                            SimpleNamespace(__args__=(int,))):
            ast.Constant._field_types = dict(old_types, value=replacement)
            compare(lambda: ast.parse("3", mode="eval"), "eval")

        class FieldType:
            @property
            def __origin__(self):
                events.append("origin")
                return list

            @property
            def __args__(self):
                events.append("args")
                return (ast.expr,)

        ast.Constant._field_types = dict(old_types, value=FieldType())
        compare(lambda: ast.parse("3", mode="eval"), "eval")

        class Key:
            def __hash__(self):
                return hash("value")

            def __eq__(self, other):
                events.append("key")
                return other == "value"

        altered = dict(old_types)
        del altered["value"]
        altered[Key()] = object
        ast.Constant._field_types = altered
        compare(lambda: ast.parse("3", mode="eval"), "eval")
    finally:
        ast.Constant._field_types = old_types

    cyclic = ast.UnaryOp(op=ast.USub(), lineno=1, col_offset=0,
                         end_lineno=1, end_col_offset=1)
    cyclic.operand = cyclic
    assert not native(ast.Expression(body=cyclic), ast.AST, ast._SUM_TYPES)
    cyclic.operand = ast.Constant(value=1, lineno=1, col_offset=0,
                                  end_lineno=1, end_col_offset=1)

print("native AST field validation: ok")
