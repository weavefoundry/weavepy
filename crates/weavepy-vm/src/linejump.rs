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

/// First offsets of each line: `linestarts[i] = line` when instruction
/// `i` starts a new line (mirrors CPython's `marklines`), else -1.
fn marklines(code: &CodeObject) -> Vec<i64> {
    let n = code.instructions.len();
    let mut out = vec![-1i64; n];
    let mut last_line: i64 = -1;
    for (i, slot) in out.iter_mut().enumerate() {
        let line = i64::from(code.linetable.get(i).copied().unwrap_or(0));
        if line != last_line && line != 0 {
            *slot = line;
            last_line = line;
        } else if line != 0 {
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
        O::CheckExcMatch => (1, 1),
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
                    // [.., __exit__(O), exc(E)] -> [.., exc(E), result(O)]
                    let below = pop_kind(pop_kind(cur));
                    let after = push_kind(push_kind(below, Kind::Except), Kind::Object);
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
    let first_lineno = i64::from(code.linetable.iter().copied().find(|l| *l > 0).unwrap_or(1));
    if new_lineno < first_lineno {
        return Err(value_error(format!(
            "line {new_lineno} comes before the current code block"
        )));
    }
    let n = code.instructions.len();
    let lines = marklines(code);
    // First line at-or-after the requested one that actually starts an
    // instruction run (CPython `first_line_not_before`).
    let mut resolved = i64::MAX;
    for &l in &lines {
        if l >= new_lineno && l < resolved {
            resolved = l;
        }
    }
    if resolved == i64::MAX {
        return Err(value_error(format!(
            "line {new_lineno} comes after the current code block"
        )));
    }
    let marked = mark_stacks(code);
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
        if l != resolved {
            continue;
        }
        let target_stack = marked.stacks[i];
        let target_exc = marked.exc_depths[i].max(0);
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
