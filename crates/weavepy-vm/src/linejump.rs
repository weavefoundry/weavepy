//! RFC 0050 — real `frame.f_lineno` assignment ("set next statement").
//!
//! CPython lets a *trace function* move a live frame to a different line
//! by assigning `frame.f_lineno`, subject to stack-safety rules
//! (`Objects/frameobject.c:frame_setlineno`). The legality analysis
//! models the evaluation stack at every instruction offset as a stack of
//! abstract *kinds* (`Iterator` / `Except` / `Object` / `Null`), packed
//! 3 bits per entry into an `i64` exactly like CPython's `mark_stacks`.
//! A jump is legal when the target's kind-stack can be reached from the
//! current one by popping values; applying it pops the frame's value
//! stack down to the target depth and (WeavePy-specific) pops the
//! separate `exc_handlers`/`exc_info` bookkeeping for any `except`
//! blocks the jump leaves.
//!
//! WeavePy divergence from CPython's model: our `PUSH_EXC_INFO` /
//! `POP_EXCEPT` track the handled exception on a side stack rather than
//! as a value-stack slot, so the analysis carries a parallel per-offset
//! *exception depth* and the jump plan records how many side-stack
//! entries to pop. The handler-entry exception *instance* (pushed by
//! `handle_exception`) is still a value-stack slot and is marked
//! `Except`, which is what makes "can't jump into an 'except' block"
//! fall out the same way it does in CPython.

use weavepy_compiler::bytecode::{Instruction, OpCode};
use weavepy_compiler::{CodeObject, Constant};

use crate::error::{value_error, RuntimeError};

/// Abstract stack-slot kinds, numerically identical to CPython's so the
/// packing/compatibility logic can be ported verbatim.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
enum Kind {
    Iterator = 1,
    Except = 2,
    Object = 3,
    Null = 4,
}

const BITS_PER_BLOCK: i64 = 3;
const MASK: i64 = (1 << BITS_PER_BLOCK) - 1;
const UNINITIALIZED: i64 = -2;
const OVERFLOWED: i64 = -1;
const EMPTY_STACK: i64 = 0;
const MAX_STACK_ENTRIES: i64 = 63 / BITS_PER_BLOCK;
const WILL_OVERFLOW: u64 = 1 << ((MAX_STACK_ENTRIES - 1) * BITS_PER_BLOCK);

fn push_kind(stack: i64, kind: Kind) -> i64 {
    if stack < 0 || (stack as u64) >= WILL_OVERFLOW {
        OVERFLOWED
    } else {
        (stack << BITS_PER_BLOCK) | kind as i64
    }
}

fn pop_kind(stack: i64) -> i64 {
    stack >> BITS_PER_BLOCK
}

fn top_kind(stack: i64) -> i64 {
    stack & MASK
}

fn peek_kind(stack: i64, n: u32) -> i64 {
    (stack >> (BITS_PER_BLOCK * i64::from(n - 1))) & MASK
}

fn stack_depth(mut stack: i64) -> u32 {
    let mut d = 0;
    while stack > 0 {
        stack = pop_kind(stack);
        d += 1;
    }
    d
}

fn swap_kinds(stack: i64, n: u32) -> i64 {
    if n < 2 {
        return stack;
    }
    let to_swap = peek_kind(stack, n);
    let top = top_kind(stack);
    let shift = BITS_PER_BLOCK * i64::from(n - 1);
    let replaced_low = (stack & !(MASK << shift)) | (top << shift);
    (replaced_low & !MASK) | to_swap
}

fn pop_to_level(mut stack: i64, level: u32) -> i64 {
    if level == 0 {
        return EMPTY_STACK;
    }
    let max_item: i64 = MASK;
    let level_max_stack = max_item << ((i64::from(level) - 1) * BITS_PER_BLOCK);
    while stack > level_max_stack {
        stack = pop_kind(stack);
    }
    stack
}

fn compatible_kind(from: i64, to: i64) -> bool {
    if to == 0 {
        return false;
    }
    if to == Kind::Object as i64 {
        return from != Kind::Null as i64;
    }
    if to == Kind::Null as i64 {
        return true;
    }
    from == to
}

