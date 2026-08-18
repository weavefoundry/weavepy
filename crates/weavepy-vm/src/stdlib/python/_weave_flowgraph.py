"""``_weave_flowgraph`` — CPython 3.13's compiler flowgraph stage.

A faithful Python port of ``Python/flowgraph.c`` (v3.13) over the
CPython opcode space, backing ``_testinternalcapi.optimize_cfg`` (RFC
0068 WS1). The graded contract is ``Lib/test/test_peepholer``'s
``DirectCfgOptimizerTests`` and ``test_compile``'s CFG legs: a
pseudo-instruction sequence goes in, the optimized sequence comes out,
with CPython's exact NOP/jump/const shapes.

Every function mirrors its C namesake; the pass order in
``optimize_code_unit`` is ``_PyCfg_OptimizeCodeUnit``'s.
"""

import opcode as _opcode

_OPMAP = _opcode.opmap


def _op(name):
    return _OPMAP[name]


NOP = _op("NOP")
POP_TOP = _op("POP_TOP")
COPY = _op("COPY")
SWAP = _op("SWAP")
LOAD_CONST = _op("LOAD_CONST")
RETURN_VALUE = _op("RETURN_VALUE")
RETURN_CONST = _op("RETURN_CONST")
RAISE_VARARGS = _op("RAISE_VARARGS")
RERAISE = _op("RERAISE")
JUMP = _op("JUMP")
JUMP_NO_INTERRUPT = _op("JUMP_NO_INTERRUPT")
JUMP_FORWARD = _op("JUMP_FORWARD")
JUMP_BACKWARD = _op("JUMP_BACKWARD")
JUMP_BACKWARD_NO_INTERRUPT = _op("JUMP_BACKWARD_NO_INTERRUPT")
POP_JUMP_IF_FALSE = _op("POP_JUMP_IF_FALSE")
POP_JUMP_IF_TRUE = _op("POP_JUMP_IF_TRUE")
POP_JUMP_IF_NONE = _op("POP_JUMP_IF_NONE")
POP_JUMP_IF_NOT_NONE = _op("POP_JUMP_IF_NOT_NONE")
FOR_ITER = _op("FOR_ITER")
STORE_FAST = _op("STORE_FAST")
STORE_FAST_MAYBE_NULL = _op("STORE_FAST_MAYBE_NULL")
LOAD_FAST = _op("LOAD_FAST")
LOAD_FAST_CHECK = _op("LOAD_FAST_CHECK")
LOAD_FAST_AND_CLEAR = _op("LOAD_FAST_AND_CLEAR")
DELETE_FAST = _op("DELETE_FAST")
BUILD_TUPLE = _op("BUILD_TUPLE")
UNPACK_SEQUENCE = _op("UNPACK_SEQUENCE")
IS_OP = _op("IS_OP")
CONTAINS_OP = _op("CONTAINS_OP")
COMPARE_OP = _op("COMPARE_OP")
TO_BOOL = _op("TO_BOOL")
UNARY_NOT = _op("UNARY_NOT")
LOAD_GLOBAL = _op("LOAD_GLOBAL")
PUSH_NULL = _op("PUSH_NULL")
SETUP_FINALLY = _op("SETUP_FINALLY")
SETUP_CLEANUP = _op("SETUP_CLEANUP")
SETUP_WITH = _op("SETUP_WITH")
POP_BLOCK = _op("POP_BLOCK")
YIELD_VALUE = _op("YIELD_VALUE")
RESUME = _op("RESUME")
LOAD_CLOSURE = _op("LOAD_CLOSURE")
LOAD_FAST_LOAD_FAST = _op("LOAD_FAST_LOAD_FAST")
STORE_FAST_LOAD_FAST = _op("STORE_FAST_LOAD_FAST")
STORE_FAST_STORE_FAST = _op("STORE_FAST_STORE_FAST")
CALL = _op("CALL")
CALL_KW = _op("CALL_KW")
CALL_FUNCTION_EX = _op("CALL_FUNCTION_EX")

RESUME_AT_FUNC_START = 0
RESUME_OPARG_DEPTH1_MASK = 0x2

_HAS_ARG = frozenset(_opcode.hasarg)
_BLOCK_PUSH = frozenset(_opcode.hasexc)  # SETUP_FINALLY/SETUP_CLEANUP/SETUP_WITH
_JUMP_OPS = frozenset(_opcode.hasjrel) | frozenset(_opcode.hasjabs)
HAS_TARGET_OPS = _JUMP_OPS | _BLOCK_PUSH
_SCOPE_EXIT = frozenset((RETURN_VALUE, RETURN_CONST, RAISE_VARARGS, RERAISE))
_UNCOND_JUMP = frozenset(
    (JUMP, JUMP_NO_INTERRUPT, JUMP_FORWARD, JUMP_BACKWARD, JUMP_BACKWARD_NO_INTERRUPT)
)
_TERMINATOR = _JUMP_OPS | _SCOPE_EXIT
# Compiler-visible instructions with HAS_EVAL_BREAK_FLAG (3.13
# pycore_opcode_metadata.h; specialized forms never reach the CFG).
_EVAL_BREAK = frozenset((RESUME, CALL, CALL_KW, CALL_FUNCTION_EX, JUMP_BACKWARD, JUMP))
_HAS_CONST = frozenset(_opcode.hasconst)

