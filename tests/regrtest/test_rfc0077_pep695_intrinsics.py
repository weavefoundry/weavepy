"""RFC 0077 WS10 — PEP 695 through CPython's intrinsics, one canary each.

Generic `def`/`class`/`type` statements compile to CPython 3.14's shape:
a `<generic parameters of X>` hidden scope that binds the type
parameters with `CALL_INTRINSIC_1`/`CALL_INTRINSIC_2` (TYPEVAR,
TYPEVAR_WITH_BOUND, TYPEVAR_WITH_CONSTRAINTS, PARAMSPEC, TYPEVARTUPLE,
SET_TYPEPARAM_DEFAULT, SUBSCRIPT_GENERIC, SET_FUNCTION_TYPE_PARAMS,
TYPEALIAS), lazy `(format, /)` thunks for bounds, defaults, and alias
values, and hoisted `.defaults`/`.kwdefaults` parameters. The old
`__weavepy_*__` builtin desugaring is gone.

1. Bytecode shape: the intrinsics, the hidden scope's name, the hoisted
   defaults, and `SET_FUNCTION_TYPE_PARAMS` on the produced function.
2. Scope analysis: an enclosing binding with a type parameter's name
   stays a plain local (the hidden scope binds the parameter); in a
   class-visible thunk, a name the class body binds resolves through the
   class dict and globals (`analyze_name`'s `class_entry` shortcut),
   never an enclosing function's cell, while nested lambdas still pass
   through; and a generic class's header mangles only its own type
   parameters, against the class itself.
3. Qualnames look through annotation scopes: a lambda inside an
   `__annotate__` or a bound thunk is named from the thunk's parent.
4. The 3.14 `evaluate_bound` / `evaluate_constraints` /
   `evaluate_default` / `evaluate_value` surface and `_ConstEvaluator`,
   plus `types.FunctionType` accepting `closure=` before `argdefs=`
   (how `annotationlib.call_evaluate_function` rebuilds a thunk).
"""

import dis
import types
import typing
from typing import ParamSpec, TypeAliasType, TypeVar, TypeVarTuple

# ------------- 1. bytecode shape -------------


def _codes(co):
    yield co
    for k in co.co_consts:
        if isinstance(k, types.CodeType):
            yield from _codes(k)


def _ops(co):
    return [(i.opname, i.argrepr) for i in dis.get_instructions(co)]


_src = """
def deco(f):
    return f

@deco
def gf[T: int, U: (int, str), *Ts, **P = [int]](x: T, y=1, *, z=2) -> T:
    return x

class Base:
    pass

class GC[T](Base, metaclass=type):
    attr: T

type Alias[K] = dict[K, GC[K]]
"""
_mod = compile(_src, "<pep695>", "exec")
_by_name = {c.co_qualname: c for c in _codes(_mod)}
assert "<generic parameters of gf>" in _by_name, sorted(_by_name)
assert "<generic parameters of GC>" in _by_name, sorted(_by_name)
assert "<generic parameters of Alias>" in _by_name, sorted(_by_name)
for _thunk in ("T", "U", "P", "Alias"):
    assert _by_name[_thunk].co_varnames == (".format",), (_thunk, _by_name[_thunk].co_varnames)

