"""opcode — CPython 3.13 opcode tables (RFC 0033).

Self-contained: derived from CPython 3.13's opcode/_opcode data so
`dis` and other tools see CPython-faithful numbers. WeavePy emits only
canonical (non-specialized) opcodes, so the specialization tables are
intentionally empty."""

__all__ = ["cmp_op", "stack_effect", "hascompare", "opname", "opmap",
           "HAVE_ARGUMENT", "EXTENDED_ARG", "hasarg", "hasconst", "hasname",
           "hasjump", "hasjrel", "hasjabs", "hasfree", "haslocal", "hasexc"]

opmap = {
    'CACHE': 0,
    'RESERVED': 17,
    'RESUME': 149,
    'INSTRUMENTED_LINE': 254,
    'BEFORE_ASYNC_WITH': 1,
    'BEFORE_WITH': 2,
    'BINARY_SLICE': 4,
    'BINARY_SUBSCR': 5,
    'CHECK_EG_MATCH': 6,
    'CHECK_EXC_MATCH': 7,
    'CLEANUP_THROW': 8,
    'DELETE_SUBSCR': 9,
    'END_ASYNC_FOR': 10,
    'END_FOR': 11,
    'END_SEND': 12,
    'EXIT_INIT_CHECK': 13,
    'FORMAT_SIMPLE': 14,
    'FORMAT_WITH_SPEC': 15,
    'GET_AITER': 16,
    'GET_ANEXT': 18,
    'GET_ITER': 19,
    'GET_LEN': 20,
    'GET_YIELD_FROM_ITER': 21,
    'INTERPRETER_EXIT': 22,
    'LOAD_ASSERTION_ERROR': 23,
    'LOAD_BUILD_CLASS': 24,
    'LOAD_LOCALS': 25,
    'MAKE_FUNCTION': 26,
    'MATCH_KEYS': 27,
    'MATCH_MAPPING': 28,
    'MATCH_SEQUENCE': 29,
    'NOP': 30,
    'POP_EXCEPT': 31,
    'POP_TOP': 32,
    'PUSH_EXC_INFO': 33,
    'PUSH_NULL': 34,
    'RETURN_GENERATOR': 35,
    'RETURN_VALUE': 36,
    'SETUP_ANNOTATIONS': 37,
    'STORE_SLICE': 38,
    'STORE_SUBSCR': 39,
    'TO_BOOL': 40,
    'UNARY_INVERT': 41,
    'UNARY_NEGATIVE': 42,
    'UNARY_NOT': 43,
    'WITH_EXCEPT_START': 44,
    'BINARY_OP': 45,
    'BUILD_CONST_KEY_MAP': 46,
    'BUILD_LIST': 47,
    'BUILD_MAP': 48,
    'BUILD_SET': 49,
    'BUILD_SLICE': 50,
    'BUILD_STRING': 51,
    'BUILD_TUPLE': 52,
    'CALL': 53,
    'CALL_FUNCTION_EX': 54,
    'CALL_INTRINSIC_1': 55,
    'CALL_INTRINSIC_2': 56,
    'CALL_KW': 57,
    'COMPARE_OP': 58,
    'CONTAINS_OP': 59,
    'CONVERT_VALUE': 60,
    'COPY': 61,
    'COPY_FREE_VARS': 62,
    'DELETE_ATTR': 63,
    'DELETE_DEREF': 64,
    'DELETE_FAST': 65,
    'DELETE_GLOBAL': 66,
    'DELETE_NAME': 67,
    'DICT_MERGE': 68,
    'DICT_UPDATE': 69,
    'ENTER_EXECUTOR': 70,
    'EXTENDED_ARG': 71,
    'FOR_ITER': 72,
    'GET_AWAITABLE': 73,
    'IMPORT_FROM': 74,
    'IMPORT_NAME': 75,
    'IS_OP': 76,
    'JUMP_BACKWARD': 77,
    'JUMP_BACKWARD_NO_INTERRUPT': 78,
    'JUMP_FORWARD': 79,
    'LIST_APPEND': 80,
    'LIST_EXTEND': 81,
    'LOAD_ATTR': 82,
    'LOAD_CONST': 83,
    'LOAD_DEREF': 84,
    'LOAD_FAST': 85,
    'LOAD_FAST_AND_CLEAR': 86,
    'LOAD_FAST_CHECK': 87,
    'LOAD_FAST_LOAD_FAST': 88,
    'LOAD_FROM_DICT_OR_DEREF': 89,
    'LOAD_FROM_DICT_OR_GLOBALS': 90,
    'LOAD_GLOBAL': 91,
    'LOAD_NAME': 92,
    'LOAD_SUPER_ATTR': 93,
    'MAKE_CELL': 94,
    'MAP_ADD': 95,
    'MATCH_CLASS': 96,
    'POP_JUMP_IF_FALSE': 97,
    'POP_JUMP_IF_NONE': 98,
    'POP_JUMP_IF_NOT_NONE': 99,
    'POP_JUMP_IF_TRUE': 100,
    'RAISE_VARARGS': 101,
    'RERAISE': 102,
    'RETURN_CONST': 103,
    'SEND': 104,
    'SET_ADD': 105,
    'SET_FUNCTION_ATTRIBUTE': 106,
    'SET_UPDATE': 107,
    'STORE_ATTR': 108,
    'STORE_DEREF': 109,
    'STORE_FAST': 110,
    'STORE_FAST_LOAD_FAST': 111,
    'STORE_FAST_STORE_FAST': 112,
    'STORE_GLOBAL': 113,
    'STORE_NAME': 114,
    'SWAP': 115,
    'UNPACK_EX': 116,
    'UNPACK_SEQUENCE': 117,
    'YIELD_VALUE': 118,
    'INSTRUMENTED_RESUME': 236,
    'INSTRUMENTED_END_FOR': 237,
    'INSTRUMENTED_END_SEND': 238,
    'INSTRUMENTED_RETURN_VALUE': 239,
    'INSTRUMENTED_RETURN_CONST': 240,
    'INSTRUMENTED_YIELD_VALUE': 241,
    'INSTRUMENTED_LOAD_SUPER_ATTR': 242,
    'INSTRUMENTED_FOR_ITER': 243,
    'INSTRUMENTED_CALL': 244,
    'INSTRUMENTED_CALL_KW': 245,
    'INSTRUMENTED_CALL_FUNCTION_EX': 246,
    'INSTRUMENTED_INSTRUCTION': 247,
    'INSTRUMENTED_JUMP_FORWARD': 248,
    'INSTRUMENTED_JUMP_BACKWARD': 249,
    'INSTRUMENTED_POP_JUMP_IF_TRUE': 250,
    'INSTRUMENTED_POP_JUMP_IF_FALSE': 251,
    'INSTRUMENTED_POP_JUMP_IF_NONE': 252,
    'INSTRUMENTED_POP_JUMP_IF_NOT_NONE': 253,
    'JUMP': 256,
    'JUMP_NO_INTERRUPT': 257,
    'LOAD_CLOSURE': 258,
    'LOAD_METHOD': 259,
    'LOAD_SUPER_METHOD': 260,
    'LOAD_ZERO_SUPER_ATTR': 261,
    'LOAD_ZERO_SUPER_METHOD': 262,
    'POP_BLOCK': 263,
    'SETUP_CLEANUP': 264,
    'SETUP_FINALLY': 265,
    'SETUP_WITH': 266,
    'STORE_FAST_MAYBE_NULL': 267,
}

