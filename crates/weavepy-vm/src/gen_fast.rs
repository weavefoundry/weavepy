//! Fast steps for simple generator bodies.
//!
//! Resuming a generator the ordinary way switches activations: the quiet
//! loop (or the core loop's inline resume) makes the generator's frame the
//! running one, runs to the next `yield`, and switches back, with the
//! bookkeeping every activation needs (pending-caller lists, recursion
//! guards, the dispatch loop's reload). For a body that only moves scalars
//! through locals and loops over ranges, lists, or other such generators,
//! that bookkeeping is most of the cost of each item.
//!
//! [`Interpreter::gen_fast_step`] runs such a body directly on its frame:
//! from the frame's pc (the sent value already on its stack) to the next
//! `yield`. It never raises and never runs Python code: an instruction it
//! can't finish that way (an overflow, a non-scalar operand, a `return`, an
//! instruction outside its set) ends the step *before* that instruction,
//! with the frame at an ordinary instruction boundary, and the caller
//! continues the resume in the general loop from there. A fast step is
//! therefore always a prefix of the ordinary run.
//!
//! A generator the step iterates in turn is stepped the same way
//! ([`Interpreter::gen_fast_next`]). If that inner step stops partway, the
//! inner generator is parked having consumed its sent value
//! ([`crate::Frame::sent_consumed`]), and the outer step stops before its
//! `FOR_ITER`, which the general loop then runs (resuming the inner one
//! without pushing another value).

use weavepy_compiler::{BinOpKind, CodeObject, CompareKind, OpCode, COMPARE_OP_TO_BOOL_FLAG};

use crate::object::{GeneratorState, Object, PyGenerator, PyIterator};
use crate::sync::Rc;
use crate::{FoldSink, Frame, Interpreter};

/// How a fast step ended.
pub(crate) enum GenStep {
    /// The body yielded this value; the frame is suspended past the yield.
    Yielded(Object),
    /// The next instruction needs the general loop.
    Bail,
}

/// How a fast `next()` of a generator ended.
pub(crate) enum GenNext {
    Yielded(Object),
    /// Nothing happened: the generator is as it was.
    Declined,
    /// The generator consumed its sent value and advanced, then stopped
    /// short of a yield; it's parked for the general loop to continue.
    Partial,
}

/// `FOR_ITER`'s outcome in a fast step.
enum ForNext {
    Value(Object),
    Exhausted,
    Bail,
}

/// Nested fast steps at most this deep.
const MAX_DEPTH: u8 = 8;

/// The instructions a fast step runs (see the module docs). The scan
/// also admits the generator prologue's `RETURN_GENERATOR` and the
/// implicit PEP 479 handler, which a resume or only an exception reaches.
fn op_supported(op: OpCode) -> bool {
    matches!(
        op,
        OpCode::Nop
            | OpCode::NotTaken
            | OpCode::Resume
            | OpCode::PopTop
            | OpCode::LoadFast
            | OpCode::LoadFastBorrow
            | OpCode::LoadFastCheck
            | OpCode::LoadFastLoadFast
            | OpCode::LoadFastBorrowLoadFastBorrow
            | OpCode::StoreFast
            | OpCode::LoadSmallInt
            | OpCode::LoadConst
            | OpCode::BinaryOp
            | OpCode::CompareOp
            | OpCode::ToBool
            | OpCode::PopJumpIfFalse
            | OpCode::PopJumpIfTrue
            | OpCode::JumpForward
            | OpCode::JumpBackward
            | OpCode::GetIter
            | OpCode::ForIter
            | OpCode::EndFor
            | OpCode::PopIter
            | OpCode::YieldValue
            | OpCode::ReturnValue
            | OpCode::ReturnGenerator
            | OpCode::StopIterationError
            | OpCode::CallIntrinsic1
            | OpCode::Reraise
    )
}

