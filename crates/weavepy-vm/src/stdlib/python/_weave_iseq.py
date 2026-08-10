"""``_weave_iseq`` — the ``_testinternalcapi`` instruction-sequence fixture.

CPython's ``_testinternalcapi.new_instruction_sequence()`` +
``assemble_code_object(filename, seq, metadata)`` expose the compiler's
assemble stage to ``Lib/test`` (``test_compiler_assemble``, via
``test.support.bytecode_helper.AssemblerTestCase``). WeavePy's compiler
assembles its own instruction set, so this module reimplements CPython
3.13's assemble stage over the *CPython* opcode space instead: label
resolution, pseudo-instruction lowering (``LOAD_CLOSURE``,
``SETUP_FINALLY``/``SETUP_CLEANUP``/``SETUP_WITH``/``POP_BLOCK``,
``JUMP``), exception-table and location-table encoding (the formats in
``Objects/exception_handling_notes.txt`` and ``Objects/locations.md``),
and finally ``types.CodeType(...)`` — whose RFC 0060 constructor decodes
the CPython bytecode back into a runnable WeavePy code object.
"""

import opcode as _opcode
import types as _types

_OPMAP = _opcode.opmap
_HAS_ARG = set(_opcode.hasarg)
_CACHES = dict(getattr(_opcode, "_inline_cache_entries", {}))

_SETUP_FINALLY = _OPMAP["SETUP_FINALLY"]
_SETUP_CLEANUP = _OPMAP["SETUP_CLEANUP"]
_SETUP_WITH = _OPMAP["SETUP_WITH"]
_POP_BLOCK = _OPMAP["POP_BLOCK"]
_LOAD_CLOSURE = _OPMAP["LOAD_CLOSURE"]
_JUMP = _OPMAP.get("JUMP")
_JUMP_NO_INTERRUPT = _OPMAP.get("JUMP_NO_INTERRUPT")
_JUMP_FORWARD = _OPMAP["JUMP_FORWARD"]
_JUMP_BACKWARD = _OPMAP["JUMP_BACKWARD"]
_JUMP_BACKWARD_NO_INTERRUPT = _OPMAP.get("JUMP_BACKWARD_NO_INTERRUPT")
_EXTENDED_ARG = _OPMAP["EXTENDED_ARG"]
_CACHE = _OPMAP.get("CACHE", 0)

# Pseudo setup ops: preserve-lasti flag (SETUP_FINALLY re-raises without
# restoring f_lasti; SETUP_CLEANUP/SETUP_WITH handlers pop a lasti slot).
_SETUP_OPS = {
    _SETUP_FINALLY: False,
    _SETUP_CLEANUP: True,
    _SETUP_WITH: True,
}

# Ops that never fall through to the next instruction.
_TERMINATORS = {
    _OPMAP[n]
    for n in (
        "RETURN_VALUE",
        "RETURN_CONST",
        "RERAISE",
        "RAISE_VARARGS",
        "JUMP_FORWARD",
        "JUMP_BACKWARD",
    )
    if n in _OPMAP
} | {op for op in (_JUMP, _JUMP_NO_INTERRUPT, _JUMP_BACKWARD_NO_INTERRUPT) if op is not None}

_JUMP_OPS = set(_opcode.hasjrel) | set(getattr(_opcode, "hasjabs", ()))
_JUMP_OPS |= {op for op in (_JUMP, _JUMP_NO_INTERRUPT) if op is not None}

# stack_effect fallbacks for ops the host may not model (pseudo ops).
_EFFECT_FALLBACK = {
    _SETUP_FINALLY: 0,
    _SETUP_CLEANUP: 0,
    _SETUP_WITH: 0,
    _POP_BLOCK: 0,
    _LOAD_CLOSURE: 1,
}
if _JUMP is not None:
    _EFFECT_FALLBACK[_JUMP] = 0
if _JUMP_NO_INTERRUPT is not None:
    _EFFECT_FALLBACK[_JUMP_NO_INTERRUPT] = 0


