//! Pre-decoded leaf bodies for the frameless leaf evaluator.
//!
//! A pure or effect leaf (see `code_pure_leaf_decide`) runs without an
//! activation: its operands are scalars or borrowed pointers, and any
//! miss abandons the evaluation having done nothing observable. This
//! module translates such a body once into a [`LeafPlan`]: every local
//! and every operand-stack position gets a fixed register, jumps name
//! their target op directly, and local loads, `POP_TOP`, `COPY` and
//! `SWAP` mostly disappear into operand addressing. Running a plan is
//! then one dispatch per real operation, with no stack pointer, no
//! bounds checks, and no per-read "was this local assigned" test.

use weavepy_compiler::{BinOpKind, CodeObject, CompareKind, OpCode, UnaryKind};

#[cfg(feature = "jit")]
mod jit;

use crate::object::Object;
use crate::sync::Rc;
use crate::{CodeConstObjects, Interpreter};

/// A leaf operand: a scalar by value, or a borrowed heap object.
#[derive(Clone, Copy)]
// Every payload sits at offset 8 (a primitive representation), so a copy
// is two words rather than a byte-wise shuffle.
#[repr(u64)]
pub(crate) enum V {
    /// A resolved callee: a function the class or namespace holds.
    Fn(*const crate::object::PyFunction),
    /// A builtin type's method body, from the leaf method table (which
    /// holds it for the interpreter's lifetime).
    Bi(*const crate::object::BuiltinFn),
    /// A call's empty self slot.
    Null,
    /// A heap object, borrowed.
    R(*const Object),
    I(i64),
    F(f64),
    B(bool),
    N,
}

#[inline(always)]
pub(crate) fn norm(p: *const Object) -> V {
    // SAFETY: `p` names a live object (see `Interpreter::leaf_eval`).
    match unsafe { &*p } {
        Object::Int(i) => V::I(*i),
        Object::Float(x) => V::F(*x),
        Object::Bool(b) => V::B(*b),
        Object::None => V::N,
        _ => V::R(p),
    }
}

#[inline(always)]
pub(crate) fn truth(v: V) -> Option<bool> {
    Some(match v {
        V::B(b) => b,
        V::I(i) => i != 0,
        V::F(x) => x != 0.0,
        V::N => false,
        // SAFETY: as `norm`.
        V::R(p) => match unsafe { &*p } {
            Object::Str(s) => !s.is_empty(),
            Object::Tuple(t) => !t.is_empty(),
            // SAFETY: a read with nothing running (see `peek`).
            Object::List(l) => !unsafe { l.peek() }?.is_empty(),
            Object::Dict(d) => !unsafe { d.peek() }?.is_empty(),
            _ => return None,
        },
        V::Fn(_) | V::Bi(_) | V::Null => return None,
    })
}