/// Whether `code` is a plain generator whose every instruction is one a
/// fast step runs (cached in the code's extension table).
fn code_ok(code: &CodeObject) -> bool {
    use std::sync::atomic::Ordering;
    let Some(ext) = crate::code_vm_ext(code) else {
        return false;
    };
    match ext.gen_fast.load(Ordering::Relaxed) {
        1 => return false,
        2 => return true,
        _ => {}
    }
    let ok = code.is_generator
        && !code.is_coroutine
        && !code.is_async_generator
        && code.cellvars.is_empty()
        && code.freevars.is_empty()
        && code.instructions.iter().all(|i| op_supported(i.op))
        && in_bounds(code, ext.objects.len());
    ext.gen_fast
        .store(if ok { 2 } else { 1 }, Ordering::Relaxed);
    ok
}

/// Whether every jump, local and constant `code`'s instructions name is
/// in range, and the last instruction can't fall through: the step then
/// reads instructions, locals and constants without per-use checks.
fn in_bounds(code: &CodeObject, nconsts: usize) -> bool {
    let instrs = &code.instructions;
    let (n, nvars) = (instrs.len(), code.varnames.len());
    let terminal = |op| {
        matches!(
            op,
            OpCode::ReturnValue | OpCode::Reraise | OpCode::JumpBackward | OpCode::JumpForward
        )
    };
    if instrs.last().is_none_or(|i| !terminal(i.op)) {
        return false;
    }
    instrs.iter().enumerate().all(|(p, i)| {
        let arg = i.arg as usize;
        match i.op {
            OpCode::PopJumpIfFalse
            | OpCode::PopJumpIfTrue
            | OpCode::JumpForward
            | OpCode::ForIter => p + 1 + arg < n,
            OpCode::LoadFast
            | OpCode::LoadFastBorrow
            | OpCode::LoadFastCheck
            | OpCode::StoreFast => arg < nvars,
            OpCode::LoadFastLoadFast | OpCode::LoadFastBorrowLoadFastBorrow => {
                arg >> 4 < nvars && arg & 15 < nvars
            }
            OpCode::LoadConst => arg < nconsts,
            _ => true,
        }
    })
}

/// A machine scalar: its release owes nothing.
#[inline(always)]
fn scalar(v: &Object) -> bool {
    matches!(
        v,
        Object::Int(_) | Object::Float(_) | Object::Bool(_) | Object::None
    )
}

/// A value's copy for the operand stack: scalars inline, anything else a
/// new reference.
#[inline(always)]
fn copy(v: &Object) -> Object {
    match v {
        Object::Int(x) => Object::Int(*x),
        Object::Float(x) => Object::Float(*x),
        Object::Bool(x) => Object::Bool(*x),
        Object::None => Object::None,
        other => crate::clone_hot(other),
    }
}

/// The result of `a <kind> b` for machine scalars, when it can't raise or
/// leave the machine range.
#[inline(always)]
fn scalar_binop(kind: BinOpKind, a: &Object, b: &Object) -> Option<Object> {
    Some(match (a, b) {
        (Object::Int(a), Object::Int(b)) => {
            let (a, b) = (*a, *b);
            Object::Int(match kind {
                BinOpKind::Add => a.checked_add(b)?,
                BinOpKind::Sub => a.checked_sub(b)?,
                BinOpKind::Mult => a.checked_mul(b)?,
                BinOpKind::BitAnd => a & b,
                BinOpKind::BitOr => a | b,
                BinOpKind::BitXor => a ^ b,
                BinOpKind::FloorDiv if b != 0 && !(a == i64::MIN && b == -1) => {
                    a.div_euclid(b) - i64::from(b < 0 && a.rem_euclid(b) != 0)
                }
                BinOpKind::Mod if b != 0 => {
                    let r = a.checked_rem(b)?;
                    if r != 0 && (r < 0) != (b < 0) {
                        r + b
                    } else {
                        r
                    }
                }
                BinOpKind::RShift if (0..64).contains(&b) => a >> b,
                BinOpKind::LShift if (0..63).contains(&b) => {
                    let r = a.checked_shl(b as u32)?;
                    if r >> b != a {
                        return None;
                    }
                    r
                }
                _ => return None,
            })
        }
        (Object::Float(_) | Object::Int(_), Object::Float(_) | Object::Int(_)) => {
            let f = |o: &Object| match o {
                Object::Float(x) => Some(*x),
                // Exactly representable ints only (Python converts exactly
                // or raises for huge ones; both stay on the general path).
                Object::Int(i) if i.unsigned_abs() < (1 << 53) => Some(*i as f64),
                _ => None,
            };
            let (a, b) = (f(a)?, f(b)?);
            Object::Float(match kind {
                BinOpKind::Add => a + b,
                BinOpKind::Sub => a - b,
                BinOpKind::Mult => a * b,
                BinOpKind::Div if b != 0.0 => a / b,
                _ => return None,
            })
        }
        _ => return None,
    })
}