HAVE_ARGUMENT = 44
MIN_INSTRUMENTED_OPCODE = 236
EXTENDED_ARG = opmap['EXTENDED_ARG']

opname = ['<%r>' % (op,) for op in range(max(opmap.values()) + 1)]
for _op, _i in opmap.items():
    opname[_i] = _op

cmp_op = ('<', '<=', '==', '!=', '>', '>=')

hasarg = [149, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116, 117, 118, 236, 240, 241, 242, 243, 244, 245, 248, 249, 250, 251, 252, 253, 256, 257, 258, 259, 260, 261, 262, 264, 265, 266, 267]
hasconst = [83, 103, 240]
hasname = [63, 66, 67, 74, 75, 82, 90, 91, 92, 93, 108, 113, 114, 259, 260, 261, 262]
hasjrel = [72, 77, 78, 79, 97, 98, 99, 100, 104, 256, 257]
hasjabs = []
hasfree = [64, 84, 89, 94, 109]
haslocal = [65, 85, 86, 87, 88, 110, 111, 112, 258, 267]
hasexc = [264, 265, 266]
hasjump = hasjrel
hascompare = [opmap["COMPARE_OP"]]

_nb_ops = [('NB_ADD', '+'), ('NB_AND', '&'), ('NB_FLOOR_DIVIDE', '//'), ('NB_LSHIFT', '<<'), ('NB_MATRIX_MULTIPLY', '@'), ('NB_MULTIPLY', '*'), ('NB_REMAINDER', '%'), ('NB_OR', '|'), ('NB_POWER', '**'), ('NB_RSHIFT', '>>'), ('NB_SUBTRACT', '-'), ('NB_TRUE_DIVIDE', '/'), ('NB_XOR', '^'), ('NB_INPLACE_ADD', '+='), ('NB_INPLACE_AND', '&='), ('NB_INPLACE_FLOOR_DIVIDE', '//='), ('NB_INPLACE_LSHIFT', '<<='), ('NB_INPLACE_MATRIX_MULTIPLY', '@='), ('NB_INPLACE_MULTIPLY', '*='), ('NB_INPLACE_REMAINDER', '%='), ('NB_INPLACE_OR', '|='), ('NB_INPLACE_POWER', '**='), ('NB_INPLACE_RSHIFT', '>>='), ('NB_INPLACE_SUBTRACT', '-='), ('NB_INPLACE_TRUE_DIVIDE', '/='), ('NB_INPLACE_XOR', '^=')]
_intrinsic_1_descs = ['INTRINSIC_1_INVALID', 'INTRINSIC_PRINT', 'INTRINSIC_IMPORT_STAR', 'INTRINSIC_STOPITERATION_ERROR', 'INTRINSIC_ASYNC_GEN_WRAP', 'INTRINSIC_UNARY_POSITIVE', 'INTRINSIC_LIST_TO_TUPLE', 'INTRINSIC_TYPEVAR', 'INTRINSIC_PARAMSPEC', 'INTRINSIC_TYPEVARTUPLE', 'INTRINSIC_SUBSCRIPT_GENERIC', 'INTRINSIC_TYPEALIAS']
_intrinsic_2_descs = ['INTRINSIC_2_INVALID', 'INTRINSIC_PREP_RERAISE_STAR', 'INTRINSIC_TYPEVAR_WITH_BOUND', 'INTRINSIC_TYPEVAR_WITH_CONSTRAINTS', 'INTRINSIC_SET_FUNCTION_TYPE_PARAMS', 'INTRINSIC_SET_TYPEPARAM_DEFAULT']