/// The callback-free comparison rules both the decoded field shapes and
/// the plan runner use. NaNs and unsupported operands fall back to the
/// interpreter.
#[inline(always)]
pub(crate) fn compare(a: V, b: V, kind: CompareKind) -> Option<bool> {
    const EXACT: u64 = 1 << 53;
    let ord = match (a, b) {
        (V::I(x), V::I(y)) => x.cmp(&y),
        (V::F(x), V::F(y)) => x.partial_cmp(&y)?,
        (V::I(x), V::F(y)) if x.unsigned_abs() < EXACT => (x as f64).partial_cmp(&y)?,
        (V::F(x), V::I(y)) if y.unsigned_abs() < EXACT => x.partial_cmp(&(y as f64))?,
        (V::B(x), V::B(y)) => x.cmp(&y),
        (V::B(x), V::I(y)) => i64::from(x).cmp(&y),
        (V::I(x), V::B(y)) => x.cmp(&i64::from(y)),
        (V::N, V::N) if matches!(kind, CompareKind::Eq | CompareKind::NotEq) => {
            std::cmp::Ordering::Equal
        }
        // SAFETY: these pointers name values owned by live arguments or
        // the evaluator's scratch; no Python runs.
        (V::R(p), V::R(q)) => match (unsafe { &*p }, unsafe { &*q }) {
            (Object::Str(s), Object::Str(t)) => (**s).cmp(&**t),
            _ => return None,
        },
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

/// Registers a plan may address: the locals, then the operand stack.
const REGS: usize = 32;

/// Nested leaf calls evaluated within one evaluation at most this deep.
const NEST: u8 = 3;

/// One plan operation. Registers are `u8` indices below [`REGS`]; `pc`
/// is the source instruction (its inline caches and site slots), and
/// `name` its `co_names` index.
#[derive(Clone, Copy, Debug)]
enum Op {
    /// `regs[dst] = regs[src]`.
    Move {
        dst: u8,
        src: u8,
    },
    /// `regs[dst] = consts[k]`.
    Const {
        dst: u8,
        k: u16,
    },
    Global {
        dst: u8,
        pc: u16,
    },
    Attr {
        dst: u8,
        src: u8,
        pc: u16,
        name: u16,
    },
    /// The method-form load: `regs[dst]` the function, `regs[dst + 1]`
    /// the receiver (or the empty self slot for a class receiver).
    Method {
        dst: u8,
        src: u8,
        pc: u16,
        name: u16,
    },
    Compare {
        dst: u8,
        a: u8,
        b: u8,
        kind: u8,
    },
    Is {
        dst: u8,
        a: u8,
        b: u8,
        invert: bool,
    },
    Truth {
        dst: u8,
        src: u8,
    },
    Unary {
        dst: u8,
        src: u8,
        kind: u8,
    },
    Binary {
        dst: u8,
        a: u8,
        b: u8,
        kind: u8,
    },
    /// `regs[dst] = regs[a][regs[b]]` (see [`subscr`]).
    Subscr {
        dst: u8,
        a: u8,
        b: u8,
    },
    /// Exchange two registers.
    Swap {
        a: u8,
        b: u8,
    },
    /// Jump to op `target` when `regs[src]`'s truth is `when`.
    BranchIf {
        src: u8,
        when: bool,
        target: u16,
    },
    /// Jump to op `target` when `regs[src]` `is None` is `when`.
    BranchNone {
        src: u8,
        when: bool,
        target: u16,
    },
    Jump {
        target: u16,
    },
    /// `regs[at]` the callee, `regs[at + 1]` its self slot, then `argc`
    /// arguments; the result lands in `regs[at]`.
    Call {
        at: u8,
        argc: u8,
        pc: u16,
    },
    Return {
        src: u8,
    },
    /// `regs[dst]` a new empty list (`BUILD_LIST 0`) or, with `dict`,
    /// an empty dict (`BUILD_MAP 0`), held by the owned scratch.
    New {
        dst: u8,
        dict: bool,
    },
    /// Abandon the evaluation: the path reached an instruction the plan
    /// can't run.
    Decline,
    /// An effect leaf's buffered `recv.name = val`.
    StoreAttr {
        recv: u8,
        val: u8,
        pc: u16,
        name: u16,
    },
}

/// A leaf body, translated (see the module docs).
pub(crate) struct LeafPlan {
    ops: Box<[Op]>,
    /// The loaded constants, normalized. A heap constant points into the
    /// code extension's materialized constants, which outlive the plan.
    consts: Box<[V]>,
    /// Arguments, copied into the first registers on entry.
    nargs: u8,
    /// Registers the plan uses (the locals, then its deepest stack).
    nregs: u8,
    /// Every attribute store goes to the first argument (never
    /// reassigned), each to a different name: the buffered stores share
    /// one receiver and each is the latest to its attribute.
    unique_stores: bool,
    /// Evaluations so far, toward compiling the plan (see [`jit`]).
    #[cfg(feature = "jit")]
    runs: std::sync::atomic::AtomicU32,
    /// The native code for a pure and an effect evaluation, once compiled
    /// (`None` inside: the plan stays interpreted).
    #[cfg(feature = "jit")]
    native: [std::sync::OnceLock<Option<jit::Native>>; 2],
}

// SAFETY: the constants' pointers name the code extension's immutable
// materialized constants (`Object`s shared the way the extension itself
// shares them); a plan is only read.
unsafe impl Send for LeafPlan {}
// SAFETY: as above.
unsafe impl Sync for LeafPlan {}

/// A value on the translator's abstract operand stack: the register
/// that holds it. A stack position's own register is `nl + position`;
/// a local's value is read in place from the local's register until
/// something would overwrite it.
type Opnd = u8;

struct Builder<'a> {
    code: &'a CodeObject,
    ext: &'a CodeConstObjects,
    ops: Vec<Op>,
    consts: Vec<V>,
    /// Number of locals (registers below it).
    nl: usize,
    stack: Vec<Opnd>,
    /// The names stored into so far, while every store goes to the
    /// first argument (see [`LeafPlan::unique_stores`]); `None` once one
    /// doesn't.
    stored: Option<Vec<u32>>,
    /// Locals definitely assigned on the current path (bit per local).
    assigned: u32,
    /// Per bytecode target: the stack depth and assigned set on the
    /// edges seen so far.
    targets: std::collections::HashMap<usize, (usize, u32)>,
    /// `(op index, bytecode target)` of every emitted jump.
    fixups: Vec<(usize, usize)>,
    /// The op index each bytecode instruction starts at.
    starts: Vec<u16>,
    /// One past the highest register handed out.
    top: std::cell::Cell<usize>,
}

impl Builder<'_> {
    fn slot(&self, pos: usize) -> Option<u8> {
        let r = self.nl + pos;
        self.top.set(self.top.get().max(r + 1));
        (r < REGS).then_some(r as u8)
    }

    fn push_new(&mut self) -> Option<u8> {
        let r = self.slot(self.stack.len())?;
        self.stack.push(r);
        Some(r)
    }

    fn pop(&mut self) -> Option<Opnd> {
        self.stack.pop()
    }

    /// Put every stack entry in its own position's register.
    fn canonicalize(&mut self) -> Option<()> {
        for pos in 0..self.stack.len() {
            let own = self.slot(pos)?;
            if self.stack[pos] != own {
                // Entries only ever name locals or their own position
                // (see `swap` and `copy`), so this write clobbers none.
                self.ops.push(Op::Move {
                    dst: own,
                    src: self.stack[pos],
                });
                self.stack[pos] = own;
            }
        }
        Some(())
    }

    /// Before local `i` is overwritten: every stack entry still reading
    /// it moves into its own position's register.
    fn release_local(&mut self, i: u8) -> Option<()> {
        for pos in 0..self.stack.len() {
            if self.stack[pos] == i {
                let own = self.slot(pos)?;
                self.ops.push(Op::Move { dst: own, src: i });
                self.stack[pos] = own;
            }
        }
        Some(())
    }

    fn load_local(&mut self, i: u32) -> Option<()> {
        let i = i as usize;
        if i >= self.nl || self.assigned & (1 << i) == 0 {
            return None;
        }
        self.stack.push(i as u8);
        Some(())
    }

    fn store_local(&mut self, i: u32) -> Option<()> {
        let i = i as usize;
        if i >= self.nl {
            return None;
        }
        let src = self.pop()?;
        if src != i as u8 {
            self.release_local(i as u8)?;
            self.ops.push(Op::Move { dst: i as u8, src });
        }
        if i == 0 {
            // The first argument no longer names one receiver.
            self.stored = None;
        }
        self.assigned |= 1 << i;
        Some(())
    }

    fn constant(&mut self, v: V) -> Option<()> {
        let k = u16::try_from(self.consts.len()).ok()?;
        self.consts.push(v);
        let dst = self.push_new()?;
        self.ops.push(Op::Const { dst, k });
        Some(())
    }

    /// Record a jump edge to bytecode `target` with the current state
    /// (already canonical), and emit its fixup.
    fn edge(&mut self, target: usize) -> Option<()> {
        let state = (self.stack.len(), self.assigned);
        match self.targets.get_mut(&target) {
            Some((depth, assigned)) => {
                if *depth != state.0 {
                    return None;
                }
                *assigned &= state.1;
            }
            None => {
                self.targets.insert(target, state);
            }
        }
        self.fixups.push((self.ops.len() - 1, target));
        Some(())
    }

    /// Translate the instruction at `pc`: whether control falls through
    /// to the next one. `None` for one the plan can't express (the
    /// caller rolls back what it emitted and declines on that path).
    fn step(&mut self, pc: usize, ins: weavepy_compiler::Instruction) -> Option<bool> {
        let arg = ins.arg;
        match ins.op {
            OpCode::Resume | OpCode::Nop | OpCode::NotTaken => {}
            OpCode::LoadFast | OpCode::LoadFastBorrow | OpCode::LoadFastCheck => {
                self.load_local(arg)?;
            }
            OpCode::LoadFastLoadFast | OpCode::LoadFastBorrowLoadFastBorrow => {
                self.load_local(arg >> 4)?;
                self.load_local(arg & 15)?;
            }
            OpCode::StoreFast => self.store_local(arg)?,
            OpCode::StoreFastLoadFast => {
                self.store_local(arg >> 4)?;
                self.load_local(arg & 15)?;
            }
            OpCode::StoreFastStoreFast => {
                self.store_local(arg >> 4)?;
                self.store_local(arg & 15)?;
            }
            OpCode::LoadConst => {
                let v = norm(self.ext.objects.get(arg as usize)?);
                self.constant(v)?;
            }
            OpCode::LoadSmallInt => self.constant(V::I(i64::from(arg)))?,
            OpCode::PushNull => self.constant(V::Null)?,
            OpCode::BuildList | OpCode::BuildMap if arg == 0 => {
                let dst = self.push_new()?;
                self.ops.push(Op::New {
                    dst,
                    dict: ins.op == OpCode::BuildMap,
                });
            }
            OpCode::LoadGlobal => {
                let dst = self.push_new()?;
                self.ops.push(Op::Global {
                    dst,
                    pc: u16::try_from(pc).ok()?,
                });
            }
            OpCode::LoadGlobalPushNull => {
                let dst = self.push_new()?;
                self.ops.push(Op::Global {
                    dst,
                    pc: u16::try_from(pc).ok()?,
                });
                self.constant(V::Null)?;
            }
            OpCode::LoadAttr => {
                let src = self.pop()?;
                let dst = self.push_new()?;
                self.ops.push(Op::Attr {
                    dst,
                    src,
                    pc: u16::try_from(pc).ok()?,
                    name: u16::try_from(arg).ok()?,
                });
            }
            OpCode::LoadMethodAttr => {
                let src = self.pop()?;
                let dst = self.push_new()?;
                self.push_new()?;
                self.ops.push(Op::Method {
                    dst,
                    src,
                    pc: u16::try_from(pc).ok()?,
                    name: u16::try_from(arg).ok()?,
                });
            }
            OpCode::CompareOp => {
                let b = self.pop()?;
                let a = self.pop()?;
                let dst = self.push_new()?;
                let kind = (arg & !weavepy_compiler::COMPARE_OP_TO_BOOL_FLAG) as u8;
                if kind > CompareKind::GtE as u8 {
                    return None;
                }
                self.ops.push(Op::Compare { dst, a, b, kind });
            }
            OpCode::IsOp => {
                let b = self.pop()?;
                let a = self.pop()?;
                let dst = self.push_new()?;
                self.ops.push(Op::Is {
                    dst,
                    a,
                    b,
                    invert: arg == 1,
                });
            }
            OpCode::ToBool => {
                let src = self.pop()?;
                let dst = self.push_new()?;
                self.ops.push(Op::Truth { dst, src });
            }
            OpCode::UnaryOp => {
                let src = self.pop()?;
                let dst = self.push_new()?;
                self.ops.push(Op::Unary {
                    dst,
                    src,
                    kind: arg as u8,
                });
            }
            OpCode::BinaryOp => {
                let b = self.pop()?;
                let a = self.pop()?;
                let dst = self.push_new()?;
                // (The in-place flag sits above the kind's byte, and
                // means nothing for the scalars this runs.)
                self.ops.push(Op::Binary {
                    dst,
                    a,
                    b,
                    kind: arg as u8,
                });
            }
            OpCode::BinarySubscr => {
                let b = self.pop()?;
                let a = self.pop()?;
                let dst = self.push_new()?;
                self.ops.push(Op::Subscr { dst, a, b });
            }
            OpCode::CopyTop => {
                let n = (arg as usize).max(1);
                let depth = self.stack.len();
                if n > depth {
                    return None;
                }
                let src = self.stack[depth - n];
                let dst = self.push_new()?;
                self.ops.push(Op::Move { dst, src });
            }
            OpCode::Swap => {
                let n = arg as usize;
                let depth = self.stack.len();
                if n < 2 || n > depth {
                    return None;
                }
                // Both entries move into their own registers first, so
                // the exchange keeps every entry in its own position.
                let (lo, hi) = (depth - n, depth - 1);
                for pos in [lo, hi] {
                    let own = self.slot(pos)?;
                    if self.stack[pos] != own {
                        self.ops.push(Op::Move {
                            dst: own,
                            src: self.stack[pos],
                        });
                        self.stack[pos] = own;
                    }
                }
                self.ops.push(Op::Swap {
                    a: self.stack[lo],
                    b: self.stack[hi],
                });
            }
            OpCode::PopTop => {
                self.pop()?;
            }
            OpCode::PopJumpIfFalse
            | OpCode::PopJumpIfTrue
            | OpCode::PopJumpIfNone
            | OpCode::PopJumpIfNotNone => {
                // The condition is a local or its own position's
                // register, which lies above the remaining entries:
                // canonicalizing them writes neither.
                let src = self.pop()?;
                self.canonicalize()?;
                let target = pc + 1 + arg as usize;
                self.ops.push(match ins.op {
                    OpCode::PopJumpIfFalse => Op::BranchIf {
                        src,
                        when: false,
                        target: 0,
                    },
                    OpCode::PopJumpIfTrue => Op::BranchIf {
                        src,
                        when: true,
                        target: 0,
                    },
                    OpCode::PopJumpIfNone => Op::BranchNone {
                        src,
                        when: true,
                        target: 0,
                    },
                    _ => Op::BranchNone {
                        src,
                        when: false,
                        target: 0,
                    },
                });
                self.edge(target)?;
            }
            OpCode::JumpForward => {
                self.canonicalize()?;
                self.ops.push(Op::Jump { target: 0 });
                self.edge(pc + 1 + arg as usize)?;
                return Some(false);
            }
            OpCode::Call => {
                let argc = arg as usize;
                let depth = self.stack.len();
                if depth < argc + 2 {
                    return None;
                }
                self.canonicalize()?;
                let at = depth - argc - 2;
                self.stack.truncate(at);
                let r = self.push_new()?;
                self.ops.push(Op::Call {
                    at: r,
                    argc: u8::try_from(argc).ok()?,
                    pc: u16::try_from(pc).ok()?,
                });
            }
            OpCode::ReturnValue => {
                let src = self.pop()?;
                self.ops.push(Op::Return { src });
                return Some(false);
            }
            OpCode::StoreAttr => {
                let recv = self.pop()?;
                let val = self.pop()?;
                if let Some(names) = &mut self.stored {
                    if recv != 0 || names.contains(&arg) {
                        self.stored = None;
                    } else {
                        names.push(arg);
                    }
                }
                self.ops.push(Op::StoreAttr {
                    recv,
                    val,
                    pc: u16::try_from(pc).ok()?,
                    name: u16::try_from(arg).ok()?,
                });
            }
            _ => return None,
        }
        Some(true)
    }

    fn build(mut self) -> Option<LeafPlan> {
        let instrs = &self.code.instructions;
        // Whether the previous instruction falls through to this one.
        let mut live = true;
        #[allow(clippy::needless_range_loop)]
        for pc in 0..instrs.len() {
            if let Some(&(depth, assigned)) = self.targets.get(&pc) {
                if live {
                    // A fallthrough into a merge point arrives canonical
                    // (its moves run before the point the jumps land on).
                    self.canonicalize()?;
                    if self.stack.len() != depth {
                        return None;
                    }
                    self.assigned &= assigned;
                } else {
                    self.assigned = assigned;
                }
                self.stack.clear();
                for pos in 0..depth {
                    let r = self.slot(pos)?;
                    self.stack.push(r);
                }
                live = true;
            }
            self.starts.push(u16::try_from(self.ops.len()).ok()?);
            if !live {
                // Unreachable code: nothing jumps here.
                continue;
            }
            let mark = (self.ops.len(), self.fixups.len());
            match self.step(pc, instrs[pc]) {
                Some(next) => live = next,
                None => {
                    // The path that reaches this instruction declines
                    // (the old evaluator's behaviour: only a path that
                    // runs an unsupported instruction abandons).
                    self.ops.truncate(mark.0);
                    self.fixups.truncate(mark.1);
                    self.ops.push(Op::Decline);
                    live = false;
                }
            }
        }
        // Every path ends in a return or a jump: the runner never steps
        // past the last op.
        if live || self.ops.is_empty() {
            return None;
        }
        for &(at, target) in &self.fixups {
            let to = *self.starts.get(target)?;
            if usize::from(to) >= self.ops.len() {
                return None;
            }
            match &mut self.ops[at] {
                Op::BranchIf { target, .. }
                | Op::BranchNone { target, .. }
                | Op::Jump { target } => {
                    *target = to;
                }
                _ => return None,
            }
        }
        Some(LeafPlan {
            ops: self.ops.into_boxed_slice(),
            consts: self.consts.into_boxed_slice(),
            nargs: u8::try_from(crate::leaf_arity(self.code)).ok()?,
            nregs: u8::try_from(self.top.get().clamp(self.nl, REGS)).ok()?,
            unique_stores: self.stored.is_some() && self.code.arg_count > 0,
            #[cfg(feature = "jit")]
            runs: std::sync::atomic::AtomicU32::new(0),
            #[cfg(feature = "jit")]
            native: [std::sync::OnceLock::new(), std::sync::OnceLock::new()],
        })
    }
}

