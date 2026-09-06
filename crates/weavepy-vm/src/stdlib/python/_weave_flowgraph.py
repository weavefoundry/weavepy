"""``_weave_flowgraph`` — CPython 3.14's compiler flowgraph stage.

A faithful Python port of ``Python/flowgraph.c`` (v3.14.7) over the
CPython opcode space, backing ``_testinternalcapi.optimize_cfg`` (RFC
0068 WS1; RFC 0077 moved the port up from 3.13). The graded contract is
``Lib/test/test_peepholer``'s ``DirectCfgOptimizerTests`` /
``OptimizeLoadFastTestCase`` and ``test_compile``'s CFG legs: a
pseudo-instruction sequence goes in, the optimized sequence comes out,
with CPython's exact NOP/jump/const shapes.

Every function mirrors its C namesake; the pass order in
``optimize_code_unit`` is ``_PyCfg_OptimizeCodeUnit``'s, and
``optimize_cfg_rows`` is ``_PyCompile_OptimizeCfg`` (which additionally
runs ``calculate_stackdepth`` and ``optimize_load_fast``).

3.14 deltas: constant folding of binary/unary ops and of list/set/tuple
displays lives here now (the AST optimizer only folds a handful of
shapes); ``LOAD_SMALL_INT`` replaces ``LOAD_CONST`` of 0..255;
``RETURN_CONST`` is gone; ``JUMP_IF_TRUE`` / ``JUMP_IF_FALSE`` pseudo
jumps are threaded; ``LOAD_FAST`` strength-reduces to
``LOAD_FAST_BORROW`` when the borrowed reference provably dies before
its local does.
"""

import opcode as _opcode
import _opcode as _opcode_tables

_OPMAP = _opcode.opmap


def _op(name):
    return _OPMAP[name]


NOP = _op("NOP")
POP_TOP = _op("POP_TOP")
COPY = _op("COPY")
SWAP = _op("SWAP")
LOAD_CONST = _op("LOAD_CONST")
LOAD_SMALL_INT = _op("LOAD_SMALL_INT")
RETURN_VALUE = _op("RETURN_VALUE")
RAISE_VARARGS = _op("RAISE_VARARGS")
RERAISE = _op("RERAISE")
JUMP = _op("JUMP")
JUMP_NO_INTERRUPT = _op("JUMP_NO_INTERRUPT")
JUMP_FORWARD = _op("JUMP_FORWARD")
JUMP_BACKWARD = _op("JUMP_BACKWARD")
JUMP_BACKWARD_NO_INTERRUPT = _op("JUMP_BACKWARD_NO_INTERRUPT")
JUMP_IF_FALSE = _op("JUMP_IF_FALSE")
JUMP_IF_TRUE = _op("JUMP_IF_TRUE")
POP_JUMP_IF_FALSE = _op("POP_JUMP_IF_FALSE")
POP_JUMP_IF_TRUE = _op("POP_JUMP_IF_TRUE")
POP_JUMP_IF_NONE = _op("POP_JUMP_IF_NONE")
POP_JUMP_IF_NOT_NONE = _op("POP_JUMP_IF_NOT_NONE")
FOR_ITER = _op("FOR_ITER")
SEND = _op("SEND")
END_ASYNC_FOR = _op("END_ASYNC_FOR")
STORE_FAST = _op("STORE_FAST")
STORE_FAST_MAYBE_NULL = _op("STORE_FAST_MAYBE_NULL")
LOAD_FAST = _op("LOAD_FAST")
LOAD_FAST_CHECK = _op("LOAD_FAST_CHECK")
LOAD_FAST_AND_CLEAR = _op("LOAD_FAST_AND_CLEAR")
LOAD_FAST_BORROW = _op("LOAD_FAST_BORROW")
LOAD_FAST_BORROW_LOAD_FAST_BORROW = _op("LOAD_FAST_BORROW_LOAD_FAST_BORROW")
DELETE_FAST = _op("DELETE_FAST")
BUILD_TUPLE = _op("BUILD_TUPLE")
BUILD_LIST = _op("BUILD_LIST")
BUILD_SET = _op("BUILD_SET")
LIST_APPEND = _op("LIST_APPEND")
LIST_EXTEND = _op("LIST_EXTEND")
SET_ADD = _op("SET_ADD")
SET_UPDATE = _op("SET_UPDATE")
MAP_ADD = _op("MAP_ADD")
DICT_MERGE = _op("DICT_MERGE")
DICT_UPDATE = _op("DICT_UPDATE")
UNPACK_SEQUENCE = _op("UNPACK_SEQUENCE")
IS_OP = _op("IS_OP")
CONTAINS_OP = _op("CONTAINS_OP")
COMPARE_OP = _op("COMPARE_OP")
TO_BOOL = _op("TO_BOOL")
UNARY_NOT = _op("UNARY_NOT")
UNARY_INVERT = _op("UNARY_INVERT")
UNARY_NEGATIVE = _op("UNARY_NEGATIVE")
BINARY_OP = _op("BINARY_OP")
CALL_INTRINSIC_1 = _op("CALL_INTRINSIC_1")
GET_ITER = _op("GET_ITER")
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
ANNOTATIONS_PLACEHOLDER = _op("ANNOTATIONS_PLACEHOLDER")
FORMAT_SIMPLE = _op("FORMAT_SIMPLE")
GET_ANEXT = _op("GET_ANEXT")
GET_LEN = _op("GET_LEN")
GET_YIELD_FROM_ITER = _op("GET_YIELD_FROM_ITER")
IMPORT_FROM = _op("IMPORT_FROM")
MATCH_KEYS = _op("MATCH_KEYS")
MATCH_MAPPING = _op("MATCH_MAPPING")
MATCH_SEQUENCE = _op("MATCH_SEQUENCE")
WITH_EXCEPT_START = _op("WITH_EXCEPT_START")
END_SEND = _op("END_SEND")
SET_FUNCTION_ATTRIBUTE = _op("SET_FUNCTION_ATTRIBUTE")
CHECK_EXC_MATCH = _op("CHECK_EXC_MATCH")
LOAD_ATTR = _op("LOAD_ATTR")
LOAD_SUPER_ATTR = _op("LOAD_SUPER_ATTR")
LOAD_SPECIAL = _op("LOAD_SPECIAL")
PUSH_EXC_INFO = _op("PUSH_EXC_INFO")