# WeavePy never emits adaptive/specialized opcodes.
_specializations = {}
_specialized_opmap = {}

_cache_format = {
    'LOAD_GLOBAL': {'counter': 1, 'index': 1, 'module_keys_version': 1, 'builtin_keys_version': 1},
    'BINARY_OP': {'counter': 1},
    'UNPACK_SEQUENCE': {'counter': 1},
    'COMPARE_OP': {'counter': 1},
    'CONTAINS_OP': {'counter': 1},
    'BINARY_SUBSCR': {'counter': 1},
    'FOR_ITER': {'counter': 1},
    'LOAD_SUPER_ATTR': {'counter': 1},
    'LOAD_ATTR': {'counter': 1, 'version': 2, 'keys_version': 2, 'descr': 4},
    'STORE_ATTR': {'counter': 1, 'version': 2, 'index': 1},
    'CALL': {'counter': 1, 'func_version': 2},
    'STORE_SUBSCR': {'counter': 1},
    'SEND': {'counter': 1},
    'JUMP_BACKWARD': {'counter': 1},
    'TO_BOOL': {'counter': 1, 'version': 2},
    'POP_JUMP_IF_TRUE': {'counter': 1},
    'POP_JUMP_IF_FALSE': {'counter': 1},
    'POP_JUMP_IF_NONE': {'counter': 1},
    'POP_JUMP_IF_NOT_NONE': {'counter': 1},
}

_inline_cache_entries = {
    name: sum(value.values()) for (name, value) in _cache_format.items()
}


def stack_effect(opcode, oparg=None, *, jump=None):
    """Best-effort stack-effect stub.

    WeavePy computes `co_stacksize` natively; `dis` does not depend on
    this value, so a precise table is not maintained here."""
    return 0