/// Translate `code` (a pure or effect leaf) into a plan; `None` for a
/// body the plan can't express (the ordinary call runs it instead).
pub(crate) fn build(code: &CodeObject, ext: &CodeConstObjects) -> Option<LeafPlan> {
    let nl = code.varnames.len();
    let nargs = crate::leaf_arity(code);
    if nl > 16 || nargs > nl || nargs > 8 || code.instructions.len() > u16::MAX as usize {
        return None;
    }
    Builder {
        code,
        ext,
        ops: Vec::new(),
        consts: Vec::new(),
        nl,
        stack: Vec::new(),
        stored: Some(Vec::new()),
        assigned: (1u32 << nargs) - 1,
        targets: std::collections::HashMap::new(),
        fixups: Vec::new(),
        starts: Vec::with_capacity(code.instructions.len()),
        top: std::cell::Cell::new(nl),
    }
    .build()
}

/// How many owned values one evaluation (its callee frames included)
/// may hold.
const OWNED: usize = 8;

/// Values a leaf path hands back owned (a polymorphic or class read, a
/// nested call's result) stay here until the evaluation ends; registers
/// point in.
struct Owned {
    buf: [std::mem::MaybeUninit<Object>; OWNED],
    n: usize,
}

impl Drop for Owned {
    // Usually nothing is held: no call for the empty case.
    #[inline(always)]
    fn drop(&mut self) {
        if self.n != 0 {
            self.release();
        }
    }
}