NO_LOCATION = (-1, -1, -1, -1)

MAX_COPY_SIZE = 4


class Instr:
    __slots__ = ("opcode", "oparg", "loc", "target", "except_")

    def __init__(self, opcode, oparg, loc, target=None):
        self.opcode = opcode
        self.oparg = oparg
        self.loc = loc  # (lineno, end_lineno, col, end_col)
        self.target = target  # Block, for jumps/block-pushes
        self.except_ = None  # Block: innermost handler covering this instr

    def set_op0(self, opcode):
        self.opcode = opcode
        self.oparg = 0

    def set_op1(self, opcode, oparg):
        self.opcode = opcode
        self.oparg = oparg


def is_block_push(instr):
    return instr.opcode in _BLOCK_PUSH


def is_jump(instr):
    return instr.opcode in _JUMP_OPS


class Block:
    __slots__ = (
        "label",
        "instrs",
        "next",
        "predecessors",
        "visited",
        "except_handler",
        "preserve_lasti",
        "startdepth",
        "cold",
        "warm",
        "unsafe_locals_mask",
        "exceptstack",
    )

    def __init__(self):
        self.label = -1
        self.instrs = []
        self.next = None
        self.predecessors = 0
        self.visited = 0
        self.except_handler = False
        self.preserve_lasti = False
        self.startdepth = -1
        self.cold = False
        self.warm = False
        self.unsafe_locals_mask = 0
        self.exceptstack = None

    def last_instr(self):
        return self.instrs[-1] if self.instrs else None

    def no_fallthrough(self):
        last = self.last_instr()
        return last is not None and (
            last.opcode in _SCOPE_EXIT or last.opcode in _UNCOND_JUMP
        )

    def has_fallthrough(self):
        return not self.no_fallthrough()

    def exits_scope(self):
        last = self.last_instr()
        return last is not None and last.opcode in _SCOPE_EXIT

    def has_eval_break(self):
        return any(i.opcode in _EVAL_BREAK for i in self.instrs)

    def has_no_lineno(self):
        return all(i.loc[0] < 0 for i in self.instrs)


class CfgBuilder:
    def __init__(self):
        self.entry = Block()
        self.blocks = [self.entry]  # creation order (g_block_list equivalent)
        self.cur = self.entry
        self.current_label = -1

    def new_block(self):
        b = Block()
        self.blocks.append(b)
        return b

    def iter_blocks(self):
        b = self.entry
        while b is not None:
            yield b
            b = b.next

    def _current_block_is_terminated(self):
        last = self.cur.last_instr()
        if last is not None and last.opcode in _TERMINATOR:
            return True
        if self.current_label >= 0:
            if last is not None or self.cur.label >= 0:
                return True
            self.cur.label = self.current_label
            self.current_label = -1
        return False

    def _maybe_start_new_block(self):
        if self._current_block_is_terminated():
            b = self.new_block()
            b.label = self.current_label
            self.current_label = -1
            self.cur.next = b
            self.cur = b

    def use_label(self, lbl):
        self.current_label = lbl
        self._maybe_start_new_block()

    def addop(self, opcode, oparg, loc):
        self._maybe_start_new_block()
        self.cur.instrs.append(Instr(opcode, oparg, loc))


def sequence_to_cfg(rows):
    """``instr_sequence_to_cfg``: rows are (opcode, oparg, loc) with jump
    opargs already resolved to instruction indices."""
    is_target = [False] * len(rows)
    for opcode, oparg, _loc in rows:
        if opcode in HAS_TARGET_OPS:
            if not (0 <= oparg < len(rows)):
                raise ValueError("target out of range")
            is_target[oparg] = True
    g = CfgBuilder()
    for i, (opcode, oparg, loc) in enumerate(rows):
        if is_target[i]:
            g.use_label(i)
        g.addop(opcode, oparg, loc)
    return g


def get_max_label(g):
    return max((b.label for b in g.iter_blocks()), default=-1)


def translate_jump_labels_to_targets(g):
    label2block = {}
    for b in g.iter_blocks():
        if b.label >= 0:
            label2block[b.label] = b
    for b in g.iter_blocks():
        for instr in b.instrs:
            if instr.opcode in HAS_TARGET_OPS:
                instr.target = label2block[instr.oparg]


def mark_except_handlers(g):
    for b in g.iter_blocks():
        for instr in b.instrs:
            if is_block_push(instr):
                instr.target.except_handler = True