/// `a <kind> b` for machine scalars (NaN and mixed shapes decline).
#[inline(always)]
fn scalar_compare(kind: CompareKind, a: &Object, b: &Object) -> Option<bool> {
    let ord = match (a, b) {
        (Object::Int(a), Object::Int(b)) => a.cmp(b),
        (Object::Float(a), Object::Float(b)) => a.partial_cmp(b)?,
        _ => return None,
    };
    Some(match kind {
        CompareKind::Lt => ord.is_lt(),
        CompareKind::LtE => ord.is_le(),
        CompareKind::Eq => ord.is_eq(),
        CompareKind::NotEq => ord.is_ne(),
        CompareKind::Gt => ord.is_gt(),
        CompareKind::GtE => ord.is_ge(),
    })
}

/// The truth of a scalar (`None` for anything else).
#[inline(always)]
fn scalar_truth(v: &Object) -> Option<bool> {
    Some(match v {
        Object::Bool(b) => *b,
        Object::Int(i) => *i != 0,
        Object::None => false,
        _ => return None,
    })
}

impl Interpreter {
    /// Whether `frame`, a suspended generator's, may take fast steps: a
    /// fast-step body the general machinery holds no extra state for (no
    /// Python-visible frame, no saved exception state, no parked native
    /// activation).
    #[inline]
    pub(crate) fn gen_fast_frame_ok(frame: &Frame) -> bool {
        frame.py_frame.is_none()
            && frame.saved_exc_info.is_empty()
            && frame.pc != 0
            && !frame.shell_cache.as_ref().is_some_and(|c| {
                c.has_materialized
                    .load(std::sync::atomic::Ordering::Relaxed)
            })
            && {
                #[cfg(feature = "jit")]
                {
                    frame.parked_native.is_none()
                }
                #[cfg(not(feature = "jit"))]
                {
                    true
                }
            }
            && code_ok(&frame.code)
    }

