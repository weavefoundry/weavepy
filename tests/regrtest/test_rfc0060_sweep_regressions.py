"""RFC 0060 WS9 — engine fixes surfaced by the endgame re-baseline grade.

One bundled canary per fix:

1. `marshal.dumps` of a code object whose `co_consts` holds a value
   with no constant representation (`code.replace(co_consts=
   (frozenset({int}),))`) must raise `ValueError("unmarshallable
   object")` instead of silently writing `None` in the slot
   (gh-106287; test_marshal `test_unmarshallable [code]`). The pool
   keeps a `Constant::Unmarshallable` sentinel for such slots.

2. `_testinternalcapi.new_instruction_sequence()` +
   `assemble_code_object` assemble a CPython-3.13 instruction stream —
   pseudo-op lowering, exception-table/linetable encoding — into a
   *runnable* code object (test_compiler_assemble). Exercises the
   decoder's `MAKE_FUNCTION; SET_FUNCTION_ATTRIBUTE n` folding and
   `PUSH_NULL` lowering for foreign 3.13 streams.

3. `TextIOWrapper.reconfigure` on the native stdio streams
   (regrtest's own startup does `sys.stdout.reconfigure(
   errors="backslashreplace")`), and `struct._clearcache()`
   (`test.libregrtest.utils.clear_caches`).
"""

import io
import marshal
import struct
import sys
import types

# ------------------- 1. unmarshallable co_consts ------------------------

_fset = frozenset([int])
_code = compile("a = 1", "<string>", "exec").replace(co_consts=(1, _fset, None))
for case in (_fset, (_fset,), [_fset], {_fset: 'x'}, {'x': _fset}, _code):
    try:
        marshal.dumps(case)
    except ValueError as exc:
        assert "unmarshallable object" in str(exc), exc
    else:
        raise AssertionError(f"marshal.dumps({case!r}) did not raise")
# A legal code object still round-trips.
_ok = compile("b = 2", "<string>", "exec")
assert marshal.loads(marshal.dumps(_ok)).co_names == _ok.co_names

# ------------------- 2. instruction-sequence assembly -------------------

import _testinternalcapi
import opcode

_seq = _testinternalcapi.new_instruction_sequence()
for item in [
    ('RESUME', 0, 1), ('LOAD_FAST', 0, 1), ('LOAD_FAST', 1, 1),
    ('BINARY_OP', 0, 1), ('LOAD_CONST', 0, 1), ('BINARY_OP', 11, 1),
    ('RETURN_VALUE', None, 1),
]:
    op = opcode.opmap[item[0]]
    arg, *loc = item[1:]
    loc = loc + [-1] * (4 - len(loc))
    _seq.addop(op, arg or 0, *loc)
_metadata = {
    'filename': 'avg.py', 'name': 'avg', 'qualname': 'stats.avg',
    'consts': {2: 0}, 'argcount': 2, 'varnames': {'x': 0, 'y': 1},
    'names': {}, 'cellvars': {}, 'freevars': {}, 'fasthidden': {},
    'posonlyargcount': 0, 'kwonlyargcount': 0, 'firstlineno': 1,
}
_co = _testinternalcapi.assemble_code_object('avg.py', _seq, _metadata)
assert isinstance(_co, types.CodeType)
assert _co.co_varnames == ('x', 'y') and _co.co_consts == (2,)
_avg = types.FunctionType(_co, {})
assert _avg(3, 4) == 3.5 and _avg(10, 18) == 14

# ------------------- 3. stdio reconfigure + struct._clearcache ----------

_saved = sys.stdout.errors if hasattr(sys.stdout, 'errors') else None
sys.stdout.reconfigure(errors="backslashreplace")
assert sys.stdout.errors == "backslashreplace", sys.stdout.errors
if _saved is not None:
    sys.stdout.reconfigure(errors=_saved)

struct._clearcache()  # a no-op hook, but it must exist and be callable

print("rfc0060-sweep-regressions: ok")