def label_exception_targets(g):
    """Per-instruction innermost-handler assignment; POP_BLOCK becomes NOP."""
    for b in g.iter_blocks():
        b.visited = 0
    entry = g.entry
    entry.visited = 1
    entry.exceptstack = []
    todo = [entry]
    while todo:
        b = todo.pop()
        except_stack = b.exceptstack
        b.exceptstack = None
        handler = except_stack[-1] if except_stack else None
        last_yield_except_depth = -1
        for instr in b.instrs:
            if is_block_push(instr):
                if not instr.target.visited:
                    instr.target.exceptstack = list(except_stack)
                    instr.target.visited = 1
                    todo.append(instr.target)
                if instr.opcode in (SETUP_WITH, SETUP_CLEANUP):
                    instr.target.preserve_lasti = True
                except_stack.append(instr.target)
                handler = instr.target
            elif instr.opcode == POP_BLOCK:
                except_stack.pop()
                handler = except_stack[-1] if except_stack else None
                instr.set_op0(NOP)
            elif is_jump(instr):
                instr.except_ = handler
                if not instr.target.visited:
                    if b.has_fallthrough():
                        instr.target.exceptstack = list(except_stack)
                    else:
                        instr.target.exceptstack = except_stack
                        except_stack = None
                    instr.target.visited = 1
                    todo.append(instr.target)
            elif instr.opcode == YIELD_VALUE:
                instr.except_ = handler
                last_yield_except_depth = len(except_stack)
            elif instr.opcode == RESUME:
                instr.except_ = handler
                if instr.oparg != RESUME_AT_FUNC_START:
                    if last_yield_except_depth == 1:
                        instr.oparg |= RESUME_OPARG_DEPTH1_MASK
                    last_yield_except_depth = -1
            else:
                instr.except_ = handler
        if b.has_fallthrough() and b.next is not None and not b.next.visited:
            b.next.exceptstack = except_stack
            b.next.visited = 1
            todo.append(b.next)


def check_cfg(g):
    for b in g.iter_blocks():
        for i, instr in enumerate(b.instrs):
            if instr.opcode in _TERMINATOR and i != len(b.instrs) - 1:
                raise SystemError("malformed control flow graph.")


def next_nonempty_block(b):
    while b is not None and not b.instrs:
        b = b.next
    return b


# ---- optimize_cfg passes -------------------------------------------------


def basicblock_inline_small_or_no_lineno_blocks(bb):
    last = bb.last_instr()
    if last is None or last.opcode not in _UNCOND_JUMP:
        return False
    target = last.target
    small_exit_block = target.exits_scope() and len(target.instrs) <= MAX_COPY_SIZE
    no_lineno_no_fallthrough = target.has_no_lineno() and target.no_fallthrough()
    if small_exit_block or no_lineno_no_fallthrough:
        removed_jump_opcode = last.opcode
        last.set_op0(NOP)
        last.target = None
        bb.instrs.extend(Instr(i.opcode, i.oparg, i.loc, i.target) for i in target.instrs)
        for src, dup in zip(target.instrs, bb.instrs[-len(target.instrs):]):
            dup.except_ = src.except_
        if no_lineno_no_fallthrough:
            last = bb.last_instr()
            if last.opcode in _UNCOND_JUMP and removed_jump_opcode == JUMP:
                # Make sure we don't lose eval breaker checks.
                last.opcode = JUMP
        target.predecessors -= 1
        return True
    return False


def inline_small_or_no_lineno_blocks(g):
    changes = True
    while changes:
        changes = False
        for b in g.iter_blocks():
            if basicblock_inline_small_or_no_lineno_blocks(b):
                changes = True


def remove_unreachable(g):
    for b in g.iter_blocks():
        b.predecessors = 0
        b.visited = 0
    entry = g.entry
    entry.predecessors = 1
    entry.visited = 1
    stack = [entry]
    while stack:
        b = stack.pop()
        if b.next is not None and b.has_fallthrough():
            if not b.next.visited:
                stack.append(b.next)
                b.next.visited = 1
            b.next.predecessors += 1
        for instr in b.instrs:
            if is_jump(instr) or is_block_push(instr):
                target = instr.target
                if not target.visited:
                    stack.append(target)
                    target.visited = 1
                target.predecessors += 1
    for b in g.iter_blocks():
        if b.predecessors == 0:
            b.instrs = []
            b.except_handler = False


def basicblock_remove_redundant_nops(bb):
    out = []
    prev_lineno = -1
    n = len(bb.instrs)
    for src in range(n):
        instr = bb.instrs[src]
        lineno = instr.loc[0]
        if instr.opcode == NOP:
            if lineno < 0:
                continue
            if prev_lineno == lineno:
                continue
            if src < n - 1:
                next_lineno = bb.instrs[src + 1].loc[0]
                if next_lineno == lineno:
                    continue
                if next_lineno < 0:
                    bb.instrs[src + 1].loc = instr.loc
                    continue
            else:
                nxt = next_nonempty_block(bb.next)
                if nxt is not None:
                    next_loc = NO_LOCATION
                    for ni in nxt.instrs:
                        if ni.opcode == NOP and ni.loc[0] == -1:
                            continue
                        next_loc = ni.loc
                        break
                    if lineno == next_loc[0]:
                        continue
        out.append(instr)
        prev_lineno = lineno
    removed = n - len(out)
    bb.instrs = out
    return removed


def remove_redundant_nops(g):
    changes = 0
    for b in g.iter_blocks():
        changes += basicblock_remove_redundant_nops(b)
    return changes