impl Owned {
    #[inline(never)]
    fn release(&mut self) {
        for k in 0..self.n {
            // SAFETY: the first `n` entries are initialized.
            crate::drop_hot(unsafe { self.buf[k].assume_init_read() });
        }
    }
}

impl Owned {
    #[inline(always)]
    fn new() -> Self {
        Owned {
            buf: [const { std::mem::MaybeUninit::uninit() }; OWNED],
            n: 0,
        }
    }

    /// Whether `p` names one of the held values.
    #[inline(always)]
    fn holds(&self, p: *const Object) -> bool {
        let base = self.buf.as_ptr().cast::<Object>();
        // SAFETY: one past the buffer's end.
        let end = unsafe { base.add(self.buf.len()) };
        p >= base && p < end
    }

    /// Hold `v` (a scalar needs no holding) and name it.
    #[inline]
    fn own(&mut self, v: Object) -> Option<V> {
        let scalar = match v {
            Object::Int(i) => V::I(i),
            Object::Float(x) => V::F(x),
            Object::Bool(b) => V::B(b),
            Object::None => V::N,
            _ => {
                if self.n == self.buf.len() {
                    return None;
                }
                let slot = &mut self.buf[self.n];
                slot.write(v);
                self.n += 1;
                return Some(V::R(slot.as_ptr()));
            }
        };
        // A scalar owns nothing: no drop glue to run.
        std::mem::forget(v);
        Some(scalar)
    }
}

/// An effect leaf's attribute stores, in order, until the return commits
/// them: receiver, store pc, name index, value (all owned). Entries are
/// only appended, so a value read back from one stays put while the
/// evaluation runs.
const PENDING: usize = 6;

/// The receiver is a stable location for the whole evaluation: an
/// argument (rooted by the caller) or an owned-scratch clone.
type Store = (*const Object, u32, u32, Object);

struct Pending {
    buf: [std::mem::MaybeUninit<Store>; PENDING],
    n: usize,
    /// Entries whose value the return moved into a dict.
    moved: u8,
}

impl Pending {
    #[inline(always)]
    fn new() -> Self {
        Pending {
            buf: [const { std::mem::MaybeUninit::uninit() }; PENDING],
            n: 0,
            moved: 0,
        }
    }

    fn get(&self, k: usize) -> &Store {
        debug_assert!(k < self.n);
        // SAFETY: the first `n` entries are initialized.
        unsafe { self.buf[k].assume_init_ref() }
    }

    /// Whether entry `k` is the last store to its attribute.
    fn latest(&self, k: usize) -> bool {
        let (r0, _, n0, _) = self.get(k);
        !(k + 1..self.n).any(|j| {
            let (r1, _, n1, _) = self.get(j);
            // SAFETY: stable receivers (see `Store`).
            n1 == n0 && unsafe { (**r1).is_same(&**r0) }
        })
    }
}

impl Drop for Pending {
    // Usually nothing is buffered: no call for the empty case.
    #[inline(always)]
    fn drop(&mut self) {
        if self.n != 0 {
            self.release();
        }
    }
}

impl Pending {
    #[inline(never)]
    fn release(&mut self) {
        for k in 0..self.n {
            // SAFETY: the first `n` entries are initialized; a moved value
            // is left in place, not dropped again.
            let (_, _, _, value) = unsafe { self.buf[k].assume_init_read() };
            if self.moved & (1 << k) == 0 {
                crate::drop_hot(value);
            } else {
                std::mem::forget(value);
            }
        }
    }
}

/// A leaf call's callee: a Python function to evaluate in place, or a
/// builtin (with its owner, when a namespace holds one).
enum Callee<'a> {
    Py(*const crate::object::PyFunction),
    Native(
        *const crate::object::BuiltinFn,
        Option<&'a Rc<crate::object::BuiltinFn>>,
    ),
}

/// A leaf evaluation's result: a value the caller may borrow for the rest
/// of its own evaluation (an argument, a field of one, a namespace entry
/// or a constant: nothing the evaluation owned), or an owned object.
pub(crate) enum LeafRet {
    Borrowed(V),
    Owned(Object),
}

impl LeafRet {
    /// The result as an owned object (a borrowed value is cloned).
    #[inline(always)]
    pub(crate) fn into_object(self) -> Option<Object> {
        match self {
            LeafRet::Borrowed(v) => to_object(v),
            LeafRet::Owned(o) => Some(o),
        }
    }
}

/// An owned object for a leaf value (the return value, a buffered
/// store's value); `None` for the markers that are never values.
#[inline(always)]
pub(super) fn to_object(v: V) -> Option<Object> {
    Some(match v {
        // SAFETY: as `norm` (an owned value is cloned before its holder
        // drops).
        V::R(p) => crate::clone_hot(unsafe { &*p }),
        V::I(i) => Object::Int(i),
        V::F(x) => Object::Float(x),
        V::B(b) => Object::Bool(b),
        V::N => Object::None,
        V::Fn(_) | V::Bi(_) | V::Null => return None,
    })
}

impl Interpreter {
    /// Run `plan` (the translation of `code`, whose namespaces are `f`'s)
    /// on borrowed `args`, at call-nesting depth `nest`: the leaf's
    /// result, or `None` having done nothing observable (see
    /// `Interpreter::leaf_eval`).
    ///
    /// `FRESH`: the first argument is an instance nothing else has seen
    /// (a constructor's `self`), so a store into it lands at once — a
    /// later decline leaves it half-built, and the caller discards it.
    #[inline(always)]
    pub(crate) fn leaf_run<const GETTER: bool, const EFFECT: bool, const FRESH: bool>(
        &self,
        code: &CodeObject,
        ext: &CodeConstObjects,
        plan: &LeafPlan,
        f: &crate::object::PyFunction,
        args: &[*const Object],
        nest: u8,
    ) -> Option<Object> {
        self.leaf_run_ret::<GETTER, EFFECT, FRESH>(code, ext, plan, f, args, nest)?
            .into_object()
    }