def _effect(op, arg):
    try:
        if op in _HAS_ARG:
            return _opcode.stack_effect(op, arg)
        return _opcode.stack_effect(op)
    except (ValueError, TypeError):
        return _EFFECT_FALLBACK.get(op, 0)


class InstructionSequence:
    """A CPython-shaped instruction sequence: ``addop`` appends
    ``(op, arg, line, end_line, col, end_col)`` rows; ``use_label(v)``
    binds label id ``v`` to the *next* instruction index."""

    def __init__(self):
        self._insts = []
        self._labelmap = {}

    def use_label(self, label):
        self._labelmap[int(label)] = len(self._insts)

    def addop(self, op, arg, line, end_line, col, end_col):
        self._insts.append((int(op), int(arg), line, end_line, col, end_col))


def new_instruction_sequence():
    return InstructionSequence()


def _index_map(mapping):
    """Invert a bytecode_helper-style ``{value: index}`` dict to a tuple."""
    if not mapping:
        return ()
    out = [None] * (max(mapping.values()) + 1)
    for value, index in mapping.items():
        out[index] = value
    return tuple(out)


def _compute_regions(insts, resolve):
    """Per-instruction exception-handler assignment, CPython's
    ``label_exception_targets`` walk: SETUP_* pseudo ops push
    ``(target, depth-at-setup, lasti)``; POP_BLOCK pops; a handler block
    starts under the *enclosing* handler with the unwinder's pushes
    accounted for in its entry depth."""
    n = len(insts)
    handler = [None] * n
    visited = [False] * n
    # (index, tuple-of-entries, depth)
    work = [(0, (), 0)]
    while work:
        i, stack, depth = work.pop()
        while 0 <= i < n and not visited[i]:
            visited[i] = True
            op, arg = insts[i][0], insts[i][1]
            if stack:
                handler[i] = stack[-1]
            if op in _SETUP_OPS:
                lasti = _SETUP_OPS[op]
                target = resolve(arg)
                entry = (target, depth, lasti)
                # The handler runs under the enclosing handler, entered
                # with the unwinder's pushes: exc (+ lasti slot).
                work.append((target, stack, depth + (2 if lasti else 1)))
                stack = stack + (entry,)
                handler[i] = None  # pseudo op emits nothing
                i += 1
                continue
            if op == _POP_BLOCK:
                stack = stack[:-1]
                handler[i] = None
                i += 1
                continue
            if op in _JUMP_OPS:
                work.append((resolve(arg), stack, depth + _effect(op, arg)))
            depth += _effect(op, arg)
            if op in _TERMINATORS:
                break
            i += 1
    return handler


def _encode_varint_le(out, value):
    """Location-table varint: 6-bit chunks, least significant first,
    0x40 continuation (Objects/locations.md)."""
    while value >= 64:
        out.append(0x40 | (value & 0x3F))
        value >>= 6
    out.append(value)


def _encode_svarint(out, value):
    if value < 0:
        _encode_varint_le(out, ((-value) << 1) | 1)
    else:
        _encode_varint_le(out, value << 1)


def _encode_location(out, loc, length, cur_line):
    """One location entry covering ``length`` code units. Returns the new
    running line (CPython ``assemble.c write_location_info_entry``)."""
    line, end_line, col, end_col = loc
    assert 1 <= length <= 8
    if line is None or line < 0:
        out.append(0x80 | (15 << 3) | (length - 1))
        return cur_line
    if end_line is None or end_line < 0:
        end_line = line
    line_delta = line - cur_line
    if col is None or col < 0 or end_col is None or end_col < 0:
        if end_line == line:
            out.append(0x80 | (13 << 3) | (length - 1))
            _encode_svarint(out, line_delta)
            return line
    elif end_line == line:
        if line_delta == 0 and col < 80 and 0 <= end_col - col < 16:
            out.append(0x80 | ((col >> 3) << 3) | (length - 1))
            out.append(((col & 7) << 4) | (end_col - col))
            return line
        if 0 <= line_delta < 3 and col < 128 and end_col < 128:
            out.append(0x80 | ((10 + line_delta) << 3) | (length - 1))
            out.append(col)
            out.append(end_col)
            return line
    # Long form.
    out.append(0x80 | (14 << 3) | (length - 1))
    _encode_svarint(out, line_delta)
    _encode_varint_le(out, end_line - line)
    _encode_varint_le(out, 0 if col is None or col < 0 else col + 1)
    _encode_varint_le(out, 0 if end_col is None or end_col < 0 else end_col + 1)
    return line


