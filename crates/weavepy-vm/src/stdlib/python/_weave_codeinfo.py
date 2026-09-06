"""Code-object introspection behind `_testinternalcapi` (CPython 3.14).

Python ports of `Objects/codeobject.c`'s `_PyCode_GetVarCounts`,
`_PyCode_SetUnboundVarCounts`, `_PyCode_VerifyStateless`, and
`_PyCode_ReturnsOnlyNone`, plus the `_testinternalcapi` wrappers
(`get_code_var_counts`, `verify_stateless_code`, `get_co_localskinds`,
`code_returns_only_none`) that `test_code` drives. They read the same
inputs the C code does: `co_localspluskinds`, `co_names`, and the
de-specialized instruction stream.
"""

import types as _types

CO_FAST_ARG_POS = 0x02
CO_FAST_ARG_KW = 0x04
CO_FAST_ARG_VAR = 0x08
CO_FAST_HIDDEN = 0x10
CO_FAST_LOCAL = 0x20
CO_FAST_CELL = 0x40
CO_FAST_FREE = 0x80
CO_FAST_ARG = CO_FAST_ARG_POS | CO_FAST_ARG_KW | CO_FAST_ARG_VAR

CO_GENERATOR = 0x20
CO_COROUTINE = 0x80
CO_ITERABLE_COROUTINE = 0x100
CO_ASYNC_GENERATOR = 0x200


def _code_of(arg, globalsns, builtinsns):
    if isinstance(arg, _types.FunctionType):
        if globalsns is None:
            globalsns = arg.__globals__
        if builtinsns is None:
            builtinsns = arg.__builtins__
        arg = arg.__code__
    elif not isinstance(arg, _types.CodeType):
        raise TypeError("argument must be a code object or a function")
    return arg, globalsns, builtinsns


def _instructions(co):
    import dis

    return dis.get_instructions(co, adaptive=False)


def get_co_localskinds(code):
    if not isinstance(code, _types.CodeType):
        raise TypeError("argument must be a code object")
    kinds = code.co_localspluskinds
    return {
        name: kinds[i] for i, name in enumerate(code.co_localsplusnames)
    }


def _locals_counts(co):
    locals_ = {
        "total": 0,
        "args": {
            "total": 0,
            "numposonly": 0,
            "numposorkw": 0,
            "numkwonly": 0,
            "varargs": 0,
            "varkwargs": 0,
        },
        "numpure": 0,
        "cells": {"total": 0, "numargs": 0, "numothers": 0},
        "hidden": {"total": 0, "numpure": 0, "numcells": 0},
    }
    numfree = 0
    for kind in co.co_localspluskinds:
        if kind & CO_FAST_FREE:
            numfree += 1
            continue
        locals_["total"] += 1
        if kind & CO_FAST_ARG:
            args = locals_["args"]
            args["total"] += 1
            if kind & CO_FAST_ARG_VAR:
                if kind & CO_FAST_ARG_POS:
                    args["varargs"] = 1
                else:
                    args["varkwargs"] = 1
            elif kind & CO_FAST_ARG_POS:
                if kind & CO_FAST_ARG_KW:
                    args["numposorkw"] += 1
                else:
                    args["numposonly"] += 1
            else:
                args["numkwonly"] += 1
            if kind & CO_FAST_CELL:
                locals_["cells"]["total"] += 1
                locals_["cells"]["numargs"] += 1
        else:
            if kind & CO_FAST_CELL:
                locals_["cells"]["total"] += 1
                locals_["cells"]["numothers"] += 1
                if kind & CO_FAST_HIDDEN:
                    locals_["hidden"]["total"] += 1
                    locals_["hidden"]["numcells"] += 1
            else:
                locals_["numpure"] += 1
                if kind & CO_FAST_HIDDEN:
                    locals_["hidden"]["total"] += 1
                    locals_["hidden"]["numpure"] += 1
    return locals_, numfree


def _identify_unbound_names(co, globalnames, attrnames, globalsns, builtinsns):
    unbound = {
        "total": 0,
        "numattrs": 0,
        "numunknown": 0,
        "globals": {"total": 0, "numglobal": 0, "numbuiltin": 0, "numunknown": 0},
    }
    numdupes = 0
    for inst in _instructions(co):
        if inst.opname == "LOAD_ATTR":
            name = co.co_names[inst.arg >> 1]
            if name in attrnames:
                continue
            unbound["total"] += 1
            unbound["numattrs"] += 1
            attrnames.add(name)
            if name in globalnames:
                numdupes += 1
        elif inst.opname == "LOAD_GLOBAL":
            name = co.co_names[inst.arg >> 1]
            if name in globalnames:
                continue
            unbound["total"] += 1
            g = unbound["globals"]
            g["total"] += 1
            if globalsns is not None and name in globalsns:
                g["numglobal"] += 1
            elif builtinsns is not None and name in builtinsns:
                g["numbuiltin"] += 1
            else:
                g["numunknown"] += 1
            globalnames.add(name)
            if name in attrnames:
                numdupes += 1
    return unbound, numdupes