RESUME_AT_FUNC_START = 0
RESUME_OPARG_DEPTH1_MASK = 0x2

# `Include/internal/pycore_intrinsics.h`
INTRINSIC_UNARY_POSITIVE = 5
INTRINSIC_LIST_TO_TUPLE = 6

# `Include/opcode_ids.h` / `pycore_code.h`: BINARY_OP opargs.
NB_ADD = 0
NB_AND = 1
NB_FLOOR_DIVIDE = 2
NB_LSHIFT = 3
NB_MATRIX_MULTIPLY = 4
NB_MULTIPLY = 5
NB_REMAINDER = 6
NB_OR = 7
NB_POWER = 8
NB_RSHIFT = 9
NB_SUBTRACT = 10
NB_TRUE_DIVIDE = 11
NB_XOR = 12
NB_INPLACE_ADD = 13
NB_INPLACE_XOR = 25
NB_SUBSCR = 26
NB_OPARG_LAST = NB_SUBSCR

# `Include/internal/pycore_compile.h`
STACK_USE_GUIDELINE = 30
MIN_CONST_SEQUENCE_SIZE = 3

_HAS_ARG = frozenset(_opcode.hasarg)
_BLOCK_PUSH = frozenset(_opcode.hasexc)  # SETUP_FINALLY/SETUP_CLEANUP/SETUP_WITH
_JUMP_OPS = frozenset(_opcode.hasjrel) | frozenset(_opcode.hasjabs)
HAS_TARGET_OPS = _JUMP_OPS | _BLOCK_PUSH
_SCOPE_EXIT = frozenset((RETURN_VALUE, RAISE_VARARGS, RERAISE))
_UNCOND_JUMP = frozenset(
    (JUMP, JUMP_NO_INTERRUPT, JUMP_FORWARD, JUMP_BACKWARD, JUMP_BACKWARD_NO_INTERRUPT)
)
_TERMINATOR = _JUMP_OPS | _SCOPE_EXIT
# Compiler-visible instructions with HAS_EVAL_BREAK_FLAG (3.14
# pycore_opcode_metadata.h; specialized forms never reach the CFG).
_EVAL_BREAK = frozenset(
    (RESUME, CALL, CALL_KW, CALL_FUNCTION_EX, JUMP_BACKWARD, JUMP)
)
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

    def set_loc(self, loc):
        self.loc = loc


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
    """``_PyCfg_FromInstructionSequence``: rows are (opcode, oparg, loc)
    with jump opargs already resolved to instruction indices. The
    ``ANNOTATIONS_PLACEHOLDER`` pseudo-instruction is dropped here (this
    entry point carries no deferred-annotations code), shifting every
    later label and jump target by one, exactly as the C loop's
    ``offset`` bookkeeping does."""
    is_target = [False] * len(rows)
    for opcode, oparg, _loc in rows:
        if opcode in HAS_TARGET_OPS:
            if not (0 <= oparg < len(rows)):
                raise ValueError("target out of range")
            is_target[oparg] = True
    g = CfgBuilder()
    offset = 0
    for i, (opcode, oparg, loc) in enumerate(rows):
        if opcode == ANNOTATIONS_PLACEHOLDER:
            offset -= 1
            continue
        if is_target[i]:
            g.use_label(i + offset)
        if opcode in HAS_TARGET_OPS:
            oparg += offset
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


# ---- stack effects ---------------------------------------------------------

_POPPED = _opcode_tables._POPPED
_PUSHED = _opcode_tables._PUSHED
_DEOPT = _opcode_tables._DEOPT
_MAX_REAL_OPCODE = _opcode_tables._MAX_REAL_OPCODE


def num_popped(opcode, oparg):
    """``_PyOpcode_num_popped``; ``-1`` for an unknown opcode."""
    fn = _POPPED.get(opcode)
    return -1 if fn is None else fn(oparg)


def num_pushed(opcode, oparg):
    """``_PyOpcode_num_pushed``; ``-1`` for an unknown opcode."""
    fn = _PUSHED.get(opcode)
    return -1 if fn is None else fn(oparg)


def get_stack_effects(opcode, oparg, jump):
    """``get_stack_effects``: the net effect, or ``None`` when the opcode
    has no entry (specialized forms, out-of-range numbers)."""
    if opcode < 0:
        return None
    if opcode <= _MAX_REAL_OPCODE and _DEOPT.get(opcode, opcode) != opcode:
        return None
    popped = num_popped(opcode, oparg)
    pushed = num_pushed(opcode, oparg)
    if popped < 0 or pushed < 0:
        return None
    if opcode in _BLOCK_PUSH and not jump:
        return 0
    return pushed - popped