def _encode_exc_item(out, value, first):
    """Exception-table item: 6-bit chunks, most significant first, 0x40
    continuation on all but the last, 0x80 on an entry's first byte
    (Objects/exception_handling_notes.txt)."""
    chunks = []
    while True:
        chunks.append(value & 0x3F)
        value >>= 6
        if not value:
            break
    chunks.reverse()
    for j, c in enumerate(chunks):
        b = c
        if j < len(chunks) - 1:
            b |= 0x40
        if first and j == 0:
            b |= 0x80
        out.append(b)


def assemble_code_object(filename, seq, metadata):
    insts = list(seq._insts)
    labelmap = dict(seq._labelmap)

    def resolve(arg):
        # A jump arg is a label id when one was bound, otherwise it is
        # already an instruction index (bytecode_helper passes both).
        return labelmap.get(arg, arg)

    handler = _compute_regions(insts, resolve)

    varnames = _index_map(metadata.get("varnames", {}))
    cellvars = _index_map(metadata.get("cellvars", {}))
    freevars = _index_map(metadata.get("freevars", {}))
    # CPython's localsplus merges a cellvar into its same-named argument
    # slot; WeavePy keeps `varnames ++ cellvars ++ freevars` disjoint.
    # Translate a LOAD_CLOSURE index from the merged numbering (what
    # bytecode_helper streams use) into WeavePy's, so the decoder sees a
    # cell-space fast load and the frame prologue's arg-to-cell copy
    # applies.
    merged = list(varnames) + [n for n in cellvars if n not in varnames] + list(freevars)

    def closure_slot(k):
        name = merged[k] if k < len(merged) else None
        if name in cellvars:
            return len(varnames) + cellvars.index(name)
        if name in freevars:
            return len(varnames) + len(cellvars) + freevars.index(name)
        return k

    # ---- lower pseudo instructions, remapping indices ----
    final = []  # (op, arg_or_target_index, loc, handler_entry, is_jump)
    new_index = {}  # pseudo index -> final index (next real instruction)
    pending = []
    for i, row in enumerate(insts):
        pending.append(i)
        op, arg = row[0], row[1]
        if op in _SETUP_OPS or op == _POP_BLOCK:
            continue
        if op == _LOAD_CLOSURE:
            op = _OPMAP["LOAD_FAST"]
            arg = closure_slot(arg)
        loc = row[2:6]
        for p in pending:
            new_index[p] = len(final)
        pending.clear()
        final.append([op, arg, loc, handler[i], op in _JUMP_OPS])
    for p in pending:
        new_index[p] = len(final)

    def remap(pseudo_index):
        # Clamp: a target one-past-the-end stays one-past-the-end.
        return new_index.get(pseudo_index, len(final))

    n = len(final)
    for row in final:
        if row[4]:
            row[1] = remap(resolve(row[1]))
        if row[3] is not None:
            target, depth, lasti = row[3]
            row[3] = (remap(target), depth, lasti)

    # JUMP pseudo → directional jump, now that final indices are known.
    for idx, row in enumerate(final):
        if row[0] == _JUMP or row[0] == _JUMP_NO_INTERRUPT:
            backward = row[1] <= idx
            if row[0] == _JUMP_NO_INTERRUPT and backward and _JUMP_BACKWARD_NO_INTERRUPT:
                row[0] = _JUMP_BACKWARD_NO_INTERRUPT
            else:
                row[0] = _JUMP_BACKWARD if backward else _JUMP_FORWARD

    # ---- fixpoint offsets (EXTENDED_ARG growth) ----
    opname = _opcode.opname
    ext = [0] * n  # EXTENDED_ARG prefixes per instruction
    while True:
        offsets = []
        off = 0
        for idx, row in enumerate(final):
            offsets.append(off)
            off += 1 + ext[idx] + _CACHES.get(opname[row[0]], 0)
        end = off
        changed = False
        for idx, row in enumerate(final):
            op, arg = row[0], row[1]
            if row[4]:
                size = 1 + ext[idx] + _CACHES.get(opname[op], 0)
                tgt = offsets[arg] if arg < n else end
                after = offsets[idx] + size
                delta = after - tgt if op in (_JUMP_BACKWARD, _JUMP_BACKWARD_NO_INTERRUPT) else tgt - after
                arg = max(delta, 0)
            need = 0 if arg < 0x100 else (arg.bit_length() - 1) // 8
            if need > ext[idx]:
                ext[idx] = need
                changed = True
        if not changed:
            break

    # ---- emit code bytes ----
    code = bytearray()
    for idx, row in enumerate(final):
        op, arg = row[0], row[1]
        if row[4]:
            size = 1 + ext[idx] + _CACHES.get(opname[op], 0)
            tgt = offsets[arg] if arg < n else end
            after = offsets[idx] + size
            delta = after - tgt if op in (_JUMP_BACKWARD, _JUMP_BACKWARD_NO_INTERRUPT) else tgt - after
            arg = max(delta, 0)
        for k in range(ext[idx], 0, -1):
            code.append(_EXTENDED_ARG)
            code.append((arg >> (8 * k)) & 0xFF)
        code.append(op)
        code.append(arg & 0xFF)
        for _ in range(_CACHES.get(opname[op], 0)):
            code.append(_CACHE)
            code.append(0)

    # ---- location table ----
    firstlineno = metadata.get("firstlineno", 1)
    linetable = bytearray()
    cur_line = firstlineno
    for idx, row in enumerate(final):
        loc = tuple(row[2])
        units = 1 + ext[idx] + _CACHES.get(opname[row[0]], 0)
        while units > 8:
            cur_line = _encode_location(linetable, loc, 8, cur_line)
            units -= 8
        cur_line = _encode_location(linetable, loc, units, cur_line)

    # ---- exception table (regions of equal handler entry) ----
    exctable = bytearray()
    i = 0
    while i < n:
        entry = final[i][3]
        if entry is None:
            i += 1
            continue
        j = i
        while j < n and final[j][3] == entry:
            j += 1
        start = offsets[i]
        size = (offsets[j] if j < n else end) - start
        target, depth, lasti = entry
        _encode_exc_item(exctable, start, True)
        _encode_exc_item(exctable, size, False)
        _encode_exc_item(exctable, offsets[target] if target < n else end, False)
        _encode_exc_item(exctable, (depth << 1) | (1 if lasti else 0), False)
        i = j

    # ---- stack size: simulated max depth plus handler-entry depths ----
    max_depth = 0
    depth = 0
    for row in final:
        depth += _effect(row[0], row[1])
        max_depth = max(max_depth, depth)
        if row[3] is not None:
            max_depth = max(max_depth, row[3][1] + 2)
    stacksize = max(max_depth, 1) + 1

    consts = _index_map(metadata.get("consts", {}))
    names = _index_map(metadata.get("names", {}))
    name = metadata.get("name", "name")
    qualname = metadata.get("qualname", name)

    CO_OPTIMIZED, CO_NEWLOCALS = 0x0001, 0x0002
    return _types.CodeType(
        metadata.get("argcount", 0),
        metadata.get("posonlyargcount", 0),
        metadata.get("kwonlyargcount", 0),
        len(varnames),
        stacksize,
        CO_OPTIMIZED | CO_NEWLOCALS,
        bytes(code),
        consts,
        names,
        varnames,
        metadata.get("filename", filename),
        name,
        qualname,
        firstlineno,
        bytes(linetable),
        bytes(exctable),
        freevars,
        cellvars,
    )