def remove_redundant_nops_and_pairs(g):
    done = False
    while not done:
        done = True
        instr = None
        for b in g.iter_blocks():
            basicblock_remove_redundant_nops(b)
            if b.label >= 0:
                instr = None
            for i in range(len(b.instrs)):
                prev_instr = instr
                instr = b.instrs[i]
                prev_opcode = prev_instr.opcode if prev_instr else 0
                prev_oparg = prev_instr.oparg if prev_instr else 0
                is_redundant_pair = False
                if instr.opcode == POP_TOP:
                    if prev_opcode == LOAD_CONST:
                        is_redundant_pair = True
                    elif prev_opcode == COPY and prev_oparg == 1:
                        is_redundant_pair = True
                if is_redundant_pair:
                    prev_instr.set_op0(NOP)
                    instr.set_op0(NOP)
                    done = False
            if (instr is not None and is_jump(instr)) or not b.has_fallthrough():
                instr = None


def remove_redundant_jumps(g):
    changes = 0
    for b in g.iter_blocks():
        last = b.last_instr()
        if last is None:
            continue
        if last.opcode in _UNCOND_JUMP:
            jump_target = next_nonempty_block(last.target)
            if jump_target is None:
                raise SystemError("jump with NULL target")
            nxt = next_nonempty_block(b.next)
            if jump_target is nxt:
                changes += 1
                last.set_op0(NOP)
                last.target = None
    return changes


def remove_redundant_nops_and_jumps(g):
    while True:
        removed = remove_redundant_nops(g)
        removed += remove_redundant_jumps(g)
        if not removed:
            break


# ---- location resolution -------------------------------------------------


def is_exit_or_eval_check_without_lineno(b):
    if b.exits_scope() or b.has_eval_break():
        return b.has_no_lineno()
    return False


def duplicate_exits_without_lineno(g):
    next_lbl = get_max_label(g) + 1
    for b in g.iter_blocks():
        last = b.last_instr()
        if last is None:
            continue
        if is_jump(last):
            target = next_nonempty_block(last.target)
            if is_exit_or_eval_check_without_lineno(target) and target.predecessors > 1:
                new_target = g.new_block()
                new_target.instrs = [
                    Instr(i.opcode, i.oparg, i.loc, i.target) for i in target.instrs
                ]
                for src, dup in zip(target.instrs, new_target.instrs):
                    dup.except_ = src.except_
                new_target.instrs[0].loc = last.loc
                last.target = new_target
                target.predecessors -= 1
                new_target.predecessors = 1
                new_target.next = target.next
                new_target.label = next_lbl
                next_lbl += 1
                target.next = new_target
    for b in g.iter_blocks():
        if b.has_fallthrough() and b.next is not None and b.instrs:
            if is_exit_or_eval_check_without_lineno(b.next):
                b.next.instrs[0].loc = b.last_instr().loc


def propagate_line_numbers(g):
    for b in g.iter_blocks():
        last = b.last_instr()
        if last is None:
            continue
        prev_location = NO_LOCATION
        for instr in b.instrs:
            if instr.loc[0] < 0:
                instr.loc = prev_location
            else:
                prev_location = instr.loc
        if b.has_fallthrough() and b.next.predecessors == 1:
            if b.next.instrs and b.next.instrs[0].loc[0] < 0:
                b.next.instrs[0].loc = prev_location
        if is_jump(last):
            target = last.target
            if target.predecessors == 1:
                if target.instrs and target.instrs[0].loc[0] < 0:
                    target.instrs[0].loc = prev_location


def resolve_line_numbers(g, firstlineno):
    duplicate_exits_without_lineno(g)
    propagate_line_numbers(g)


# ---- constant folding / peephole ------------------------------------------


def get_const_value(opcode, oparg, consts):
    assert opcode in _HAS_CONST
    if opcode == LOAD_CONST:
        return consts[oparg]
    raise SystemError("Internal error: failed to get value of a constant")


class _ConstKey:
    """CPython's const-cache key: value plus type (and recursively for
    containers), so 0 / 0.0 / False stay distinct constants."""

    __slots__ = ("key",)

    def __init__(self, value):
        self.key = self._make(value)

    @staticmethod
    def _make(value):
        t = type(value)
        if t is tuple:
            return (t, tuple(_ConstKey._make(v) for v in value))
        if t is frozenset:
            return (t, frozenset(_ConstKey._make(v) for v in value))
        if t is float:
            # Distinguish 0.0 / -0.0 (equal, same type, different consts).
            if value == 0.0:
                import math

                return (t, "-0.0" if math.copysign(1.0, value) < 0 else "0.0")
            return (t, value)
        if t is complex:
            import math

            key = (value.real, value.imag)
            if value.real == 0.0 or value.imag == 0.0:
                key = (
                    math.copysign(1.0, value.real),
                    value.real,
                    math.copysign(1.0, value.imag),
                    value.imag,
                )
            return (t, key)
        if t is slice:
            return (t, (_ConstKey._make(value.start), _ConstKey._make(value.stop), _ConstKey._make(value.step)))
        try:
            hash(value)
        except TypeError:
            return (t, id(value))
        return (t, value)

    def __hash__(self):
        return hash(self.key)

    def __eq__(self, other):
        return isinstance(other, _ConstKey) and self.key == other.key


