# RFC 0033: ``symtable`` module drop-in.
#
# ``symtable`` exposes CPython's scope analysis: which names are
# local / global / free / cell, parameters, and the nested block
# structure. This exercises the public ``SymbolTable`` / ``Symbol``
# surface over WeavePy's native ``_symtable`` analyzer.

import symtable

SRC = '''\
GLOBAL_CONST = 1


def outer(a, b, *args, kw=0, **kwargs):
    captured = a + b

    def inner():
        return captured + GLOBAL_CONST

    return inner


class C:
    attr = 10

    def method(self):
        return self.attr
'''

top = symtable.symtable(SRC, "<symtable-test>", "exec")

# ---------- module table ----------
assert top.get_type() == "module", top.get_type()
ids = set(top.get_identifiers())
assert "GLOBAL_CONST" in ids
assert "outer" in ids
assert "C" in ids

# ---------- function table ----------
outer = top.lookup("outer").get_namespace()
assert outer.get_type() == "function"
params = set(outer.get_parameters())
assert params == {"a", "b", "args", "kw", "kwargs"}, params

# `captured` is a local that is also a cell (captured by `inner`).
captured = outer.lookup("captured")
assert captured.is_local()
assert captured.is_namespace() is False
# It is referenced by a nested function, so it must be a cell var.
frees_in_inner = None
for child in outer.get_children():
    if child.get_name() == "inner":
        frees_in_inner = set(child.get_frees())
assert frees_in_inner is not None
assert "captured" in frees_in_inner, frees_in_inner

# `GLOBAL_CONST` used inside inner resolves to a global, not a free.
inner_tab = [c for c in outer.get_children() if c.get_name() == "inner"][0]
gc = inner_tab.lookup("GLOBAL_CONST")
assert gc.is_global(), "module-level name is global inside nested fn"
assert not gc.is_local()

# ---------- parameter symbols ----------
a_sym = outer.lookup("a")
assert a_sym.is_parameter()
assert a_sym.is_local()

# ---------- class table ----------
cls = top.lookup("C").get_namespace()
assert cls.get_type() == "class"
methods = set(cls.get_methods())
assert "method" in methods, methods
assert "attr" in set(cls.get_identifiers())

# ---------- get_symbols ----------
syms = {s.get_name() for s in top.get_symbols()}
assert "outer" in syms and "C" in syms

# ---------- nested-name lookup is local to its block ----------
method_tab = cls.lookup("method").get_namespace()
assert method_tab.get_type() == "function"
assert "self" in set(method_tab.get_parameters())

print("test_symtable_dropin: OK")