def opcode_stack_effect(opcode, oparg):
    """``PyCompile_OpcodeStackEffect`` (``jump = -1``, the maximal case);
    ``None`` stands in for ``PY_INVALID_STACK_EFFECT``."""
    return get_stack_effects(opcode, oparg, -1)


_INT_MIN = -(2**31)


def _stackdepth_push(stack, b, depth):
    if not (b.startdepth < 0 or b.startdepth == depth):
        raise ValueError("Invalid CFG, inconsistent stackdepth")
    if b.startdepth < depth and b.startdepth < 100:
        b.startdepth = depth
        stack.append(b)


def calculate_stackdepth(g):
    """``calculate_stackdepth``: find the flow path needing the largest
    stack (cycles are assumed to have no net effect), recording every
    block's entry depth in ``startdepth`` on the way."""
    for b in g.iter_blocks():
        b.startdepth = _INT_MIN
    stack = []
    maxdepth = 0
    _stackdepth_push(stack, g.entry, 0)
    while stack:
        b = stack.pop()
        depth = b.startdepth
        nxt = b.next
        for instr in b.instrs:
            effect = get_stack_effects(instr.opcode, instr.oparg, 0)
            if effect is None:
                raise SystemError(
                    "Invalid stack effect for opcode=%d, arg=%d"
                    % (instr.opcode, instr.oparg)
                )
            new_depth = depth + effect
            if new_depth < 0:
                raise ValueError("Invalid CFG, stack underflow")
            maxdepth = max(maxdepth, depth)
            if instr.opcode in HAS_TARGET_OPS and instr.opcode != END_ASYNC_FOR:
                effect = get_stack_effects(instr.opcode, instr.oparg, 1)
                if effect is None:
                    raise SystemError(
                        "Invalid stack effect for opcode=%d, arg=%d"
                        % (instr.opcode, instr.oparg)
                    )
                target_depth = depth + effect
                maxdepth = max(maxdepth, depth)
                _stackdepth_push(stack, instr.target, target_depth)
            depth = new_depth
            if instr.opcode in _UNCOND_JUMP or instr.opcode in _SCOPE_EXIT:
                nxt = None
                break
        if nxt is not None:
            _stackdepth_push(stack, nxt, depth)
    return maxdepth


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
                    if prev_opcode in (LOAD_CONST, LOAD_SMALL_INT):
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


def loads_const(opcode):
    return opcode in _HAS_CONST or opcode == LOAD_SMALL_INT


def get_const_value(opcode, oparg, consts):
    assert loads_const(opcode)
    if opcode == LOAD_CONST:
        n = len(consts)
        if oparg < 0 or oparg >= n:
            raise ValueError(
                "LOAD_CONST index %d is out of range for consts (len=%d)" % (oparg, n)
            )
        return consts[oparg]
    if opcode == LOAD_SMALL_INT:
        return int(oparg)
    raise SystemError("Internal error: failed to get value of a constant")


class _ConstKey:
    """CPython's const-cache key (``_PyCompile_ConstCacheMergeOne``): value
    plus type (and recursively for containers), so 0 / 0.0 / False stay
    distinct constants."""

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


class ConstsIndex:
    """The 3.14 ``consts_index`` hashtable: object identity -> index into
    ``consts``, seeded from the incoming list (first occurrence wins)."""

    __slots__ = ("by_id", "keep")

    def __init__(self, consts):
        self.by_id = {}
        self.keep = []  # pin ids: the objects live in ``consts`` anyway
        for i, item in enumerate(consts):
            self.by_id.setdefault(id(item), i)

    def get(self, obj):
        return self.by_id.get(id(obj))

    def set(self, obj, index):
        self.by_id[id(obj)] = index
        self.keep.append(obj)


def add_const(newconst, consts, const_cache, consts_index):
    """``add_const``: merge through the const cache, then look the object
    up by identity; append when new. Returns the index."""
    key = _ConstKey(newconst)
    cached = const_cache.get(key)
    if cached is not None:
        newconst = cached
    else:
        const_cache[key] = newconst
    index = consts_index.get(newconst)
    if index is not None:
        return index
    index = len(consts)
    if index >= 2**31 - 2:
        raise OverflowError("too many constants")
    consts.append(newconst)
    consts_index.set(newconst, index)
    return index


def get_const_loading_instrs(bb, start, size):
    """Walk ``bb.instrs`` backwards from ``start`` skipping NOPs and collect
    ``size`` consecutive constant loads (oldest first). Returns the list, or
    ``None`` when the run is not all constants."""
    assert start < len(bb.instrs)
    assert 0 <= size <= STACK_USE_GUIDELINE
    out = [None] * size
    while start >= 0 and size > 0:
        instr = bb.instrs[start]
        start -= 1
        if instr.opcode == NOP:
            continue
        if not loads_const(instr.opcode):
            return None
        size -= 1
        out[size] = instr
    return out if size == 0 else None


def nop_out(instrs):
    """Turn every instruction in ``instrs`` into a location-less NOP."""
    for instr in instrs:
        assert instr.opcode != NOP
        instr.set_op0(NOP)
        instr.set_loc(NO_LOCATION)