    /// [`Self::leaf_run`] with its result borrowed when the caller may
    /// (see [`LeafRet`]).
    #[inline(never)]
    pub(crate) fn leaf_run_ret<const GETTER: bool, const EFFECT: bool, const FRESH: bool>(
        &self,
        code: &CodeObject,
        ext: &CodeConstObjects,
        plan: &LeafPlan,
        f: &crate::object::PyFunction,
        args: &[*const Object],
        nest: u8,
    ) -> Option<LeafRet> {
        /// A pure-leaf callee runs as a frame of this evaluation: its
        /// registers are the next `REGS` window, and its caller's state
        /// waits here until its return.
        struct Caller<'p> {
            ip: usize,
            code: &'p CodeObject,
            ext: &'p CodeConstObjects,
            plan: &'p LeafPlan,
            f: &'p crate::object::PyFunction,
            /// The caller's register for the result.
            at: u8,
            /// The caller's registers (its plan's `nregs`).
            saved: [std::mem::MaybeUninit<V>; REGS],
        }
        let nargs = usize::from(plan.nargs);
        if args.len() != nargs {
            return None;
        }
        // A warm plan runs as native code (a getter's or constructor's
        // evaluation stays here).
        #[cfg(feature = "jit")]
        if !GETTER && !FRESH {
            if let Some(r) = jit::run::<EFFECT>(self, code, ext, plan, f, args, nest) {
                return r;
            }
        }
        let (mut code, mut ext, mut plan, mut f) = (code, ext, plan, f);
        let mut regs = [const { std::mem::MaybeUninit::<V>::uninit() }; REGS];
        for (k, &a) in args.iter().enumerate() {
            regs[k].write(norm(a));
        }
        let mut callers = [const { std::mem::MaybeUninit::<Caller<'_>>::uninit() }; NEST as usize];
        let mut depth = 0usize;
        // The translation proves every register is written before it's
        // read, and that every index is below `REGS`.
        macro_rules! get {
            ($r:expr) => {
                // SAFETY: see above.
                unsafe { regs.get_unchecked(usize::from($r)).assume_init() }
            };
        }
        macro_rules! set {
            ($r:expr, $v:expr) => {{
                let v = $v;
                // SAFETY: see above.
                unsafe { regs.get_unchecked_mut(usize::from($r)).write(v) };
            }};
        }
        let mut consts: &[V] = &plan.consts;
        let mut stamps: &[crate::StampSlot] = ext.stamp_slots.get().map_or(&[], |s| &s[..]);
        let mut owned = Owned::new();
        let mut pend = Pending::new();
        let mut ops: &[Op] = &plan.ops;
        let mut ip = 0usize;
        loop {
            // SAFETY: every path ends in a return or a jump, and every
            // jump target is an op (checked when the plan was built).
            // Matched in place: each arm loads only its own operands.
            let op: &Op = unsafe { ops.get_unchecked(ip) };
            ip += 1;
            match *op {
                Op::Move { dst, src } => set!(dst, get!(src)),
                // SAFETY: `k` names a plan constant (the translation).
                Op::Const { dst, k } => set!(dst, unsafe { *consts.get_unchecked(usize::from(k)) }),
                Op::Global { dst, pc } => set!(dst, self.plan_global(code, stamps, f, pc)?),
                Op::Attr { dst, src, pc, name } => {
                    let v = self.plan_attr::<GETTER, EFFECT>(
                        code,
                        ext,
                        stamps,
                        &mut owned,
                        &pend,
                        get!(src),
                        pc,
                        name,
                    )?;
                    set!(dst, v);
                }
                Op::Method { dst, src, pc, name } => {
                    let (func, recv) = self.plan_method(code, get!(src), pc, name)?;
                    set!(dst, func);
                    set!(dst + 1, recv);
                }
                Op::Compare { dst, a, b, kind } => {
                    // SAFETY: the translation checked `kind` names a
                    // comparison.
                    let kind: CompareKind = unsafe { std::mem::transmute(kind) };
                    set!(dst, V::B(compare(get!(a), get!(b), kind)?));
                }
                Op::Is { dst, a, b, invert } => {
                    set!(dst, V::B(is_same(get!(a), get!(b)) != invert))
                }
                Op::Truth { dst, src } => set!(dst, V::B(truth(get!(src))?)),
                Op::Unary { dst, src, kind } => set!(dst, unary(get!(src), kind)?),
                Op::Binary { dst, a, b, kind } => set!(dst, binary(get!(a), get!(b), kind)?),
                Op::Subscr { dst, a, b } => set!(dst, subscr(&mut owned, get!(a), get!(b))?),
                Op::Swap { a, b } => {
                    let (x, y) = (get!(a), get!(b));
                    set!(a, y);
                    set!(b, x);
                }
                Op::BranchIf { src, when, target } => {
                    if truth(get!(src))? == when {
                        ip = usize::from(target);
                    }
                }
                Op::BranchNone { src, when, target } => {
                    if matches!(get!(src), V::N) == when {
                        ip = usize::from(target);
                    }
                }
                Op::Jump { target } => ip = usize::from(target),
                Op::New { dst, dict } => set!(dst, new_container(&mut owned, dict)?),
                Op::Decline => return None,
                Op::Call { at, argc, pc } => {
                    // A pure-leaf callee, evaluated in place: only while
                    // no store is buffered (it would not see one).
                    if (EFFECT && pend.n > 0) || usize::from(nest) + depth >= usize::from(NEST) {
                        return None;
                    }
                    let callee = match get!(at) {
                        V::Fn(fp) => Callee::Py(fp),
                        V::Bi(b) => Callee::Native(b, None),
                        // SAFETY: as `norm`.
                        V::R(p) => match unsafe { &*p } {
                            Object::Function(func) => Callee::Py(Rc::as_ptr(func)),
                            Object::Builtin(b) => Callee::Native(Rc::as_ptr(b), Some(b)),
                            _ => return None,
                        },
                        _ => return None,
                    };
                    let first = if matches!(get!(at + 1), V::Null) {
                        at + 2
                    } else {
                        at + 1
                    };
                    let n = usize::from(at + 2 + argc - first);
                    if n > 8 {
                        return None;
                    }
                    let fp = match callee {
                        Callee::Py(fp) => fp,
                        Callee::Native(b, rc) => {
                            // A read-only builtin, on borrowed copies of the
                            // arguments: never dropped, so no reference moves.
                            let mut staged =
                                [const { std::mem::MaybeUninit::<Object>::uninit() }; 8];
                            #[allow(clippy::needless_range_loop)]
                            for k in 0..n {
                                let o = match get!(first + k as u8) {
                                    // SAFETY: as `norm`; the copy is forgotten.
                                    V::R(p) => unsafe { std::ptr::read(p) },
                                    V::I(i) => Object::Int(i),
                                    V::F(x) => Object::Float(x),
                                    V::B(b) => Object::Bool(b),
                                    V::N => Object::None,
                                    V::Fn(_) | V::Bi(_) | V::Null => return None,
                                };
                                staged[k].write(o);
                            }
                            // SAFETY: the first `n` entries were written.
                            let args = unsafe {
                                std::slice::from_raw_parts(staged.as_ptr().cast::<Object>(), n)
                            };
                            let r = self.leaf_pure_builtin(code, usize::from(pc), b, rc, args)?;
                            set!(at, owned.own(r)?);
                            continue;
                        }
                    };
                    // SAFETY: the class or the namespace holds the callee,
                    // and nothing here runs code that could release it.
                    let callee = unsafe { &*fp };
                    // SAFETY: GIL-serialized raw read of the code cell.
                    let ccode: &Rc<CodeObject> = unsafe { &*callee.code.as_ptr() };
                    // (A positional call binds no `**kwargs` dictionary.)
                    if !crate::code_is_pure_leaf(ccode)
                        || !Self::leaf_code_ok(ccode)
                        || n != crate::leaf_arity(ccode)
                        || ccode.has_varkeywords
                        || crate::recursion::current_depth() + usize::from(nest) + depth + 1
                            >= crate::recursion::recursion_limit()
                    {
                        return None;
                    }
                    let cext = crate::code_vm_ext(ccode)?;
                    // A tiny certified shape (a constant or field return,
                    // a field comparison) keeps its dedicated evaluator.
                    if !GETTER && cext.pure_leaf.load(std::sync::atomic::Ordering::Relaxed) < 3 {
                        // The callee runs as a frame of this evaluation:
                        // the caller's registers wait in its record, and
                        // the arguments (normalized already) become the
                        // callee's first registers.
                        let cplan = cext
                            .leaf_plan
                            .get_or_init(|| build(ccode, cext).map(Box::new))
                            .as_deref()?;
                        let mut cargs = [V::N; 8];
                        for (k, slot) in cargs.iter_mut().enumerate().take(n) {
                            let v = get!(first + k as u8);
                            if matches!(v, V::Fn(_) | V::Bi(_) | V::Null) {
                                return None;
                            }
                            *slot = v;
                        }
                        // SAFETY: `depth < NEST` (checked above).
                        let c = unsafe { callers.get_unchecked_mut(depth) }.as_mut_ptr();
                        // SAFETY: `c` is this frame's record; the saved
                        // registers are the caller plan's own, all below
                        // `REGS`.
                        unsafe {
                            std::ptr::addr_of_mut!((*c).ip).write(ip);
                            std::ptr::addr_of_mut!((*c).code).write(code);
                            std::ptr::addr_of_mut!((*c).ext).write(ext);
                            std::ptr::addr_of_mut!((*c).plan).write(plan);
                            std::ptr::addr_of_mut!((*c).f).write(f);
                            std::ptr::addr_of_mut!((*c).at).write(at);
                            std::ptr::copy_nonoverlapping(
                                regs.as_ptr(),
                                std::ptr::addr_of_mut!((*c).saved).cast(),
                                usize::from(plan.nregs),
                            );
                        }
                        for (k, &v) in cargs.iter().enumerate().take(n) {
                            regs[k].write(v);
                        }
                        depth += 1;
                        (code, ext, plan, f) = (&**ccode, cext, cplan, callee);
                        consts = &plan.consts;
                        stamps = ext.stamp_slots.get().map_or(&[], |s| &s[..]);
                        ops = &plan.ops;
                        ip = 0;
                        continue;
                    }
                    // Scalar arguments are staged as objects (no drop glue).
                    let mut staged = [const { std::mem::MaybeUninit::<Object>::uninit() }; 8];
                    let mut ptrs = [std::ptr::null::<Object>(); 8];
                    for k in 0..n {
                        let o = match get!(first + k as u8) {
                            V::R(p) => {
                                ptrs[k] = p;
                                continue;
                            }
                            V::I(i) => Object::Int(i),
                            V::F(x) => Object::Float(x),
                            V::B(b) => Object::Bool(b),
                            V::N => Object::None,
                            V::Fn(_) | V::Bi(_) | V::Null => return None,
                        };
                        ptrs[k] = staged[k].write(o);
                    }
                    match self.leaf_eval_nested(ccode, callee, &ptrs[..n], nest + 1)? {
                        // A constructor's stores land at once, so what it
                        // borrows from its fresh instance could move.
                        LeafRet::Borrowed(v) if !FRESH => set!(at, v),
                        LeafRet::Borrowed(v) => set!(at, owned.own(to_object(v)?)?),
                        LeafRet::Owned(r) => set!(at, owned.own(r)?),
                    }
                }
                Op::Return { src } => {
                    let v = get!(src);
                    if depth > 0 {
                        // Back to the caller's frame, the result in its
                        // register. A constructor's stores land at once,
                        // so what it borrows from its fresh instance could
                        // move: it holds its own reference.
                        let v = match v {
                            V::R(p) if FRESH && !owned.holds(p) => owned.own(to_object(v)?)?,
                            v => v,
                        };
                        depth -= 1;
                        // SAFETY: frame `depth`'s record was written at its
                        // call (its registers as far as its `nregs`).
                        let c = unsafe { &*callers.get_unchecked(depth).as_ptr() };
                        (ip, code, ext, plan, f) = (c.ip, c.code, c.ext, c.plan, c.f);
                        consts = &plan.consts;
                        stamps = ext.stamp_slots.get().map_or(&[], |s| &s[..]);
                        ops = &plan.ops;
                        // SAFETY: as above.
                        unsafe {
                            std::ptr::copy_nonoverlapping(
                                c.saved.as_ptr(),
                                regs.as_mut_ptr(),
                                usize::from(plan.nregs),
                            );
                        }
                        set!(c.at, v);
                        continue;
                    }
                    return self.plan_finish::<EFFECT>(code, ext, plan, &owned, &mut pend, v);
                }
                Op::StoreAttr {
                    recv,
                    val,
                    pc,
                    name,
                } => {
                    if !EFFECT {
                        return None;
                    }
                    let V::R(rp) = get!(recv) else {
                        return None;
                    };
                    // SAFETY: as `norm`.
                    let recv = unsafe { &*rp };
                    if FRESH && args.first().is_some_and(|&a| std::ptr::eq(a, rp)) {
                        let Object::Instance(inst) = recv else {
                            return None;
                        };
                        let value = std::mem::ManuallyDrop::new(to_object(get!(val))?);
                        // On `true` the value moved into the instance.
                        if !Self::core_store_attr(
                            code,
                            inst,
                            usize::from(pc),
                            u32::from(name),
                            &value,
                        ) {
                            drop(std::mem::ManuallyDrop::into_inner(value));
                            return None;
                        }
                        continue;
                    }
                    plan_store(args, &mut owned, &mut pend, rp, get!(val), pc, name)?;
                }
            }
        }
    }
}

/// `a is b` on two leaf values.
#[inline(always)]
pub(crate) fn is_same(a: V, b: V) -> bool {
    match (a, b) {
        (V::N, V::N) => true,
        (V::B(x), V::B(y)) => x == y,
        (V::I(x), V::I(y)) => Object::Int(x).is_same(&Object::Int(y)),
        (V::F(x), V::F(y)) => Object::Float(x).is_same(&Object::Float(y)),
        // SAFETY: as `norm`.
        (V::R(p), V::R(q)) => unsafe { (*p).is_same(&*q) },
        _ => false,
    }
}

/// A unary operator on a scalar; `None` for any other operand.
#[inline(always)]
pub(crate) fn unary(v: V, kind: u8) -> Option<V> {
    Some(match kind {
        k if k == UnaryKind::Not as u8 => V::B(!truth(v)?),
        k if k == UnaryKind::Neg as u8 => match v {
            V::I(i) => V::I(i.checked_neg()?),
            V::F(x) => V::F(-x),
            _ => return None,
        },
        k if k == UnaryKind::Pos as u8 => match v {
            V::I(_) | V::F(_) => v,
            _ => return None,
        },
        k if k == UnaryKind::Invert as u8 => match v {
            V::I(i) => V::I(!i),
            _ => return None,
        },
        _ => return None,
    })
}

/// A binary operator on two scalars; `None` for any other operands or a
/// result the interpreter must produce (an overflow, a negative operand
/// of a floor division).
#[inline(always)]
pub(crate) fn binary(a: V, b: V, kind: u8) -> Option<V> {
    Some(match (a, b) {
        (V::I(a), V::I(b)) => V::I(match kind {
            k if k == BinOpKind::Add as u8 => a.checked_add(b)?,
            k if k == BinOpKind::Sub as u8 => a.checked_sub(b)?,
            k if k == BinOpKind::Mult as u8 => a.checked_mul(b)?,
            k if k == BinOpKind::BitAnd as u8 => a & b,
            k if k == BinOpKind::BitOr as u8 => a | b,
            k if k == BinOpKind::BitXor as u8 => a ^ b,
            k if k == BinOpKind::RShift as u8 && (0..64).contains(&b) => a >> b,
            k if k == BinOpKind::LShift as u8 && (0..63).contains(&b) && ((a << b) >> b) == a => {
                a << b
            }
            k if k == BinOpKind::FloorDiv as u8 && b > 0 && a >= 0 => a / b,
            k if k == BinOpKind::Mod as u8 && b > 0 && a >= 0 => a % b,
            _ => return None,
        }),
        (x, y) => {
            let (x, y) = match (x, y) {
                (V::F(x), V::F(y)) => (x, y),
                (V::I(x), V::F(y)) => (x as f64, y),
                (V::F(x), V::I(y)) => (x, y as f64),
                _ => return None,
            };
            // SAFETY: as the core loop's `BINARY_OP` arm (the compiler
            // emits only valid kinds).
            let kind: BinOpKind = unsafe { std::mem::transmute(kind) };
            match Interpreter::leaf_float_op(x, y, kind)? {
                Object::Float(x) => V::F(x),
                _ => return None,
            }
        }
    })
}

/// `a[b]` that can't run code or raise: a list's or tuple's item at an
/// int index (borrowed: a leaf mutates no container), a dict's value for
/// a `str` or `int` key (the core loop's probe, which compares no other
/// key type), or an ASCII string's character (held by `owned`). Anything
/// else, a miss included, declines.
#[inline]
fn subscr(owned: &mut Owned, a: V, b: V) -> Option<V> {
    let V::R(c) = a else {
        return None;
    };
    let index = |i: i64, n: usize| {
        let n = n as i64;
        let i = if i < 0 { i + n } else { i };
        (0..n).contains(&i).then_some(i as usize)
    };
    // SAFETY: as `norm`; the borrows end before anything could change the
    // containers (no Python runs during an evaluation).
    match (unsafe { &*c }, b) {
        (Object::List(l), V::I(i)) => {
            let xs = unsafe { l.peek() }?;
            Some(norm(&xs[index(i, xs.len())?]))
        }
        (Object::Tuple(t), V::I(i)) => Some(norm(&t[index(i, t.len())?])),
        (Object::Str(s), V::I(i)) => {
            if crate::object::str_char_len(s) != s.len() {
                return None;
            }
            owned.own(Object::from_char(s.as_bytes()[index(i, s.len())?] as char))
        }
        (Object::Dict(d), V::I(_) | V::R(_)) => {
            let int;
            let key = match b {
                V::I(i) => {
                    int = Object::Int(i);
                    &int
                }
                // SAFETY: as above.
                V::R(k) if matches!(unsafe { &*k }, Object::Str(_)) => unsafe { &*k },
                _ => return None,
            };
            let probe = crate::object::LeafProbe::new(key)?;
            let v = unsafe { d.peek() }?.get(&probe)?;
            Some(norm(v))
        }
        _ => None,
    }
}

/// `BUILD_LIST 0` / `BUILD_MAP 0`: a new empty list or dict, tracked like
/// the core loop's and held by the owned scratch. An allocation that would
/// trigger a collection is left to the core loop.
#[inline(always)]
fn new_container(owned: &mut Owned, dict: bool) -> Option<V> {
    if crate::stdlib::tracemalloc_real::is_tracking()
        || crate::stdlib::testinternalcapi_mod::reftrace_print_active()
        || crate::gc_trace::auto_collect_due()
    {
        return None;
    }
    let obj = if dict {
        Object::Dict(Rc::new(crate::sync::RefCell::new(
            crate::object::DictData::with_capacity_and_hasher(0, crate::fasthash::FxBuildHasher),
        )))
    } else {
        Object::new_list(Vec::new())
    };
    crate::gc_trace::track(&obj);
    owned.own(obj)
}

/// An effect leaf's `recv.name = val` (at store `pc`), buffered until the
/// return commits it. An argument receiver is used in place; any other (a
/// field's value) is held by the owned scratch.
#[inline(always)]
fn plan_store(
    args: &[*const Object],
    owned: &mut Owned,
    pend: &mut Pending,
    rp: *const Object,
    val: V,
    pc: u16,
    name: u16,
) -> Option<()> {
    // SAFETY: as `norm`.
    let recv = unsafe { &*rp };
    if !matches!(recv, Object::Instance(_)) || pend.n == PENDING {
        return None;
    }
    let value = to_object(val)?;
    let rp = if args.iter().any(|&a| std::ptr::eq(a, rp)) {
        rp
    } else {
        let V::R(held) = owned.own(crate::clone_hot(recv))? else {
            return None;
        };
        held
    };
    pend.buf[pend.n].write((rp, u32::from(pc), u32::from(name), value));
    pend.n += 1;
    Some(())
}

impl Interpreter {
    /// `LOAD_GLOBAL` at `pc` of `code` in `f`'s namespaces, stamp-validated
    /// as the core loop's arm.
    #[inline(always)]
    fn plan_global(
        &self,
        code: &CodeObject,
        stamps: &[crate::StampSlot],
        f: &crate::object::PyFunction,
        pc: u16,
    ) -> Option<V> {
        self.plan_global_at(code, stamps.get(usize::from(pc))?, f, pc)
    }