def add_const(newconst, consts, const_cache):
    key = _ConstKey(newconst)
    cached = const_cache.get(key)
    if cached is not None:
        newconst = cached
    else:
        const_cache[key] = newconst
    for index, c in enumerate(consts):
        if c is newconst:
            return index
    consts.append(newconst)
    return len(consts) - 1


def fold_tuple_on_constants(const_cache, instrs, start, n, consts):
    """instrs[start:start+n] are candidate LOAD_CONSTs; instrs[start+n]
    is BUILD_TUPLE(n)."""
    for i in range(start, start + n):
        if instrs[i].opcode not in _HAS_CONST:
            return
    newconst = tuple(
        get_const_value(instrs[i].opcode, instrs[i].oparg, consts)
        for i in range(start, start + n)
    )
    index = add_const(newconst, consts, const_cache)
    for i in range(start, start + n):
        instrs[i].set_op0(NOP)
    instrs[start + n].set_op1(LOAD_CONST, index)


def swaptimize(block, ix):
    """Replace a run of SWAPs/NOPs with an optimal one. Returns the new
    scan index (last instruction of the run)."""
    instructions = block.instrs
    assert instructions[ix].opcode == SWAP
    depth = instructions[ix].oparg
    length = 0
    more = False
    limit = len(instructions) - ix
    while True:
        length += 1
        if length >= limit:
            break
        op = instructions[ix + length].opcode
        if op == SWAP:
            depth = max(depth, instructions[ix + length].oparg)
            more = True
        elif op != NOP:
            break
    if not more:
        return ix
    stack = list(range(depth))
    for i in range(length):
        instr = instructions[ix + i]
        if instr.opcode == SWAP:
            oparg = instr.oparg
            stack[0], stack[oparg - 1] = stack[oparg - 1], stack[0]
    VISITED = -1
    current = length - 1
    for i in range(depth):
        if stack[i] == VISITED or stack[i] == i:
            continue
        j = i
        while True:
            if j:
                assert current >= 0
                instructions[ix + current].opcode = SWAP
                instructions[ix + current].oparg = j + 1
                current -= 1
            if stack[j] == VISITED:
                break
            next_j = stack[j]
            stack[j] = VISITED
            j = next_j
    while current >= 0:
        instructions[ix + current].set_op0(NOP)
        current -= 1
    return ix + length - 1


def _swappable(opcode):
    return opcode in (STORE_FAST, STORE_FAST_MAYBE_NULL, POP_TOP)


def _stores_to(instr):
    if instr.opcode in (STORE_FAST, STORE_FAST_MAYBE_NULL):
        return instr.oparg
    return -1


def next_swappable_instruction(block, i, lineno):
    n = len(block.instrs)
    i += 1
    while i < n:
        instruction = block.instrs[i]
        if 0 <= lineno and instruction.loc[0] != lineno:
            return -1
        if instruction.opcode == NOP:
            i += 1
            continue
        if _swappable(instruction.opcode):
            return i
        return -1
    return -1


def apply_static_swaps(block, i):
    while i >= 0:
        swap = block.instrs[i]
        if swap.opcode != SWAP:
            if swap.opcode == NOP or _swappable(swap.opcode):
                i -= 1
                continue
            return
        j = next_swappable_instruction(block, i, -1)
        if j < 0:
            return
        k = j
        lineno = block.instrs[j].loc[0]
        for _count in range(swap.oparg - 1, 0, -1):
            k = next_swappable_instruction(block, k, lineno)
            if k < 0:
                return
        store_j = _stores_to(block.instrs[j])
        store_k = _stores_to(block.instrs[k])
        if store_j >= 0 or store_k >= 0:
            if store_j == store_k:
                return
            for idx in range(j + 1, k):
                store_idx = _stores_to(block.instrs[idx])
                if store_idx >= 0 and (store_idx == store_j or store_idx == store_k):
                    return
        swap.set_op0(NOP)
        block.instrs[j], block.instrs[k] = block.instrs[k], block.instrs[j]
        i -= 1