def maybe_instr_make_load_smallint(instr, newconst):
    """Rewrite ``instr`` to ``LOAD_SMALL_INT`` when ``newconst`` is an exact
    int in 0..255. Returns True when it did."""
    if type(newconst) is int and 0 <= newconst <= 255:
        instr.set_op1(LOAD_SMALL_INT, newconst)
        return True
    return False


def instr_make_load_const(instr, newconst, consts, const_cache, consts_index):
    if maybe_instr_make_load_smallint(instr, newconst):
        return
    oparg = add_const(newconst, consts, const_cache, consts_index)
    instr.set_op1(LOAD_CONST, oparg)


def fold_tuple_of_constants(bb, i, consts, const_cache, consts_index):
    """``LOAD_CONST c1 .. LOAD_CONST cN BUILD_TUPLE N`` -> ``LOAD_CONST
    (c1, .., cN)``."""
    instr = bb.instrs[i]
    assert instr.opcode == BUILD_TUPLE
    seq_size = instr.oparg
    if seq_size > STACK_USE_GUIDELINE:
        return
    const_instrs = get_const_loading_instrs(bb, i - 1, seq_size)
    if const_instrs is None:
        return
    const_tuple = tuple(
        get_const_value(inst.opcode, inst.oparg, consts) for inst in const_instrs
    )
    nop_out(const_instrs)
    instr_make_load_const(instr, const_tuple, consts, const_cache, consts_index)


def fold_constant_intrinsic_list_to_tuple(bb, i, consts, const_cache, consts_index):
    """``BUILD_LIST 0 (LOAD_CONST c LIST_APPEND 1)* CALL_INTRINSIC_1
    INTRINSIC_LIST_TO_TUPLE`` -> ``LOAD_CONST (c1, .., cN)``."""
    intrinsic = bb.instrs[i]
    assert intrinsic.opcode == CALL_INTRINSIC_1
    assert intrinsic.oparg == INTRINSIC_LIST_TO_TUPLE
    consts_found = 0
    expect_append = True
    pos = i - 1
    while pos >= 0:
        instr = bb.instrs[pos]
        opcode = instr.opcode
        oparg = instr.oparg
        if opcode == NOP:
            pos -= 1
            continue
        if opcode == BUILD_LIST and oparg == 0:
            if not expect_append:
                # Not a sequence start.
                return
            # Sequence start, we are done.
            items = [None] * consts_found
            for newpos in range(i - 1, pos - 1, -1):
                instr = bb.instrs[newpos]
                if instr.opcode == NOP:
                    continue
                if loads_const(instr.opcode):
                    assert consts_found > 0
                    consts_found -= 1
                    items[consts_found] = get_const_value(
                        instr.opcode, instr.oparg, consts
                    )
                nop_out([instr])
            assert consts_found == 0
            instr_make_load_const(
                intrinsic, tuple(items), consts, const_cache, consts_index
            )
            return
        if expect_append:
            if opcode != LIST_APPEND or oparg != 1:
                return
        else:
            if not loads_const(opcode):
                return
            consts_found += 1
        expect_append = not expect_append
        pos -= 1
    # Did not find sequence start.


def optimize_lists_and_sets(bb, i, nextop, consts, const_cache, consts_index):
    """Optimize list and set displays:

    1. ``for`` loops, comprehensions and ``in`` / ``not in`` tests: a
       literal list or set of constants becomes a constant tuple or
       frozenset; a list of non-constants becomes a tuple.
    2. Constant displays of at least ``MIN_CONST_SEQUENCE_SIZE`` items:
       ``LOAD_CONST c1 .. LOAD_CONST cN BUILD_LIST N`` becomes
       ``BUILD_LIST 0, LOAD_CONST (c1, .., cN), LIST_EXTEND 1`` (and
       ``BUILD_SET`` / ``SET_UPDATE`` respectively).
    """
    instr = bb.instrs[i]
    assert instr.opcode in (BUILD_LIST, BUILD_SET)
    contains_or_iter = nextop in (GET_ITER, CONTAINS_OP)
    seq_size = instr.oparg
    if seq_size > STACK_USE_GUIDELINE or (
        seq_size < MIN_CONST_SEQUENCE_SIZE and not contains_or_iter
    ):
        return
    const_instrs = get_const_loading_instrs(bb, i - 1, seq_size)
    if const_instrs is None:
        # Not a const sequence.
        if contains_or_iter and instr.opcode == BUILD_LIST:
            # Iterate over a tuple instead of a list.
            instr.set_op1(BUILD_TUPLE, instr.oparg)
        return
    const_result = tuple(
        get_const_value(inst.opcode, inst.oparg, consts) for inst in const_instrs
    )
    if instr.opcode == BUILD_SET:
        const_result = frozenset(const_result)
    index = add_const(const_result, consts, const_cache, consts_index)
    nop_out(const_instrs)
    if contains_or_iter:
        instr.set_op1(LOAD_CONST, index)
    else:
        assert i >= 2
        bb.instrs[i - 2].set_loc(instr.loc)
        bb.instrs[i - 2].set_op1(instr.opcode, 0)
        bb.instrs[i - 1].set_op1(LOAD_CONST, index)
        bb.instrs[i].set_op1(LIST_EXTEND if instr.opcode == BUILD_LIST else SET_UPDATE, 1)


# Guards against creating constants that are slow to hash or huge.
MAX_INT_SIZE = 128  # bits
MAX_COLLECTION_SIZE = 256  # items
MAX_STR_SIZE = 4096  # characters
MAX_TOTAL_ITEMS = 1024  # including nested collections