    /// [`Self::plan_global`] with the site's stamp slot in hand.
    #[inline(always)]
    fn plan_global_at(
        &self,
        code: &CodeObject,
        slot: &crate::StampSlot,
        f: &crate::object::PyFunction,
        pc: u16,
    ) -> Option<V> {
        use weavepy_compiler::InlineCache as IC;
        let pc = usize::from(pc);
        let (gdict, bdict) = (f.globals.as_ptr(), f.builtins.as_ptr());
        let gid = crate::specialize::rc_id(&f.globals);
        // SAFETY (raw dict reads): nothing runs code here.
        let g_stamp = unsafe { (*gdict).mutation_stamp() };
        let hit = match code.caches.get(pc as u32) {
            IC::LoadGlobalModule {
                globals_id,
                key_idx,
            } if globals_id == gid && slot.get() == [gid, g_stamp, 0] => unsafe {
                (*gdict).get_index(key_idx as usize)
            },
            IC::LoadGlobalBuiltin {
                builtins_id,
                key_idx,
            } if builtins_id == crate::specialize::rc_id(&f.builtins)
                && !self.globals_missing_any.get()
                && slot.get() == [gid, g_stamp, unsafe { (*bdict).mutation_stamp() }] =>
            unsafe { (*bdict).get_index(key_idx as usize) },
            _ => return None,
        };
        Some(norm(hit?.1))
    }