def basicblock_optimize_load_const(const_cache, bb, consts):
    opcode = 0
    oparg = 0
    i = 0
    while i < len(bb.instrs):
        inst = bb.instrs[i]
        is_copy_of_load_const = (
            opcode == LOAD_CONST and inst.opcode == COPY and inst.oparg == 1
        )
        if not is_copy_of_load_const:
            opcode = inst.opcode
            oparg = inst.oparg
        if opcode != LOAD_CONST:
            i += 1
            continue
        nextop = bb.instrs[i + 1].opcode if i + 1 < len(bb.instrs) else 0
        if nextop in (POP_JUMP_IF_FALSE, POP_JUMP_IF_TRUE):
            cnt = get_const_value(opcode, oparg, consts)
            is_true = bool(cnt)
            inst.set_op0(NOP)
            jump_if_true = nextop == POP_JUMP_IF_TRUE
            if is_true == jump_if_true:
                bb.instrs[i + 1].opcode = JUMP
            else:
                bb.instrs[i + 1].set_op0(NOP)
                bb.instrs[i + 1].target = None
        elif nextop == IS_OP:
            cnt = get_const_value(opcode, oparg, consts)
            if cnt is not None:
                i += 1
                continue
            if len(bb.instrs) <= i + 2:
                i += 1
                continue
            is_instr = bb.instrs[i + 1]
            jump_instr = bb.instrs[i + 2]
            if jump_instr.opcode == TO_BOOL:
                jump_instr.set_op0(NOP)
                if len(bb.instrs) <= i + 3:
                    i += 1
                    continue
                jump_instr = bb.instrs[i + 3]
            invert = bool(is_instr.oparg)
            if jump_instr.opcode == POP_JUMP_IF_FALSE:
                invert = not invert
            elif jump_instr.opcode != POP_JUMP_IF_TRUE:
                i += 1
                continue
            inst.set_op0(NOP)
            is_instr.set_op0(NOP)
            jump_instr.opcode = POP_JUMP_IF_NOT_NONE if invert else POP_JUMP_IF_NONE
        elif nextop == RETURN_VALUE:
            inst.set_op0(NOP)
            i += 1
            bb.instrs[i].set_op1(RETURN_CONST, oparg)
        elif nextop == TO_BOOL:
            cnt = get_const_value(opcode, oparg, consts)
            is_true = bool(cnt)
            index = add_const(bool(is_true), consts, const_cache)
            inst.set_op0(NOP)
            bb.instrs[i + 1].set_op1(LOAD_CONST, index)
        i += 1


def optimize_load_const(const_cache, g, consts):
    for b in g.iter_blocks():
        basicblock_optimize_load_const(const_cache, b, consts)


def jump_thread(bb, inst, target, opcode):
    """NOP out inst and append a jump to target.target."""
    assert is_jump(inst)
    assert is_jump(target)
    assert inst is bb.last_instr()
    if inst.target is not target.target:
        new_target = target.target
        new_loc = target.loc
        inst.set_op0(NOP)
        inst.target = None
        jump = Instr(opcode, new_target.label, new_loc, new_target)
        bb.instrs.append(jump)
        return True
    return False


def optimize_basic_block(const_cache, bb, consts):
    i = 0
    while i < len(bb.instrs):
        inst = bb.instrs[i]
        opcode = inst.opcode
        oparg = inst.oparg
        if opcode in HAS_TARGET_OPS and inst.target is not None:
            assert inst.target.instrs
            target = inst.target.instrs[0]
        else:
            target = Instr(NOP, 0, NO_LOCATION)
        nextop = bb.instrs[i + 1].opcode if i + 1 < len(bb.instrs) else 0
        if opcode == BUILD_TUPLE:
            if nextop == UNPACK_SEQUENCE and oparg == bb.instrs[i + 1].oparg:
                if oparg == 1:
                    inst.set_op0(NOP)
                    bb.instrs[i + 1].set_op0(NOP)
                    i += 1
                    continue
                if oparg in (2, 3):
                    inst.set_op0(NOP)
                    bb.instrs[i + 1].opcode = SWAP
                    i += 1
                    continue
            if i >= oparg:
                fold_tuple_on_constants(const_cache, bb.instrs, i - oparg, oparg, consts)
        elif opcode in (POP_JUMP_IF_NOT_NONE, POP_JUMP_IF_NONE):
            if target.opcode == JUMP:
                i -= jump_thread(bb, inst, target, opcode)
        elif opcode == POP_JUMP_IF_FALSE:
            if target.opcode == JUMP:
                i -= jump_thread(bb, inst, target, POP_JUMP_IF_FALSE)
        elif opcode == POP_JUMP_IF_TRUE:
            if target.opcode == JUMP:
                i -= jump_thread(bb, inst, target, POP_JUMP_IF_TRUE)
        elif opcode in (JUMP, JUMP_NO_INTERRUPT):
            if target.opcode == JUMP:
                i -= jump_thread(bb, inst, target, JUMP)
                i += 1
                continue
            if target.opcode == JUMP_NO_INTERRUPT:
                i -= jump_thread(bb, inst, target, opcode)
                i += 1
                continue
        elif opcode == STORE_FAST:
            if (
                nextop == STORE_FAST
                and oparg == bb.instrs[i + 1].oparg
                and inst.loc[0] == bb.instrs[i + 1].loc[0]
            ):
                inst.opcode = POP_TOP
                inst.oparg = 0
        elif opcode == SWAP:
            if oparg == 1:
                inst.set_op0(NOP)
        elif opcode == LOAD_GLOBAL:
            if nextop == PUSH_NULL and (oparg & 1) == 0:
                inst.set_op1(LOAD_GLOBAL, oparg | 1)
                bb.instrs[i + 1].set_op0(NOP)
        elif opcode == COMPARE_OP:
            if nextop == TO_BOOL:
                inst.set_op0(NOP)
                bb.instrs[i + 1].set_op1(COMPARE_OP, oparg | 16)
                i += 1
                continue
        elif opcode in (CONTAINS_OP, IS_OP):
            if nextop == TO_BOOL:
                inst.set_op0(NOP)
                bb.instrs[i + 1].set_op1(opcode, oparg)
                i += 1
                continue
        elif opcode == TO_BOOL:
            if nextop == TO_BOOL:
                inst.set_op0(NOP)
                i += 1
                continue
        elif opcode == UNARY_NOT:
            if nextop == TO_BOOL:
                inst.set_op0(NOP)
                bb.instrs[i + 1].set_op0(UNARY_NOT)
                i += 1
                continue
            if nextop == UNARY_NOT:
                inst.set_op0(NOP)
                bb.instrs[i + 1].set_op0(NOP)
                i += 1
                continue
        i += 1

    i = 0
    while i < len(bb.instrs):
        if bb.instrs[i].opcode == SWAP:
            i = swaptimize(bb, i)
            apply_static_swaps(bb, i)
        i += 1