fn compatible_stack(mut from_stack: i64, to_stack: i64) -> bool {
    if from_stack < 0 || to_stack < 0 {
        return false;
    }
    let mut to = to_stack;
    while from_stack > to {
        from_stack = pop_kind(from_stack);
    }
    while from_stack != 0 {
        if !compatible_kind(top_kind(from_stack), top_kind(to)) {
            return false;
        }
        from_stack = pop_kind(from_stack);
        to = pop_kind(to);
    }
    to == 0
}

fn explain_incompatible_stack(to_stack: i64) -> &'static str {
    if to_stack == OVERFLOWED {
        return "stack is too deep to analyze";
    }
    if to_stack == UNINITIALIZED {
        return "can't jump into an exception handler, or code may be unreachable";
    }
    match top_kind(to_stack) {
        k if k == Kind::Except as i64 => {
            "can't jump into an 'except' block as there's no exception"
        }
        k if k == Kind::Iterator as i64 => "can't jump into the body of a for loop",
        _ => "incompatible stacks",
    }
}

/// The trace event a frame is currently dispatching (mirrors the subset
/// of `sys.monitoring` events CPython consults in `frame_setlineno`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TraceEvent {
    #[default]
    None,
    Call,
    Line,
    Return,
    Exception,
    Opcode,
    Yield,
}

/// A validated pending jump, stored on the `PyFrame` by the `f_lineno`
/// setter and applied to the live `Frame` by the dispatch loop.
#[derive(Clone, Copy, Debug)]
pub struct PendingJump {
    /// Instruction index to resume at.
    pub target_pc: u32,
    /// Value-stack depth at the target (pop down to this).
    pub target_depth: u32,
    /// How many `exc_handlers`/`exc_info` entries the jump leaves.
    pub exc_pops: u32,
}

/// Per-offset analysis result.
struct Marked {
    stacks: Vec<i64>,
    exc_depths: Vec<i32>,
}

/// Must-be-bound analysis for the synthetic `.with_exit`/`.aexit` fast
/// locals. CPython keeps a `with` block's bound `__exit__` on the value
/// stack, so its `mark_stacks` alone rejects jumps into a `with` body
/// (deeper target stack → "incompatible stacks"). WeavePy stashes the
/// exit in a synthetic local instead, which the kind-stack analysis
/// can't see — this parallel forward dataflow (bit per exit slot,
/// merge = intersection) recovers the same legality judgement: a jump
/// whose target requires an exit slot the source hasn't bound is the
/// CPython "incompatible stacks" case.
fn mark_bound_exits(code: &CodeObject) -> Option<Vec<u64>> {
    use OpCode as O;
    let mut slot_bits = vec![0u64; code.varnames.len()];
    let mut any = false;
    let mut next_bit = 0u32;
    for (i, name) in code.varnames.iter().enumerate() {
        if name.starts_with(".with_exit") || name.starts_with(".aexit") {
            if next_bit >= 64 {
                return None; // absurd nesting — skip the refinement
            }
            slot_bits[i] = 1 << next_bit;
            next_bit += 1;
            any = true;
        }
    }
    if !any {
        return None;
    }
    let n = code.instructions.len();
    const TOP: u64 = u64::MAX; // unreached
    let mut bound = vec![TOP; n + 1];
    bound[0] = 0;
    let mut todo = true;
    while todo {
        todo = false;
        let meet = |bound: &mut Vec<u64>, idx: usize, v: u64| {
            if idx < bound.len() {
                let merged = bound[idx] & v;
                if merged != bound[idx] {
                    bound[idx] = merged;
                    return true;
                }
            }
            false
        };
        for i in 0..n {
            let cur = bound[i];
            if cur == TOP {
                continue;
            }
            let ins = code.instructions[i];
            let arg = ins.arg as usize;
            let next_i = i + 1;
            match ins.op {
                O::StoreFast => {
                    let v = cur | slot_bits.get(arg).copied().unwrap_or(0);
                    todo |= meet(&mut bound, next_i, v);
                }
                O::DeleteFast => {
                    let v = cur & !slot_bits.get(arg).copied().unwrap_or(0);
                    todo |= meet(&mut bound, next_i, v);
                }
                O::PopJumpIfFalse | O::PopJumpIfTrue | O::ForIter | O::Send => {
                    todo |= meet(&mut bound, next_i + arg, cur);
                    todo |= meet(&mut bound, next_i, cur);
                }
                O::JumpForward => {
                    todo |= meet(&mut bound, next_i + arg, cur);
                }
                O::JumpBackward => {
                    todo |= meet(&mut bound, next_i.saturating_sub(arg), cur);
                }
                O::ReturnValue | O::RaiseVarargs | O::Reraise => {}
                _ => {
                    todo |= meet(&mut bound, next_i, cur);
                }
            }
        }
        for h in &code.exception_table {
            let handler = h.handler as usize;
            for i in (h.start as usize)..(h.end as usize).min(n) {
                let cur = bound[i];
                if cur == TOP {
                    continue;
                }
                if handler < bound.len() {
                    let merged = bound[handler] & cur;
                    if merged != bound[handler] {
                        bound[handler] = merged;
                        todo = true;
                    }
                }
            }
        }
    }
    Some(bound)
}