    /// `src.name` (the `LOAD_ATTR` at `pc`): the latest buffered store to
    /// it, a cached instance field, or what the site resolves; a value the
    /// read hands back owned is held by the owned scratch.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn plan_attr<const GETTER: bool, const EFFECT: bool>(
        &self,
        code: &CodeObject,
        ext: &CodeConstObjects,
        stamps: &[crate::StampSlot],
        owned: &mut Owned,
        pend: &Pending,
        src: V,
        pc: u16,
        name: u16,
    ) -> Option<V> {
        let V::R(p) = src else {
            return None;
        };
        let (pc, name) = (u32::from(pc), u32::from(name));
        // SAFETY: as `norm`.
        let recv = unsafe { &*p };
        if EFFECT && pend.n > 0 {
            // A buffered store to this attribute, the latest.
            let hit = (0..pend.n)
                .rev()
                .map(|k| pend.get(k))
                // SAFETY: stable receivers (see `Store`).
                .find(|(r, _, n, _)| *n == name && unsafe { (**r).is_same(recv) });
            if let Some((_, _, _, v)) = hit {
                return Some(norm(v));
            }
        }
        match recv {
            Object::Instance(inst) => {
                if GETTER && inst.cls_raw().native_kind.get() != 0 {
                    return None;
                }
                // SAFETY: the receiver remains rooted by an argument or
                // owned scratch; no Python runs.
                match unsafe { Self::leaf_cached_instance_field(ext, code, inst, pc, name) } {
                    Some(v) => Some(norm(v)),
                    // A stale or absent site cache resolves through the
                    // site's own entries.
                    None => owned.own(Self::leaf_attr_resolve_site(code, inst, recv, pc, name)?),
                }
            }
            Object::Type(cls) => {
                if GETTER && !Self::plain_metaclass(cls) {
                    return None;
                }
                match stamps
                    .get(pc as usize)
                    .and_then(|s| crate::class_attr_hit(s, cls))
                {
                    Some(v) => owned.own(v),
                    None => {
                        let v = Self::leaf_load_type_attr(code, cls, pc, name)?;
                        // A scalar is a plain class value (see
                        // `leaf_load_type_attr`): remembered for the next
                        // read, which native code makes in line.
                        crate::class_attr_fill(code, cls, pc as usize, &v);
                        owned.own(v)
                    }
                }
            }
            Object::Module(module) => {
                if GETTER
                    && (crate::object::module_class(module).is_some()
                        || code.names.get(name as usize)?.starts_with("__"))
                {
                    return None;
                }
                owned.own(Self::leaf_load_attr_recv(code, recv, pc, name)?)
            }
            _ => None,
        }
    }

