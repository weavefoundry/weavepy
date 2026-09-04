#!/usr/bin/env python3
"""Generate the bundled ``_opcode`` module from CPython's opcode metadata.

CPython implements ``_opcode`` in C over the tables the cases generator
writes into ``Include/internal/pycore_opcode_metadata.h``: per-opcode
flag words (``HAS_ARG_FLAG`` and friends), the ``_PyOpcode_num_popped`` /
``_PyOpcode_num_pushed`` stack-effect switches, and the ``_PyOpcode_Deopt``
table that maps specialized forms back to their base instruction. WeavePy
has no C ``_opcode``, so this script transliterates those tables into a
pure-Python module with the same public surface (RFC 0077 WS9):

    stack_effect, is_valid, has_arg, has_const, has_name, has_jump,
    has_free, has_local, has_exc, get_specialization_stats, get_executor,
    get_nb_ops, get_intrinsic1_descs, get_intrinsic2_descs,
    get_special_method_names, ENABLE_SPECIALIZATION,
    ENABLE_SPECIALIZATION_FT

Opcode *numbers* are not repeated here: the generated module resolves
names through ``_opcode_metadata`` (vendored verbatim from ``Lib``), so
the two files can only drift if the header and ``Lib`` disagree, which
they never do inside one CPython tag.

Usage::

    tools/gen_opcode_shim.py <path/to/pycore_opcode_metadata.h> \
        [--version 3.14.7] [-o crates/weavepy-vm/src/stdlib/python/_opcode.py]

The stack-effect expressions in the header are plain C arithmetic over
``oparg`` (``1 + (oparg & 0xFF) + (oparg >> 8)``, ``oparg*2`` ...), which
is also valid Python with identical results for the non-negative
``oparg`` values the compiler produces; they are emitted as lambdas.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

FLAG_NAMES = [
    "HAS_ARG_FLAG",
    "HAS_CONST_FLAG",
    "HAS_NAME_FLAG",
    "HAS_JUMP_FLAG",
    "HAS_FREE_FLAG",
    "HAS_LOCAL_FLAG",
    "HAS_EVAL_BREAK_FLAG",
    "HAS_DEOPT_FLAG",
    "HAS_ERROR_FLAG",
    "HAS_ESCAPES_FLAG",
    "HAS_EXIT_FLAG",
    "HAS_PURE_FLAG",
    "HAS_PASSTHROUGH_FLAG",
    "HAS_OPARG_AND_1_FLAG",
    "HAS_ERROR_NO_POP_FLAG",
    "HAS_NO_SAVE_IP_FLAG",
]

# `Modules/_opcode.c` (ADD_NB_OP) and `Include/internal/pycore_intrinsics.h`
# / `Python/intrinsics.c`; `_Py_SpecialMethods` lives in `Python/ceval.c`.
NB_OPS = [
    ("NB_ADD", "+"),
    ("NB_AND", "&"),
    ("NB_FLOOR_DIVIDE", "//"),
    ("NB_LSHIFT", "<<"),
    ("NB_MATRIX_MULTIPLY", "@"),
    ("NB_MULTIPLY", "*"),
    ("NB_REMAINDER", "%"),
    ("NB_OR", "|"),
    ("NB_POWER", "**"),
    ("NB_RSHIFT", ">>"),
    ("NB_SUBTRACT", "-"),
    ("NB_TRUE_DIVIDE", "/"),
    ("NB_XOR", "^"),
    ("NB_INPLACE_ADD", "+="),
    ("NB_INPLACE_AND", "&="),
    ("NB_INPLACE_FLOOR_DIVIDE", "//="),
    ("NB_INPLACE_LSHIFT", "<<="),
    ("NB_INPLACE_MATRIX_MULTIPLY", "@="),
    ("NB_INPLACE_MULTIPLY", "*="),
    ("NB_INPLACE_REMAINDER", "%="),
    ("NB_INPLACE_OR", "|="),
    ("NB_INPLACE_POWER", "**="),
    ("NB_INPLACE_RSHIFT", ">>="),
    ("NB_INPLACE_SUBTRACT", "-="),
    ("NB_INPLACE_TRUE_DIVIDE", "/="),
    ("NB_INPLACE_XOR", "^="),
    ("NB_SUBSCR", "[]"),
]
INTRINSIC_1 = [
    "INTRINSIC_1_INVALID",
    "INTRINSIC_PRINT",
    "INTRINSIC_IMPORT_STAR",
    "INTRINSIC_STOPITERATION_ERROR",
    "INTRINSIC_ASYNC_GEN_WRAP",
    "INTRINSIC_UNARY_POSITIVE",
    "INTRINSIC_LIST_TO_TUPLE",
    "INTRINSIC_TYPEVAR",
    "INTRINSIC_PARAMSPEC",
    "INTRINSIC_TYPEVARTUPLE",
    "INTRINSIC_SUBSCRIPT_GENERIC",
    "INTRINSIC_TYPEALIAS",
]
INTRINSIC_2 = [
    "INTRINSIC_2_INVALID",
    "INTRINSIC_PREP_RERAISE_STAR",
    "INTRINSIC_TYPEVAR_WITH_BOUND",
    "INTRINSIC_TYPEVAR_WITH_CONSTRAINTS",
    "INTRINSIC_SET_FUNCTION_TYPE_PARAMS",
    "INTRINSIC_SET_TYPEPARAM_DEFAULT",
]
SPECIAL_METHOD_NAMES = ["__enter__", "__exit__", "__aenter__", "__aexit__"]
BLOCK_PUSH = ["SETUP_FINALLY", "SETUP_WITH", "SETUP_CLEANUP"]


def parse_switch(header: str, fn: str) -> list[tuple[str, str]]:
    m = re.search(
        r"int %s\(int opcode, int oparg\)\s*\{\s*switch\(opcode\)\s*\{(.*?)\n    \}" % fn,
        header,
        re.S,
    )
    if not m:
        sys.exit(f"could not find {fn} in header")
    return re.findall(r"case (\w+):\s*return ([^;]+);", m.group(1))


def parse_metadata(header: str) -> tuple[int, list[tuple[str, int]]]:
    m = re.search(
        r"const struct opcode_metadata _PyOpcode_opcode_metadata\[(\d+)\] = \{(.*?)\n\};",
        header,
        re.S,
    )
    if not m:
        sys.exit("could not find _PyOpcode_opcode_metadata")
    size = int(m.group(1))
    flag_values = {name: 1 << i for i, name in enumerate(FLAG_NAMES)}
    for name in FLAG_NAMES:
        fm = re.search(r"#define %s \((\d+)\)" % name, header)
        if fm and int(fm.group(1)) != flag_values[name]:
            sys.exit(f"flag {name} moved; update FLAG_NAMES")
    out = []
    for name, valid, _fmt, flags in re.findall(
        r"\[(\w+)\] = \{ (true|false), (-?\w+), ([^}]+)\}", m.group(2)
    ):
        if valid != "true":
            continue
        word = 0
        for f in flags.split("|"):
            f = f.strip()
            if f and f != "0":
                word |= flag_values[f]
        out.append((name, word))
    return size, out


def parse_deopt(header: str) -> list[tuple[str, str]]:
    m = re.search(r"const uint8_t _PyOpcode_Deopt\[256\] = \{(.*?)\n\};", header, re.S)
    if not m:
        sys.exit("could not find _PyOpcode_Deopt")
    return [
        (a, b)
        for a, b in re.findall(r"\[(\w+)\] = (\w+),", m.group(1))
        if not a.isdigit() and a != b
    ]


def py_expr(expr: str) -> str:
    expr = expr.strip()
    if not re.fullmatch(r"[\w\s()&|+\-*>x0-9]+", expr):
        sys.exit(f"unexpected stack-effect expression {expr!r}")
    return expr


def render(version: str, size: int, meta, popped, pushed, deopt) -> str:
    lines: list[str] = []
    w = lines.append
    w('"""_opcode -- CPython %s `_opcode` accelerator surface (RFC 0077 WS9).' % version)
    w("")
    w("GENERATED by tools/gen_opcode_shim.py from Include/internal/")
    w("pycore_opcode_metadata.h (CPython v%s); do not edit by hand." % version)
    w("")
    w("Pure Python over the same tables CPython's C module reads: the")
    w("per-opcode flag words, the `_PyOpcode_num_popped` / `_PyOpcode_num_pushed`")
    w("stack-effect functions and the `_PyOpcode_Deopt` map. Opcode numbers")
    w("come from the vendored `_opcode_metadata`, so the two never drift.")
    w("")
    w("WeavePy specializes out of band (RFC 0077 Pillar I) rather than through")
    w("CPython's quickened tier-1 stream, so `ENABLE_SPECIALIZATION` is 0")
    w("(`@requires_specialization` tests skip, the faithful verdict) and")
    w("`get_executor` reports that this build has no tier-2 executors.")
    w('"""')
    w("")
    w("from _opcode_metadata import opmap as _opmap, _specialized_opmap")
    w("")
    w("ENABLE_SPECIALIZATION = 0")
    w("ENABLE_SPECIALIZATION_FT = 0")
    w("")
    w("_NUM_OPCODES = %d" % size)
    w("_MAX_REAL_OPCODE = 255")
    w("")
    for i, name in enumerate(FLAG_NAMES):
        w("%s = %d" % (name, 1 << i))
    w("")
    w("_names = dict(_opmap)")
    w("_names.update(_specialized_opmap)")
    w("")
    w("# `_PyOpcode_opcode_metadata[op].flags` for every valid entry.")
    w("_FLAGS = {")
    for name, word in meta:
        w("    _names[%r]: %d," % (name, word))
    w("}")
    w("")
    w("# `_PyOpcode_Deopt`: specialized form -> base instruction.")
    w("_DEOPT = {")
    for a, b in deopt:
        w("    _names[%r]: _names[%r]," % (a, b))
    w("}")
    w("")
    w("# `_PyOpcode_num_popped(opcode, oparg)`.")
    w("_POPPED = {")
    for name, expr in popped:
        w("    _names[%r]: lambda oparg: %s," % (name, py_expr(expr)))
    w("}")
    w("")
    w("# `_PyOpcode_num_pushed(opcode, oparg)`.")
    w("_PUSHED = {")
    for name, expr in pushed:
        w("    _names[%r]: lambda oparg: %s," % (name, py_expr(expr)))
    w("}")
    w("")
    w("_BLOCK_PUSH = frozenset(_names[n] for n in %r)" % (BLOCK_PUSH,))
    w("")
    w("")
    w("def is_valid(opcode):")
    w('    """Return True if opcode is valid, False otherwise."""')
    w("    return 0 <= opcode < _NUM_OPCODES and opcode in _FLAGS")
    w("")
    w("")
    w("def _has(opcode, flag):")
    w("    return is_valid(opcode) and bool(_FLAGS[opcode] & flag)")
    w("")
    w("")
    for fn, flag, doc in [
        ("has_arg", "HAS_ARG_FLAG", "Return True if the opcode uses its oparg, False otherwise."),
        ("has_const", "HAS_CONST_FLAG", "Return True if the opcode accesses a constant, False otherwise."),
        ("has_name", "HAS_NAME_FLAG", "Return True if the opcode accesses an attribute by name, False otherwise."),
        ("has_jump", "HAS_JUMP_FLAG", "Return True if the opcode has a jump target, False otherwise."),
        ("has_free", "HAS_FREE_FLAG", "Return True if the opcode accesses a free variable, False otherwise."),
        ("has_local", "HAS_LOCAL_FLAG", "Return True if the opcode accesses a local variable, False otherwise."),
    ]:
        w("def %s(opcode):" % fn)
        w('    """%s"""' % doc)
        w("    return _has(opcode, %s)" % flag)
        w("")
        w("")
    w("def has_exc(opcode):")
    w('    """Return True if the opcode sets an exception handler, False otherwise."""')
    w("    return is_valid(opcode) and opcode in _BLOCK_PUSH")
    w("")
    w("")
    w("def stack_effect(opcode, oparg=None, /, *, jump=None):")
    w('    """Compute the stack effect of the opcode."""')
    w("    oparg_int = 0 if oparg is None else int(oparg)")
    w("    if jump is None:")
    w("        jump_int = -1")
    w("    elif jump is True:")
    w("        jump_int = 1")
    w("    elif jump is False:")
    w("        jump_int = 0")
    w("    else:")
    w('        raise ValueError("stack_effect: jump must be False, True or None")')
    w("    # `get_stack_effects` (Python/flowgraph.c): specialized forms and")
    w("    # unknown opcodes have no effect entry.")
    w("    if opcode < 0 or (opcode <= _MAX_REAL_OPCODE and _DEOPT.get(opcode, opcode) != opcode):")
    w('        raise ValueError("invalid opcode or oparg")')
    w("    popped = _POPPED.get(opcode)")
    w("    pushed = _PUSHED.get(opcode)")
    w("    if popped is None or pushed is None:")
    w('        raise ValueError("invalid opcode or oparg")')
    w("    npop = popped(oparg_int)")
    w("    npush = pushed(oparg_int)")
    w("    if npop < 0 or npush < 0:")
    w('        raise ValueError("invalid opcode or oparg")')
    w("    if opcode in _BLOCK_PUSH and not jump_int:")
    w("        return 0")
    w("    return npush - npop")
    w("")
    w("")
    w("def get_specialization_stats():")
    w('    """Return the specialization stats (None outside a `--enable-pystats` build)."""')
    w("    return None")
    w("")
    w("")
    w("def get_executor(code, offset):")
    w('    """Return the executor object at offset in code if exists, None otherwise."""')
    w('    if type(code).__name__ != "code":')
    w("        raise TypeError(")
    w('            "expected a code object, not \'%s\'" % type(code).__name__')
    w("        )")
    w('    raise RuntimeError("Executors are not available in this build")')
    w("")
    w("")
    w("def get_nb_ops():")
    w('    """Return array of symbols of binary ops, indexed by the BINARY_OP oparg."""')
    w("    return [")
    for pair in NB_OPS:
        w("        %r," % (pair,))
    w("    ]")
    w("")
    w("")
    w("def get_intrinsic1_descs():")
    w('    """Return a list of names of the unary intrinsics."""')
    w("    return [")
    for n in INTRINSIC_1:
        w("        %r," % n)
    w("    ]")
    w("")
    w("")
    w("def get_intrinsic2_descs():")
    w('    """Return a list of names of the binary intrinsics."""')
    w("    return [")
    for n in INTRINSIC_2:
        w("        %r," % n)
    w("    ]")
    w("")
    w("")
    w("def get_special_method_names():")
    w('    """Return a list of special method names."""')
    w("    return %r" % (SPECIAL_METHOD_NAMES,))
    return "\n".join(lines) + "\n"


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("header", type=Path, help="pycore_opcode_metadata.h from the CPython tag")
    ap.add_argument("--version", default="3.14.7")
    ap.add_argument(
        "-o",
        "--output",
        type=Path,
        default=Path(__file__).resolve().parents[1]
        / "crates/weavepy-vm/src/stdlib/python/_opcode.py",
    )
    args = ap.parse_args()
    header = args.header.read_text()
    size, meta = parse_metadata(header)
    popped = parse_switch(header, "_PyOpcode_num_popped")
    pushed = parse_switch(header, "_PyOpcode_num_pushed")
    deopt = parse_deopt(header)
    args.output.write_text(render(args.version, size, meta, popped, pushed, deopt))
    print(f"wrote {args.output} ({len(meta)} opcodes, {len(deopt)} specializations)")


if __name__ == "__main__":
    main()