/// First offsets of each line: `linestarts[i] = line` when instruction
/// `i` starts a new line (mirrors CPython's `marklines`), else -1.
fn marklines(code: &CodeObject) -> Vec<i64> {
    let n = code.instructions.len();
    let mut out = vec![-1i64; n];
    let mut last_line: i64 = -1;
    #[allow(clippy::needless_range_loop)] // `i` also indexes the neighbours
    for i in 0..n {
        let line = i64::from(code.linetable.get(i).copied().unwrap_or(0));
        // The duplicated per-path implicit-return stubs
        // (`Compiler::finish`) sit after the primary epilogue, each
        // stamped with its jump site's line. CPython's equivalent
        // RETURN_CONST copies are *inline* in their predecessor blocks —
        // contiguous with the line run they end — so they never mark a
        // line start. Left in, a stub becomes a bogus jump candidate:
        // jumping "into" a bare `except` body would land on its `return
        // None` and silently end the function instead of raising
        // (test_no_jump_into_bare_except_block).
        let is_return_stub = i > 0
            && matches!(code.instructions[i].op, OpCode::LoadConst)
            && matches!(
                code.instructions.get(i + 1).map(|x| x.op),
                Some(OpCode::ReturnValue)
            )
            && matches!(
                code.instructions[i - 1].op,
                OpCode::ReturnValue | OpCode::Reraise
            );
        if line != 0 {
            if line != last_line && !is_return_stub {
                out[i] = line;
            }
            last_line = line;
        }
    }
    out
}

/// The size of the keyword-names tuple feeding a `CallKw` at `i`, when
/// statically determinable (the compiler always emits `LoadConst names`
/// immediately before `CallKw`).
fn callkw_names_len(code: &CodeObject, i: usize) -> Option<usize> {
    if i == 0 {
        return None;
    }
    let prev = code.instructions.get(i - 1)?;
    if prev.op != OpCode::LoadConst {
        return None;
    }
    match code.constants.get(prev.arg as usize)? {
        Constant::Tuple(items) => Some(items.len()),
        _ => None,
    }
}