    /// Run `frame` from its pc to its next `yield` (see the module docs).
    /// `snap_gen` is the caller's quiet-loop generation; `depth` counts
    /// the fast steps this one is nested in.
    ///
    /// The loop works on raw views of the frame's operand stack and
    /// locals (as the core loop does), writing the stack length and pc
    /// back when it ends.
    ///
    /// With `fold` (a draining consumer's sink, top level only), a yield
    /// the sink takes resumes the body at once with `None` sent.
    pub(crate) fn gen_fast_step(
        &mut self,
        frame: &mut Frame,
        snap_gen: u64,
        depth: u8,
        fold: Option<FoldSink>,
    ) -> GenStep {
        // SAFETY: the frame's code is immutable and outlives the step (the
        // frame holds it, and nothing here replaces it).
        let code: &CodeObject = unsafe { &*Rc::as_ptr(&frame.code) };
        let (instrs, ninstrs) = (code.instructions.as_ptr(), code.instructions.len());
        let Some(ext) = crate::code_vm_ext(code) else {
            return GenStep::Bail;
        };
        let cbase = ext.objects.as_ptr();
        // SAFETY: no guard is live on the locals (`peek_mut` checks), and
        // nothing below runs code that could reach them.
        let Some(locals) = (unsafe { frame.locals.peek_mut() }) else {
            return GenStep::Bail;
        };
        // (The scan checked every local index against the code's
        // variables, which the frame's locals cover.)
        if locals.len() < code.varnames.len() {
            return GenStep::Bail;
        }
        let lbase = locals.as_mut_ptr();
        let stack = &mut frame.stack;
        if stack.capacity() - stack.len() < 8 {
            stack.reserve(8);
        }
        let (base, cap) = (stack.as_mut_ptr(), stack.capacity());
        let mut len = stack.len();
        let mut pc = frame.pc as usize;
        if pc >= ninstrs {
            return GenStep::Bail;
        }
        // SAFETY (throughout): `base` indexes only below `len` (initialized)
        // or `cap` as checked; `lbase`, `cbase` and `instrs` only at the
        // indices and pcs the eligibility scan proved in range (`in_bounds`:
        // every jump lands on an instruction and the last can't fall
        // through, so `pc < ninstrs` whenever an instruction is read).
        let yielded = loop {
            let ins = unsafe { *instrs.add(pc) };
            match ins.op {
                OpCode::Nop | OpCode::NotTaken | OpCode::Resume => pc += 1,
                OpCode::PopTop => {
                    if len == 0 {
                        break None;
                    }
                    let top = unsafe { &*base.add(len - 1) };
                    if !scalar(top) {
                        if !Self::core_droppable(top) {
                            break None;
                        }
                        crate::drop_hot(unsafe { base.add(len - 1).read() });
                    }
                    len -= 1;
                    pc += 1;
                }
                OpCode::LoadFast | OpCode::LoadFastBorrow | OpCode::LoadFastCheck => {
                    let i = ins.arg as usize;
                    if len == cap {
                        break None;
                    }
                    let v = unsafe { &*lbase.add(i) };
                    if matches!(v, Object::Unbound | Object::Cell(_)) {
                        break None;
                    }
                    unsafe { base.add(len).write(copy(v)) };
                    len += 1;
                    pc += 1;
                }
                OpCode::LoadFastLoadFast | OpCode::LoadFastBorrowLoadFastBorrow => {
                    let (i, j) = ((ins.arg >> 4) as usize, (ins.arg & 15) as usize);
                    if len + 2 > cap {
                        break None;
                    }
                    let (a, b) = unsafe { (&*lbase.add(i), &*lbase.add(j)) };
                    if matches!(a, Object::Unbound | Object::Cell(_))
                        || matches!(b, Object::Unbound | Object::Cell(_))
                    {
                        break None;
                    }
                    unsafe {
                        base.add(len).write(copy(a));
                        base.add(len + 1).write(copy(b));
                    }
                    len += 2;
                    pc += 1;
                }
                OpCode::StoreFast => {
                    let i = ins.arg as usize;
                    if len == 0 {
                        break None;
                    }
                    let slot = unsafe { lbase.add(i) };
                    let old = unsafe { &*slot };
                    if matches!(unsafe { &*base.add(len - 1) }, Object::Cell(_)) {
                        break None;
                    }
                    if scalar(old) || matches!(old, Object::Unbound) {
                        // Nothing to release.
                        len -= 1;
                        unsafe { slot.write(base.add(len).read()) };
                    } else {
                        if !Self::core_droppable(old) {
                            break None;
                        }
                        len -= 1;
                        unsafe { crate::drop_hot(std::ptr::replace(slot, base.add(len).read())) };
                    }
                    pc += 1;
                }
                OpCode::LoadSmallInt => {
                    if len == cap {
                        break None;
                    }
                    unsafe { base.add(len).write(Object::Int(i64::from(ins.arg))) };
                    len += 1;
                    pc += 1;
                }
                OpCode::LoadConst => {
                    if len == cap {
                        break None;
                    }
                    unsafe { base.add(len).write(copy(&*cbase.add(ins.arg as usize))) };
                    len += 1;
                    pc += 1;
                }
                OpCode::BinaryOp => {
                    if len < 2 {
                        break None;
                    }
                    // SAFETY: `BinOpKind` is `repr(u8)` and the compiler only
                    // emits valid kinds (as the core loop's arm).
                    let kind: BinOpKind = unsafe { std::mem::transmute(ins.arg as u8) };
                    let (a, b) = unsafe { (&*base.add(len - 2), &*base.add(len - 1)) };
                    let Some(r) = scalar_binop(kind, a, b) else {
                        break None;
                    };
                    // Both operands are scalars (no drop owed).
                    len -= 1;
                    unsafe { base.add(len - 1).write(r) };
                    pc += 1;
                }
                OpCode::CompareOp => {
                    if len < 2 {
                        break None;
                    }
                    let kind = match ins.arg & !COMPARE_OP_TO_BOOL_FLAG {
                        x if x == CompareKind::Lt as u32 => CompareKind::Lt,
                        x if x == CompareKind::LtE as u32 => CompareKind::LtE,
                        x if x == CompareKind::Eq as u32 => CompareKind::Eq,
                        x if x == CompareKind::NotEq as u32 => CompareKind::NotEq,
                        x if x == CompareKind::Gt as u32 => CompareKind::Gt,
                        x if x == CompareKind::GtE as u32 => CompareKind::GtE,
                        _ => break None,
                    };
                    let (a, b) = unsafe { (&*base.add(len - 2), &*base.add(len - 1)) };
                    let Some(r) = scalar_compare(kind, a, b) else {
                        break None;
                    };
                    len -= 1;
                    unsafe { base.add(len - 1).write(Object::Bool(r)) };
                    pc += 1;
                }
                OpCode::ToBool => {
                    let Some(t) = (len > 0)
                        .then(|| unsafe { &*base.add(len - 1) })
                        .and_then(scalar_truth)
                    else {
                        break None;
                    };
                    unsafe { base.add(len - 1).write(Object::Bool(t)) };
                    pc += 1;
                }
                OpCode::PopJumpIfFalse | OpCode::PopJumpIfTrue => {
                    let Some(t) = (len > 0)
                        .then(|| unsafe { &*base.add(len - 1) })
                        .and_then(scalar_truth)
                    else {
                        break None;
                    };
                    len -= 1;
                    pc += 1;
                    if t == (ins.op == OpCode::PopJumpIfTrue) {
                        pc += ins.arg as usize;
                    }
                }
                OpCode::JumpForward => pc += 1 + ins.arg as usize,
                OpCode::JumpBackward => {
                    // The back edge is the eval-breaker (as the core loop's).
                    if self.gil_countdown <= 1 || crate::hot_gates::loop_gen() != snap_gen {
                        break None;
                    }
                    self.gil_countdown -= 1;
                    pc = (pc + 1).saturating_sub(ins.arg as usize);
                }
                // `iter()` of an iterator or a generator is itself.
                OpCode::GetIter => {
                    if len == 0
                        || !matches!(
                            unsafe { &*base.add(len - 1) },
                            Object::Iter(_) | Object::Generator(_)
                        )
                    {
                        break None;
                    }
                    pc += 1;
                }
                OpCode::ForIter => {
                    if len == 0 || len == cap {
                        break None;
                    }
                    let top = unsafe { &*base.add(len - 1) };
                    // A live range's next value in line (the commonest loop).
                    if let Object::Iter(it) = top {
                        // SAFETY: nothing runs code while the view is held.
                        if let Some(PyIterator::Range {
                            current,
                            stop,
                            step,
                        }) = unsafe { it.peek_mut() }
                        {
                            if *step > 0 && *current < *stop {
                                let v = *current;
                                *current = current.wrapping_add(*step);
                                unsafe { base.add(len).write(Object::Int(v)) };
                                len += 1;
                                pc += 1;
                                continue;
                            }
                        }
                    }
                    match self.gen_fast_for_iter(top, snap_gen, depth) {
                        ForNext::Value(v) => {
                            unsafe { base.add(len).write(v) };
                            len += 1;
                            pc += 1;
                        }
                        ForNext::Exhausted => {
                            // The iterator (its sole owner is the stack)
                            // leaves, and the loop exits past its
                            // `END_FOR`/`POP_ITER` pair.
                            len -= 1;
                            crate::drop_hot(unsafe { base.add(len).read() });
                            pc += 1 + ins.arg as usize;
                            let op_at =
                                |pc: usize| (pc < ninstrs).then(|| unsafe { (*instrs.add(pc)).op });
                            if op_at(pc) == Some(OpCode::EndFor) {
                                pc += 1;
                                if matches!(op_at(pc), Some(OpCode::PopIter | OpCode::PopTop)) {
                                    pc += 1;
                                }
                            }
                        }
                        ForNext::Bail => break None,
                    }
                }
                OpCode::YieldValue => {
                    if len == 0 {
                        break None;
                    }
                    pc += 1;
                    // SAFETY: the slot is initialized; a folded value moves
                    // into the sink (or is a scalar), and the sent `None`
                    // takes its place.
                    let top = unsafe { base.add(len - 1) };
                    let folded = match fold {
                        // SAFETY: the sink is the consumer's own, live and
                        // untouched while its resume runs (as the core
                        // loop's fold).
                        Some(FoldSink::Sum(acc)) => unsafe { (*acc).add_scalar(&*top) },
                        Some(FoldSink::Collect(out)) => {
                            unsafe { (*out).push(top.read()) };
                            true
                        }
                        None => false,
                    };
                    if folded {
                        unsafe { top.write(Object::None) };
                        continue;
                    }
                    len -= 1;
                    frame.agen_yielded_value = ins.arg == 0;
                    break Some(unsafe { top.read() });
                }
                _ => break None,
            }
        };
        // SAFETY: the first `len` slots are initialized (pushes wrote them,
        // pops moved values out).
        unsafe { frame.stack.set_len(len) };
        frame.pc = pc as u32;
        match yielded {
            Some(v) => GenStep::Yielded(v),
            None => GenStep::Bail,
        }
    }

