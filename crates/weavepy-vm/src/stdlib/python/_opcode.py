"""_opcode — CPython 3.13 `_opcode` accelerator surface (RFC 0048).

Pure-Python over the self-contained frozen `opcode` tables. WeavePy's
`opcode.py` does not import `_opcode` (its tables are inlined), so this
module exists for the *external* consumers CPython 3.13 grew:
`test.support` (`ENABLE_SPECIALIZATION`, `requires_specialization`),
`dis` (`get_executor`), and code that probes the opcode predicates
directly.

WeavePy has no CPython-style tier-1 specializing interpreter, so
`ENABLE_SPECIALIZATION` is `False` (tests decorated
`@requires_specialization` skip, which is the faithful verdict) and
`get_executor` reports that no tier-2 executor is attached.
"""

import opcode as _opcode_tables

ENABLE_SPECIALIZATION = False

_HAVE_ARGUMENT = _opcode_tables.HAVE_ARGUMENT
_VALID_OPS = frozenset(_opcode_tables.opmap.values())


def is_valid(opcode):
    """is_valid(opcode) -> bool: whether `opcode` is a valid opcode."""
    return opcode in _VALID_OPS


def has_arg(opcode):
    return opcode in _VALID_OPS and opcode >= _HAVE_ARGUMENT


def has_const(opcode):
    return opcode in _opcode_tables.hasconst


def has_name(opcode):
    return opcode in _opcode_tables.hasname


def has_jump(opcode):
    return opcode in _opcode_tables.hasjump


def has_free(opcode):
    return opcode in _opcode_tables.hasfree


def has_local(opcode):
    return opcode in _opcode_tables.haslocal


def has_exc(opcode):
    return opcode in _opcode_tables.hasexc


def stack_effect(opcode, oparg=None, jump=None):
    return _opcode_tables.stack_effect(opcode, oparg, jump=jump)


def get_executor(code, offset):
    """No tier-2 executor is ever attached to WeavePy code objects."""
    if not hasattr(code, "co_code"):
        raise TypeError(f"expected a code object, not '{type(code).__name__}'")
    return None


def get_nb_ops():
    """The BINARY_OP sub-operation table (name, symbol) pairs."""
    return [
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
    ]


def get_intrinsic1_descs():
    return [
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


def get_intrinsic2_descs():
    return [
        "INTRINSIC_2_INVALID",
        "INTRINSIC_PREP_RERAISE_STAR",
        "INTRINSIC_TYPEVAR_WITH_BOUND",
        "INTRINSIC_TYPEVAR_WITH_CONSTRAINTS",
        "INTRINSIC_SET_FUNCTION_TYPE_PARAMS",
        "INTRINSIC_SET_TYPEPARAM_DEFAULT",
    ]


def get_special_op_descs():
    return []