/// `(pops, pushes)` for the plain-effect opcodes (everything without
/// bespoke kind or control-flow handling in `mark_stacks`). `None`
/// marks the offset unanalyzable.
fn plain_effect(code: &CodeObject, i: usize, ins: Instruction) -> Option<(u32, u32)> {
    use OpCode as O;
    let arg = ins.arg;
    Some(match ins.op {
        O::Nop | O::Resume | O::MakeCell | O::SetupAnnotations | O::EndFor => (0, 0),
        // WeavePy's generator bootstrap suspends and *resumes at the
        // next instruction* with an unchanged stack (the generator
        // object lands on the caller's stack, not this frame's) —
        // unlike CPython, where RETURN_GENERATOR pushes the generator
        // and is followed by POP_TOP.
        O::ReturnGenerator => (0, 0),
        O::DeleteFast | O::DeleteGlobal | O::DeleteName | O::DeleteDeref => (0, 0),
        O::LoadConst
        | O::LoadName
        | O::LoadGlobal
        | O::LoadFast
        | O::LoadDeref
        | O::LoadClosure
        | O::LoadClassderef
        | O::LoadBuildClass
        | O::LoadAssertionError => (0, 1),
        O::StoreFast | O::StoreGlobal | O::StoreName | O::StoreDeref => (1, 0),
        O::LoadClassdictOrDeref | O::LoadClassdictOrGlobal => (1, 1),
        O::LoadAttr | O::UnaryOp => (1, 1),
        O::StoreAttr => (2, 0),
        O::DeleteAttr => (1, 0),
        O::BinarySubscr | O::BinaryOp | O::CompareOp | O::IsOp | O::ContainsOp => (2, 1),
        O::StoreSubscr => (3, 0),
        O::DeleteSubscr => (2, 0),
        O::PopTop | O::PrintExpr | O::ImportStar | O::DictUpdate => (1, 0),
        O::Call => (arg + 1, 1),
        O::CallKw => {
            let kwc = u32::try_from(callkw_names_len(code, i)?).ok()?;
            (arg + kwc + 2, 1)
        }
        O::CallEx => (2 + arg, 1),
        O::BuildList | O::BuildTuple | O::BuildSet | O::BuildString => (arg, 1),
        O::BuildMap => (2 * arg, 1),
        O::ListAppend | O::SetAdd => (1, 0),
        O::MapAdd => (2, 0),
        O::UnpackSequence => (1, arg),
        O::UnpackEx => (1, (arg >> 8) + 1 + (arg & 0xFF)),
        O::MakeFunction => (1 + (arg & 0xF).count_ones(), 1),
        O::BuildSlice => (3, 1),
        // Consumes both the `CopyTop`ed exc and the type, pushes the
        // match bool (see the VM opcode; CPython's version peeks the exc
        // instead, so its effect differs).
        O::CheckExcMatch => (2, 1),
        O::CheckEGMatch => (2, 2),
        O::PrepReraiseStar => (2, 1),
        O::ImportName => (2, 1),
        O::ImportFrom => (0, 1),
        O::FormatValue => (if arg & 0x4 != 0 { 2 } else { 1 }, 1),
        O::YieldValue => (1, 1),
        O::EndSend => (2, 1),
        O::GetAnext => (0, 1),
        O::EndAsyncFor => (2, 0),
        O::BeforeWith | O::BeforeAsyncWith => (1, 2),
        O::MatchSequence | O::MatchMapping | O::GetLen => (0, 1),
        O::MatchClass => (3, 1),
        O::MatchKeys => (1, 1),
        // Handled specially in `mark_stacks`; unreachable here.
        O::PopJumpIfFalse
        | O::PopJumpIfTrue
        | O::JumpForward
        | O::JumpBackward
        | O::GetIter
        | O::GetAiter
        | O::GetYieldFromIter
        | O::GetAwaitable
        | O::ForIter
        | O::Send
        | O::CopyTop
        | O::Swap
        | O::PushExcInfo
        | O::PopExcept
        | O::WithExceptStart
        | O::ReturnValue
        | O::RaiseVarargs
        | O::Reraise => return None,
    })
}

/// Merge a successor state, returning whether anything changed (drives
/// the fixpoint worklist).
fn merge(stacks: &mut [i64], exc_depths: &mut [i32], idx: usize, stack: i64, exc: i32) -> bool {
    let mut changed = false;
    if idx < stacks.len() {
        if stacks[idx] == UNINITIALIZED {
            stacks[idx] = stack;
            changed = true;
        } else if stacks[idx] != stack && stacks[idx] != OVERFLOWED {
            // Conflicting flows (shouldn't happen with compiler-emitted
            // code) — poison rather than mis-analyze.
            stacks[idx] = OVERFLOWED;
            changed = true;
        }
        if exc_depths[idx] == -1 {
            exc_depths[idx] = exc;
            changed = true;
        }
    }
    changed
}