def _var_counts(co, globalnames, attrnames, globalsns, builtinsns):
    locals_, numfree = _locals_counts(co)
    numunbound = len(co.co_names)
    counts = {
        "total": locals_["total"] + numfree + numunbound,
        "locals": locals_,
        "numfree": numfree,
        "unbound": {
            "total": numunbound,
            "numattrs": 0,
            "numunknown": numunbound,
            "globals": {"total": 0, "numglobal": 0, "numbuiltin": 0, "numunknown": 0},
        },
    }
    # _PyCode_SetUnboundVarCounts
    if globalnames is None:
        globalnames = set()
    elif not isinstance(globalnames, (set, frozenset)):
        raise TypeError(
            f'expected a set for "globalnames", got {globalnames!r}'
        )
    if attrnames is None:
        attrnames = set()
    elif not isinstance(attrnames, (set, frozenset)):
        raise TypeError(f'expected a set for "attrnames", got {attrnames!r}')
    unbound, numdupes = _identify_unbound_names(
        co, globalnames, attrnames, globalsns, builtinsns
    )
    totalunbound = counts["unbound"]["total"] + numdupes
    unbound["numunknown"] = totalunbound - unbound["total"]
    unbound["total"] = totalunbound
    counts["unbound"] = unbound
    counts["total"] += numdupes
    return counts


def get_code_var_counts(
    code, globalnames=None, attrnames=None, globalsns=None, builtinsns=None
):
    if globalsns is not None and not isinstance(globalsns, dict):
        raise TypeError("get_code_var_counts() argument 4 must be dict")
    if builtinsns is not None and not isinstance(builtinsns, dict):
        raise TypeError("get_code_var_counts() argument 5 must be dict")
    co, globalsns, builtinsns = _code_of(code, globalsns, builtinsns)
    return _var_counts(co, globalnames, attrnames, globalsns, builtinsns)


def verify_stateless_code(code, globalnames=None, globalsns=None, builtinsns=None):
    if globalnames is not None and not isinstance(globalnames, set):
        raise TypeError("verify_stateless_code() argument 2 must be set")
    if globalsns is not None and not isinstance(globalsns, dict):
        raise TypeError("verify_stateless_code() argument 3 must be dict")
    if builtinsns is not None and not isinstance(builtinsns, dict):
        raise TypeError("verify_stateless_code() argument 4 must be dict")
    co, globalsns, builtinsns = _code_of(code, globalsns, builtinsns)
    counts = _var_counts(co, globalnames, None, globalsns, builtinsns)
    # _PyCode_CheckNoInternalState: co_extra is never set here.
    g = counts["unbound"]["globals"]
    numbuiltin = g["numbuiltin"]
    if builtinsns is not None:
        # Make sure the next check fails for globals, even if there
        # aren't any builtins.
        numbuiltin += 1
    if counts["numfree"] > 0:
        raise ValueError("closures not supported")
    if g["numglobal"] > 0:
        raise ValueError("globals not supported")
    if numbuiltin > 0 and g["numunknown"] > 0:
        raise ValueError("globals not supported")
    return None


def _is_pure_function(co):
    return not (
        co.co_flags
        & (CO_GENERATOR | CO_COROUTINE | CO_ITERABLE_COROUTINE | CO_ASYNC_GENERATOR)
    )


def code_returns_only_none(code):
    if not isinstance(code, _types.CodeType):
        raise TypeError("argument must be a code object")
    co = code
    if not _is_pure_function(co):
        return False
    insts = list(_instructions(co))
    if not insts:
        return True
    consts = co.co_consts
    none_index = next((i for i, c in enumerate(consts) if c is None), len(consts))
    returns = {"RETURN_VALUE"}
    if none_index == len(consts):
        final = insts[-1]
        if final.opname in returns:
            return False
        for inst in insts:
            if inst.opname in returns:
                return False
        return True
    for i, inst in enumerate(insts):
        if inst.opname in returns:
            prev = insts[i - 1] if i > 0 else None
            if prev is not None and prev.opname == "LOAD_CONST" and prev.arg == none_index:
                continue
            return False
    return True
