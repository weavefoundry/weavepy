# RFC 0033: ``ast`` module drop-in.
#
# ``ast`` is among the highest-traffic missing modules — black, mypy,
# flake8, and pytest's assertion rewriting all import it. This
# exercises ``parse`` / ``dump`` / ``walk`` / ``NodeVisitor`` /
# ``NodeTransformer`` / ``literal_eval`` / ``get_docstring`` and the
# node ``_fields`` / location-attribute contract.

import ast

SRC = '''\
"""module docstring"""
import os
from collections import OrderedDict


def greet(name, *, excited=False):
    """greet docstring"""
    msg = "hi " + name
    if excited:
        msg = msg + "!"
    return msg


class Greeter:
    def __init__(self, n):
        self.n = n
'''

tree = ast.parse(SRC)
assert isinstance(tree, ast.Module)
assert isinstance(tree.body, list)

# ---------- node identity & fields ----------
func = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef))
assert func.name == "greet"
assert "name" in func._fields  # FunctionDef._fields includes 'name'
assert "lineno" in func._attributes

# ---------- location attributes ----------
assert func.lineno > 0
assert func.col_offset == 0
assert func.end_lineno >= func.lineno

# ---------- docstrings ----------
assert ast.get_docstring(tree) == "module docstring"
assert ast.get_docstring(func) == "greet docstring"

# ---------- walk / iter_child_nodes ----------
names = sorted({n.id for n in ast.walk(tree) if isinstance(n, ast.Name)})
assert "msg" in names and "name" in names and "excited" in names, names

classdef = next(n for n in ast.walk(tree) if isinstance(n, ast.ClassDef))
assert classdef.name == "Greeter"
child_funcs = [n for n in ast.iter_child_nodes(classdef)
               if isinstance(n, ast.FunctionDef)]
assert [f.name for f in child_funcs] == ["__init__"]

# ---------- imports ----------
imports = [n for n in ast.walk(tree) if isinstance(n, ast.Import)]
importfroms = [n for n in ast.walk(tree) if isinstance(n, ast.ImportFrom)]
assert imports[0].names[0].name == "os"
assert importfroms[0].module == "collections"

# ---------- ctx (Store vs Load) ----------
assigns = [n for n in ast.walk(tree) if isinstance(n, ast.Assign)]
target = assigns[0].targets[0]
assert isinstance(target.ctx, ast.Store), type(target.ctx)
load_name = next(n for n in ast.walk(tree)
                 if isinstance(n, ast.Name) and n.id == "name")
assert isinstance(load_name.ctx, ast.Load)

# ---------- dump round-trips structurally ----------
dumped = ast.dump(tree)
assert "FunctionDef" in dumped
assert "ClassDef" in dumped
assert dumped == ast.dump(ast.parse(SRC)), "dump must be deterministic"

# ---------- literal_eval ----------
assert ast.literal_eval("[1, 2, {'a': (3, 4)}]") == [1, 2, {"a": (3, 4)}]
assert ast.literal_eval("(True, None, -5, 2.5)") == (True, None, -5, 2.5)

# ---------- NodeVisitor ----------
class Collector(ast.NodeVisitor):
    def __init__(self):
        self.funcs = []

    def visit_FunctionDef(self, node):
        self.funcs.append(node.name)
        self.generic_visit(node)


c = Collector()
c.visit(tree)
assert "greet" in c.funcs and "__init__" in c.funcs, c.funcs

# ---------- NodeTransformer ----------
class Renamer(ast.NodeTransformer):
    def visit_Name(self, node):
        if node.id == "msg":
            node.id = "message"
        return node


expr = ast.parse("msg = 1\n")
Renamer().visit(expr)
assert any(isinstance(n, ast.Name) and n.id == "message"
           for n in ast.walk(expr))

# ---------- fix_missing_locations / increment_lineno helpers ----------
mod = ast.parse("x = 1\n")
ast.increment_lineno(mod, 5)
first = mod.body[0]
assert first.lineno == 6, first.lineno
ast.fix_missing_locations(mod)  # must not raise

print("test_ast_dropin: OK")