/// Compute the abstract kind-stack and exception-handler depth at every
/// instruction offset (CPython `mark_stacks`, adapted to WeavePy's
/// instruction set and split exception bookkeeping).
fn mark_stacks(code: &CodeObject) -> Marked {
    use OpCode as O;
    let n = code.instructions.len();
    let mut stacks = vec![UNINITIALIZED; n + 1];
    let mut exc_depths = vec![-1i32; n + 1];
    stacks[0] = EMPTY_STACK;
    exc_depths[0] = 0;
    let mut todo = true;
    while todo {
        todo = false;
        for i in 0..n {
            let cur = stacks[i];
            if cur == UNINITIALIZED {
                continue;
            }
            let exc = exc_depths[i].max(0);
            let ins = code.instructions[i];
            let next_i = i + 1;
            let arg = ins.arg;
            match ins.op {
                O::PopJumpIfFalse | O::PopJumpIfTrue => {
                    let after = pop_kind(cur);
                    let target = next_i + arg as usize;
                    todo |= merge(&mut stacks, &mut exc_depths, target, after, exc);
                    todo |= merge(&mut stacks, &mut exc_depths, next_i, after, exc);
                }
                O::JumpForward => {
                    let target = next_i + arg as usize;
                    todo |= merge(&mut stacks, &mut exc_depths, target, cur, exc);
                }
                O::JumpBackward => {
                    let target = next_i.saturating_sub(arg as usize);
                    todo |= merge(&mut stacks, &mut exc_depths, target, cur, exc);
                }
                O::GetIter | O::GetAiter | O::GetYieldFromIter | O::GetAwaitable => {
                    let after = push_kind(pop_kind(cur), Kind::Iterator);
                    todo |= merge(&mut stacks, &mut exc_depths, next_i, after, exc);
                }
                O::ForIter => {
                    // Fallthrough: loop body sees [.., iter, value].
                    let body = push_kind(cur, Kind::Object);
                    todo |= merge(&mut stacks, &mut exc_depths, next_i, body, exc);
                    // Exhaustion: WeavePy pops the iterator *before*
                    // jumping (END_FOR is a no-op).
                    let target = next_i + arg as usize;
                    todo |= merge(&mut stacks, &mut exc_depths, target, pop_kind(cur), exc);
                }
                O::Send => {
                    // [.., iter, v] on both paths ([.., iter, yielded]
                    // fallthrough / [.., iter, retval] jump).
                    let target = next_i + arg as usize;
                    todo |= merge(&mut stacks, &mut exc_depths, target, cur, exc);
                    todo |= merge(&mut stacks, &mut exc_depths, next_i, cur, exc);
                }
                O::CopyTop => {
                    let after = push_kind(cur, kind_of(top_kind(cur)));
                    todo |= merge(&mut stacks, &mut exc_depths, next_i, after, exc);
                }
                O::Swap => {
                    let after = swap_kinds(cur, arg.max(2));
                    todo |= merge(&mut stacks, &mut exc_depths, next_i, after, exc);
                }
                O::PushExcInfo => {
                    todo |= merge(&mut stacks, &mut exc_depths, next_i, cur, exc + 1);
                }
                O::PopExcept => {
                    todo |= merge(&mut stacks, &mut exc_depths, next_i, cur, (exc - 1).max(0));
                }
                O::WithExceptStart => {
                    // Push-only, like the VM opcode: reads `exc` (TOS) and
                    // `__exit__` (TOS1) in place and pushes the call
                    // result: [.., __exit__, exc] -> [.., __exit__, exc,
                    // result]. Modelling it as pop-2/push-2 dropped the
                    // `__exit__` slot and desynced every offset after a
                    // `with` cleanup (test_jump_out_of_with_block_within_
                    // for_block saw "stack to deep to analyze").
                    let after = push_kind(cur, Kind::Object);
                    todo |= merge(&mut stacks, &mut exc_depths, next_i, after, exc);
                }
                O::ReturnValue | O::RaiseVarargs | O::Reraise => {
                    // Block enders: no fallthrough.
                }
                _ => {
                    if let Some((pops, pushes)) = plain_effect(code, i, ins) {
                        let mut after = cur;
                        for _ in 0..pops {
                            after = pop_kind(after);
                        }
                        for _ in 0..pushes {
                            after = push_kind(after, Kind::Object);
                        }
                        todo |= merge(&mut stacks, &mut exc_depths, next_i, after, exc);
                    } else {
                        todo |= merge(&mut stacks, &mut exc_depths, next_i, OVERFLOWED, exc);
                    }
                }
            }
        }
        // Exception-table edges: the handler entry sees the protected
        // range's stack popped to the recorded depth plus the pushed
        // exception instance.
        for h in &code.exception_table {
            let start = h.start as usize;
            let handler = h.handler as usize;
            if start < stacks.len() && stacks[start] != UNINITIALIZED {
                if handler < stacks.len() && stacks[handler] == UNINITIALIZED {
                    let target = push_kind(pop_to_level(stacks[start], h.depth), Kind::Except);
                    stacks[handler] = target;
                    exc_depths[handler] = exc_depths[start].max(0);
                    todo = true;
                }
            }
        }
    }
    Marked { stacks, exc_depths }
}