def const_folding_check_complexity(obj, limit):
    if isinstance(obj, tuple):
        limit -= len(obj)
        for item in obj:
            if limit < 0:
                break
            limit = const_folding_check_complexity(item, limit)
            if limit < 0:
                return limit
    return limit


class _NoFold(Exception):
    """Stands in for the C helpers returning NULL without an exception set."""


def _is_int(v):
    return isinstance(v, int)


def const_folding_safe_multiply(v, w):
    if _is_int(v) and _is_int(w) and v != 0 and w != 0:
        if abs(v).bit_length() + abs(w).bit_length() > MAX_INT_SIZE:
            raise _NoFold
    elif _is_int(v) and isinstance(w, tuple):
        size = len(w)
        if size:
            n = v
            if n < 0 or n > MAX_COLLECTION_SIZE // size:
                raise _NoFold
            if n and const_folding_check_complexity(w, MAX_TOTAL_ITEMS // n) < 0:
                raise _NoFold
    elif _is_int(v) and isinstance(w, (str, bytes)):
        size = len(w)
        if size:
            n = v
            if n < 0 or n > MAX_STR_SIZE // size:
                raise _NoFold
    elif _is_int(w) and isinstance(v, (tuple, str, bytes)):
        return const_folding_safe_multiply(w, v)
    return v * w


def const_folding_safe_power(v, w):
    if _is_int(v) and _is_int(w) and v != 0 and w > 0:
        vbits = abs(v).bit_length()
        if w >= 2**64:
            raise _NoFold
        if vbits > MAX_INT_SIZE // w:
            raise _NoFold
    return pow(v, w)


def const_folding_safe_lshift(v, w):
    if _is_int(v) and _is_int(w) and v != 0 and w != 0:
        vbits = abs(v).bit_length()
        if w < 0 or w >= 2**64:
            raise _NoFold
        if w > MAX_INT_SIZE or vbits > MAX_INT_SIZE - w:
            raise _NoFold
    return v << w


def const_folding_safe_mod(v, w):
    if isinstance(v, (str, bytes)):
        raise _NoFold
    return v % w


def eval_const_binop(left, op, right):
    """``eval_const_binop``: the folded value, or raise (any exception
    means "don't fold")."""
    assert 0 <= op <= NB_OPARG_LAST
    if op == NB_ADD:
        return left + right
    if op == NB_SUBTRACT:
        return left - right
    if op == NB_MULTIPLY:
        return const_folding_safe_multiply(left, right)
    if op == NB_TRUE_DIVIDE:
        return left / right
    if op == NB_FLOOR_DIVIDE:
        return left // right
    if op == NB_REMAINDER:
        return const_folding_safe_mod(left, right)
    if op == NB_POWER:
        return const_folding_safe_power(left, right)
    if op == NB_LSHIFT:
        return const_folding_safe_lshift(left, right)
    if op == NB_RSHIFT:
        return left >> right
    if op == NB_OR:
        return left | right
    if op == NB_XOR:
        return left ^ right
    if op == NB_AND:
        return left & right
    if op == NB_SUBSCR:
        return left[right]
    # NB_MATRIX_MULTIPLY: no builtin constants implement it; the in-place
    # variants never reach the folder from codegen.
    raise _NoFold


def fold_const_binop(bb, i, consts, const_cache, consts_index):
    binop = bb.instrs[i]
    assert binop.opcode == BINARY_OP
    operands = get_const_loading_instrs(bb, i - 1, 2)
    if operands is None:
        return
    lhs_instr, rhs_instr = operands
    lhs = get_const_value(lhs_instr.opcode, lhs_instr.oparg, consts)
    rhs = get_const_value(rhs_instr.opcode, rhs_instr.oparg, consts)
    try:
        newconst = eval_const_binop(lhs, binop.oparg, rhs)
    except KeyboardInterrupt:
        raise
    except Exception:
        return
    nop_out(operands)
    instr_make_load_const(binop, newconst, consts, const_cache, consts_index)


def eval_const_unaryop(operand, opcode, oparg):
    assert (
        opcode in (UNARY_NEGATIVE, UNARY_INVERT, UNARY_NOT)
        or (opcode == CALL_INTRINSIC_1 and oparg == INTRINSIC_UNARY_POSITIVE)
    )
    if opcode == UNARY_NEGATIVE:
        return -operand
    if opcode == UNARY_INVERT:
        # XXX: This should be removed once the ~bool deprecation expires.
        if isinstance(operand, bool):
            raise _NoFold
        return ~operand
    if opcode == UNARY_NOT:
        return not operand
    return +operand


def fold_const_unaryop(bb, i, consts, const_cache, consts_index):
    unaryop = bb.instrs[i]
    operands = get_const_loading_instrs(bb, i - 1, 1)
    if operands is None:
        return
    (operand_instr,) = operands
    operand = get_const_value(operand_instr.opcode, operand_instr.oparg, consts)
    try:
        newconst = eval_const_unaryop(operand, unaryop.opcode, unaryop.oparg)
    except KeyboardInterrupt:
        raise
    except Exception:
        return
    if unaryop.opcode == UNARY_NOT:
        assert isinstance(newconst, bool)
    nop_out(operands)
    instr_make_load_const(unaryop, newconst, consts, const_cache, consts_index)


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


def basicblock_optimize_load_const(const_cache, bb, consts, consts_index):
    opcode = 0
    oparg = 0
    i = 0
    while i < len(bb.instrs):
        inst = bb.instrs[i]
        if inst.opcode == LOAD_CONST:
            constant = get_const_value(inst.opcode, inst.oparg, consts)
            maybe_instr_make_load_smallint(inst, constant)
        is_copy_of_load_const = (
            opcode == LOAD_CONST and inst.opcode == COPY and inst.oparg == 1
        )
        if not is_copy_of_load_const:
            opcode = inst.opcode
            oparg = inst.oparg
        if opcode != LOAD_CONST and opcode != LOAD_SMALL_INT:
            i += 1
            continue
        nextop = bb.instrs[i + 1].opcode if i + 1 < len(bb.instrs) else 0
        if nextop in (POP_JUMP_IF_FALSE, POP_JUMP_IF_TRUE, JUMP_IF_FALSE, JUMP_IF_TRUE):
            # Remove LOAD_CONST const; conditional jump.
            cnt = get_const_value(opcode, oparg, consts)
            is_true = bool(cnt)
            if opcode_stack_effect(nextop, 0) == -1:
                # POP_JUMP_IF_FALSE or POP_JUMP_IF_TRUE
                inst.set_op0(NOP)
            jump_if_true = nextop in (POP_JUMP_IF_TRUE, JUMP_IF_TRUE)
            if is_true == jump_if_true:
                bb.instrs[i + 1].opcode = JUMP
            else:
                bb.instrs[i + 1].set_op0(NOP)
                bb.instrs[i + 1].target = None
        elif nextop == IS_OP:
            # Fold to POP_JUMP_IF_NONE:
            # - LOAD_CONST(None) IS_OP(0) POP_JUMP_IF_TRUE
            # - LOAD_CONST(None) IS_OP(1) POP_JUMP_IF_FALSE
            # - LOAD_CONST(None) IS_OP(0) TO_BOOL POP_JUMP_IF_TRUE
            # - LOAD_CONST(None) IS_OP(1) TO_BOOL POP_JUMP_IF_FALSE
            # Fold to POP_JUMP_IF_NOT_NONE:
            # - LOAD_CONST(None) IS_OP(0) POP_JUMP_IF_FALSE
            # - LOAD_CONST(None) IS_OP(1) POP_JUMP_IF_TRUE
            # - LOAD_CONST(None) IS_OP(0) TO_BOOL POP_JUMP_IF_FALSE
            # - LOAD_CONST(None) IS_OP(1) TO_BOOL POP_JUMP_IF_TRUE
            cnt = get_const_value(opcode, oparg, consts)
            if cnt is not None:
                i += 1
                continue
            if len(bb.instrs) <= i + 2:
                i += 1
                continue
            is_instr = bb.instrs[i + 1]
            jump_instr = bb.instrs[i + 2]
            # Get rid of TO_BOOL regardless:
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
        elif nextop == TO_BOOL:
            cnt = get_const_value(opcode, oparg, consts)
            is_true = bool(cnt)
            index = add_const(is_true, consts, const_cache, consts_index)
            inst.set_op0(NOP)
            bb.instrs[i + 1].set_op1(LOAD_CONST, index)
        i += 1


def optimize_load_const(const_cache, g, consts, consts_index):
    for b in g.iter_blocks():
        basicblock_optimize_load_const(const_cache, b, consts, consts_index)


def jump_thread(bb, inst, target, opcode):
    """NOP out inst and append a jump to target.target."""
    assert is_jump(inst)
    assert is_jump(target)
    assert inst is bb.last_instr()
    # bpo-45773: if inst.target is target.target nothing changes (and we
    # would loop forever).
    if inst.target is not target.target:
        new_target = target.target
        new_loc = target.loc
        inst.set_op0(NOP)
        inst.target = None
        jump = Instr(opcode, new_target.label, new_loc, new_target)
        bb.instrs.append(jump)
        return True
    return False


def optimize_basic_block(const_cache, bb, consts, consts_index):
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
            # Try to fold tuples of constants.
            # Skip over BUILD_TUPLE(1) UNPACK_SEQUENCE(1).
            # Replace BUILD_TUPLE(2) UNPACK_SEQUENCE(2) with SWAP(2).
            # Replace BUILD_TUPLE(3) UNPACK_SEQUENCE(3) with SWAP(3).
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
            fold_tuple_of_constants(bb, i, consts, const_cache, consts_index)
        elif opcode in (BUILD_LIST, BUILD_SET):
            optimize_lists_and_sets(bb, i, nextop, consts, const_cache, consts_index)
        elif opcode in (POP_JUMP_IF_NOT_NONE, POP_JUMP_IF_NONE):
            if target.opcode == JUMP:
                i -= jump_thread(bb, inst, target, opcode)
        elif opcode == POP_JUMP_IF_FALSE:
            if target.opcode == JUMP:
                i -= jump_thread(bb, inst, target, POP_JUMP_IF_FALSE)
        elif opcode == POP_JUMP_IF_TRUE:
            if target.opcode == JUMP:
                i -= jump_thread(bb, inst, target, POP_JUMP_IF_TRUE)
        elif opcode == JUMP_IF_FALSE:
            if target.opcode in (JUMP, JUMP_IF_FALSE):
                i -= jump_thread(bb, inst, target, JUMP_IF_FALSE)
                i += 1
                continue
            if target.opcode == JUMP_IF_TRUE:
                # No need to check for loops here, a block's next cannot
                # point to itself.
                assert inst.target is not inst.target.next
                inst.target = inst.target.next
                continue
        elif opcode == JUMP_IF_TRUE:
            if target.opcode in (JUMP, JUMP_IF_TRUE):
                i -= jump_thread(bb, inst, target, JUMP_IF_TRUE)
                i += 1
                continue
            if target.opcode == JUMP_IF_FALSE:
                assert inst.target is not inst.target.next
                inst.target = inst.target.next
                continue
        elif opcode in (JUMP, JUMP_NO_INTERRUPT):
            if target.opcode == JUMP:
                i -= jump_thread(bb, inst, target, JUMP)
                i += 1
                continue
            if target.opcode == JUMP_NO_INTERRUPT:
                i -= jump_thread(bb, inst, target, opcode)
                i += 1
                continue
        elif opcode == FOR_ITER:
            # Threading FOR_ITER through a JUMP is disabled upstream: the
            # jump could be backward and FOR_ITER only jumps forward.
            pass
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
            if nextop == UNARY_NOT:
                inst.set_op0(NOP)
                inverted = oparg ^ 1
                assert inverted in (0, 1)
                bb.instrs[i + 1].set_op1(opcode, inverted)
                i += 1
                continue
        elif opcode == TO_BOOL:
            if nextop == TO_BOOL:
                inst.set_op0(NOP)
                i += 1
                continue
        elif opcode in (UNARY_NOT, UNARY_INVERT, UNARY_NEGATIVE):
            if opcode == UNARY_NOT:
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
            fold_const_unaryop(bb, i, consts, const_cache, consts_index)
        elif opcode == CALL_INTRINSIC_1:
            if oparg == INTRINSIC_LIST_TO_TUPLE:
                if nextop == GET_ITER:
                    inst.set_op0(NOP)
                else:
                    fold_constant_intrinsic_list_to_tuple(
                        bb, i, consts, const_cache, consts_index
                    )
            elif oparg == INTRINSIC_UNARY_POSITIVE:
                fold_const_unaryop(bb, i, consts, const_cache, consts_index)
        elif opcode == BINARY_OP:
            fold_const_binop(bb, i, consts, const_cache, consts_index)
        i += 1

    i = 0
    while i < len(bb.instrs):
        if bb.instrs[i].opcode == SWAP:
            i = swaptimize(bb, i)
            apply_static_swaps(bb, i)
        i += 1


def optimize_cfg(g, consts, const_cache, consts_index, firstlineno):
    check_cfg(g)
    inline_small_or_no_lineno_blocks(g)
    remove_unreachable(g)
    resolve_line_numbers(g, firstlineno)
    optimize_load_const(const_cache, g, consts, consts_index)
    for b in g.iter_blocks():
        optimize_basic_block(const_cache, b, consts, consts_index)
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


# ---- LOAD_FAST -> LOAD_FAST_BORROW -------------------------------------------

NOT_LOCAL = -1
DUMMY_INSTR = -1

# LoadFastInstrFlag
SUPPORT_KILLED = 1  # the loaded reference is still on the stack when the local is killed
STORED_AS_LOCAL = 2  # the loaded reference is stored into a local
REF_UNCONSUMED = 4  # the loaded reference is still on the stack at the end of the block


def _kill_local(instr_flags, refs, local):
    for r_instr, r_local in refs:
        if r_local == local:
            assert r_instr >= 0
            instr_flags[r_instr] |= SUPPORT_KILLED


def _store_local(instr_flags, refs, local, r):
    _kill_local(instr_flags, refs, local)
    if r[0] != DUMMY_INSTR:
        instr_flags[r[0]] |= STORED_AS_LOCAL


def _load_fast_push_block(stack, target, start_depth):
    # The C asserts `target->b_startdepth == start_depth`; release builds
    # (and this port) just follow the graph.
    if not target.visited:
        target.visited = 1
        stack.append(target)


# Opcodes that consume no inputs.
_LF_CONSUME_NONE = frozenset(
    (
        FORMAT_SIMPLE,
        GET_ANEXT,
        GET_LEN,
        GET_YIELD_FROM_ITER,
        IMPORT_FROM,
        MATCH_KEYS,
        MATCH_MAPPING,
        MATCH_SEQUENCE,
        WITH_EXCEPT_START,
    )
)
# Opcodes that consume some inputs and push no new values.
_LF_PUSH_NONE = frozenset(
    (DICT_MERGE, DICT_UPDATE, LIST_APPEND, LIST_EXTEND, MAP_ADD, RERAISE, SET_ADD, SET_UPDATE)
)


def optimize_load_fast(g):
    """Strength-reduce ``LOAD_FAST{_LOAD_FAST}`` into the ``_BORROW``
    variants where the frame's reference provably outlives the borrowed
    one: abstract interpretation over a stack of ``(producing instr,
    local)`` refs, per basic block (see the 3.14 ``flowgraph.c`` comment
    for the lifetime argument)."""
    for b in g.iter_blocks():
        b.visited = 0
    entry = g.entry
    stack = [entry]
    entry.startdepth = 0
    entry.visited = 1
    while stack:
        block = stack.pop()
        assert block.startdepth > -1
        n = len(block.instrs)
        instr_flags = [0] * n
        # We don't track references on the stack across basic blocks, but
        # the bytecode will expect their presence: add dummies.
        refs = [(DUMMY_INSTR, NOT_LOCAL)] * block.startdepth
        for i in range(n):
            instr = block.instrs[i]
            opcode = instr.opcode
            oparg = instr.oparg
            # Opcodes that load and store locals.
            if opcode == DELETE_FAST:
                _kill_local(instr_flags, refs, oparg)
            elif opcode == LOAD_FAST:
                refs.append((i, oparg))
            elif opcode == LOAD_FAST_AND_CLEAR:
                _kill_local(instr_flags, refs, oparg)
                refs.append((i, oparg))
            elif opcode == LOAD_FAST_LOAD_FAST:
                refs.append((i, oparg >> 4))
                refs.append((i, oparg & 15))
            elif opcode == STORE_FAST:
                r = refs.pop()
                _store_local(instr_flags, refs, oparg, r)
            elif opcode == STORE_FAST_LOAD_FAST:
                r = refs.pop()
                _store_local(instr_flags, refs, oparg >> 4, r)
                refs.append((i, oparg & 15))
            elif opcode == STORE_FAST_STORE_FAST:
                r = refs.pop()
                _store_local(instr_flags, refs, oparg >> 4, r)
                r = refs.pop()
                _store_local(instr_flags, refs, oparg & 15, r)
            # Opcodes that shuffle values on the stack.
            elif opcode == COPY:
                assert oparg > 0
                refs.append(refs[len(refs) - oparg])
            elif opcode == SWAP:
                assert oparg >= 2
                idx = len(refs) - oparg
                refs[idx], refs[-1] = refs[-1], refs[idx]
            # Opcodes that do not consume all of their inputs are handled
            # case by case; there is no generic way to know how many inputs
            # stay on the stack.
            elif opcode in _LF_CONSUME_NONE:
                net_pushed = num_pushed(opcode, oparg) - num_popped(opcode, oparg)
                assert net_pushed >= 0
                # Upstream shadows the instruction index with the loop
                # counter here (`for (int i = 0; ...) PUSH_REF(i, ...)`);
                # mirror that so the same instructions get flagged.
                for k in range(net_pushed):
                    refs.append((k, NOT_LOCAL))
            elif opcode in _LF_PUSH_NONE:
                net_popped = num_popped(opcode, oparg) - num_pushed(opcode, oparg)
                assert net_popped > 0
                del refs[len(refs) - net_popped:]
            elif opcode in (END_SEND, SET_FUNCTION_ATTRIBUTE):
                tos = refs.pop()
                refs.pop()
                refs.append(tos)
            # Opcodes that consume some inputs and push new values.
            elif opcode == CHECK_EXC_MATCH:
                refs.pop()
                refs.append((i, NOT_LOCAL))
            elif opcode == FOR_ITER:
                _load_fast_push_block(stack, instr.target, len(refs) + 1)
                refs.append((i, NOT_LOCAL))
            elif opcode in (LOAD_ATTR, LOAD_SUPER_ATTR):
                self_ref = refs.pop()
                if opcode == LOAD_SUPER_ATTR:
                    refs.pop()
                    refs.pop()
                refs.append((i, NOT_LOCAL))
                if oparg & 1:
                    # A method call; conservatively assume that self is
                    # pushed back onto the stack.
                    refs.append(self_ref)
            elif opcode in (LOAD_SPECIAL, PUSH_EXC_INFO):
                tos = refs.pop()
                refs.append((i, NOT_LOCAL))
                refs.append(tos)
            elif opcode == SEND:
                _load_fast_push_block(stack, instr.target, len(refs))
                refs.pop()
                refs.append((i, NOT_LOCAL))
            # Opcodes that consume all of their inputs.
            else:
                popped = num_popped(opcode, oparg)
                pushed = num_pushed(opcode, oparg)
                if opcode in HAS_TARGET_OPS:
                    _load_fast_push_block(
                        stack, instr.target, len(refs) - popped + pushed
                    )
                if opcode not in _BLOCK_PUSH:
                    # Block push opcodes only affect the stack when jumping
                    # to the target.
                    for _k in range(popped):
                        refs.pop()
                    for _k in range(pushed):
                        refs.append((i, NOT_LOCAL))

        # Push the fallthrough block.
        term = block.last_instr()
        if (
            term is not None
            and block.next is not None
            and not (term.opcode in _UNCOND_JUMP or term.opcode in _SCOPE_EXIT)
        ):
            assert block.has_fallthrough()
            _load_fast_push_block(stack, block.next, len(refs))

        # Mark instructions whose values are still on the stack at the end
        # of the block.
        for r_instr, _r_local in refs:
            if r_instr != -1:
                instr_flags[r_instr] |= REF_UNCONSUMED

        # Optimize instructions.
        for i in range(n):
            if not instr_flags[i]:
                instr = block.instrs[i]
                if instr.opcode == LOAD_FAST:
                    instr.opcode = LOAD_FAST_BORROW
                elif instr.opcode == LOAD_FAST_LOAD_FAST:
                    instr.opcode = LOAD_FAST_BORROW_LOAD_FAST_BORROW


def optimize_code_unit(g, consts, const_cache, nlocals, nparams, firstlineno):
    """``_PyCfg_OptimizeCodeUnit``: the full pass pipeline."""
    translate_jump_labels_to_targets(g)
    mark_except_handlers(g)
    label_exception_targets(g)
    consts_index = ConstsIndex(consts)
    optimize_cfg(g, consts, const_cache, consts_index, firstlineno)
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
    calculate_stackdepth(g)
    optimize_load_fast(g)
    return cfg_to_rows(g)