    /// `FOR_ITER`'s step on `top` for a fast step (out of line: the
    /// loop keeps its registers): a range, list or tuple iterator's next
    /// value, a range's end (when the loop alone holds it), or a fast
    /// generator's next yield.
    #[inline(never)]
    fn gen_fast_for_iter(&mut self, top: &Object, snap_gen: u64, depth: u8) -> ForNext {
        match top {
            Object::Iter(it) => {
                let unique = Rc::strong_count(it) == 1;
                // SAFETY: nothing below runs code until `it`'s last use
                // (the guard-free `peek`).
                let Some(it) = (unsafe { it.peek_mut() }) else {
                    return ForNext::Bail;
                };
                match it {
                    PyIterator::Range {
                        current,
                        stop,
                        step,
                    } => {
                        let live = if *step > 0 {
                            *current < *stop
                        } else {
                            *step < 0 && *current > *stop
                        };
                        if live {
                            let v = *current;
                            *current = current.wrapping_add(*step);
                            ForNext::Value(Object::Int(v))
                        } else if unique {
                            ForNext::Exhausted
                        } else {
                            ForNext::Bail
                        }
                    }
                    PyIterator::List { items, index, .. } => {
                        // SAFETY: as above.
                        let Some(v) = (unsafe { items.peek() }).and_then(|xs| xs.get(*index))
                        else {
                            return ForNext::Bail;
                        };
                        let v = copy(v);
                        *index += 1;
                        ForNext::Value(v)
                    }
                    PyIterator::Tuple { items, index } => {
                        let Some(v) = items.get(*index) else {
                            return ForNext::Bail;
                        };
                        let v = copy(v);
                        *index += 1;
                        ForNext::Value(v)
                    }
                    _ => ForNext::Bail,
                }
            }
            Object::Generator(g) => {
                let g = g.clone();
                match self.gen_fast_next(&g, snap_gen, depth + 1) {
                    GenNext::Yielded(v) => ForNext::Value(v),
                    GenNext::Declined | GenNext::Partial => ForNext::Bail,
                }
            }
            _ => ForNext::Bail,
        }
    }