fn kind_of(raw: i64) -> Kind {
    match raw {
        k if k == Kind::Iterator as i64 => Kind::Iterator,
        k if k == Kind::Except as i64 => Kind::Except,
        k if k == Kind::Null as i64 => Kind::Null,
        _ => Kind::Object,
    }
}

/// Which instructions carry a `'line'` trace event — a port of
/// CPython 3.13's `initialize_lines` (instrumentation.c), which decides
/// where `INSTRUMENTED_LINE` is placed when `sys.settrace` /
/// `sys.monitoring` LINE events are enabled:
///
///   1. every instruction that *starts a line* (its line differs from
///      the previous instruction's, walking the stream in order), except
///      `RESUME` / `END_FOR` / `END_SEND` / `END_ASYNC_FOR`;
///   2. every jump target with a real line (a conditional branch or loop
///      edge can land mid-line — think `if x: a; b` — and the landing
///      must still be traceable);
///   3. every exception-table handler entry with a real line.
///
/// The dispatch loop then fires a `'line'` event when execution reaches
/// a marked instruction *and* the previously executed instruction was on
/// a different line (see `_Py_call_instrumentation_line`).
pub(crate) fn line_event_starts(code: &CodeObject) -> Vec<bool> {
    use OpCode as O;
    let n = code.instructions.len();
    let mut out = vec![false; n];
    let line_at = |i: usize| code.linetable.get(i).copied().unwrap_or(0);
    // Pass 1: line starts. `current_line` tracks the previous
    // instruction's line; 0 ("no line", e.g. a handler-entry
    // PUSH_EXC_INFO) resets it so the next real-line instruction starts
    // a run even if it repeats the pre-gap line — exactly CPython's
    // NO_LINE behaviour.
    let mut current_line: u32 = u32::MAX;
    for (i, slot) in out.iter_mut().enumerate() {
        match code.instructions[i].op {
            // Never carry line events: RESUME is needed for
            // instrumentation bookkeeping, END_FOR/END_SEND are skipped
            // over by FOR_ITER/SEND, END_ASYNC_FOR merely closes an
            // `async for`. These also don't advance `current_line`.
            O::Resume | O::EndFor | O::EndSend | O::EndAsyncFor => {}
            _ => {
                let line = line_at(i);
                if line != 0 && line != current_line {
                    *slot = true;
                }
                current_line = line;
            }
        }
    }
    // Pass 2: jump targets (branch landings can be mid-line).
    for i in 0..n {
        let ins = code.instructions[i];
        let arg = ins.arg as usize;
        let target = match ins.op {
            O::PopJumpIfFalse | O::PopJumpIfTrue | O::JumpForward | O::ForIter | O::Send => {
                i + 1 + arg
            }
            O::JumpBackward => (i + 1).saturating_sub(arg),
            _ => continue,
        };
        // CPython's FOR_ITER/SEND targets skip over END_FOR/END_SEND;
        // WeavePy's exhaustion target *is* the END_FOR, so hop past it.
        let mut t = target;
        while t < n
            && matches!(
                code.instructions[t].op,
                O::EndFor | O::EndSend | O::EndAsyncFor
            )
        {
            t += 1;
        }
        if t < n && line_at(t) != 0 {
            out[t] = true;
        }
    }
    // Pass 3: exception-handler entries.
    for h in &code.exception_table {
        let t = h.handler as usize;
        if t < n && line_at(t) != 0 && !matches!(code.instructions[t].op, O::EndAsyncFor) {
            out[t] = true;
        }
    }
    out
}