_hidden = _by_name["<generic parameters of gf>"]
# `.defaults` and `.kwdefaults` are the hidden scope's parameters.
assert _hidden.co_varnames[:2] == (".defaults", ".kwdefaults"), _hidden.co_varnames
assert _hidden.co_argcount == 2, _hidden.co_argcount
_ops_gf = _ops(_hidden)
_intrinsics = [arg for op, arg in _ops_gf if op.startswith("CALL_INTRINSIC")]
assert _intrinsics == [
    "INTRINSIC_TYPEVAR_WITH_BOUND",
    "INTRINSIC_TYPEVAR_WITH_CONSTRAINTS",
    "INTRINSIC_TYPEVARTUPLE",
    "INTRINSIC_PARAMSPEC",
    "INTRINSIC_SET_TYPEPARAM_DEFAULT",
    "INTRINSIC_SET_FUNCTION_TYPE_PARAMS",
], _intrinsics
_ops_gc = _ops(_by_name["<generic parameters of GC>"])
assert ("CALL_INTRINSIC_1", "INTRINSIC_SUBSCRIPT_GENERIC") in _ops_gc, _ops_gc
assert ("STORE_DEREF", ".type_params") in _ops_gc, _ops_gc
_ops_alias = _ops(_by_name["<generic parameters of Alias>"])
assert ("CALL_INTRINSIC_1", "INTRINSIC_TYPEALIAS") in _ops_alias, _ops_alias
# The enclosing scope evaluates the defaults and decorator, then calls
# the hidden scope with the defaults as arguments.
_mod_ops = [(i.opname, i.arg) for i in dis.get_instructions(_mod)]
assert ("LOAD_NAME", _mod.co_names.index("deco")) in _mod_ops, _mod_ops
_i = _mod_ops.index(("BUILD_MAP", 1))
assert [op for op, _ in _mod_ops[_i : _i + 7]] == [
    "BUILD_MAP",
    "SWAP",
    "LOAD_CONST",
    "MAKE_FUNCTION",
    "SWAP",
    "CALL",
    "CALL",
], _mod_ops[_i : _i + 7]
assert _mod_ops[_i + 4] == ("SWAP", 3) and _mod_ops[_i + 5] == ("CALL", 1), _mod_ops[_i : _i + 7]

_ns = {}
exec(_mod, _ns)
_gf = _ns["gf"]
_T, _U, _Ts, _P = _gf.__type_params__
assert (_T.__bound__, _U.__constraints__) == (int, (int, str)), (_T.__bound__, _U.__constraints__)
assert isinstance(_Ts, TypeVarTuple) and isinstance(_P, ParamSpec)
assert _P.__default__ == [int] and not _T.has_default()
assert _gf.__defaults__ == (1,) and _gf.__kwdefaults__ == {"z": 2}
assert _gf.__qualname__ == "gf" and _gf(5) == 5
_GC = _ns["GC"]
assert _GC.__type_params__ == (_GC.__orig_bases__[1].__parameters__[0],), _GC.__orig_bases__
assert _GC.__mro__[1] is _ns["Base"] and typing.Generic in _GC.__mro__
assert _GC.__annotations__ == {"attr": _GC.__type_params__[0]}
_Alias = _ns["Alias"]
assert isinstance(_Alias, TypeAliasType) and _Alias.__name__ == "Alias"
(_K,) = _Alias.__type_params__
assert _Alias.__value__ == dict[_K, _GC[_K]], _Alias.__value__

# ------------- 2. scope analysis -------------


def _outer():
    T = "outer"
    x = 1

    def f[T](a: T = x) -> T:
        return T

    class C[T]:
        y = x

        def g(self) -> T:
            return T

    return f, C, T


_f, _C, _T_outer = _outer()
assert _T_outer == "outer" and "T" not in _outer.__code__.co_cellvars, _outer.__code__.co_cellvars
assert _f.__type_params__[0] is _f() and _f.__defaults__ == (1,)
assert _C().g() is _C.__type_params__[0]


def _class_entry():
    T = 1
    V = 2

    class X:
        T = int
        V = str

        def foo[U: (lambda: T)(), W: V](self, x: T = V): ...

        class Y[Z: V]:
            pass

        type A[Q: T] = V

    return X


_X = _class_entry()
# `T` is captured by the lambda (a cell); `V` is only read directly by
# class-visible thunks, which resolve it through the class dict.
assert _class_entry.__code__.co_cellvars == ("T",), _class_entry.__code__.co_cellvars
_U, _W = _X.foo.__type_params__
assert _U.__bound__ == 1 and _W.__bound__ is str, (_U.__bound__, _W.__bound__)
assert _X.Y.__type_params__[0].__bound__ is str
(_Q,) = _X.A.__type_params__
assert _Q.__bound__ is int and _X.A.__value__ is str
assert _X.foo.__annotations__ == {"x": int}