def optimize_cfg(g, consts, const_cache, firstlineno):
    check_cfg(g)
    inline_small_or_no_lineno_blocks(g)
    remove_unreachable(g)
    resolve_line_numbers(g, firstlineno)
    optimize_load_const(const_cache, g, consts)
    for b in g.iter_blocks():
        optimize_basic_block(const_cache, b, consts)
    remove_redundant_nops_and_pairs(g)
    remove_unreachable(g)
    remove_redundant_nops_and_jumps(g)


# ---- post-optimization passes ---------------------------------------------


def remove_unused_consts(g, consts):
    nconsts = len(consts)
    if nconsts == 0:
        return
    used = [False] * nconsts
    used[0] = True  # first constant may be a docstring; always kept
    for b in g.iter_blocks():
        for instr in b.instrs:
            if instr.opcode in _HAS_CONST:
                used[instr.oparg] = True
    if all(used):
        return
    reverse = {}
    new_consts = []
    for i, keep in enumerate(used):
        if keep:
            reverse[i] = len(new_consts)
            new_consts.append(consts[i])
    consts[:] = new_consts
    for b in g.iter_blocks():
        for instr in b.instrs:
            if instr.opcode in _HAS_CONST:
                instr.oparg = reverse[instr.oparg]


def _maybe_push(b, unsafe_mask, stack):
    both = b.unsafe_locals_mask | unsafe_mask
    if b.unsafe_locals_mask != both:
        b.unsafe_locals_mask = both
        if not b.visited:
            stack.append(b)
            b.visited = 1


def scan_block_for_locals(b, stack):
    unsafe_mask = b.unsafe_locals_mask
    for instr in b.instrs:
        if instr.except_ is not None:
            _maybe_push(instr.except_, unsafe_mask, stack)
        if instr.oparg >= 64:
            continue
        bit = 1 << instr.oparg
        op = instr.opcode
        if op in (DELETE_FAST, LOAD_FAST_AND_CLEAR, STORE_FAST_MAYBE_NULL):
            unsafe_mask |= bit
        elif op == STORE_FAST:
            unsafe_mask &= ~bit
        elif op == LOAD_FAST_CHECK:
            unsafe_mask &= ~bit
        elif op == LOAD_FAST:
            if unsafe_mask & bit:
                instr.opcode = LOAD_FAST_CHECK
            unsafe_mask &= ~bit
    if b.next is not None and b.has_fallthrough():
        _maybe_push(b.next, unsafe_mask, stack)
    last = b.last_instr()
    if last is not None and is_jump(last):
        _maybe_push(last.target, unsafe_mask, stack)


def fast_scan_many_locals(g, nlocals):
    states = [0] * (nlocals - 64)
    blocknum = 0
    for b in g.iter_blocks():
        blocknum += 1
        for instr in b.instrs:
            arg = instr.oparg
            if arg < 64:
                continue
            op = instr.opcode
            if op in (DELETE_FAST, LOAD_FAST_AND_CLEAR, STORE_FAST_MAYBE_NULL):
                states[arg - 64] = blocknum - 1
            elif op == STORE_FAST:
                states[arg - 64] = blocknum
            elif op == LOAD_FAST:
                if states[arg - 64] != blocknum:
                    instr.opcode = LOAD_FAST_CHECK
                states[arg - 64] = blocknum


def add_checks_for_loads_of_uninitialized_variables(g, nlocals, nparams):
    if nlocals == 0:
        return
    if nlocals > 64:
        fast_scan_many_locals(g, nlocals)
        nlocals = 64
    for b in g.iter_blocks():
        b.visited = 0
        b.unsafe_locals_mask = 0
    stack = []
    start_mask = 0
    for i in range(nparams, nlocals):
        start_mask |= 1 << i
    _maybe_push(g.entry, start_mask, stack)
    for b in g.iter_blocks():
        scan_block_for_locals(b, stack)
    while stack:
        b = stack.pop()
        b.visited = 0
        scan_block_for_locals(b, stack)


def make_super_instruction(inst1, inst2, super_op):
    line1 = inst1.loc[0]
    line2 = inst2.loc[0]
    if line1 >= 0 and line2 >= 0 and line1 != line2:
        return
    if inst1.oparg >= 16 or inst2.oparg >= 16:
        return
    inst1.set_op1(super_op, (inst1.oparg << 4) | inst2.oparg)
    inst2.set_op0(NOP)