    /// The method-form load of `src.name` off the site's slot: the function
    /// and the receiver, or a class's function and the empty self slot (as
    /// the core loop's arm).
    #[inline(always)]
    fn plan_method(&self, code: &CodeObject, src: V, pc: u16, name: u16) -> Option<(V, V)> {
        let ms = crate::code_method_slot(code, u32::from(pc))?;
        self.plan_method_at(code, ms, src, name)
    }

    /// [`Self::plan_method`] with the site's method slot in hand.
    #[inline(always)]
    fn plan_method_at(
        &self,
        code: &CodeObject,
        ms: &crate::MethodSlot,
        src: V,
        name: u16,
    ) -> Option<(V, V)> {
        let V::R(p) = src else {
            return None;
        };
        // SAFETY: as `norm`.
        match unsafe { &*p } {
            Object::Instance(inst) => {
                let cls = inst.cls_raw();
                if !Self::default_getattribute(cls) {
                    return None;
                }
                let fp = ms.peek_fn(cls.attr_version.get())?;
                // The instance's attributes must not shadow the method.
                if crate::inst_may_shadow(inst, code, u32::from(name)) {
                    return None;
                }
                Some((V::Fn(fp), V::R(p)))
            }
            Object::Type(cls) => Some((V::Fn(ms.peek_unbound(cls.attr_version.get())?), V::Null)),
            recv => {
                let b = self.leaf_builtin_method_ptr(ms, recv, code, name)?;
                Some((V::Bi(b), V::R(p)))
            }
        }
    }

    /// The evaluation's return of `v`: borrowed when the evaluation's
    /// scratch doesn't hold it (a pure body stores nothing, so it outlives
    /// the evaluation), otherwise owned, with an effect leaf's buffered
    /// stores committed first. The latest store to each attribute is the
    /// one that lands; every one must go through before any does (a decline
    /// touched nothing, and the ordinary call runs the body instead). A
    /// lone store is its own check: it declines whole.
    #[inline(always)]
    fn plan_finish<const EFFECT: bool>(
        &self,
        code: &CodeObject,
        ext: &CodeConstObjects,
        plan: &LeafPlan,
        owned: &Owned,
        pend: &mut Pending,
        v: V,
    ) -> Option<LeafRet> {
        if !EFFECT && !matches!(v, V::R(p) if owned.holds(p)) {
            return Some(LeafRet::Borrowed(v));
        }
        let r = to_object(v)?;
        if EFFECT && pend.n > 0 {
            let unique = plan.unique_stores;
            if pend.n > 1 {
                // With one receiver, appends of new attributes land in
                // order: `cursor` is its split length once the earlier ones
                // have. A store the shortcut can't vouch for may move the
                // layout: the rest check in full.
                let mut cursor = None;
                let mut split_ok = unique;
                for k in 0..pend.n {
                    if !unique && !pend.latest(k) {
                        continue;
                    }
                    let (rp, spc, name, _) = pend.get(k);
                    // SAFETY: stable receivers (see `Store`).
                    let Object::Instance(inst) = (unsafe { &**rp }) else {
                        return None;
                    };
                    let (spc, name) = (*spc as usize, *name);
                    let ready = if split_ok {
                        Self::core_store_attr_ready_split(ext, inst, spc, &mut cursor)
                    } else {
                        None
                    };
                    split_ok &= ready.is_some();
                    if !ready.unwrap_or_else(|| Self::core_store_attr_ready(code, inst, spc, name))
                    {
                        return None;
                    }
                }
            }
            let n = pend.n;
            for k in 0..n {
                if !unique && !pend.latest(k) {
                    continue;
                }
                let (rp, spc, name, value) = pend.get(k);
                // SAFETY: stable receivers (see `Store`).
                let Object::Instance(inst) = (unsafe { &**rp }) else {
                    return None;
                };
                // On `true` the value moved into the dict.
                if Self::core_store_attr(code, inst, *spc as usize, *name, value) {
                    pend.moved |= 1 << k;
                } else if n == 1 {
                    // The lone store declined, untouched.
                    return None;
                } else {
                    debug_assert!(false, "a ready store declined");
                }
            }
        }
        Some(LeafRet::Owned(r))
    }
}