_m_src = """
class Mangled:
    class N[__U: __T, __V = __W](__Base, kw=__Kw):
        pass

    def f[__T: __B](self, x: __T) -> __T:
        return x
"""
_m_by_name = {c.co_name: c for c in _codes(compile(_m_src, "<m>", "exec"))}
_hidden_N = _m_by_name["<generic parameters of N>"]
# A generic class's header mangles only the class's own type
# parameters, against the class itself (`ste_mangled_names`).
assert _hidden_N.co_cellvars == (".type_params",), _hidden_N.co_cellvars
assert set(_hidden_N.co_varnames) >= {"_N__U", "_N__V"}, _hidden_N.co_varnames
assert set(_hidden_N.co_names) >= {"__Base", "__Kw"}, _hidden_N.co_names
assert _m_by_name["__U"].co_names == ("__T",), _m_by_name["__U"].co_names
assert _m_by_name["__V"].co_names == ("__W",), _m_by_name["__V"].co_names
# A generic *method*'s parameters and bounds mangle against the
# enclosing class.
_hidden_f = _m_by_name["<generic parameters of f>"]
assert _hidden_f.co_cellvars == ("_Mangled__T",), _hidden_f.co_cellvars
assert _m_by_name["__T"].co_names == ("_Mangled__B",), _m_by_name["__T"].co_names


class _Mangled:
    def f[__T](self, x: __T) -> __T:
        return x


(_mT,) = _Mangled.f.__type_params__
assert _mT.__name__ == "__T" and _mT is _Mangled.f.__annotations__["x"]

# ------------- 3. qualnames look through annotation scopes -------------

_q_src = """
def outer():
    def f(x: (lambda: 1)()): pass
    class C:
        a: (lambda: 2)()
        def m[T: (lambda: 3)()](self): pass
def g[T: (lambda: 7)()](): pass
type A[T: (lambda: 8)()] = (lambda: 9)()
"""
_q_names = sorted(c.co_qualname for c in _codes(compile(_q_src, "<q>", "exec")) if c.co_name == "<lambda>")
assert _q_names == [
    "<generic parameters of A>.<lambda>",
    "<generic parameters of A>.<lambda>",
    "<generic parameters of g>.<lambda>",
    "outer.<locals>.<lambda>",
    "outer.<locals>.C.<generic parameters of m>.<lambda>",
    "outer.<locals>.C.<lambda>",
], _q_names

# ------------- 4. evaluate_* surface, _ConstEvaluator, FunctionType -------------

_TV = TypeVar("TV", bound=int)
assert repr(_TV.evaluate_bound) == "<constevaluator <class 'int'>>", repr(_TV.evaluate_bound)
assert _TV.evaluate_bound(1) is int and _TV.evaluate_bound(4) == "int"
assert _TV.evaluate_constraints is None
assert repr(_TV.evaluate_default) == "<constevaluator typing.NoDefault>"
_TC = TypeVar("TC", int, str)
assert _TC.evaluate_constraints(1) == (int, str) and _TC.evaluate_constraints(4) == "(int, str)"
assert TypeAliasType("A2", list[int]).evaluate_value(1) == list[int]
assert isinstance(_T.evaluate_bound, types.FunctionType) and _T.evaluate_bound(1) is int
assert _T.evaluate_default is None and _P.evaluate_default(1) == [int]
assert isinstance(_Alias.evaluate_value, types.FunctionType)
_CE = type(_TV.evaluate_bound)
assert _CE.__module__ == "_typing" and _CE.__name__ == "_ConstEvaluator"
for _bad in (lambda: _CE(), lambda: setattr(_CE, "attribute", 1)):
    try:
        _bad()
    except TypeError as e:
        assert "_typing._ConstEvaluator" in str(e), e
    else:
        raise AssertionError("_ConstEvaluator was constructible or mutable")

_thunk = _T.evaluate_bound
_rebuilt = types.FunctionType(
    _thunk.__code__,
    {"int": float},
    closure=_thunk.__closure__,
    argdefs=_thunk.__defaults__,
    kwdefaults=_thunk.__kwdefaults__,
)
assert _rebuilt(1) is float and _rebuilt() is float
try:
    types.FunctionType(_thunk.__code__, {}, None, (), argdefs=())
except TypeError as e:
    assert "given by name ('argdefs') and position (4)" in str(e), e
else:
    raise AssertionError("duplicate argdefs accepted")

print("rfc0077-pep695-intrinsics: ok")