def insert_superinstructions(g):
    for b in g.iter_blocks():
        for i in range(len(b.instrs)):
            inst = b.instrs[i]
            nextop = b.instrs[i + 1].opcode if i + 1 < len(b.instrs) else 0
            if inst.opcode == LOAD_FAST:
                if nextop == LOAD_FAST:
                    make_super_instruction(inst, b.instrs[i + 1], LOAD_FAST_LOAD_FAST)
            elif inst.opcode == STORE_FAST:
                if nextop == LOAD_FAST:
                    make_super_instruction(inst, b.instrs[i + 1], STORE_FAST_LOAD_FAST)
                elif nextop == STORE_FAST:
                    make_super_instruction(inst, b.instrs[i + 1], STORE_FAST_STORE_FAST)
    remove_redundant_nops(g)


def mark_warm(g):
    for b in g.iter_blocks():
        b.visited = 0
    entry = g.entry
    stack = [entry]
    entry.visited = 1
    while stack:
        b = stack.pop()
        b.warm = True
        nxt = b.next
        if nxt is not None and b.has_fallthrough() and not nxt.visited:
            stack.append(nxt)
            nxt.visited = 1
        for instr in b.instrs:
            if is_jump(instr) and not instr.target.visited:
                stack.append(instr.target)
                instr.target.visited = 1


def mark_cold(g):
    mark_warm(g)
    for b in g.iter_blocks():
        b.visited = 0
    stack = []
    for b in g.iter_blocks():
        if b.except_handler:
            stack.append(b)
            b.visited = 1
    while stack:
        b = stack.pop()
        b.cold = True
        nxt = b.next
        if nxt is not None and b.has_fallthrough():
            if not nxt.warm and not nxt.visited:
                stack.append(nxt)
                nxt.visited = 1
        for instr in b.instrs:
            if is_jump(instr):
                target = instr.target
                if not target.warm and not target.visited:
                    stack.append(target)
                    target.visited = 1


def push_cold_blocks_to_end(g):
    if g.entry.next is None:
        return
    mark_cold(g)
    next_lbl = get_max_label(g) + 1
    for b in g.iter_blocks():
        if b.cold and b.has_fallthrough() and b.next is not None and b.next.warm:
            explicit_jump = g.new_block()
            if b.next.label < 0:
                b.next.label = next_lbl
                next_lbl += 1
            explicit_jump.instrs.append(
                Instr(JUMP_NO_INTERRUPT, b.next.label, NO_LOCATION, b.next)
            )
            explicit_jump.cold = True
            explicit_jump.next = b.next
            explicit_jump.predecessors = 1
            b.next = explicit_jump

    cold_blocks = None
    cold_blocks_tail = None
    b = g.entry
    while b.next is not None:
        while b.next is not None and not b.next.cold:
            b = b.next
        if b.next is None:
            break
        b_end = b.next
        while b_end.next is not None and b_end.next.cold:
            b_end = b_end.next
        if cold_blocks is None:
            cold_blocks = b.next
        else:
            cold_blocks_tail.next = b.next
        cold_blocks_tail = b_end
        b.next = b_end.next
        b_end.next = None
    b.next = cold_blocks
    if cold_blocks is not None:
        remove_redundant_nops_and_jumps(g)


def optimize_code_unit(g, consts, const_cache, nlocals, nparams, firstlineno):
    """``_PyCfg_OptimizeCodeUnit``: the full pass pipeline."""
    translate_jump_labels_to_targets(g)
    mark_except_handlers(g)
    label_exception_targets(g)
    optimize_cfg(g, consts, const_cache, firstlineno)
    remove_unused_consts(g, consts)
    add_checks_for_loads_of_uninitialized_variables(g, nlocals, nparams)
    insert_superinstructions(g)
    push_cold_blocks_to_end(g)
    resolve_line_numbers(g, firstlineno)


def cfg_to_rows(g):
    """``_PyCfg_ToInstructionSequence`` + ``ApplyLabelMap``: relabel blocks
    consecutively, emit (opcode, oparg, loc) rows with jump args resolved
    to instruction indices."""
    blocks = list(g.iter_blocks())
    for lbl, b in enumerate(blocks):
        b.label = lbl
    # label -> first instruction index (labels mark block starts)
    label_index = {}
    idx = 0
    for b in blocks:
        label_index[b.label] = idx
        idx += len(b.instrs)
    rows = []
    for b in blocks:
        for instr in b.instrs:
            oparg = instr.oparg
            if instr.opcode in HAS_TARGET_OPS:
                oparg = label_index[instr.target.label]
            rows.append((instr.opcode, oparg, instr.loc))
    return rows


def optimize_cfg_rows(rows, consts, nlocals):
    """``_PyCompile_OptimizeCfg``: rows in, optimized rows out. ``consts``
    must be a mutable list (new constants are appended)."""
    g = sequence_to_cfg(rows)
    const_cache = {}
    optimize_code_unit(g, consts, const_cache, nlocals, nparams=0, firstlineno=1)
    return cfg_to_rows(g)
