# RFC 0033: ``dis`` disassembler drop-in.
#
# Exercises the public ``dis`` surface over WeavePy's CPython-shaped
# code objects: ``dis.dis`` text output, ``Bytecode`` (which must
# *return* a string, not print), ``get_instructions``, the
# ``Instruction`` namedtuple fields, opcode classification, and
# ``findlinestarts``.

import dis
import io


def fn(a, b):
    c = a + b
    if c > 10:
        return c
    return -c


# ---------- get_instructions ----------
instrs = list(dis.get_instructions(fn))
assert len(instrs) > 0
first = instrs[0]
# Every instruction exposes the CPython Instruction fields.
for attr in ("opname", "opcode", "arg", "argval", "offset"):
    assert hasattr(first, attr), attr
assert all(isinstance(i.opname, str) for i in instrs)
assert all(isinstance(i.opcode, int) for i in instrs)
opnames = {i.opname for i in instrs}
assert "LOAD_FAST" in opnames, opnames
assert any(name.startswith("RETURN") for name in opnames), opnames

# Offsets are monotonically increasing code-unit positions.
offsets = [i.offset for i in instrs]
assert offsets == sorted(offsets)
assert offsets[0] == 0

# ---------- dis.Bytecode RETURNS a string (does not print) ----------
bc = dis.Bytecode(fn)
text = bc.dis()
assert isinstance(text, str), type(text)
assert "LOAD_FAST" in text, text
assert len(list(bc)) == len(instrs)

# ---------- dis.dis(obj, file=...) writes to the given file ----------
buf = io.StringIO()
dis.dis(fn, file=buf)
captured = buf.getvalue()
assert "LOAD_FAST" in captured, captured
assert captured.strip(), "dis.dis must write to the provided file"

# ---------- code_info ----------
info = dis.code_info(fn)
assert "Argument count" in info, info
assert "fn" in info

# ---------- findlinestarts ----------
starts = list(dis.findlinestarts(fn.__code__))
assert len(starts) > 0
for offset, lineno in starts:
    assert isinstance(offset, int)
    assert isinstance(lineno, int)

# ---------- opcode tables are consistent ----------
import opcode

assert opcode.opname[opcode.opmap["LOAD_FAST"]] == "LOAD_FAST"
assert 0 <= opcode.HAVE_ARGUMENT <= 255
assert opcode.opmap["EXTENDED_ARG"] == opcode.EXTENDED_ARG

# Disassembling a string of source compiled with compile() works too.
code = compile("x = 1\ny = x + 2\n", "<embedded>", "exec")
code_text = dis.Bytecode(code).dis()
assert "STORE_NAME" in code_text or "STORE_FAST" in code_text, code_text

print("test_dis_dropin: OK")