    /// A resume's fast steps (its sent value already pushed): the value
    /// the body yields next, with a draining consumer's `fold` taking the
    /// yields it can along the way, or `None` for the general loop to
    /// continue from wherever the steps stopped.
    pub(crate) fn gen_fast_run(
        &mut self,
        frame: &mut Frame,
        snap_gen: u64,
        fold: Option<FoldSink>,
    ) -> Option<Object> {
        if !Self::gen_fast_frame_ok(frame) {
            return None;
        }
        match self.gen_fast_step(frame, snap_gen, 0, fold) {
            GenStep::Yielded(v) => Some(v),
            GenStep::Bail => None,
        }
    }

    /// `next(g)` by fast step (see the module docs): resumes `g` with
    /// `None` and runs it to its next yield when both it and its body
    /// allow. `depth` counts the fast steps this one is nested in.
    pub(crate) fn gen_fast_next(
        &mut self,
        g: &Rc<PyGenerator>,
        snap_gen: u64,
        depth: u8,
    ) -> GenNext {
        if depth > MAX_DEPTH
            || crate::recursion::current_depth() + usize::from(depth) + 2
                >= crate::recursion::recursion_limit()
        {
            return GenNext::Declined;
        }
        // Validate and take the frame under one exclusive view; no Python
        // runs while it's held.
        // SAFETY: nothing below reaches the cell again until `state`'s
        // last use (`peek_mut` rejects a live guard or shared cells).
        let Some(state) = (unsafe { g.state.peek_mut() }) else {
            return GenNext::Declined;
        };
        let (GeneratorState::Suspended(boxed) | GeneratorState::Created(boxed)) = &*state else {
            return GenNext::Declined;
        };
        let first_resume = matches!(*state, GeneratorState::Created(_));
        if !Self::gen_fast_frame_ok(boxed) {
            return GenNext::Declined;
        }
        // A body an earlier fast step left partway (at the eval breaker,
        // say) continues where it stopped, with no value sent.
        let partial = boxed.sent_consumed;
        let prev = std::mem::replace(state, GeneratorState::Running);
        let (GeneratorState::Suspended(mut boxed) | GeneratorState::Created(mut boxed)) = prev
        else {
            unreachable!("checked above");
        };
        let frame: &mut Frame = &mut boxed;
        frame.gen_first_resume = first_resume;
        let start = frame.pc;
        if partial {
            frame.sent_consumed = false;
        } else {
            frame.stack.push(Object::None);
        }
        debug_assert!(!frame.stack.is_empty());
        let out = match self.gen_fast_step(frame, snap_gen, depth, None) {
            GenStep::Yielded(v) => GenNext::Yielded(v),
            GenStep::Bail if frame.pc == start => {
                // Nothing ran: the resume is undone.
                if partial {
                    frame.sent_consumed = true;
                } else {
                    frame.stack.pop();
                }
                GenNext::Declined
            }
            GenStep::Bail => {
                frame.sent_consumed = true;
                GenNext::Partial
            }
        };
        Self::park_suspended_boxed(g, boxed);
        out
    }
}