/// Validate a `f_lineno` assignment and compute the jump plan.
///
/// `cur_pc` is the frame's current instruction index; `suspended` is
/// true when jumping from a generator's yield ('return'-after-yield
/// trace event), where the analysis stack includes the resume slot the
/// physical stack doesn't hold yet.
pub fn compute_jump(
    code: &CodeObject,
    cur_pc: u32,
    new_lineno: i64,
    suspended: bool,
) -> Result<(PendingJump, u32), RuntimeError> {
    // Same convention as `co_firstlineno`: a module code object reports
    // 1 no matter how many blank/comment lines precede the first
    // statement, so pdb can jump to line 1 of exec'd module code
    // (test_sys_settrace test_jump_to_firstlineno).
    let first_lineno = if code.name == "<module>" {
        1
    } else {
        i64::from(code.linetable.iter().copied().find(|l| *l > 0).unwrap_or(1))
    };
    if new_lineno < first_lineno {
        return Err(value_error(format!(
            "line {new_lineno} comes before the current code block"
        )));
    }
    let n = code.instructions.len();
    let lines = marklines(code);
    let marked = mark_stacks(code);
    // First line at-or-after the requested one that actually starts an
    // instruction run (CPython `first_line_not_before`). Unreachable
    // instructions don't count: CPython's compiler deletes dead code, so
    // a line that only exists in unreachable instructions (our compiler
    // keeps them) is "after the current code block" there, not a
    // handler-entry mismatch (test_no_jump_infinite_while_loop).
    let mut resolved = i64::MAX;
    for (i, &l) in lines.iter().enumerate() {
        if l >= new_lineno && l < resolved && marked.stacks[i] != UNINITIALIZED {
            resolved = l;
        }
    }
    if resolved == i64::MAX {
        return Err(value_error(format!(
            "line {new_lineno} comes after the current code block"
        )));
    }
    let bound_exits = mark_bound_exits(code);
    let start_bound = bound_exits
        .as_ref()
        .and_then(|b| b.get(cur_pc as usize).copied())
        .unwrap_or(0);
    let mut start_stack = *marked.stacks.get(cur_pc as usize).unwrap_or(&UNINITIALIZED);
    let start_exc = marked
        .exc_depths
        .get(cur_pc as usize)
        .copied()
        .unwrap_or(-1)
        .max(0);
    if suspended {
        // Account for the resume slot the yield hasn't pushed yet.
        start_stack = pop_kind(start_stack);
    }
    let mut best_stack = OVERFLOWED;
    let mut best_addr: Option<usize> = None;
    let mut err_msg: Option<&'static str> = None;
    for (i, &l) in lines.iter().enumerate().take(n) {
        if l != resolved || marked.stacks[i] == UNINITIALIZED {
            continue;
        }
        let target_stack = marked.stacks[i];
        let target_exc = marked.exc_depths[i].max(0);
        // The target must not rely on a `.with_exit`/`.aexit` slot the
        // source hasn't bound — the WeavePy face of CPython's "the
        // target's stack holds a with-block `__exit__` the source
        // doesn't" incompatibility (see `mark_bound_exits`).
        let exits_ok = bound_exits
            .as_ref()
            .and_then(|b| b.get(i).copied())
            .is_none_or(|tb| tb == u64::MAX || tb & !start_bound == 0);
        if !exits_ok {
            if err_msg.is_none() {
                err_msg = Some("incompatible stacks");
            }
            continue;
        }
        if target_exc <= start_exc && compatible_stack(start_stack, target_stack) {
            if best_addr.is_none() || target_stack > best_stack {
                best_stack = target_stack;
                best_addr = Some(i);
            }
        } else if err_msg.is_none() {
            err_msg = Some(if start_stack == OVERFLOWED {
                "stack to deep to analyze"
            } else if start_stack == UNINITIALIZED {
                "can't jump from unreachable code"
            } else if target_exc > start_exc {
                "can't jump into an 'except' block as there's no exception"
            } else {
                explain_incompatible_stack(target_stack)
            });
        }
    }
    let Some(addr) = best_addr else {
        return Err(value_error(
            err_msg.unwrap_or("cannot find bytecode for specified line"),
        ));
    };
    let target_exc = marked.exc_depths[addr].max(0);
    let jump = PendingJump {
        target_pc: addr as u32,
        target_depth: stack_depth(best_stack),
        exc_pops: u32::try_from(start_exc - target_exc).unwrap_or(0),
    };
    Ok((jump, u32::try_from(resolved).unwrap_or(0)))
}
