"""Compiling a Python AST leaves its nodes and metadata unchanged."""
import ast
import contextlib
import io

cases = [
    ("exec", "'module doc'\nvalue = (1 + 2, 3 * 4)\nassert __debug__\n"),
    ("eval", "(1 + 2, 3 * 4)"),
    ("single", "(1 + 2, 3 * 4)\n"),
]
for mode, source in cases:
    tree = ast.parse(source, filename="owned_ast.py", mode=mode)
    before = ast.dump(tree, include_attributes=True)
    for optimize in range(3):
        code = compile(tree, "owned_ast.py", mode, optimize=optimize)
        assert ast.dump(tree, include_attributes=True) == before
        namespace = {}
        if mode == "eval":
            assert eval(code, namespace) == (3, 12)
        elif mode == "single":
            stream = io.StringIO()
            with contextlib.redirect_stdout(stream):
                exec(code, namespace)
            assert stream.getvalue() == "(3, 12)\n"
        else:
            exec(code, namespace)
            assert namespace["value"] == (3, 12)
            assert namespace.get("__doc__") == (None if optimize == 2 else "module doc")

source = "from __future__ import annotations\ndef outer(x: int):\n    def inner():\n        return x + 1\n    return inner\n"
tree = ast.parse(source)
before = ast.dump(tree, include_attributes=True)
namespace = {}
exec(compile(tree, "closures.py", "exec"), namespace)
assert namespace["outer"](4)() == 5
assert namespace["outer"].__annotations__ == {"x": "int"}
assert ast.dump(tree, include_attributes=True) == before

# Reusing a tree after compilation must observe later caller mutations.
tree = ast.parse("1 + 2", mode="eval")
first = compile(tree, "reused.py", "eval")
tree.body.right.value = 9
second = compile(tree, "reused.py", "eval")
assert eval(first) == 3
assert eval(second) == 10
assert isinstance(tree.body, ast.BinOp)
assert tree.body.right.value == 9

for source in ["return 1", "break", "def f():\n    value = 1\n    global value\n"]:
    tree = ast.parse(source)
    before = ast.dump(tree, include_attributes=True)
    try:
        compile(tree, "invalid.py", "exec")
    except SyntaxError:
        pass
    else:
        raise AssertionError("invalid AST compiled")
    assert ast.dump(tree, include_attributes=True) == before
print("owned AST compilation: ok")
