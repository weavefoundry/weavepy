//! Native code for hot frames: a baseline compiler for the core loop.
//!
//! The core loop ([`Interpreter::leaf_core`]) decodes and dispatches one
//! instruction at a time, and every operand it handles makes a round trip
//! through the frame's operand stack. A code object that keeps running is
//! compiled here into one function over its instructions, entered at the
//! start of any basic block. The function keeps a *virtual* operand stack
//! while it lowers a block: a local or constant load is just a note of
//! where the value lives, scalar arithmetic and compares produce unboxed
//! values in registers, and a branch tests them directly, so a typical
//! `i = i + 1` or `if a < b:` touches neither the stack nor a reference
//! count. Values reach the frame's stack (copied, with their references
//! taken) only where something else must see them: at the end of a block,
//! and before any instruction the native code leaves to the core loop.
//!
//! What doesn't run in line runs through helpers (`h_*`) that do what the
//! core loop's arm for the instruction does, on the same caches: global
//! and attribute loads (with in-line caches of their own for a global's
//! value and a `__slots__` member), method loads, attribute stores,
//! builds, subscripts, and the calls the core loop runs in place. A call
//! of a Python function, and an inline activation's return, switch the
//! running activation as the core loop's arms do and return `RELOAD`.
//!
//! An instruction the native code doesn't take, and any shape neither the
//! in-line code nor a helper settles (an overflow, a Python dunder, a
//! cache miss), returns to the core loop with the frame exactly as the
//! loop would have it before that instruction: the loop runs it, and
//! enters the native code again at the next block start it reaches. The
//! core loop's fused shapes (a local receiver's method call or attribute
//! store) start at their `LOAD_FAST`, so the native code takes those at
//! it too.
//!
//! Compiling costs far more than interpreting a few iterations (some 30 to
//! 50 microseconds per instruction), and every hand-over to the core loop
//! costs a round trip, so a code object compiles once its interpreting has
//! cost about what compiling would (see `Slot::warm`), and only if the
//! loop that got hot, or for a call of loop-free code its body, runs
//! mostly in native code (see `worth_compiling`). Code a tier-2 loop may
//! still take waits for tier 2 to settle first.
//!
//! The code reads and writes [`Object`]s in place, through the layout
//! `repr(u8)` fixes: a tag byte at offset 0, a `bool` payload at offset 1,
//! and a word payload at offset 8. Instance fields are read through the
//! object layout tier 2 measures (see `tier2::obj_layout`).

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

use cranelift_codegen::ir::{
    condcodes::{FloatCC, IntCC},
    types, AbiParam, Block, InstBuilder, JumpTableData, MemFlags, SigRef, Signature, Type, Value,
};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};
use weavepy_compiler::{BinOpKind, CodeObject, CompareKind, OpCode, COMPARE_OP_TO_BOOL_FLAG};

use crate::error::RuntimeError;
use crate::object::{Object, PyIterator};
use crate::sync::Rc;
use crate::{body_pcs, CodeConstObjects, FieldSlot, Interpreter};

/// The core loop runs the instruction at `pc` itself.
pub(crate) const INTERP: u32 = 0;
/// A release queued a finalizer: the core loop stops (`LeafStop::Marked`)
/// with `pc` past the releasing instruction.
pub(crate) const MARKED: u32 = 1;
/// A call raised `State::err`, with `pc` past it.
pub(crate) const RAISED: u32 = 2;
/// A call or return switched the running activation (see
/// `Interpreter::core_call`), the frame synced first: the core loop
/// reloads.
pub(crate) const RELOAD: u32 = 4;
/// A directly called activation returned (see [`direct_call`]), its state
/// synced to the return with the value atop its stack: the caller
/// finishes the return.
const DIRECT_RET: u32 = 5;

/// The heat per instruction of a code object at which it compiles (see
/// `Slot::warm`; `WEAVEPY_FRAME_JIT_HOT` overrides it, a tuning aid).
fn hot() -> u32 {
    static HOT: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *HOT.get_or_init(|| {
        std::env::var("WEAVEPY_FRAME_JIT_HOT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2000)
    })
}

/// The IR size above which a body is lowered lean (see `compile_with`;
/// `WEAVEPY_FRAME_JIT_LEAN` overrides it, a tuning aid).
fn lean_above() -> usize {
    static LEAN: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *LEAN.get_or_init(|| {
        std::env::var("WEAVEPY_FRAME_JIT_LEAN")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3000)
    })
}

/// How many entries at a pc may get nowhere before the core loop stops
/// entering there.
const MAX_FAILS: u8 = 32;

/// The longest code compiled.
const MAX_INSTRUCTIONS: usize = 4096;

/// The running activation's state, as the core loop holds it: the native
/// code reads it on entry and writes it back on exit.
#[repr(C)]
pub(crate) struct State {
    pub(crate) locals: *mut Object,
    pub(crate) stack: *mut Object,
    pub(crate) len: usize,
    pub(crate) cap: usize,
    pub(crate) pc: usize,
    pub(crate) last: usize,
    /// The interpreter's GIL countdown (the back edges' eval breaker).
    pub(crate) countdown: *mut u32,
    /// The loop generation the burst started under.
    pub(crate) snap_gen: u64,
    /// The running thread's queued-finalizer flag.
    pub(crate) maybe_dead: *const Cell<bool>,
    pub(crate) interp: *const Interpreter,
    /// A helper's scalar result.
    pub(crate) out: u64,
    /// The thread's recursion-depth cell.
    pub(crate) depth_cell: *const Cell<usize>,
    /// What a `RAISED` exit raised.
    pub(crate) err: Option<RuntimeError>,
    /// The running frame, for its namespaces and cells (its stack and
    /// locals are the fields above).
    pub(crate) frame: *mut crate::Frame,
    /// The core loop's activation switch, for calls and returns (null in
    /// a generator's fast step).
    pub(crate) sw: *mut crate::CoreSwitch,
    /// In a generator's fast step, the fast steps it is nested in (see
    /// `gen_fast`).
    pub(crate) gen_depth: u8,
    /// A direct call's callee (see [`direct_call`]): its `RETURN_VALUE`
    /// hands the return straight back (`DIRECT_RET`) for the caller to
    /// finish, without [`h_return`].
    pub(crate) direct: bool,
}

const S_LOCALS: i32 = 0;
const S_STACK: i32 = 8;
const S_LEN: i32 = 16;
const S_CAP: i32 = 24;
const S_PC: i32 = 32;
const S_LAST: i32 = 40;
const S_COUNTDOWN: i32 = 48;
const S_SNAP: i32 = 56;
const S_DEAD: i32 = 64;
const S_OUT: i32 = 80;
const S_INTERP: i32 = std::mem::offset_of!(State, interp) as i32;
const S_SW: i32 = std::mem::offset_of!(State, sw) as i32;
const S_FRAME: i32 = std::mem::offset_of!(State, frame) as i32;
const S_DIRECT: i32 = std::mem::offset_of!(State, direct) as i32;
const _: () = {
    assert!(std::mem::offset_of!(State, locals) == S_LOCALS as usize);
    assert!(std::mem::offset_of!(State, stack) == S_STACK as usize);
    assert!(std::mem::offset_of!(State, len) == S_LEN as usize);
    assert!(std::mem::offset_of!(State, cap) == S_CAP as usize);
    assert!(std::mem::offset_of!(State, pc) == S_PC as usize);
    assert!(std::mem::offset_of!(State, last) == S_LAST as usize);
    assert!(std::mem::offset_of!(State, countdown) == S_COUNTDOWN as usize);
    assert!(std::mem::offset_of!(State, snap_gen) == S_SNAP as usize);
    assert!(std::mem::offset_of!(State, maybe_dead) == S_DEAD as usize);
    assert!(std::mem::offset_of!(State, out) == S_OUT as usize);
};

type NativeFn = unsafe extern "C" fn(*mut State) -> u32;

/// A code object's native code.
pub(crate) struct Native {
    func: NativeFn,
    /// The locals vector's length the code was compiled for.
    nlocals: usize,
    /// The pcs worth entering at: block starts whose first instruction
    /// the native code runs, until entries there keep getting nowhere
    /// (see `fails`).
    entries: Box<[AtomicBool]>,
    /// How many entries at each pc handed its first instruction straight
    /// back (an iterator or operand its helpers never take).
    fails: Box<[AtomicU8]>,
    /// The code's name, opcodes, stack depths and the stack capacity it
    /// needs (for `WEAVEPY_FRAME_JIT_STATS`).
    name: String,
    ops: Box<[OpCode]>,
    depths: Box<[i64]>,
    need: usize,
    /// The `LOAD_GLOBAL` and `LOAD_ATTR` sites' caches, which the code
    /// addresses.
    #[allow(clippy::vec_box)]
    _globals: Vec<Box<GlobalCache>>,
    #[allow(clippy::vec_box)]
    _slots: Vec<Box<SlotCache>>,
    /// The method-load sites' in-line caches.
    #[allow(clippy::vec_box)]
    _methods: Vec<Box<MethodCache>>,
    /// The `CALL` sites' direct-call caches.
    #[allow(clippy::vec_box)]
    _direct: Vec<Box<DirectSite>>,
    /// The builtins the method kernels' calls address.
    _held: Vec<Rc<crate::object::BuiltinFn>>,
    #[allow(clippy::vec_box)]
    _mods: Vec<Box<ModCache>>,
}

// SAFETY: the code only touches the state it is handed; it is only
// entered with the GIL held (never in the free-threaded build).
unsafe impl Send for Native {}
// SAFETY: as above.
unsafe impl Sync for Native {}

impl Native {
    /// The stack capacity the code needs.
    #[inline(always)]
    pub(crate) fn need(&self) -> usize {
        self.need
    }

    /// Whether entering at `pc` gets anywhere.
    #[inline(always)]
    pub(crate) fn enters_at(&self, pc: usize) -> bool {
        self.entries
            .get(pc)
            .is_some_and(|e| e.load(Ordering::Relaxed))
    }

    /// An entry at `pc` got nowhere: after a few, the core loop stops
    /// entering there.
    #[cold]
    #[inline(never)]
    fn note_fail(&self, pc: usize) {
        if let (Some(f), Some(e)) = (self.fails.get(pc), self.entries.get(pc)) {
            if f.fetch_add(1, Ordering::Relaxed) >= MAX_FAILS {
                e.store(false, Ordering::Relaxed);
            }
        }
    }

    /// [`Self::run`] for a direct call's callee, entered at `from` (its
    /// start, unless a prologue the core loop ran came first): the
    /// bookkeeping only for a run that didn't return.
    #[inline(always)]
    pub(crate) unsafe fn run_direct(&self, st: &mut State, from: usize) -> u32 {
        let (len, cap) = (st.len, st.cap);
        // SAFETY: the caller's contract.
        let status = unsafe { (self.func)(st) };
        if status != DIRECT_RET || stats::enabled() {
            self.ran(st, from, len, cap, usize::MAX, status);
        }
        status
    }

    /// [`Self::run`]'s bookkeeping for a run from `from` (with the stack
    /// `len` deep, `cap` its capacity, and `last` the last instruction).
    #[cold]
    #[inline(never)]
    fn ran(&self, st: &State, from: usize, len: usize, cap: usize, last: usize, status: u32) {
        if status == INTERP && st.pc == from && st.last == last {
            self.note_fail(from);
        }
        if stats::enabled() {
            let bail = if status != INTERP || st.pc != from {
                ""
            } else if cap < self.need {
                "<cap>"
            } else if self.depths.get(from).is_some_and(|&d| d != len as i64) {
                "<depth>"
            } else {
                ""
            };
            stats::note(&self.name, &self.ops, from, st.pc, st.last, st.sw, status, bail);
        }
    }

    /// Run from `st.pc` until an instruction the core loop must run.
    ///
    /// # Safety
    ///
    /// `st` is the running activation of the code this was compiled
    /// from, as the core loop holds it, with `nlocals` locals.
    #[inline(always)]
    pub(crate) unsafe fn run(&self, st: &mut State) -> u32 {
        let (from, len, cap, last) = (st.pc, st.len, st.cap, st.last);
        // SAFETY: the caller's contract.
        let status = unsafe { (self.func)(st) };
        if status == INTERP && st.pc == from && st.last == last {
            self.note_fail(from);
        }
        if stats::enabled() {
            // Why an entry got nowhere: a stack too small for the code, a
            // depth the block doesn't start at, or the instruction itself.
            let bail = if status != INTERP || st.pc != from {
                ""
            } else if cap < self.need {
                "<cap>"
            } else if self.depths.get(from).is_some_and(|&d| d != len as i64) {
                "<depth>"
            } else {
                ""
            };
            stats::note(&self.name, &self.ops, from, st.pc, st.last, st.sw, status, bail);
        }
        status
    }
}

/// A code object's compilation state, kept in its extension.
#[derive(Default)]
pub(crate) struct Slot {
    native: std::sync::OnceLock<Option<Box<Native>>>,
    heat: AtomicU32,
    /// An activation's heat (see `warm`), computed once: `0` until then.
    /// (Its division showed in every call's profile.)
    call_heat: AtomicU32,
    /// How many times the code got hot while start-up or an import ran
    /// (see `try_compile`).
    deferred: AtomicU32,
}

impl Slot {
    /// The native code for an activation with `nlocals` locals, if
    /// compiled.
    #[inline(always)]
    pub(crate) fn get(&self, nlocals: usize) -> Option<&Native> {
        match self.native.get() {
            Some(Some(n)) if n.nlocals == nlocals => Some(n),
            _ => None,
        }
    }

    /// Count one activation, back edge or generator step of code without
    /// native code, compiling it once it is hot.
    #[inline(always)]
    pub(crate) fn warm(&self, code: &CodeObject, ext: &CodeConstObjects, nlocals: usize, at: Heat) {
        if self.native.get().is_some() {
            return;
        }
        // The heat estimates the interpreting so far: a back edge counts its
        // loop's instructions, an activation or generator step a share of
        // the code's (most of a call's instructions save less natively
        // than a loop's). Compiling costs about the same per instruction
        // of the code, so the code compiles once the heat reaches
        // [`hot`] times its length: about when interpreting on would have
        // cost what compiling does.
        let n = code.instructions.len() as u32;
        let add = match at {
            Heat::BackEdge(pc) => code.instructions.get(pc).map_or(1, |i| i.arg.max(1)),
            Heat::Call | Heat::Step => match self.call_heat.load(Ordering::Relaxed) {
                0 => {
                    let add = (n / tuning().call_div).max(1);
                    self.call_heat.store(add, Ordering::Relaxed);
                    add
                }
                add => add,
            },
        };
        // (A racing thread losing a count is harmless.)
        let h = self.heat.load(Ordering::Relaxed).saturating_add(add);
        self.heat.store(h, Ordering::Relaxed);
        if h >= hot().saturating_mul(n) {
            self.heat.store(0, Ordering::Relaxed);
            self.try_compile(code, ext, nlocals, at);
        }
    }

    /// Count a compiled caller's call into an activation of this code that
    /// had to switch to the core loop for want of a native body: the round
    /// trip costs about what interpreting the whole body does, so it
    /// counts the code's length (a call the core loop makes counts a
    /// tenth of it), and a callee of compiled code compiles sooner.
    #[inline]
    pub(crate) fn warm_switched(&self, code: &CodeObject, ext: &CodeConstObjects, nlocals: usize) {
        if self.native.get().is_some() {
            return;
        }
        let n = code.instructions.len() as u32;
        let h = self.heat.load(Ordering::Relaxed).saturating_add(n);
        self.heat.store(h, Ordering::Relaxed);
        if h >= hot().saturating_mul(n) {
            self.heat.store(0, Ordering::Relaxed);
            self.try_compile(code, ext, nlocals, Heat::Call);
        }
    }

    #[cold]
    #[inline(never)]
    fn try_compile(&self, code: &CodeObject, ext: &CodeConstObjects, nlocals: usize, at: Heat) {
        if !enabled() {
            let _ = self.native.set(None);
            return;
        }
        // Code that start-up or an import runs is mostly run once (a
        // regular expression compiler's loops, a module's set-up), and a
        // compile costs as much as thousands of interpreted iterations:
        // there, code compiles only after sustained work, as tier 2's
        // budget has it.
        let d = self.deferred.load(Ordering::Relaxed).saturating_add(1);
        if !crate::tier2::frame_compile_allowed(d.saturating_mul(hot()), hot()) {
            self.deferred.store(d, Ordering::Relaxed);
            if stats::enabled() {
                eprintln!("frame jit: {} deferred ({d})", code.qualname);
            }
            return;
        }
        // Loops the tier-2 compiler may still take keep their back edges'
        // consultation in the core loop (a generator body the fast steps
        // run never reaches it).
        let hint = &code.jit_hint;
        if !(matches!(at, Heat::Step)
            || hint.is_not_jitable()
            || crate::tier2::jit_off_for_process()
            || hint.loop_free(code)
            || hint.is_backedge_quiet())
        {
            if stats::enabled() {
                eprintln!("frame jit: {} left to tier 2", code.qualname);
            }
            return;
        }
        if !name_admitted(&code.qualname) {
            let _ = self.native.set(None);
            return;
        }
        if !worth_compiling(code, ext, at) {
            // (Another loop may yet get hot.)
            return;
        }
        let t0 = stats::enabled().then(std::time::Instant::now);
        let n = self
            .native
            .get_or_init(|| compile(code, ext, nlocals).map(Box::new));
        if let Some(t0) = t0 {
            eprintln!(
                "frame jit: {} compiled: {} ({} instructions, {:?})",
                code.qualname,
                n.is_some(),
                code.instructions.len(),
                t0.elapsed()
            );
        }
    }
}

/// What warmed a code object.
#[derive(Clone, Copy)]
pub(crate) enum Heat {
    /// An activation.
    Call,
    /// The back edge at this pc.
    BackEdge(usize),
    /// A generator's fast step (see `gen_fast`).
    Step,
}

/// Whether `code` runs mostly in native code where it got hot: the loop
/// whose back edge did (any loop, for a generator step), or, for a call
/// of loop-free code, the body. Each instruction the core loop runs costs
/// a round trip out of the native code and back, so a loop of calls gains
/// little from compiling, while a body pays off once its native run saves
/// what its exits cost.
fn worth_compiling(code: &CodeObject, ext: &CodeConstObjects, at: Heat) -> bool {
    use weavepy_compiler::OpCode;
    let ins = &code.instructions;
    // A method call on a local runs through a helper without leaving, when
    // its callee runs in place.
    let mut helped = vec![false; ins.len()];
    // So does a call of a leaf builtin (`isinstance(x, C)`, `len(xs)`,
    // `math.floor(x)`) the site has settled.
    for (pc, h) in helped.iter_mut().enumerate() {
        if ins[pc].op == OpCode::Call && crate::call_site_in_place(ext, pc) {
            *h = true;
        }
    }
    for pc in 0..ins.len().saturating_sub(1) {
        if ins[pc].op == OpCode::LoadFast
            && ins[pc + 1].op == OpCode::LoadMethodAttr
            && crate::method_site_in_place(ext, pc + 1)
        {
            helped[pc + 1] = true;
            if let Some(call) =
                (pc + 2..ins.len().min(pc + 12)).find(|&k| ins[k].op == OpCode::Call)
            {
                helped[call] = true;
            }
        }
    }
    // So do a `with` statement's `__enter__` and `__exit__` calls when
    // its special-method sites found methods that run in place: the
    // enter call follows its `LOAD_SPECIAL`, and a normal exit's
    // `CALL 3` (over three `None`s) pairs with the innermost open exit
    // site.
    {
        use weavepy_compiler::bytecode::{SPECIAL_ENTER, SPECIAL_EXIT};
        let mut open = Vec::new();
        for pc in 0..ins.len() {
            let i = ins[pc];
            match i.op {
                OpCode::LoadSpecial if i.arg == SPECIAL_EXIT => open.push(pc),
                OpCode::LoadSpecial if i.arg == SPECIAL_ENTER => {
                    if ins
                        .get(pc + 1)
                        .is_some_and(|c| c.op == OpCode::Call && c.arg == 0)
                        && crate::method_site_in_place(ext, pc)
                    {
                        helped[pc + 1] = true;
                    }
                }
                OpCode::Call
                    if i.arg == 3
                        && pc >= 3
                        && ins[pc - 3..pc].iter().all(|c| {
                            c.op == OpCode::LoadConst
                                && matches!(
                                    code.constants.get(c.arg as usize),
                                    Some(weavepy_compiler::Constant::None)
                                )
                        }) =>
                {
                    if open
                        .pop()
                        .is_some_and(|x| crate::method_site_in_place(ext, x))
                    {
                        helped[pc] = true;
                    }
                }
                _ => {}
            }
        }
    }
    // A generator expression's creation (`MAKE_FUNCTION`, the iterable,
    // `GET_ITER`, `CALL 0`) runs in place through the call helper.
    for pc in 1..ins.len() {
        if ins[pc].op == OpCode::Call && ins[pc].arg == 0 && ins[pc - 1].op == OpCode::GetIter {
            let made = (pc.saturating_sub(16)..pc)
                .rev()
                .find(|&k| ins[k].op == OpCode::MakeFunction)
                .filter(|&k| k > 0 && ins[k - 1].op == OpCode::LoadConst)
                .and_then(|k| code.constants.get(ins[k - 1].arg as usize));
            if matches!(made, Some(weavepy_compiler::Constant::Code(c)) if c.is_generator) {
                helped[pc] = true;
            }
        }
    }
    // The fallback of an inlined `tuple(...)`, `list(...)`, `all(...)` or
    // `any(...)` of a generator expression (`LOAD_COMMON_CONSTANT; IS_OP;
    // POP_JUMP_IF_FALSE` to the plain call, which the inlined loop jumps
    // over) runs only when the name is rebound: its calls don't count.
    let mut cold = vec![false; ins.len()];
    for pc in 0..ins.len().saturating_sub(2) {
        if ins[pc].op == OpCode::LoadCommonConstant
            && ins[pc + 1].op == OpCode::IsOp
            && ins[pc + 2].op == OpCode::PopJumpIfFalse
        {
            let from = pc + 3 + ins[pc + 2].arg as usize;
            if from > pc + 3 && from <= ins.len() && ins[from - 1].op == OpCode::JumpForward {
                let to = (from + ins[from - 1].arg as usize).min(ins.len());
                cold[from..to].iter_mut().for_each(|c| *c = true);
            }
        }
    }
    let t = tuning();
    // What running an instruction natively saves, in halves of an
    // in-line instruction's: one run in line saves its whole dispatch and
    // operand traffic, one run through a helper (as the core loop's arm
    // runs it) about half of that, and one handed to the core loop costs
    // a round trip.
    let gain = |k: usize| -> Option<usize> {
        let op = ins[k].op;
        // A call runs through its helpers (`call_op`), as the core loop's
        // arm runs it: a compiled callee is called directly, any other
        // activation switched to without a round trip through the loop.
        if helped[k] || matches!(op, OpCode::Call | OpCode::CallKw | OpCode::CallEx) {
            return Some(1);
        }
        if !native_at(code, k) {
            return None;
        }
        let in_line = match op {
            OpCode::LoadAttr => crate::field_site(ext, k),
            OpCode::LoadGlobal
            | OpCode::LoadMethodAttr
            | OpCode::StoreAttr
            | OpCode::BuildTuple
            | OpCode::BuildList
            | OpCode::BuildMap
            | OpCode::LoadDeref
            | OpCode::GetIter
            | OpCode::BinarySubscr
            | OpCode::BinarySlice
            | OpCode::StoreSubscr
            | OpCode::ListAppend
            | OpCode::SetAdd
            | OpCode::MapAdd
            | OpCode::UnpackSequence
            | OpCode::ContainsOp
            | OpCode::LoadClosure
            | OpCode::LoadClosureBorrow
            | OpCode::StoreDeref
            | OpCode::LoadCommonConstant
            | OpCode::MakeFunction
            | OpCode::SetFunctionAttribute
            | OpCode::ListToTuple
            | OpCode::UnpackEx
            | OpCode::LoadSuperAttr
            | OpCode::LoadSpecial
            | OpCode::PushExcInfo
            | OpCode::CheckExcMatch
            | OpCode::PopExcept
            | OpCode::DeleteFast
            | OpCode::StoreSlice
            | OpCode::DeleteSubscr
            | OpCode::GetAwaitable
            | OpCode::GetYieldFromIter
            | OpCode::Send
            | OpCode::EndSend => false,
            _ => true,
        };
        Some(if in_line { 2 } else { 1 })
    };
    if matches!(at, Heat::Call) && code.jit_hint.loop_free(code) {
        // The body as a call runs it (the handlers aside).
        let (mut n, mut cost) = (0usize, 0usize);
        for k in body_pcs(code) {
            match gain(k) {
                Some(g) => n += g,
                None => {
                    cost += match ins[k].op {
                        // Every activation leaves at one; the core loop
                        // would dispatch it too.
                        OpCode::ReturnValue => 0,
                        OpCode::Call | OpCode::CallKw | OpCode::CallEx => t.call_exit,
                        _ => t.exit,
                    }
                }
            }
        }
        if stats::enabled() {
            eprintln!(
                "frame jit: {} body: gain {n}, exit cost {cost}",
                code.qualname
            );
        }
        return n >= 2 * (cost + t.body_min);
    }
    let at_loop = match at {
        Heat::BackEdge(pc) => {
            Some(pc).filter(|&pc| ins.get(pc).is_some_and(|i| i.op == OpCode::JumpBackward))
        }
        Heat::Call | Heat::Step => None,
    };
    ins.iter().enumerate().any(|(pc, i)| {
        if i.op != OpCode::JumpBackward || at_loop.is_some_and(|at| pc != at) {
            return false;
        }
        let top = (pc + 1).saturating_sub(i.arg as usize);
        let (mut n, mut exits) = (0usize, 0usize);
        for (k, ins_k) in ins.iter().enumerate().take(pc + 1).skip(top) {
            if cold[k] {
                continue;
            }
            match gain(k) {
                Some(g) => n += g,
                // (A loop's epilogue inside a region a handler's back edge
                // spans: `FOR_ITER`'s exit skips the first two, and a
                // return leaves anyway.)
                None if matches!(
                    ins_k.op,
                    OpCode::EndFor | OpCode::PopIter | OpCode::ReturnValue
                ) => {}
                // A resume ends at its yield whether or not the body runs
                // natively: no round trip to pay for.
                None if ins_k.op == OpCode::YieldValue
                    && (matches!(at, Heat::Step)
                        || code.is_generator
                        || code.is_coroutine
                        || code.is_async_generator) => {}
                None => exits += 1,
            }
        }
        if stats::enabled() {
            eprintln!(
                "frame jit: {} loop at {top}: gain {n}, {exits} exits",
                code.qualname
            );
        }
        2 * exits * t.loop_exit <= n
    })
}

/// The compile verdicts' weights (`WEAVEPY_FRAME_JIT_TUNE`, comma-separated
/// overrides in field order, a tuning aid).
#[derive(Clone, Copy)]
struct Tuning {
    /// Natively run instructions a loop needs per instruction that leaves.
    loop_exit: usize,
    /// What a body's instruction that leaves costs, in natively run ones:
    /// a call, and anything else.
    call_exit: usize,
    exit: usize,
    /// The natively run instructions a body needs beyond its exits' cost
    /// (none by default: a compiled body's call from native code runs
    /// directly, see `direct_call`, which costs less than the core loop's
    /// call even for a short body).
    body_min: usize,
    /// An activation's heat: the code's length over this.
    call_div: u32,
}

fn tuning() -> Tuning {
    static T: std::sync::OnceLock<Tuning> = std::sync::OnceLock::new();
    *T.get_or_init(|| {
        let mut v = [9usize, 1, 3, 0, 10];
        if let Ok(s) = std::env::var("WEAVEPY_FRAME_JIT_TUNE") {
            for (slot, x) in v.iter_mut().zip(s.split(',')) {
                if let Ok(x) = x.trim().parse() {
                    *slot = x;
                }
            }
        }
        Tuning {
            loop_exit: v[0],
            call_exit: v[1],
            exit: v[2],
            body_min: v[3],
            call_div: (v[4] as u32).max(1),
        }
    })
}

/// Whether a code object of this qualified name may compile:
/// `WEAVEPY_FRAME_JIT_ONLY` / `WEAVEPY_FRAME_JIT_SKIP` admit only, or
/// skip, names containing their value (a debugging aid).
fn name_admitted(name: &str) -> bool {
    static FILTER: std::sync::OnceLock<(Option<String>, Option<String>)> =
        std::sync::OnceLock::new();
    let (only, skip) = FILTER.get_or_init(|| {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        (var("WEAVEPY_FRAME_JIT_ONLY"), var("WEAVEPY_FRAME_JIT_SKIP"))
    });
    only.as_deref().is_none_or(|o| name.contains(o))
        && skip.as_deref().is_none_or(|s| !name.contains(s))
}

/// Whether `WEAVEPY_FRAME_JIT_NO` (opcode names, comma-separated) leaves
/// `op` to the core loop (a debugging aid).
fn op_disabled(op: OpCode) -> bool {
    static NO: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    let no = NO.get_or_init(|| {
        std::env::var("WEAVEPY_FRAME_JIT_NO")
            .map(|v| v.split(',').map(|s| s.trim().to_owned()).collect())
            .unwrap_or_default()
    });
    !no.is_empty() && no.iter().any(|n| *n == format!("{op:?}"))
}

/// Whether frames compile at all (`WEAVEPY_FRAME_JIT=0` turns them off).
fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("WEAVEPY_FRAME_JIT").is_none_or(|v| v != "0")
            && !crate::gil::free_threading_enabled()
    })
}

/// `WEAVEPY_FRAME_JIT_STATS=1`: entries and the instructions native code
/// returned at, by code object, printed at exit (a tuning aid).
mod stats {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use weavepy_compiler::OpCode;

    type Counts = HashMap<(String, String), [u64; 2]>;

    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    static COUNTS: Mutex<Option<Counts>> = Mutex::new(None);

    #[inline(always)]
    pub(super) fn enabled() -> bool {
        *ON.get_or_init(|| {
            let on = std::env::var_os("WEAVEPY_FRAME_JIT_STATS").is_some();
            if on {
                extern "C" fn dump() {
                    let counts = COUNTS.lock().unwrap_or_else(|e| e.into_inner());
                    let mut rows: Vec<_> = counts.iter().flat_map(|c| c.iter()).collect();
                    rows.sort_by_key(|(_, n)| std::cmp::Reverse(n[0]));
                    for ((name, op), n) in rows.iter().take(40) {
                        eprintln!("{:>10} {:>10}  {name}: {op}", n[0], n[1]);
                    }
                    // The same by instruction alone.
                    let mut by_op: HashMap<&str, [u64; 2]> = HashMap::new();
                    for ((_, op), n) in &rows {
                        let e = by_op.entry(op.as_str()).or_default();
                        e[0] += n[0];
                        e[1] += n[1];
                    }
                    let mut ops: Vec<_> = by_op.into_iter().collect();
                    ops.sort_by_key(|(_, n)| std::cmp::Reverse(n[0]));
                    for (op, n) in ops.iter().take(30) {
                        eprintln!("{:>10} {:>10}  * {op}", n[0], n[1]);
                    }
                }
                // SAFETY: registering a plain function.
                unsafe { libc::atexit(dump) };
            }
            on
        })
    }

    /// [`super::run_pushed_directly`] declined, for `why`: `None`.
    #[inline(always)]
    pub(super) fn declined(why: &'static str) -> Option<u32> {
        if enabled() {
            note_declined(why);
        }
        None
    }

    #[cold]
    #[inline(never)]
    fn note_declined(why: &'static str) {
        let mut counts = COUNTS.lock().unwrap_or_else(|e| e.into_inner());
        let e = counts
            .get_or_insert_with(HashMap::new)
            .entry(("<pushed call declined>".to_owned(), why.to_owned()))
            .or_default();
        e[0] += 1;
    }

    #[cold]
    pub(super) fn note(
        name: &str,
        ops: &[OpCode],
        from: usize,
        at: usize,
        last: usize,
        sw: *mut crate::CoreSwitch,
        status: u32,
        bail: &str,
    ) {
        let op = match (status, ops.get(at)) {
            (0, Some(op)) => format!("{op:?}{bail}"),
            (0, None) => "<end>".to_owned(),
            // (Where it switched: the last instruction it ran, and the
            // activation it switched to.)
            (super::RELOAD, _) => {
                // SAFETY: a live switch's running activation.
                let to = (!sw.is_null())
                    .then(|| unsafe {
                        let cur: &crate::Frame = &*(*sw).cur;
                        cur.code.qualname.clone()
                    })
                    .unwrap_or_default();
                match ops.get(last) {
                    Some(op) => format!("<switched> at {op:?}@{last}, {:?} to {to}", ops.get(last + 1)),
                    None => "<switched>".to_owned(),
                }
            }
            (super::RAISED, _) => "<raised>".to_owned(),
            (super::DIRECT_RET, _) => "<direct return>".to_owned(),
            _ => "<marked>".to_owned(),
        };
        let mut counts = COUNTS.lock().unwrap_or_else(|e| e.into_inner());
        let e = counts
            .get_or_insert_with(HashMap::new)
            .entry((name.to_owned(), op))
            .or_default();
        e[0] += 1;
        e[1] += u64::from(at != from);
    }
}

// ---- the object layout ----------------------------------------------------

/// The tags of the variants the code handles in line.
#[derive(Clone, Copy)]
struct Tags {
    none: u8,
    unbound: u8,
    boolean: u8,
    int: u8,
    float: u8,
    cell: u8,
    instance: u8,
    /// A Python function's (see [`direct_call`]).
    function: u8,
    /// A list's tag, and where its items are (see [`ListLayout`]).
    list: u8,
    list_layout: Option<ListLayout>,
    /// A tuple's tag, and where its length and items are, from its
    /// payload word: `(length, first item)` byte offsets.
    tuple: u8,
    tuple_layout: Option<(i32, i32)>,
    /// The `dict`, `set` and `str` tags (the method kernels' receivers).
    dict: u8,
    set: u8,
    str: u8,
    /// The heap variants whose payload word is an `Rc`'s allocation, its
    /// strong count the allocation's first word (a clone is an increment
    /// there): instances, lists, dicts and classes, as far as measured.
    rc_counted: i64,
    /// The heap variants whose payload word addresses a shared payload
    /// with its strong count two words before it (see `rc::strong_word`):
    /// strings and tuples, as far as measured.
    rc_counted_hdr: i64,
    /// The heap variants whose identity is their payload pointer (and so
    /// `is` compares payload words): instances, strings, lists, dicts,
    /// tuples, classes.
    by_pointer: i64,
}

impl Tags {
    /// `None`, `bool`, `int` and `float`: copied by value, dropped by
    /// forgetting.
    fn scalar_mask(self) -> i64 {
        (1i64 << self.none) | (1 << self.boolean) | (1 << self.int) | (1 << self.float)
    }
}

/// The tags, read off sample values, if the layout is the one the code
/// assumes.
fn tags() -> Option<Tags> {
    let tag = |v: &Object| {
        // SAFETY: `Object` is `repr(u8)`: its first byte is the tag.
        unsafe { *std::ptr::from_ref(v).cast::<u8>() }
    };
    let word = |v: &Object| {
        // SAFETY: every `Object` is 16 bytes; the second word is
        // initialized for the word payloads read here.
        unsafe { *std::ptr::from_ref(v).cast::<u64>().add(1) }
    };
    let byte1 = |v: &Object| {
        // SAFETY: as above (a `bool` payload's byte).
        unsafe { *std::ptr::from_ref(v).cast::<u8>().add(1) }
    };
    let cell = Object::Cell(Rc::new(crate::sync::RefCell::new(Object::None)));
    let func = {
        let dict = Rc::new(crate::sync::RefCell::new(crate::object::DictData::default()));
        crate::new_function(
            Rc::new(CodeObject::default()),
            &dict,
            &dict,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            None,
        )
    };
    let inst = Object::Instance(Rc::new(crate::types::PyInstance::new(
        crate::builtin_types::builtin_types().object_.clone(),
    )));
    let samples = [
        tag(&inst),
        tag(&Object::from_str("")),
        tag(&Object::new_list(Vec::new())),
        tag(&Object::new_dict()),
        tag(&Object::new_tuple_array([Object::None])),
        tag(&Object::Type(
            crate::builtin_types::builtin_types().object_.clone(),
        )),
    ];
    let t = Tags {
        none: tag(&Object::None),
        unbound: tag(&Object::Unbound),
        boolean: tag(&Object::Bool(true)),
        int: tag(&Object::Int(1)),
        float: tag(&Object::Float(1.0)),
        cell: tag(&cell),
        instance: tag(&inst),
        function: tag(&func),
        list: tag(&Object::new_list(Vec::new())),
        list_layout: list_layout(),
        tuple: tag(&Object::new_tuple_array([Object::None])),
        tuple_layout: tuple_layout(),
        rc_counted: if cfg!(debug_assertions) {
            // (A debug build's counts are atomic throughout.)
            0
        } else {
            [
                &inst,
                &Object::new_list(Vec::new()),
                &Object::new_dict(),
                &Object::Type(crate::builtin_types::builtin_types().object_.clone()),
                &func,
                &Object::new_set(),
                &Object::Builtin(Rc::new(crate::object::BuiltinFn::new("sample", |_| {
                    Ok(Object::None)
                }))),
            ]
            .into_iter()
            .filter(|o| tag(o) < 64 && counted_at_payload(o, word(o)))
            .fold(0, |m, o| m | (1i64 << tag(o)))
        },
        dict: tag(&Object::new_dict()),
        set: tag(&Object::new_set()),
        str: tag(&Object::from_str("")),
        rc_counted_hdr: if cfg!(debug_assertions) {
            0
        } else {
            // (A fresh string: the small ones are shared statics.)
            [
                &Object::from_str("not a small string"),
                &Object::new_tuple_array([Object::None, Object::None]),
            ]
            .into_iter()
            .filter(|o| tag(o) < 64 && word(o) >= 16 && counted_at_payload(o, word(o) - 16))
            .fold(0, |m, o| m | (1i64 << tag(o)))
        },
        by_pointer: samples
            .iter()
            .fold(0, |m, &t| if t < 64 { m | (1i64 << t) } else { m }),
    };
    if samples.iter().any(|&t| t >= 64) {
        return None;
    }
    let all = [
        t.none, t.unbound, t.boolean, t.int, t.float, t.cell, t.instance, t.function,
    ];
    let distinct = all
        .iter()
        .enumerate()
        .all(|(i, a)| all[i + 1..].iter().all(|b| a != b));
    (distinct
        && all.iter().all(|&t| t < 64)
        && tag(&Object::Bool(false)) == t.boolean
        && byte1(&Object::Bool(true)) == 1
        && byte1(&Object::Bool(false)) == 0
        && word(&Object::Int(0x0123_4567_89ab_cdef)) == 0x0123_4567_89ab_cdef
        && word(&Object::Float(1.5)) == 1.5f64.to_bits()
        && std::mem::size_of::<Object>() == 16)
        .then_some(t)
}

/// Where a list's items are, from its payload word (its allocation): the
/// byte offsets of its cell's borrow counter (an `i32`; negative while
/// mutably borrowed) and of its vector's pointer and length.
#[derive(Clone, Copy)]
struct ListLayout {
    borrow: i32,
    ptr: i32,
    len: i32,
}

/// [`ListLayout`], read off a sample list (`None` if it isn't the shape
/// the code assumes).
fn list_layout() -> Option<ListLayout> {
    let mut v: Vec<Object> = Vec::with_capacity(7);
    v.extend([Object::Int(1), Object::Int(2), Object::Int(3)]);
    let (vp, vl) = (v.as_ptr() as u64, v.len() as u64);
    let o = Object::new_list(v);
    let Object::List(rc) = &o else {
        return None;
    };
    // SAFETY: every `Object` is 16 bytes; a list's second word is its
    // allocation's address.
    let word = unsafe { *std::ptr::from_ref(&o).cast::<u64>().add(1) };
    let cell = Rc::as_ptr(rc) as u64;
    let at_cell = cell.checked_sub(word)?;
    let (borrow, data) = crate::sync::object_vec_cell_offsets();
    // SAFETY: the vector's three words, inside the live sample.
    let words: [u64; 3] = unsafe { std::ptr::read((cell as usize + data) as *const [u64; 3]) };
    let ptr = words.iter().position(|&w| w == vp)?;
    let len = words.iter().position(|&w| w == vl)?;
    let off = |d: usize| i32::try_from(at_cell as usize + d).ok();
    // The borrow counter reads as unborrowed, and a borrow moves it.
    // SAFETY: as above.
    let counter = unsafe { &*((cell as usize + borrow) as *const std::sync::atomic::AtomicI32) };
    let idle = counter.load(std::sync::atomic::Ordering::Relaxed);
    let held = {
        let _g = rc.borrow_mut();
        counter.load(std::sync::atomic::Ordering::Relaxed)
    };
    (idle == 0 && held < 0 && ptr != len).then_some(())?;
    Some(ListLayout {
        borrow: off(borrow)?,
        ptr: off(data + 8 * ptr)?,
        len: off(data + 8 * len)?,
    })
}

/// A tuple's `(length, first item)` byte offsets from its payload word,
/// read off a sample tuple (`None` if it isn't the shape assumed).
fn tuple_layout() -> Option<(i32, i32)> {
    const MARK: i64 = 0x5eed_7a91_e000_0001;
    let o = Object::new_tuple(vec![Object::Int(MARK), Object::Int(2), Object::Int(3)]);
    // SAFETY: every `Object` is 16 bytes; a tuple's second word addresses
    // its payload.
    let word = unsafe { *std::ptr::from_ref(&o).cast::<u64>().add(1) } as usize;
    let Object::Tuple(t) = &o else {
        return None;
    };
    // SAFETY: the payload's leading words, inside the live sample (its
    // items follow its length and cached hash).
    let words: [u64; 8] = unsafe { std::ptr::read(word as *const [u64; 8]) };
    let len = words.iter().position(|&w| w == 3)?;
    let first = std::ptr::from_ref(&t[0]) as usize;
    let items = first.checked_sub(word)?;
    let at = |i: usize| words.get(i).copied();
    (at(items / 8 + 1)? == MARK as u64 && items % 8 == 0 && len * 8 < items)
        .then(|| Some((i32::try_from(len * 8).ok()?, i32::try_from(items).ok()?)))?
}

/// Whether cloning `o` (whose payload word is `w`) adds one to the word at
/// `w` and nothing else measurable, and dropping the clone takes it away.
fn counted_at_payload(o: &Object, w: u64) -> bool {
    if w == 0 || !w.is_multiple_of(8) {
        return false;
    }
    let count = || {
        // SAFETY: probing the first word of the allocation `o` keeps alive
        // (its count, when the layout is the assumed one).
        unsafe { std::ptr::with_exposed_provenance::<usize>(w as usize).read() }
    };
    let before = count();
    let copy = o.clone();
    let during = count();
    drop(copy);
    during == before + 1 && count() == before
}

// ---- helpers (the core loop's arms, for native code) ----------------------

#[inline(always)]
fn droppable(v: &Object) -> bool {
    Interpreter::core_droppable(v)
}

/// `*dst = (*src).clone()`.
unsafe extern "C" fn h_clone(dst: *mut Object, src: *const Object) {
    // SAFETY: the code passes an initialized value and a free slot.
    unsafe { dst.write(crate::clone_hot(&*src)) }
}

/// Release the heap value at `slot` (it leaves the stack): `0` released,
/// anything else declined with the slot untouched.
unsafe extern "C" fn h_pop(slot: *mut Object) -> u32 {
    // SAFETY: the code passes an initialized stack slot.
    unsafe {
        if !droppable(&*slot) {
            return 1;
        }
        crate::drop_hot(slot.read());
    }
    0
}

/// `STORE_FAST i` of the value at stack `slot` over a heap value: `0`
/// stored (the value left the stack), anything else declined untouched.
unsafe extern "C" fn h_store(st: *mut State, slot: *mut Object, i: u64) -> u32 {
    // SAFETY: the code passes its live state, an initialized stack slot,
    // and a local index in range.
    unsafe {
        let st = &mut *st;
        let local = st.locals.add(i as usize);
        if !droppable(&*local) {
            return 1;
        }
        let v = slot.read();
        (*st.interp).release(std::mem::replace(&mut *local, v));
    }
    0
}

/// `FOR_ITER` over a range, list or tuple iterator at `it` (the next value
/// goes to `out`, the slot above): `0` an `int` in `st.out`, `1` another
/// value written to `out`, `2` the exhausted iterator retired (it left the
/// stack), anything else declined. The commonest steps (a live range, a
/// list's or tuple's next item) run here; everything else (exhaustion, a
/// generator, the other iterators) in [`h_for_iter_rest`].
unsafe extern "C" fn h_for_iter(st: *mut State, it: *mut Object, out: *mut Object, pc: u64) -> u32 {
    // SAFETY: as `h_for_iter_rest`'s; nothing here runs code while the
    // iterator is borrowed.
    unsafe {
        if let Object::Iter(rc) = &*it {
            if let Some(iter) = rc.peek_mut() {
                let v = match iter {
                    PyIterator::Range {
                        current,
                        stop,
                        step,
                    } if *step > 0 && *current < *stop => {
                        (*st).out = *current as u64;
                        *current = current.wrapping_add(*step);
                        return 0;
                    }
                    PyIterator::List { items, index, .. } => {
                        match items.peek().and_then(|xs| xs.get(*index)) {
                            Some(v) => {
                                *index += 1;
                                v
                            }
                            None => return h_for_iter_rest(st, it, out, pc),
                        }
                    }
                    PyIterator::Tuple { items, index } => match items.get(*index) {
                        Some(v) => {
                            *index += 1;
                            v
                        }
                        None => return h_for_iter_rest(st, it, out, pc),
                    },
                    // A string's next ASCII character (a shared one).
                    PyIterator::Str { s, index } => match s.as_bytes().get(*index) {
                        Some(&c) if c < 0x80 => {
                            *index += 1;
                            out.write(Object::Str(crate::shared_value::SharedStr::ascii_char(c)));
                            return 1;
                        }
                        _ => return h_for_iter_rest(st, it, out, pc),
                    },
                    _ => return h_for_iter_rest(st, it, out, pc),
                };
                if let Object::Int(i) = *v {
                    (*st).out = i as u64;
                    return 0;
                }
                out.write(crate::clone_hot(v));
                return 1;
            }
        }
        h_for_iter_rest(st, it, out, pc)
    }
}

/// [`h_for_iter`]'s other shapes.
#[inline(never)]
unsafe extern "C" fn h_for_iter_rest(
    st: *mut State,
    it: *mut Object,
    out: *mut Object,
    pc: u64,
) -> u32 {
    // SAFETY: the code passes its live state, an initialized slot with a
    // free one above it, and its `FOR_ITER`'s pc.
    let (st, top) = unsafe { (&mut *st, &*it) };
    let rc = match top {
        Object::Iter(rc) => rc,
        // A generator: a simple body's step in place (see `gen_fast`),
        // any other resumes as an inline activation, switched to as the
        // core loop's arm does (not from a generator's own fast step).
        Object::Generator(g) if !st.sw.is_null() => {
            // SAFETY: the running thread's interpreter, dormant while the
            // helper runs; the stack slot keeps `g` alive.
            let interp = unsafe { &mut *st.interp.cast_mut() };
            match interp.gen_fast_next(g, st.snap_gen, 0, st.maybe_dead) {
                crate::gen_fast::GenNext::Yielded(v) => {
                    // SAFETY: the slot above the iterator is free.
                    unsafe { out.write(v) };
                    return 1;
                }
                crate::gen_fast::GenNext::Exhausted(_) => {
                    // SAFETY: the finished generator leaves the stack.
                    drop(unsafe { it.read() });
                    // SAFETY: the running thread's own flag.
                    if !unsafe { (*st.maybe_dead).get() } {
                        return 2;
                    }
                    // Its frame released something that queued a finalizer,
                    // which runs before the next instruction: the core loop
                    // takes over past the loop, synced as for a switch.
                    // SAFETY: as below.
                    unsafe {
                        let sw = &mut *st.sw;
                        let frame = &mut *st.frame;
                        let code = &frame.code;
                        let mut to = pc as usize + 1 + code.instructions[pc as usize].arg as usize;
                        let op = |pc: usize| code.instructions.get(pc).map(|i| i.op);
                        if op(to) == Some(OpCode::EndFor) {
                            to += 1;
                            if matches!(op(to), Some(OpCode::PopIter | OpCode::PopTop)) {
                                to += 1;
                            }
                        }
                        frame.stack.set_len(it.offset_from(st.stack) as usize);
                        frame.pc = to as u32;
                        *sw.last = pc as usize;
                        sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Marked));
                    }
                    return FOR_SWITCHED;
                }
                crate::gen_fast::GenNext::Declined | crate::gen_fast::GenNext::Partial => {}
            }
            // SAFETY: the running activation's frame and switch; the
            // stack is synced (the iterator on top) before the switch.
            unsafe {
                let sw = &mut *st.sw;
                let frame = &mut *st.frame;
                let len = it.offset_from(st.stack) as usize + 1;
                frame.stack.set_len(len);
                frame.pc = pc as u32;
                *sw.last = st.last;
                if !interp.core_gen_resume(sw, pc as usize, crate::InlineResume::ForIter) {
                    sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Step));
                    return FOR_SWITCHED;
                }
                // A compiled body runs from here; its yield (the value on
                // top, past the instruction) or its return (the iterator
                // gone, past the loop) continues this code natively.
                if run_switched_directly(st) {
                    let frame = &*st.frame;
                    let at = frame.pc as usize;
                    let depth = frame.stack.len();
                    if at == pc as usize + 1 && depth == len + 1 {
                        return 1;
                    }
                    if depth == len - 1 && at == for_iter_exit(&frame.code, pc as usize) {
                        return 2;
                    }
                }
            }
            return FOR_SWITCHED;
        }
        // A generator iterated from another generator's fast step: its own
        // fast step in place, nested one deeper, as the fast step's
        // `FOR_ITER` arm takes it (and on the same terms: a body it can't
        // step leaves the instruction to that arm).
        Object::Generator(g) => {
            // SAFETY: as above.
            let interp = unsafe { &mut *st.interp.cast_mut() };
            return match interp.gen_fast_next(g, st.snap_gen, st.gen_depth + 1, st.maybe_dead) {
                crate::gen_fast::GenNext::Yielded(v) => {
                    // SAFETY: the slot above the iterator is free.
                    unsafe { out.write(v) };
                    1
                }
                crate::gen_fast::GenNext::Exhausted(_) => {
                    // SAFETY: the finished generator leaves the stack.
                    drop(unsafe { it.read() });
                    2
                }
                crate::gen_fast::GenNext::Declined | crate::gen_fast::GenNext::Partial => 3,
            };
        }
        // A native iterator class's `__next__` (a registered leaf builtin)
        // in place; exhaustion is the core loop's, a raise leaves past the
        // instruction.
        obj @ Object::Instance(inst) if !st.sw.is_null() => {
            // SAFETY: as above.
            let interp = unsafe { &*st.interp };
            return match interp.leaf_next_step(obj, inst) {
                Some(Ok(v)) => {
                    // SAFETY: the slot above the iterator is free.
                    unsafe { out.write(v) };
                    1
                }
                Some(Err(RuntimeError::PyException(exc))) if exc.type_name() == "StopIteration" => {
                    3
                }
                Some(Err(e)) => {
                    st.len = unsafe { it.offset_from(st.stack) } as usize + 1;
                    st.pc = pc as usize + 1;
                    st.err = Some(e);
                    FOR_RAISED
                }
                None => 3,
            };
        }
        // A native adapter whose step runs no code (`zip` of native
        // iterators, `itertools.repeat`, ...); its exhaustion is the
        // interpreter's.
        Object::LazyIter(l) => {
            // SAFETY: the code passes its live state, whose interpreter
            // is dormant while the helper runs.
            let interp = unsafe { &*st.interp };
            let Some(Some(v)) = interp.lazy_core_next(l) else {
                return 3;
            };
            // SAFETY: the slot above the iterator is free.
            unsafe { out.write(v) };
            return 1;
        }
        _ => return 3,
    };
    let unique = Rc::strong_count(rc) == 1;
    // SAFETY: nothing below runs code while the iterator is borrowed.
    let Some(iter) = (unsafe { rc.peek_mut() }) else {
        return 3;
    };
    let v = match iter {
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
                st.out = *current as u64;
                *current = current.wrapping_add(*step);
                return 0;
            }
            if !unique {
                return 3;
            }
            // SAFETY: the iterator leaves the stack.
            drop(unsafe { it.read() });
            return 2;
        }
        PyIterator::List {
            items,
            index,
            owner,
        } => {
            // SAFETY: as above.
            let Some(xs) = (unsafe { items.peek() }) else {
                return 3;
            };
            match xs.get(*index) {
                Some(v) => {
                    let v = crate::clone_hot(v);
                    *index += 1;
                    v
                }
                // A shared iterator (a generator expression's `.0`, which
                // its local holds too) detaches from the list, as the core
                // loop's arm does, so a later append can't resurrect it
                // (when the list lives on: its release frees nothing).
                None if owner.is_none() && (unique || crate::sync::Rc::strong_count(items) > 1) => {
                    if !unique {
                        *items = crate::sync::Rc::new(crate::sync::RefCell::new(Vec::new()));
                    }
                    // SAFETY: as above.
                    drop(unsafe { it.read() });
                    return 2;
                }
                None => return 3,
            }
        }
        PyIterator::Tuple { items, index } => {
            let Some(v) = items.get(*index).cloned() else {
                // The exhausted iterator leaves the stack (as the core
                // loop's arm retires it), when its release can't free the
                // tuple (whose items could queue finalizers): a shared
                // iterator stays exhausted, the tuple immutable.
                // (Items that finalize nothing free in place: a temporary
                // tuple of names, `for k in key[:-1]`.)
                if unique
                    && crate::shared_value::ThinArc::strong_count(items) == 1
                    && !items.iter().all(|x| {
                        matches!(
                            x,
                            Object::Int(_)
                                | Object::Float(_)
                                | Object::Bool(_)
                                | Object::None
                                | Object::Str(_)
                        )
                    })
                {
                    return 3;
                }
                // SAFETY: the iterator leaves the stack.
                drop(unsafe { it.read() });
                return 2;
            };
            *index += 1;
            v
        }
        // `reversed(xs)`: the item below the cursor.
        PyIterator::Reversed { items, index, .. } => {
            if *index < 0 {
                return 3;
            }
            // SAFETY: as above.
            let Some(xs) = (unsafe { items.peek() }) else {
                return 3;
            };
            let Some(v) = xs.get(*index as usize) else {
                return 3;
            };
            let v = crate::clone_hot(v);
            *index -= 1;
            v
        }
        // `enumerate` over a native iterator whose step runs no code.
        PyIterator::Enumerate {
            inner,
            count,
            count_big: None,
        } if *count < i64::MAX => {
            // SAFETY: as above; `inner` is a cell of its own.
            let Some(inner) = (unsafe { inner.peek_mut() }) else {
                return 3;
            };
            if !inner.pure_ready() {
                return 3;
            }
            let Some(x) = inner.next_value() else {
                return 3;
            };
            let i = *count;
            *count += 1;
            // (From the tuple free list.)
            // SAFETY: the running thread's interpreter, dormant while the
            // helper runs.
            unsafe { (*st.interp).alloc_tuple_array([Object::Int(i), x]) }
        }
        // A set's next element while the set keeps its size (a change,
        // and exhaustion, are the core loop's to report).
        PyIterator::Set { set, len, index } => {
            let Ok(items) = set.try_borrow() else {
                return 3;
            };
            if items.len() != *len {
                return 3;
            }
            let Some(k) = items.get_index(*index) else {
                return 3;
            };
            let v = crate::clone_hot(&k.0);
            *index += 1;
            v
        }
        // Any other cursor whose step runs no code (a string's, bytes');
        // its exhaustion is the core loop's.
        other if other.pure_ready() => match other.next_value() {
            Some(v) => v,
            None => return 3,
        },
        // An exhausted string's or bytes' cursor leaves the stack, as a
        // range's does: releasing it runs no code.
        PyIterator::Str { s, index } if unique && *index >= s.len() => {
            // SAFETY: as above.
            drop(unsafe { it.read() });
            return 2;
        }
        PyIterator::Bytes { data, index } if unique && *index >= data.len() => {
            // SAFETY: as above.
            drop(unsafe { it.read() });
            return 2;
        }
        _ => return 3,
    };
    if let Object::Int(i) = v {
        st.out = i as u64;
        return 0;
    }
    // SAFETY: the slot above the iterator is free.
    unsafe { out.write(v) };
    1
}

/// [`h_for_iter`] switched to a generator's activation (`RELOAD`).
const FOR_SWITCHED: u32 = 4;
/// [`h_for_iter`]'s iterator raised (`RAISED`, the state past it).
const FOR_RAISED: u32 = 5;

/// `LIST_APPEND` onto the exact list at `list` of the value at `value`:
/// `0` the value moved in, anything else declined untouched.
unsafe extern "C" fn h_list_append(list: *const Object, value: *const Object) -> u32 {
    // SAFETY: the code passes two initialized stack slots; growing the
    // list runs no code, so nothing reaches it while it is peeked at.
    unsafe {
        let Object::List(l) = &*list else {
            return 1;
        };
        let Some(items) = l.peek_mut() else {
            return 1;
        };
        crate::push_fast(items, value.read());
        0
    }
}

/// A method kernel: `recv.m(a, b)` for an exact `list`, `dict`, `set` or
/// `str` receiver (its tag checked by the caller), with the arguments the
/// site passes (null past them) read in place, and the method's builtin
/// `f` (for the kernels that run its body): `0` the result written to the
/// free slot `out`, anything else declined untouched (the call then runs
/// as before). A kernel runs no Python code, raises nothing, and releases
/// nothing that could run a finalizer.
type Kernel = unsafe extern "C" fn(
    out: *mut Object,
    recv: *const Object,
    a: *const Object,
    b: *const Object,
    f: *const crate::object::BuiltinFn,
) -> u32;

/// The kernel for the `tag` receiver's method of leaf kind `kind` called
/// with `nargs` arguments (see [`Lower::method_kernel`]).
fn kernel_for(tag: u64, kind: crate::LeafKind, nargs: usize) -> Option<Kernel> {
    use crate::LeafKind as K;
    Some(match (tag, kind, nargs) {
        (1, K::ListAppend, 1) => k_list_append,
        (1, K::ListPop, 0) => k_list_pop,
        (1, K::ListExtend, 1) => k_list_extend,
        (2, K::DictGet, 1 | 2) => k_dict_get,
        (3, K::SetAdd, 1) => k_set_add,
        (3, K::SetDiscard, 1) => k_set_discard,
        (3, K::SetRemove, 1) => k_set_remove,
        (4, K::StrStartswith, 1 | 2) => k_str_startswith,
        (4, K::StrEndswith, 1 | 2) => k_str_endswith,
        (4, K::StrIsdigit, 0) => k_str_isdigit,
        (4, K::StrIsalpha, 0) => k_str_isalpha,
        (4, K::StrIsspace, 0) => k_str_isspace,
        (4, K::StrFind, 1 | 2) => k_str_find,
        (4, K::StrLower | K::StrUpper, 0) => k_str_body,
        (4, K::StrStrip | K::StrLstrip | K::StrRstrip, 0 | 1) => k_str_strip,
        (4, K::StrSplit, 0 | 1) => k_str_split,
        (4, K::StrJoin, 1) => k_str_join,
        (4, K::StrReplace, 2) => k_str_replace,
        (4, K::Opaque, _) => k_str_body,
        _ => return None,
    })
}

/// The call of method body `b` of leaf kind `kind` on `args` (the
/// receiver first) through its [`Kernel`], for a `list`, `dict`, `set` or
/// `str` receiver that has one: its result, or `None` (nothing done).
/// (Out of line: the general call helper stays as it was.)
#[inline(never)]
fn kernel_call(
    kind: crate::LeafKind,
    args: &[Object],
    b: &Rc<crate::object::BuiltinFn>,
) -> Option<Object> {
    let tag = match args.first()? {
        Object::List(_) => 1,
        Object::Dict(_) => 2,
        Object::Set(_) => 3,
        Object::Str(_) => 4,
        _ => return None,
    };
    if args.len() > 3 {
        return None;
    }
    let kernel = kernel_for(tag, kind, args.len() - 1)?;
    let arg = |k: usize| args.get(k).map_or(std::ptr::null(), std::ptr::from_ref);
    let mut out = std::mem::MaybeUninit::<Object>::uninit();
    // SAFETY: the operands are initialized values; `out` is free.
    unsafe {
        (kernel(out.as_mut_ptr(), &args[0], arg(1), arg(2), Rc::as_ptr(b)) == 0)
            .then(|| out.assume_init())
    }
}

/// The method's own body on bitwise views of the operands (as the core
/// loop's leaf call runs it): `0` its result written to `out`, anything
/// else (an error, which the full call raises) declined.
///
/// # Safety
///
/// As [`Kernel`]'s, for a builtin whose body runs no Python code on
/// these operands.
unsafe fn run_body(
    out: *mut Object,
    recv: *const Object,
    a: *const Object,
    b: *const Object,
    f: *const crate::object::BuiltinFn,
) -> u32 {
    // SAFETY: the caller's contract; the views are never dropped.
    unsafe {
        let views = std::mem::ManuallyDrop::new([
            recv.read(),
            if a.is_null() {
                Object::Unbound
            } else {
                a.read()
            },
            if b.is_null() {
                Object::Unbound
            } else {
                b.read()
            },
        ]);
        let n = 1 + usize::from(!a.is_null()) + usize::from(!b.is_null());
        let f = &*f;
        let r = match f.call_kw.as_ref() {
            Some(ckw) => ckw(&views[..n], &[]),
            None => (f.call)(&views[..n]),
        };
        match r {
            Ok(v) => {
                out.write(v);
                0
            }
            Err(_) => 1,
        }
    }
}

unsafe extern "C" fn k_list_append(
    out: *mut Object,
    recv: *const Object,
    a: *const Object,
    _: *const Object,
    _: *const crate::object::BuiltinFn,
) -> u32 {
    // SAFETY: as `Kernel`'s; growing the list runs no code.
    unsafe {
        let Object::List(l) = &*recv else {
            return 1;
        };
        let Some(items) = l.peek_mut() else {
            return 1;
        };
        crate::push_fast(items, crate::clone_hot(&*a));
        out.write(Object::None);
        0
    }
}

unsafe extern "C" fn k_list_extend(
    out: *mut Object,
    recv: *const Object,
    a: *const Object,
    _: *const Object,
    _: *const crate::object::BuiltinFn,
) -> u32 {
    // SAFETY: as `Kernel`'s; copying an exact list's or tuple's items runs
    // no code.
    unsafe {
        let Object::List(l) = &*recv else {
            return 1;
        };
        match &*a {
            // (`xs.extend(xs)` reads the length once: it doubles.)
            Object::List(src) if Rc::ptr_eq(l, src) => {
                let Some(xs) = l.peek_mut() else {
                    return 1;
                };
                let copy: Vec<Object> = xs.iter().map(crate::clone_hot).collect();
                xs.extend(copy);
            }
            Object::List(src) => {
                let (Some(s), Some(xs)) = (src.peek(), l.peek_mut()) else {
                    return 1;
                };
                xs.extend(s.iter().map(crate::clone_hot));
            }
            Object::Tuple(t) => {
                let Some(xs) = l.peek_mut() else {
                    return 1;
                };
                xs.extend(t.iter().map(crate::clone_hot));
            }
            _ => return 1,
        }
        out.write(Object::None);
        0
    }
}

unsafe extern "C" fn k_list_pop(
    out: *mut Object,
    recv: *const Object,
    _: *const Object,
    _: *const Object,
    _: *const crate::object::BuiltinFn,
) -> u32 {
    // SAFETY: as `Kernel`'s; the item moves out.
    unsafe {
        let Object::List(l) = &*recv else {
            return 1;
        };
        let Some(v) = l.peek_mut().and_then(Vec::pop) else {
            return 1;
        };
        out.write(v);
        0
    }
}

unsafe extern "C" fn k_dict_get(
    out: *mut Object,
    recv: *const Object,
    a: *const Object,
    b: *const Object,
    _: *const crate::object::BuiltinFn,
) -> u32 {
    // SAFETY: as `Kernel`'s; the probe runs no code.
    unsafe {
        let (Object::Dict(d), k) = (&*recv, &*a) else {
            return 1;
        };
        let Some(probe) = crate::object::LeafProbe::new(k) else {
            return 1;
        };
        let Some(m) = d.peek() else {
            return 1;
        };
        let v = match m.get_hot(&probe) {
            Some(v) => crate::clone_hot(v),
            None if probe.miss_is_exact() => {
                if b.is_null() {
                    Object::None
                } else {
                    crate::clone_hot(&*b)
                }
            }
            None => return 1,
        };
        out.write(v);
        0
    }
}

unsafe extern "C" fn k_set_add(
    out: *mut Object,
    recv: *const Object,
    a: *const Object,
    _: *const Object,
    _: *const crate::object::BuiltinFn,
) -> u32 {
    // SAFETY: as `Kernel`'s; hashing and comparing a `str`, an `int` or a
    // tuple of them runs no code.
    unsafe {
        let (Object::Set(s), k) = (&*recv, &*a) else {
            return 1;
        };
        if !matches!(k, Object::Str(_) | Object::Int(_) | Object::Tuple(_)) {
            return 1;
        }
        let Some(probe) = crate::object::LeafProbe::new(k) else {
            return 1;
        };
        let Some(m) = s.peek_mut() else {
            return 1;
        };
        if m.get_index_of(&probe).is_none() {
            // (A key of another kind that may equal it: the full call's.)
            if !probe.miss_is_exact() {
                return 1;
            }
            m.insert(crate::object::DictKey(crate::clone_hot(k)));
        }
        out.write(Object::None);
        0
    }
}

/// `set.discard` (`remove`: a miss declines, for the full call's
/// `KeyError`) of a key that compares natively (see `LeafProbe`): the
/// removed key is the same object, or an equal `str`, `int` or tuple of
/// them, so its release frees nothing that runs code.
unsafe fn set_take(out: *mut Object, recv: *const Object, a: *const Object, remove: bool) -> u32 {
    // SAFETY: as `Kernel`'s.
    unsafe {
        let (Object::Set(s), k) = (&*recv, &*a) else {
            return 1;
        };
        let Some(probe) = crate::object::LeafProbe::new(k) else {
            return 1;
        };
        let Some(m) = s.peek_mut() else {
            return 1;
        };
        match m.swap_take(&probe) {
            Some(key) => crate::drop_hot(key.0),
            None if !remove && probe.miss_is_exact() => {}
            None => return 1,
        }
        out.write(Object::None);
        0
    }
}

unsafe extern "C" fn k_set_discard(
    out: *mut Object,
    recv: *const Object,
    a: *const Object,
    _: *const Object,
    _: *const crate::object::BuiltinFn,
) -> u32 {
    // SAFETY: as `Kernel`'s.
    unsafe { set_take(out, recv, a, false) }
}

unsafe extern "C" fn k_set_remove(
    out: *mut Object,
    recv: *const Object,
    a: *const Object,
    _: *const Object,
    _: *const crate::object::BuiltinFn,
) -> u32 {
    // SAFETY: as `Kernel`'s.
    unsafe { set_take(out, recv, a, true) }
}

/// The receiver's and the first argument's text, both exact `str`s.
///
/// # Safety
///
/// As [`Kernel`]'s.
unsafe fn two_strs<'s>(
    recv: *const Object,
    a: *const Object,
) -> Option<(
    &'s crate::shared_value::SharedStr,
    &'s crate::shared_value::SharedStr,
)> {
    // SAFETY: the caller's contract.
    match unsafe { (&*recv, &*a) } {
        (Object::Str(s), Object::Str(p)) => Some((s, p)),
        _ => None,
    }
}

/// The text from character `start` (a `str` method's optional start
/// position, null for none) of the receiver `s`, when `s` is ASCII (its
/// byte offsets are its indices): `Some(None)` for a start past the end
/// (the methods' "not found"), `None` to decline.
///
/// # Safety
///
/// As [`Kernel`]'s.
unsafe fn ascii_from<'s>(
    s: &'s crate::shared_value::SharedStr,
    start: *const Object,
) -> Option<Option<(&'s [u8], usize)>> {
    let bytes = s.as_bytes();
    if start.is_null() {
        return Some(Some((bytes, 0)));
    }
    // SAFETY: the caller's contract.
    let Object::Int(i) = (unsafe { &*start }) else {
        return None;
    };
    if crate::shared_value::SharedStr::char_count(s) != bytes.len() {
        return None;
    }
    let n = bytes.len() as i64;
    let i = if *i < 0 { (*i + n).max(0) } else { *i };
    if i > n {
        return Some(None);
    }
    Some(Some((&bytes[i as usize..], i as usize)))
}

unsafe extern "C" fn k_str_startswith(
    out: *mut Object,
    recv: *const Object,
    a: *const Object,
    b: *const Object,
    _: *const crate::object::BuiltinFn,
) -> u32 {
    // SAFETY: as `Kernel`'s.
    unsafe {
        let Some((s, p)) = two_strs(recv, a) else {
            return 1;
        };
        let Some(from) = ascii_from(s, b) else {
            return 1;
        };
        let hit = from.is_some_and(|(t, _)| t.starts_with(p.as_bytes()));
        out.write(Object::Bool(hit));
        0
    }
}

unsafe extern "C" fn k_str_endswith(
    out: *mut Object,
    recv: *const Object,
    a: *const Object,
    b: *const Object,
    _: *const crate::object::BuiltinFn,
) -> u32 {
    // SAFETY: as `Kernel`'s.
    unsafe {
        let Some((s, p)) = two_strs(recv, a) else {
            return 1;
        };
        let Some(from) = ascii_from(s, b) else {
            return 1;
        };
        let hit = from.is_some_and(|(t, _)| t.ends_with(p.as_bytes()));
        out.write(Object::Bool(hit));
        0
    }
}

/// An `isdigit`-style predicate of an ASCII string: true when non-empty
/// and every byte passes `test`. A non-ASCII string declines (unless an
/// earlier ASCII byte already failed).
///
/// # Safety
///
/// As [`Kernel`]'s.
unsafe fn ascii_all(out: *mut Object, recv: *const Object, test: fn(u8) -> bool) -> u32 {
    // SAFETY: the caller's contract.
    unsafe {
        let Object::Str(s) = &*recv else {
            return 1;
        };
        let bytes = s.as_bytes();
        let mut all = !bytes.is_empty();
        for &c in bytes {
            if c >= 0x80 {
                return 1;
            }
            if !test(c) {
                all = false;
                break;
            }
        }
        out.write(Object::Bool(all));
        0
    }
}

unsafe extern "C" fn k_str_isdigit(
    out: *mut Object,
    recv: *const Object,
    _: *const Object,
    _: *const Object,
    _: *const crate::object::BuiltinFn,
) -> u32 {
    // SAFETY: as `Kernel`'s.
    unsafe { ascii_all(out, recv, |c| c.is_ascii_digit()) }
}

unsafe extern "C" fn k_str_isalpha(
    out: *mut Object,
    recv: *const Object,
    _: *const Object,
    _: *const Object,
    _: *const crate::object::BuiltinFn,
) -> u32 {
    // SAFETY: as `Kernel`'s.
    unsafe { ascii_all(out, recv, |c| c.is_ascii_alphabetic()) }
}

unsafe extern "C" fn k_str_isspace(
    out: *mut Object,
    recv: *const Object,
    _: *const Object,
    _: *const Object,
    _: *const crate::object::BuiltinFn,
) -> u32 {
    // SAFETY: as `Kernel`'s.
    unsafe { ascii_all(out, recv, |c| crate::unicode_case::is_space(char::from(c))) }
}

unsafe extern "C" fn k_str_find(
    out: *mut Object,
    recv: *const Object,
    a: *const Object,
    b: *const Object,
    _: *const crate::object::BuiltinFn,
) -> u32 {
    // SAFETY: as `Kernel`'s.
    unsafe {
        let Some((s, p)) = two_strs(recv, a) else {
            return 1;
        };
        // (An ASCII receiver's byte offsets are its indices.)
        if crate::shared_value::SharedStr::char_count(s) != s.len() {
            return 1;
        }
        let Some(from) = ascii_from(s, b) else {
            return 1;
        };
        let at = from
            .and_then(|(_, i)| crate::builtins::substr_find(&s[i..], p).map(|k| (k + i) as i64));
        out.write(Object::Int(at.unwrap_or(-1)));
        0
    }
}

/// A `str` method whose body runs no Python code for any arguments
/// (`lower`, `upper`, the registered pure bodies).
unsafe extern "C" fn k_str_body(
    out: *mut Object,
    recv: *const Object,
    a: *const Object,
    b: *const Object,
    f: *const crate::object::BuiltinFn,
) -> u32 {
    // SAFETY: as `Kernel`'s.
    unsafe { run_body(out, recv, a, b, f) }
}

unsafe extern "C" fn k_str_strip(
    out: *mut Object,
    recv: *const Object,
    a: *const Object,
    b: *const Object,
    f: *const crate::object::BuiltinFn,
) -> u32 {
    // SAFETY: as `Kernel`'s.
    unsafe {
        if !a.is_null() && !matches!(&*a, Object::Str(_) | Object::None) {
            return 1;
        }
        run_body(out, recv, a, b, f)
    }
}

unsafe extern "C" fn k_str_split(
    out: *mut Object,
    recv: *const Object,
    a: *const Object,
    b: *const Object,
    f: *const crate::object::BuiltinFn,
) -> u32 {
    // SAFETY: as `Kernel`'s.
    unsafe {
        if !a.is_null() && !matches!(&*a, Object::Str(_) | Object::None) {
            return 1;
        }
        run_body(out, recv, a, b, f)
    }
}

unsafe extern "C" fn k_str_join(
    out: *mut Object,
    recv: *const Object,
    a: *const Object,
    b: *const Object,
    f: *const crate::object::BuiltinFn,
) -> u32 {
    // SAFETY: as `Kernel`'s; an exact list's or tuple's exact strings
    // join with no iteration hook.
    unsafe {
        let strs = match &*a {
            Object::List(l) => l
                .peek()
                .is_some_and(|xs| xs.iter().all(|x| matches!(x, Object::Str(_)))),
            Object::Tuple(t) => t.iter().all(|x| matches!(x, Object::Str(_))),
            _ => false,
        };
        if !strs {
            return 1;
        }
        run_body(out, recv, a, b, f)
    }
}

unsafe extern "C" fn k_str_replace(
    out: *mut Object,
    recv: *const Object,
    a: *const Object,
    b: *const Object,
    f: *const crate::object::BuiltinFn,
) -> u32 {
    // SAFETY: as `Kernel`'s.
    unsafe {
        if !matches!((&*a, &*b), (Object::Str(_), Object::Str(_))) {
            return 1;
        }
        run_body(out, recv, a, b, f)
    }
}

/// [`h_container`]'s raise: the error in `st`, `st.len` and `st.pc` past
/// the instruction.
const CONTAINER_RAISED: u64 = u64::MAX - 1;

/// A container instruction (`BINARY_SUBSCR`, `BINARY_SLICE`, `STORE_SUBSCR`,
/// `LIST_APPEND`, `UNPACK_SEQUENCE`, `UNPACK_EX`, `LOAD_SUPER_ATTR`, and an
/// `except` clause's entry, test and exit; the one at `ins`, the `pc`th of
/// `code`) on the top of a `len`-deep stack, as the core loop's arm runs
/// it: the new depth, [`CONTAINER_RAISED`] (only when `can_raise`: the
/// whole stack is on the frame's), or `u64::MAX` declined untouched.
unsafe extern "C" fn h_container(
    st: *mut State,
    ins: *const weavepy_compiler::Instruction,
    code: *const CodeObject,
    pc: u64,
    len: u64,
    can_raise: u64,
) -> u64 {
    // SAFETY: the code passes its live state, one of its own instructions
    // and its code, and its stack depth.
    unsafe {
        let st = &mut *st;
        let (ins, len) = (*ins, len as usize);
        if ins.op == OpCode::LoadSuperAttr {
            return Interpreter::core_super_attr(&*code, ins, pc as usize, st.stack, len)
                .map_or(u64::MAX, |n| n as u64);
        }
        if ins.op == OpCode::LoadSpecial {
            return Interpreter::core_load_special(&*code, ins, pc as usize, st.stack, len, st.cap)
                .map_or(u64::MAX, |n| n as u64);
        }
        if matches!(
            ins.op,
            OpCode::PushExcInfo | OpCode::CheckExcMatch | OpCode::PopExcept
        ) {
            let interp = &mut *st.interp.cast_mut();
            return interp
                .core_exc_op(ins, st.stack, len, st.cap)
                .map_or(u64::MAX, |n| n as u64);
        }
        if let Some(n) = Interpreter::core_container_op(ins, st.stack, len, st.cap) {
            return n as u64;
        }
        // `d[key]` of an exact dict missing a key that compares natively:
        // its `KeyError`, raised here (see `h_catch`).
        if ins.op == OpCode::BinarySubscr && len >= 2 && can_raise != 0 && !st.sw.is_null() {
            let (c, k) = (&*st.stack.add(len - 2), &*st.stack.add(len - 1));
            if let (Object::Dict(d), Object::Str(_) | Object::Int(_) | Object::Tuple(_)) = (c, k) {
                if let (Some(probe), true) = (crate::object::LeafProbe::new(k), droppable(c)) {
                    let missing = d
                        .peek()
                        .is_some_and(|d| d.get(&probe).is_none() && probe.miss_is_exact());
                    if missing {
                        let err = crate::error::key_error_object(k.clone());
                        crate::drop_hot(st.stack.add(len - 1).read());
                        crate::drop_hot(st.stack.add(len - 2).read());
                        st.len = len - 2;
                        st.pc = pc as usize + 1;
                        st.err = Some(err);
                        return CONTAINER_RAISED;
                    }
                }
            }
        }
        // An instance whose class's `__setitem__` is a native fast store
        // the site cached (as the core loop's arm runs it).
        if ins.op == OpCode::StoreSubscr && len >= 3 {
            let ops = std::slice::from_raw_parts(st.stack.add(len - 2), 2);
            let Some(fast) = (*st.interp).core_native_store(&*code, pc as usize, ops) else {
                return u64::MAX;
            };
            // Bitwise views in the `__setitem__` order, never dropped.
            let views = std::mem::ManuallyDrop::new([
                st.stack.add(len - 2).read(),
                st.stack.add(len - 1).read(),
                st.stack.add(len - 3).read(),
            ]);
            let Some(Ok(v)) = fast(&views[..]) else {
                return u64::MAX;
            };
            crate::drop_hot(v);
            crate::drop_hot(st.stack.add(len - 1).read());
            crate::drop_hot(st.stack.add(len - 2).read());
            crate::drop_hot(st.stack.add(len - 3).read());
            return (len - 3) as u64;
        }
        // An instance whose class's `__getitem__` is a native fast
        // subscript the site cached.
        if ins.op != OpCode::BinarySubscr || len < 2 {
            return u64::MAX;
        }
        let ops = std::slice::from_raw_parts(st.stack.add(len - 2), 2);
        let Some(fast) = (*st.interp).core_native_subscript(&*code, pc as usize, ops) else {
            return u64::MAX;
        };
        // (An error leaves the subscript to the core loop, which raises it.)
        let Some(Ok(v)) = fast(ops) else {
            return u64::MAX;
        };
        #[cfg(test)]
        crate::NATIVE_FAST_SUBSCRIPTS.with(|calls| calls.set(calls.get() + 1));
        crate::drop_hot(st.stack.add(len - 1).read());
        crate::drop_hot(st.stack.add(len - 2).read());
        st.stack.add(len - 2).write(v);
        (len - 1) as u64
    }
}

/// `CONTAINS_OP` of the item at `at` and the container above it: `0` or
/// `1` (found; both left the stack), anything else declined untouched.
unsafe extern "C" fn h_contains(at: *mut Object) -> u32 {
    // SAFETY: the code passes two initialized stack slots.
    unsafe {
        let (item, container) = (&*at, &*at.add(1));
        if !droppable(item) || !droppable(container) {
            return 2;
        }
        let Some(found) = Interpreter::leaf_contains(container, item) else {
            return 2;
        };
        crate::drop_hot(at.add(1).read());
        crate::drop_hot(at.read());
        u32::from(found)
    }
}

/// `CONTAINS_OP` of the item at `item` in the container at `cont`, each
/// read in place: a local's or constant's (borrowed), or a stack slot's
/// (owned there, its bit in `owned`: `1` the item, `2` the container),
/// which leaves the stack. `0` or `1` (found), anything else declined
/// untouched.
unsafe fn contains_fast(item: *const Object, cont: *const Object, owned: u32) -> u32 {
    // SAFETY: the code passes two initialized values, the owned ones in
    // stack slots.
    unsafe {
        let (i, c) = (&*item, &*cont);
        if owned != 0 && !(droppable(i) && droppable(c)) {
            return 2;
        }
        let Some(found) = Interpreter::leaf_contains(c, i) else {
            return 2;
        };
        if owned & 2 != 0 {
            crate::drop_hot(cont.read());
        }
        if owned & 1 != 0 {
            crate::drop_hot(item.read());
        }
        u32::from(found)
    }
}

/// `BINARY_SUBSCR` of the container at `cont` by the key at `key`, each
/// read in place as [`contains_fast`] reads its operands (`owned`: `1`
/// the container, `2` the key): an exact `dict` by a key that compares
/// natively, a `list` or `tuple` by an in-range `int`, or a `str` by an
/// in-range `int` (an ASCII one's). `0` the item written to `out` (the
/// owned operands released first), anything else declined untouched.
unsafe fn subscr_fast(
    out: *mut Object,
    cont: *const Object,
    key: *const Object,
    owned: u32,
) -> u32 {
    // SAFETY: the code passes two initialized values, the owned ones in
    // stack slots, and a free slot (or the container's own) for the item.
    unsafe {
        let (c, k) = (&*cont, &*key);
        if owned != 0 && !(droppable(c) && droppable(k)) {
            return 1;
        }
        let index = |n: usize| -> Option<usize> {
            let Object::Int(i) = *k else {
                return None;
            };
            let i = if i < 0 { i.checked_add(n as i64)? } else { i };
            usize::try_from(i).ok().filter(|&i| i < n)
        };
        let r = match c {
            Object::Dict(d) => {
                let Some(probe) = crate::object::LeafProbe::new(k) else {
                    return 1;
                };
                let Some(v) = d.peek().and_then(|m| m.get_hot(&probe)) else {
                    return 1;
                };
                crate::clone_hot(v)
            }
            Object::List(xs) => {
                let Some(xs) = xs.peek() else {
                    return 1;
                };
                let Some(i) = index(xs.len()) else {
                    return 1;
                };
                crate::clone_hot(&xs[i])
            }
            Object::Tuple(t) => {
                let Some(i) = index(t.len()) else {
                    return 1;
                };
                crate::clone_hot(&t[i])
            }
            Object::Str(s) => {
                let bytes = s.as_bytes();
                if crate::shared_value::SharedStr::char_count(s) != bytes.len() {
                    return 1;
                }
                let Some(i) = index(bytes.len()) else {
                    return 1;
                };
                Object::from_char(char::from(bytes[i]))
            }
            _ => return 1,
        };
        if owned & 2 != 0 {
            crate::drop_hot(key.read());
        }
        if owned & 1 != 0 {
            crate::drop_hot(cont.read());
        }
        out.write(r);
        0
    }
}

/// `UNPACK_SEQUENCE n` of the exact `n`-item tuple or list at `src`, read
/// in place (a local's or constant's), or owned in the stack slot `dst`
/// itself (`owned`), which leaves the stack: its items to `dst` and up,
/// the last item first, as the core loop's arm pushes them. `0` unpacked,
/// anything else declined untouched.
unsafe fn unpack_fast(
    dst: *mut Object,
    src: *const Object,
    n: u64,
    owned: u32,
    interp: &Interpreter,
) -> u32 {
    // SAFETY: the code passes an initialized value (in `dst` itself when
    // owned) and `n` free slots from `dst` up (the first the owned
    // value's).
    unsafe {
        let n = n as usize;
        let s = &*src;
        let fits = match s {
            Object::Tuple(t) => t.len() == n,
            Object::List(l) => l.peek().is_some_and(|xs| xs.len() == n),
            _ => false,
        };
        // An owned sequence's release frees nothing that could finalize
        // when this is its only reference (its items outlive it on the
        // stack); a shared one's is a plain decrement.
        let unique = match s {
            Object::Tuple(t) => crate::shared_value::ThinArc::strong_count(t) == 1,
            Object::List(l) => Rc::strong_count(l) == 1,
            _ => false,
        };
        if !fits || (owned != 0 && !unique && !droppable(s)) {
            return 1;
        }
        let seq = if owned != 0 { Some(src.read()) } else { None };
        let s = seq.as_ref().unwrap_or(s);
        match s {
            Object::Tuple(t) => {
                for (k, item) in t.iter().rev().enumerate() {
                    dst.add(k).write(crate::clone_hot(item));
                }
            }
            Object::List(l) => {
                let xs = l.peek().expect("checked above");
                for (k, item) in xs.iter().rev().enumerate() {
                    dst.add(k).write(crate::clone_hot(item));
                }
            }
            _ => unreachable!("checked above"),
        }
        // (A dead tuple's allocation is recycled, as `release` does it.)
        match seq {
            Some(seq @ Object::Tuple(_)) => interp.maybe_donate_tuple(seq),
            Some(seq) => crate::drop_hot(seq),
            None => {}
        }
        0
    }
}

/// `STORE_SUBSCR` of the value at `val` into the container at `cont` by
/// the key at `key`, each read in place as [`contains_fast`] reads its
/// operands (`owned`: `1` the value, `2` the container, `4` the key): an
/// exact `list` by an in-range `int`, or an exact `dict` by a key that
/// compares natively, as the core loop's arm stores. `0` stored (an owned
/// value moved in, a borrowed one copied; the other owned operands and
/// the displaced value released), anything else declined untouched.
unsafe fn store_subscr_fast(
    val: *const Object,
    cont: *const Object,
    key: *const Object,
    owned: u32,
) -> u32 {
    // SAFETY: the code passes three initialized values, the owned ones in
    // stack slots. The store runs no code; a displaced value is released
    // after it (a finalizer it queues runs before the next instruction).
    unsafe {
        let (c, k) = (&*cont, &*key);
        if !Interpreter::core_droppable(c) || (owned & 4 != 0 && !droppable(k)) {
            return 1;
        }
        let displaced_ok = |old: &Object| {
            matches!(
                old,
                Object::Int(_) | Object::Float(_) | Object::Bool(_) | Object::None
            ) || Interpreter::core_droppable(old)
        };
        let value = || {
            if owned & 1 != 0 {
                val.read()
            } else {
                crate::clone_hot(&*val)
            }
        };
        let old = match (c, k) {
            (Object::List(xs), Object::Int(i)) => {
                let Some(xs) = xs.peek_mut() else {
                    return 1;
                };
                let n = xs.len() as i64;
                let i = if *i < 0 { *i + n } else { *i };
                if i < 0 || i >= n || !displaced_ok(&xs[i as usize]) {
                    return 1;
                }
                std::mem::replace(&mut xs[i as usize], value())
            }
            (Object::Dict(cell), Object::Str(_) | Object::Int(_) | Object::Tuple(_)) => {
                if crate::capi_watchers::dicts_active() {
                    return 1;
                }
                let Some(probe) = crate::object::LeafProbe::new(k) else {
                    return 1;
                };
                let Some(d) = cell.peek_mut() else {
                    return 1;
                };
                // One probe finds the entry or the slot a new key takes.
                let (old, changed) = match d.probe_entry(&probe) {
                    crate::dictmap::ProbeEntry::Occupied(e) => {
                        let slot = e.into_mut();
                        if !displaced_ok(slot) {
                            return 1;
                        }
                        let old = std::mem::replace(slot, value());
                        let changed = !old.is_same(slot);
                        (old, changed)
                    }
                    crate::dictmap::ProbeEntry::Vacant(e) => {
                        if !probe.miss_is_exact() {
                            return 1;
                        }
                        e.insert(crate::object::DictKey(crate::clone_hot(k)), value());
                        (Object::None, true)
                    }
                };
                if changed {
                    crate::object::dict_mutation_event(cell);
                }
                old
            }
            _ => return 1,
        };
        if owned & 4 != 0 {
            crate::drop_hot(key.read());
        }
        if owned & 2 != 0 {
            crate::drop_hot(cont.read());
        }
        crate::drop_hot(old);
        0
    }
}

/// Copy the borrowed operands among `ops` (bit `k` of `owned` clear) to
/// the free stack slots `at.add(k)`, so that all of them are on the
/// stack, as the container helpers take them.
///
/// # Safety
///
/// Each pointer addresses an initialized value; the slots of the
/// borrowed ones are free.
unsafe fn own_operands(at: *mut Object, ops: &[*const Object], owned: u32) {
    for (k, &p) in ops.iter().enumerate() {
        if owned & (1 << k) == 0 {
            // SAFETY: the caller's contract.
            unsafe { at.add(k).write(crate::clone_hot(&*p)) };
        }
    }
}

/// `CONTAINS_OP` of a local's, constant's or stack slot's item and
/// container (bit `k` of `owned`: the one owned in the slot `at.add(k)`),
/// read in place where they lie (see [`contains_fast`]); any other shape
/// with both operands copied to the stack and through [`h_contains`],
/// whose result it is.
unsafe extern "C" fn h_contains_ref(
    item: *const Object,
    cont: *const Object,
    owned: u32,
    at: *mut Object,
) -> u32 {
    // SAFETY: the code passes the operands, and the two slots from `at`
    // (the owned operands' own).
    unsafe {
        let r = contains_fast(item, cont, owned);
        if r <= 1 {
            return r;
        }
        own_operands(at, &[item, cont], owned);
        h_contains(at)
    }
}

/// `BINARY_SUBSCR` (the `pc`th instruction `ins` of `code`) on a stack
/// `len` deep whose top two slots are the container's and key's (each
/// owned there, by the bits of `flags`, or a local's or constant's at
/// `cont` / `key`): through [`subscr_fast`], else with both operands on
/// the stack through [`h_container`] (`flags` bit 8: it may raise), whose
/// result it is.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn h_subscr_ref(
    st: *mut State,
    ins: *const weavepy_compiler::Instruction,
    code: *const CodeObject,
    pc: u64,
    len: u64,
    cont: *const Object,
    key: *const Object,
    flags: u64,
) -> u64 {
    // SAFETY: the code passes its live state, one of its own instructions
    // and its code, its depth, and the operands.
    unsafe {
        let at = (*st).stack.add(len as usize - 2);
        let owned = (flags & 0xff) as u32;
        if subscr_fast(at, cont, key, owned) == 0 {
            return len - 1;
        }
        own_operands(at, &[cont, key], owned);
        h_container(st, ins, code, pc, len, flags >> 8)
    }
}

/// `UNPACK_SEQUENCE n` as [`h_subscr_ref`] runs a subscript: through
/// [`unpack_fast`] on the sequence in the top slot or at `src`, else on
/// the stack through [`h_container`].
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn h_unpack_ref(
    st: *mut State,
    ins: *const weavepy_compiler::Instruction,
    code: *const CodeObject,
    pc: u64,
    len: u64,
    src: *const Object,
    n: u64,
    flags: u64,
) -> u64 {
    // SAFETY: as `h_subscr_ref`'s.
    unsafe {
        let at = (*st).stack.add(len as usize - 1);
        let owned = (flags & 0xff) as u32;
        if unpack_fast(at, src, n, owned, &*(*st).interp) == 0 {
            return len - 1 + n;
        }
        own_operands(at, &[src], owned);
        h_container(st, ins, code, pc, len, flags >> 8)
    }
}

/// `STORE_SUBSCR` as [`h_subscr_ref`] runs a subscript: through
/// [`store_subscr_fast`] on the value, container and key in the top
/// three slots or at `val`, `cont`, `key`, else on the stack through
/// [`h_container`].
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn h_store_subscr_ref(
    st: *mut State,
    ins: *const weavepy_compiler::Instruction,
    code: *const CodeObject,
    pc: u64,
    len: u64,
    val: *const Object,
    cont: *const Object,
    key: *const Object,
    flags: u64,
) -> u64 {
    // SAFETY: as `h_subscr_ref`'s.
    unsafe {
        let at = (*st).stack.add(len as usize - 3);
        let owned = (flags & 0xff) as u32;
        if store_subscr_fast(val, cont, key, owned) == 0 {
            return len - 3;
        }
        own_operands(at, &[val, cont, key], owned);
        h_container(st, ins, code, pc, len, flags >> 8)
    }
}

/// `BINARY_OP` of `kind` over the values at `at` and above it, for the
/// shapes the core loop's arm runs (two numbers; strings' concatenation,
/// repetition and `%` formatting; two tuples' or lists' concatenation; a
/// natively served instance's operator):
/// `0` the result at `at` (the operands released), anything else declined
/// untouched.
unsafe extern "C" fn h_binop(at: *mut Object, kind: u32) -> u32 {
    // SAFETY: the code passes two initialized stack slots, and a kind the
    // compiler emitted (`BinOpKind` is `repr(u8)`).
    unsafe {
        let (a, b) = (&*at, &*at.add(1));
        // Two numbers (the core loop's in-line arm).
        let num = |o: &Object| match *o {
            Object::Int(i) => Some(crate::leaf_plan::V::I(i)),
            Object::Float(x) => Some(crate::leaf_plan::V::F(x)),
            _ => None,
        };
        if let (Some(x), Some(y)) = (num(a), num(b)) {
            let r = match crate::leaf_plan::binary(x, y, kind as u8) {
                Some(crate::leaf_plan::V::I(i)) => Object::Int(i),
                Some(crate::leaf_plan::V::F(f)) => Object::Float(f),
                _ => return 1,
            };
            at.write(r);
            return 0;
        }
        let kind: BinOpKind = std::mem::transmute(kind as u8);
        let r = match (a, b) {
            (Object::Str(_), _) | (Object::Int(_), Object::Str(_)) => {
                Interpreter::core_str_binop(a, b, kind)
            }
            // Two exact tuples' or lists' concatenation.
            (Object::Tuple(_), Object::Tuple(_)) | (Object::List(_), Object::List(_))
                if kind == BinOpKind::Add && droppable(a) && droppable(b) =>
            {
                Interpreter::core_seq_concat(a, b)
            }
            (Object::Instance(i), _) if i.cls_raw().native_kind.get() != 0 => {
                match Interpreter::core_native_binop(kind, a, b) {
                    Some(Ok(r)) if droppable(a) && droppable(b) => Some(r),
                    _ => None,
                }
            }
            _ => None,
        };
        let Some(r) = r else {
            return 1;
        };
        crate::drop_hot(at.add(1).read());
        crate::drop_hot(at.read());
        at.write(r);
    }
    0
}

/// `BINARY_OP` of `kind` over the values at `a` and `b` (a local's or a
/// constant's, read in place) for a site that has seen a natively served
/// operand (see `crate::native_site`): `0` the result written to the free
/// slot `dst`, anything else declined untouched (the core loop then runs
/// the instruction, and raises its errors).
unsafe extern "C" fn h_binop_ref(
    dst: *mut Object,
    a: *const Object,
    b: *const Object,
    kind: u32,
) -> u32 {
    // SAFETY: the code passes a free stack slot, two initialized values
    // that outlive the call (nothing it runs can reach them), and a kind
    // the compiler emitted (`BinOpKind` is `repr(u8)`).
    unsafe {
        let (a, b) = (&*a, &*b);
        let num = |o: &Object| match *o {
            Object::Int(i) => Some(crate::leaf_plan::V::I(i)),
            Object::Float(x) => Some(crate::leaf_plan::V::F(x)),
            _ => None,
        };
        let r = if let (Some(x), Some(y)) = (num(a), num(b)) {
            match crate::leaf_plan::binary(x, y, kind as u8) {
                Some(crate::leaf_plan::V::I(i)) => Object::Int(i),
                Some(crate::leaf_plan::V::F(f)) => Object::Float(f),
                _ => return 1,
            }
        } else {
            let kind: BinOpKind = std::mem::transmute(kind as u8);
            match a {
                Object::Instance(i) if i.cls_raw().native_kind.get() != 0 => {
                    match Interpreter::core_native_binop(kind, a, b) {
                        Some(Ok(r)) => r,
                        _ => return 1,
                    }
                }
                _ => return 1,
            }
        };
        dst.write(r);
    }
    0
}

/// `COMPARE_OP` with oparg `arg` over the values at `at` and above it, for
/// the shapes the core loop's arm runs (two ints, two floats, two natively
/// served instances): `0` the `bool` at `at` (the operands released),
/// anything else declined untouched.
unsafe extern "C" fn h_compare(at: *mut Object, arg: u32) -> u32 {
    // SAFETY: the code passes two initialized stack slots, and the oparg
    // the compiler emitted (as `compare_op_step` reads it).
    unsafe {
        let (a, b) = (&*at, &*at.add(1));
        let kind: CompareKind = std::mem::transmute((arg & !COMPARE_OP_TO_BOOL_FLAG) as u8);
        let ord = match (a, b) {
            (Object::Int(x), Object::Int(y)) => x.cmp(y),
            (Object::Float(x), Object::Float(y)) => match x.partial_cmp(y) {
                Some(o) => o,
                None => return 1,
            },
            (Object::Instance(i), Object::Instance(_)) if i.cls_raw().native_kind.get() != 0 => {
                let r = match Interpreter::core_native_compare(kind, a, b) {
                    Some(Ok(r)) if droppable(a) && droppable(b) => r,
                    _ => return 1,
                };
                crate::drop_hot(at.add(1).read());
                crate::drop_hot(at.read());
                at.write(r);
                return 0;
            }
            _ => return 1,
        };
        let r = match kind {
            CompareKind::Lt => ord.is_lt(),
            CompareKind::LtE => ord.is_le(),
            CompareKind::Eq => ord.is_eq(),
            CompareKind::NotEq => ord.is_ne(),
            CompareKind::Gt => ord.is_gt(),
            CompareKind::GtE => ord.is_ge(),
        };
        at.write(Object::Bool(r));
    }
    0
}

/// `COMPARE_OP` with oparg `arg` over the values at `at` and above it,
/// for the shapes the leaf arm runs (two ints, two floats, two strings):
/// `0` or `1` the answer (the operands released), anything else declined
/// untouched.
unsafe extern "C" fn h_compare_bool(at: *mut Object, arg: u32) -> u32 {
    // SAFETY: the code passes two initialized stack slots, and the oparg
    // the compiler emitted (as `compare_op_step` reads it).
    unsafe {
        let (a, b) = (&*at, &*at.add(1));
        let kind: CompareKind = std::mem::transmute((arg & !COMPARE_OP_TO_BOOL_FLAG) as u8);
        let ord = match (a, b) {
            (Object::Int(x), Object::Int(y)) => x.cmp(y),
            (Object::Float(x), Object::Float(y)) => match x.partial_cmp(y) {
                Some(o) => o,
                None => return 2,
            },
            (Object::Str(x), Object::Str(y)) => x.as_bytes().cmp(y.as_bytes()),
            _ => return 2,
        };
        if !droppable(a) || !droppable(b) {
            return 2;
        }
        let r = match kind {
            CompareKind::Lt => ord.is_lt(),
            CompareKind::LtE => ord.is_le(),
            CompareKind::Eq => ord.is_eq(),
            CompareKind::NotEq => ord.is_ne(),
            CompareKind::Gt => ord.is_gt(),
            CompareKind::GtE => ord.is_ge(),
        };
        crate::drop_hot(at.add(1).read());
        crate::drop_hot(at.read());
        u32::from(r)
    }
}

/// `TO_BOOL` of the value at `at`, for the shapes the leaf arm runs
/// (scalars, strings, lists, tuples, dicts, an instance with a native or
/// no `__bool__` / `__len__`): `0` or `1` the truth (the operand
/// released), anything else declined untouched.
unsafe extern "C" fn h_truth(st: *mut State, at: *mut Object) -> u32 {
    // SAFETY: the code passes its live state and an initialized stack
    // slot; the truth tests run no Python code.
    unsafe {
        let v = &*at;
        let b = match v {
            Object::Bool(b) => *b,
            Object::Int(i) => *i != 0,
            Object::None => false,
            Object::Str(s) => !s.is_empty(),
            Object::Float(f) => *f != 0.0,
            Object::List(l) => match l.peek() {
                Some(l) => !l.is_empty(),
                None => return 2,
            },
            Object::Tuple(t) => !t.is_empty(),
            Object::Dict(d) => match d.peek() {
                Some(d) => !d.is_empty(),
                None => return 2,
            },
            Object::Instance(_) => match (*(*st).interp).leaf_instance_truth(v) {
                Some(b) => b,
                None => return 2,
            },
            _ => return 2,
        };
        if !droppable(v) {
            return 2;
        }
        crate::drop_hot(at.read());
        u32::from(b)
    }
}

/// `IS_OP` with oparg `arg` over the values at `at` and above it, as the
/// core loop's arm runs it: `0` or `1` the answer (both released),
/// anything else declined untouched.
unsafe extern "C" fn h_is(at: *mut Object, arg: u32) -> u32 {
    // SAFETY: the code passes two initialized stack slots.
    unsafe {
        let (a, b) = (&*at, &*at.add(1));
        if !droppable(a) || !droppable(b) {
            return 2;
        }
        let same = a.is_same(b);
        crate::drop_hot(at.add(1).read());
        crate::drop_hot(at.read());
        u32::from(same != (arg == 1))
    }
}

/// `LOAD_FAST x` (the `pc`th instruction of `code`) and the call
/// `x.m(<simple arguments>)` after it, as the core loop's fused arm runs it
/// (see `Interpreter::core_local_method`), on a stack `len` deep: `0` the
/// result pushed and `st.pc` / `st.last` past the call, `RAISED` the call
/// raised, anything else declined untouched.
unsafe extern "C" fn h_local_method(
    st: *mut State,
    code: *const CodeObject,
    ext: *const CodeConstObjects,
    pc: u64,
    nlocals: u64,
    len: u64,
) -> u32 {
    // SAFETY: the code passes its live state, its own code and extension,
    // a `LOAD_FAST` of an in-range local, and its stack depth (with room
    // for a push).
    unsafe {
        let st = &mut *st;
        let (code, ext, pc) = (&*code, &*ext, pc as usize);
        let local = &*st.locals.add(code.instructions[pc].arg as usize);
        let Some((r, call_pc)) = (*st.interp).core_local_method(
            code,
            Some(ext),
            local,
            pc,
            st.locals,
            nlocals as usize,
            &ext.objects,
            st.depth_cell,
        ) else {
            return 1;
        };
        st.last = call_pc;
        st.pc = call_pc + 1;
        match r {
            Ok(v) => {
                st.stack.add(len as usize).write(v);
                st.len = len as usize + 1;
                0
            }
            Err(e) => {
                st.len = len as usize;
                st.err = Some(e);
                RAISED
            }
        }
    }
}

/// `LOAD_FAST x` (the `pc`th instruction of `code`) and `x.attr = v`
/// after it, with `v` at stack `slot`, as the core loop's fused arm runs
/// it: `0` stored (the value left the stack), anything else declined
/// untouched.
unsafe extern "C" fn h_store_attr(
    st: *mut State,
    code: *const CodeObject,
    pc: u64,
    slot: *mut Object,
) -> u32 {
    // SAFETY: the code passes its live state, its own code, a `LOAD_FAST`
    // of an in-range local followed by `STORE_ATTR`, and the value's slot.
    unsafe {
        let st = &*st;
        let (code, pc) = (&*code, pc as usize);
        let local = &*st.locals.add(code.instructions[pc].arg as usize);
        let name = code.instructions[pc + 1].arg;
        u32::from(!Interpreter::core_store_local_attr(
            code,
            local,
            pc + 1,
            name,
            &*slot,
        ))
    }
}

/// What a helper that may move the pc did.
const PUSHED: u32 = 0;
/// A fused shape ran: `st.pc`, `st.last` and `st.len` are past it.
const FUSED: u32 = 1;
/// Declined untouched.
const DECLINED: u32 = 2;
/// [`h_call`] declined untouched (its `RAISED` is the raise status).
const CALL_DECLINED: u32 = 3;

/// `LOAD_GLOBAL` (the `pc`th instruction of `code`) on a stack `len`
/// deep, as the core loop's arm runs it: the stamped cache hit, with the
/// arm's fused shapes (`len(local)`, a pure leaf global's call, a class's
/// pure leaf method's call, a plain class's scalar attribute). `PUSHED`
/// the value written to the free slot at `len`, `FUSED` a fused shape
/// ran, `DECLINED` untouched.
unsafe extern "C" fn h_load_global(
    st: *mut State,
    code: *const CodeObject,
    ext: *const CodeConstObjects,
    pc: u64,
    nlocals: u64,
    len: u64,
    cache: *mut GlobalCache,
) -> u32 {
    use weavepy_compiler::InlineCache as IC;
    // SAFETY: the code passes its live state (whose frame is the running
    // one), its own code and extension, one of its `LOAD_GLOBAL`s, its
    // stack depth (with room for a push), and the site's cache. The dict
    // reads are the core loop's arm's: no dict borrow is held while
    // bytecode runs, and nothing here runs code.
    unsafe {
        let st = &mut *st;
        let frame = &*st.frame;
        let interp = &*st.interp;
        let (code, ext, pc, len) = (&*code, &*ext, pc as usize, len as usize);
        if frame.builtins_obj().is_some() {
            return DECLINED;
        }
        let Some(slot) = ext.stamp_slot(pc) else {
            return DECLINED;
        };
        let (gdict, bdict) = (frame.globals.as_ptr(), frame.builtins.as_ptr());
        let gid = crate::specialize::rc_id(&frame.globals);
        let g_stamp = (*gdict).mutation_stamp();
        let b_stamp = (*bdict).mutation_stamp();
        let (hit, builtin) = match code.caches.get(pc as u32) {
            IC::LoadGlobalModule {
                globals_id,
                key_idx,
            } if globals_id == gid && slot.get() == [gid, g_stamp, 0] => {
                ((*gdict).get_index(key_idx as usize), false)
            }
            IC::LoadGlobalBuiltin {
                builtins_id,
                key_idx,
            } if builtins_id == crate::specialize::rc_id(&frame.builtins)
                && !interp.globals_missing_any.get()
                && slot.get() == [gid, g_stamp, b_stamp] =>
            {
                ((*bdict).get_index(key_idx as usize), true)
            }
            _ => return DECLINED,
        };
        let Some((_, v)) = hit else {
            return DECLINED;
        };
        let ins = &code.instructions;
        let dst = st.stack.add(len);
        let op_at = |k: usize| ins.get(k).map(|i| i.op);
        // The fused shapes run Python-free leaf code that needs the
        // recursion-depth cell (a generator's fast step has none).
        let fusing = !st.depth_cell.is_null();
        let fused = |st: &mut State, r: Object, last: usize| {
            dst.write(r);
            st.len = len + 1;
            st.last = last;
            st.pc = last + 1;
            FUSED
        };
        match v {
            // `len(local)`: the length straight off the local.
            Object::Builtin(f)
                if fusing
                    && Rc::as_ptr(f) as usize == interp.leaf_fns().len_ptr
                    && op_at(pc + 1) == Some(OpCode::PushNull)
                    && op_at(pc + 2) == Some(OpCode::LoadFast)
                    && op_at(pc + 3) == Some(OpCode::Call)
                    && ins[pc + 3].arg == 1
                    && (ins[pc + 2].arg as usize) < nlocals as usize =>
            {
                let n = match &*st.locals.add(ins[pc + 2].arg as usize) {
                    Object::List(l) => l.try_borrow().ok().map(|l| l.len()),
                    Object::Tuple(t) => Some(t.len()),
                    Object::Str(s) => Some(crate::object::str_char_len(s)),
                    Object::Dict(d) => d.try_borrow().ok().map(|d| d.len()),
                    v @ Object::Instance(_) => match interp.leaf_instance_len(v) {
                        Some(Object::Int(n)) => usize::try_from(n).ok(),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(n) = n.and_then(|n| i64::try_from(n).ok()) {
                    return fused(st, Object::Int(n), pc + 3);
                }
            }
            // `f(...)` of a pure leaf global function with simple
            // arguments.
            Object::Function(f)
                if fusing
                    && op_at(pc + 1) == Some(OpCode::PushNull)
                    && crate::simple_args_prefix(ins, pc + 2)
                    && crate::fn_is_pure_leaf(f) =>
            {
                if let Some((r, call_pc)) = interp.core_pure_global_call(
                    code,
                    Rc::as_ptr(f),
                    pc + 2,
                    st.locals,
                    nlocals as usize,
                    &ext.objects,
                    st.depth_cell,
                ) {
                    return fused(st, r, call_pc);
                }
            }
            // `C.m(...)` of a class's pure leaf plain or static function,
            // called unbound.
            Object::Type(cls)
                if fusing
                    && op_at(pc + 1) == Some(OpCode::LoadMethodAttr)
                    && crate::simple_args_prefix(ins, pc + 2) =>
            {
                let fp = ext
                    .method_slot(pc + 1)
                    .and_then(|ms| ms.peek_unbound(cls.attr_version.get()))
                    .filter(|&fp| crate::fn_is_pure_leaf(&*fp));
                if let Some(fp) = fp {
                    if let Some((r, call_pc)) = interp.core_pure_global_call(
                        code,
                        fp,
                        pc + 2,
                        st.locals,
                        nlocals as usize,
                        &ext.objects,
                        st.depth_cell,
                    ) {
                        return fused(st, r, call_pc);
                    }
                }
            }
            // `Cls.CONST`: a plain class's scalar attribute straight off
            // the `LOAD_ATTR` site's stamp.
            Object::Type(cls) if op_at(pc + 1) == Some(OpCode::LoadAttr) => {
                if let Some(c) = ext
                    .stamp_slot(pc + 1)
                    .and_then(|s| crate::class_attr_hit(s, cls))
                {
                    return fused(st, c, pc + 1);
                }
            }
            _ => {}
        }
        // The value itself: the native code reads it in line while the
        // dicts' stamps hold (a stamp changes with every mutation, so the
        // value stays where it is). A fused shape that declined fills the
        // cache too: the next read tries the shape again only through a
        // miss.
        let c = &mut *cache;
        // (A namespace handle's bits: one pointer-sized word.)
        c.value = 0;
        c.globals = std::ptr::addr_of!((*st.frame).globals).cast::<u64>().read();
        c.gstamp = g_stamp;
        c.gdata = gdict as u64;
        (c.builtins, c.bstamp, c.bdata) = if builtin {
            let b = std::ptr::addr_of!((*st.frame).builtins)
                .cast::<u64>()
                .read();
            (b, b_stamp, bdict as u64)
        } else {
            (0, 0, 0)
        };
        c.value = std::ptr::from_ref(v) as u64;
        dst.write(crate::clone_hot(v));
        PUSHED
    }
}

/// A `LOAD_GLOBAL` site's in-line cache: the frame's namespaces (their
/// `Rc` handles' bits) and the dicts' stamps the value at `value` was
/// found under (`builtins` 0 for a module global; `globals` 0 empty).
#[repr(C)]
#[derive(Default)]
pub(crate) struct GlobalCache {
    globals: u64,
    gstamp: u64,
    gdata: u64,
    builtins: u64,
    bstamp: u64,
    bdata: u64,
    value: u64,
}

const GC_GLOBALS: i32 = std::mem::offset_of!(GlobalCache, globals) as i32;
const GC_GSTAMP: i32 = std::mem::offset_of!(GlobalCache, gstamp) as i32;
const GC_GDATA: i32 = std::mem::offset_of!(GlobalCache, gdata) as i32;
const GC_BUILTINS: i32 = std::mem::offset_of!(GlobalCache, builtins) as i32;
const GC_BSTAMP: i32 = std::mem::offset_of!(GlobalCache, bstamp) as i32;
const GC_BDATA: i32 = std::mem::offset_of!(GlobalCache, bdata) as i32;
const GC_VALUE: i32 = std::mem::offset_of!(GlobalCache, value) as i32;

/// Where the frame's namespaces, the interpreter's missing-globals flag
/// and a dict's stamp sit.
const F_GLOBALS: usize = std::mem::offset_of!(crate::Frame, globals);
const F_BUILTINS: usize = std::mem::offset_of!(crate::Frame, builtins);
const F_RARE: usize = std::mem::offset_of!(crate::Frame, rare);
const I_MISSING: usize = std::mem::offset_of!(Interpreter, globals_missing_any);
const DICT_STAMP: i32 = crate::object::DictData::STAMP_OFFSET as i32;
const _: () = {
    // (The cache holds a namespace handle's bits, the flag is a byte.)
    assert!(std::mem::size_of::<Rc<crate::sync::RefCell<crate::object::DictData>>>() == 8);
    assert!(std::mem::size_of::<std::cell::Cell<bool>>() == 1);
};

/// `LOAD_ATTR` with the method flag (the `pc`th instruction of `code`)
/// on the receiver atop a `len`-deep stack, as the core loop's arm runs
/// it: `0` the method and its self slot in place of the receiver (one
/// deeper), anything else declined untouched.
unsafe extern "C" fn h_load_method(
    st: *mut State,
    code: *const CodeObject,
    ext: *const CodeConstObjects,
    pc: u64,
    len: u64,
    cache: *mut MethodCache,
    mcache: *const ModCache,
) -> u32 {
    // SAFETY: the code passes its live state, its own code and extension,
    // one of its method loads, and its stack depth (at least one, with
    // room for a push). Nothing here runs code.
    unsafe {
        let st = &mut *st;
        let (code, ext, pc, len) = (&*code, &*ext, pc as usize, len as usize);
        if len == 0 || len >= st.cap {
            return 1;
        }
        let name = code.instructions[pc].arg;
        let top = st.stack.add(len - 1);
        // `gen.send(...)` and the other generator and coroutine methods: the
        // bound method with an empty self slot, as the core loop's arm loads
        // it.
        if matches!(&*top, Object::Generator(_) | Object::Coroutine(_)) {
            return u32::from(!crate::core_gen_method(code, name, top));
        }
        // `module.func(...)`: the attribute with an empty self slot, as the
        // core loop's arm loads it.
        if let Object::Module(m) = &*top {
            // The entry the site last read, while the module's dict is
            // exactly as it was then (its stamp moves with every change).
            let mc = &*mcache;
            let hit = m.dict.peek().and_then(|d| {
                (std::ptr::eq(Rc::as_ptr(&m.dict).cast::<u8>(), mc.dict.get() as *const u8)
                    && d.mutation_stamp() == mc.stamp.get())
                .then(|| d.get_index(mc.idx.get()).map(|(_, v)| crate::clone_hot(v)))
                .flatten()
            });
            let v = match hit {
                Some(v) => v,
                None => {
                    let Some(v) = Interpreter::core_module_attr(code, m, pc, name) else {
                        return 1;
                    };
                    mc.note(code, m, pc);
                    v
                }
            };
            crate::drop_hot(std::mem::replace(&mut *top, v));
            st.stack.add(len).write(Object::Unbound);
            return 0;
        }
        // A receiver class the site saw before: its cached function, while
        // the instance holds no dictionary and no value at or past the
        // name's position (see `Lower::load_method`).
        if let (Object::Instance(inst), false) = (&*top, cache.is_null()) {
            let ver = inst.cls_raw().attr_version.get();
            if let Some(e) = (*cache).poly.iter().find(|e| e.ver == ver) {
                let shadow_free = inst.dict.published().is_none()
                    && match inst.dict.split_peek() {
                        Some(split) => {
                            let (keys, n) = split.keys_and_len();
                            n == 0 || (keys as u64 == e.keys && n <= e.before as usize)
                        }
                        None => false,
                    };
                if shadow_free {
                    // The function's own bits: its class holds it while the
                    // version stands, so a clone takes a count of it.
                    let f = std::mem::ManuallyDrop::new(
                        std::ptr::from_ref(&e.obj).cast::<Object>().read(),
                    );
                    let f = crate::clone_hot(&f);
                    let recv = top.read();
                    top.write(f);
                    st.stack.add(len).write(recv);
                    return 0;
                }
            }
        }
        let Some(ms) = ext.method_slot(pc) else {
            return 1;
        };
        let f = match &*top {
            Object::Instance(inst) => {
                let cls = inst.cls_raw();
                let ver = cls.attr_version.get();
                if !Interpreter::default_getattribute(cls) {
                    return 1;
                }
                // An attribute the instance holds itself (a stored callable):
                // the value with an empty self slot, as the core loop's arm.
                if crate::inst_may_shadow(inst, code, name) {
                    return u32::from(!Interpreter::core_value_method(code, name, top));
                }
                let f = match ms.get_held(ver) {
                    Some(f) => {
                        if !cache.is_null() {
                            fill_method_cache(&mut *cache, cls, ver, &f, ext, name);
                        }
                        Object::Function(f)
                    }
                    None => match ms.get_inst_builtin(ver) {
                        Some(b) => Object::Builtin(b),
                        None => match ext.name_objs.get(name as usize).and_then(|n| match n {
                            Object::Str(n) => crate::class_cached_method_held(
                                cls,
                                crate::SharedStr::as_ptr(n).cast::<u8>() as usize,
                                ver,
                            ),
                            _ => None,
                        }) {
                            Some(f) => {
                                ms.set(ver, &f);
                                Object::Function(f)
                            }
                            None => return 1,
                        },
                    },
                };
                // The receiver moves up.
                let recv = top.read();
                top.write(f);
                st.stack.add(len).write(recv);
                return 0;
            }
            // The class leaves through a plain decrement: its count stays
            // above one.
            Object::Type(cls) if Rc::strong_count(cls) > 1 => {
                match ms.get_held_unbound(cls.attr_version.get()) {
                    Some(f) => f,
                    None => return 1,
                }
            }
            recv => {
                let Some(b) = crate::builtin_recv_tag(recv).and_then(|tag| ms.get_builtin(tag))
                else {
                    return 1;
                };
                let recv = top.read();
                top.write(Object::Builtin(b));
                st.stack.add(len).write(recv);
                return 0;
            }
        };
        crate::drop_hot(top.read());
        top.write(Object::Function(f));
        st.stack.add(len).write(Object::Unbound);
        0
    }
}

/// A method-load site's in-line cache (see `Lower::load_method`): the
/// class version (`0` empty) whose plain function `func` (its payload
/// word) the site loaded, and what proves an instance doesn't shadow it:
/// the class's shared names `keys` (`0` none) and the name's position
/// among them (an instance holding fewer values can't hold the name).
#[repr(C)]
#[derive(Default)]
pub(crate) struct MethodCache {
    ver: u64,
    func: u64,
    keys: u64,
    before: u32,
    /// The function `Object`'s first word (its tag), `func` its second.
    head: u64,
    /// The site's other receiver classes (a polymorphic call), checked by
    /// the helper before its general path, most recent first.
    poly: [MethodEntry; 3],
}

/// One of a [`MethodCache`]'s polymorphic entries.
#[derive(Default, Clone, Copy)]
struct MethodEntry {
    ver: u64,
    /// The function `Object`'s two words.
    obj: [u64; 2],
    keys: u64,
    before: u32,
}

const MC_VER: i32 = std::mem::offset_of!(MethodCache, ver) as i32;
const MC_FUNC: i32 = std::mem::offset_of!(MethodCache, func) as i32;
const MC_KEYS: i32 = std::mem::offset_of!(MethodCache, keys) as i32;
const MC_BEFORE: i32 = std::mem::offset_of!(MethodCache, before) as i32;

/// Record in `cache` that instances of `cls` at version `ver` load the
/// plain function `f` for the name `co_names[name]` (the caller proved
/// this instance doesn't shadow it, and that `cls` keeps the default
/// `__getattribute__`).
fn fill_method_cache(
    cache: &mut MethodCache,
    cls: &crate::types::TypeObject,
    ver: u64,
    f: &Rc<crate::object::PyFunction>,
    ext: &CodeConstObjects,
    name: u32,
) {
    let (keys, before) = match cls.shared_keys.get() {
        Some(keys) => {
            let (Some(Object::Str(n)), Some(&hash)) = (
                ext.name_objs.get(name as usize),
                ext.name_hashes.get(name as usize),
            ) else {
                return;
            };
            (
                std::ptr::from_ref(keys) as u64,
                u32::try_from(keys.names_before(n, hash)).unwrap_or(u32::MAX),
            )
        }
        None => (0, 0),
    };
    let obj = Object::Function(f.clone());
    // SAFETY: an `Object` is 16 bytes; its second word is the payload.
    let [head, word] = unsafe { std::ptr::from_ref(&obj).cast::<[u64; 2]>().read() };
    drop(obj);
    // The entry it replaces moves down the polymorphic list.
    if cache.ver != 0 && cache.ver != ver {
        cache.poly.copy_within(0..2, 1);
        cache.poly[0] = MethodEntry {
            ver: cache.ver,
            obj: [cache.head, cache.func],
            keys: cache.keys,
            before: cache.before,
        };
    }
    cache.ver = ver;
    cache.head = head;
    cache.func = word;
    cache.keys = keys;
    cache.before = before;
}

/// A method load's module-attribute shortcut: the module dict's address
/// and stamp when the site last read it, and the entry's position (`0`
/// address: empty).
#[derive(Default)]
pub(crate) struct ModCache {
    dict: std::cell::Cell<usize>,
    stamp: std::cell::Cell<u64>,
    idx: std::cell::Cell<usize>,
}

impl ModCache {
    /// Remember the entry the site's inline cache just read from `m`.
    #[cold]
    fn note(&self, code: &CodeObject, m: &crate::object::PyModule, pc: usize) {
        use weavepy_compiler::InlineCache as IC;
        let IC::LoadAttrModule { key_idx, .. } = code.caches.get(pc as u32) else {
            return;
        };
        // SAFETY: a read between two instructions.
        if let Some(d) = unsafe { m.dict.peek() } {
            self.dict.set(Rc::as_ptr(&m.dict) as *const u8 as usize);
            self.stamp.set(d.mutation_stamp());
            self.idx.set(key_idx as usize);
        }
    }
}

/// A `LOAD_ATTR` site's `__slots__` member shortcut: the class version and
/// the member's position the site's cache last proved (`ver` 0 empty),
/// read without decoding the site's cache.
#[derive(Default)]
pub(crate) struct SlotCache {
    ver: std::cell::Cell<u64>,
    idx: std::cell::Cell<u32>,
}

/// The `__slots__` member `recv` holds for the `LOAD_ATTR` at `pc`, through
/// the site's [`SlotCache`] (as `leaf_load_attr_recv`'s slot case reads it).
///
/// # Safety
///
/// `cache` is the site's cache.
#[inline(always)]
unsafe fn slot_hit(
    code: &CodeObject,
    recv: &Object,
    pc: usize,
    cache: *const SlotCache,
) -> Option<Object> {
    // SAFETY: the caller's contract.
    let cache = unsafe { &*cache };
    let ver = cache.ver.get();
    let Object::Instance(inst) = recv else {
        return None;
    };
    if ver == 0 || inst.cls_raw().attr_version.get() != ver {
        return None;
    }
    // SAFETY: a read between two instructions, running no code (see
    // `GilCell::peek`); the view ends with the value's clone.
    let slots = unsafe { inst.slots.peek() }?;
    let (k, v) = slots.get_index_hot(cache.idx.get() as usize)?;
    crate::slot_name_matches(code, code.instructions[pc].arg, k)
        .then(|| Interpreter::clone_operand(v))
}

/// Remember the `LOAD_ATTR` site at `pc`'s `__slots__` member, after a
/// read through the site's cache.
#[cold]
fn slot_note(code: &CodeObject, recv: &Object, pc: usize, cache: *const SlotCache) {
    use weavepy_compiler::InlineCache as IC;
    if let (IC::LoadAttrSlot { key_idx, ver }, Object::Instance(inst)) =
        (code.caches.get(pc as u32), recv)
    {
        if inst.cls_raw().attr_version.get() == ver {
            // SAFETY: the site's cache (see `slot_hit`).
            let cache = unsafe { &*cache };
            cache.ver.set(ver);
            cache.idx.set(key_idx);
        }
    }
}

/// `LOAD_ATTR` (the `pc`th instruction of `code`) on the receiver at
/// stack slot `at`, as the core loop's arm runs it (and, for an instance
/// or module, the leaf arm's cached reads): `0` the value in place of the
/// receiver (released), anything else declined untouched.
unsafe extern "C" fn h_stack_attr(
    code: *const CodeObject,
    ext: *const CodeConstObjects,
    pc: u64,
    at: *mut Object,
    cache: *const SlotCache,
) -> u32 {
    use weavepy_compiler::InlineCache as IC;
    // SAFETY: the code passes its own code and extension, one of its
    // attribute loads, an initialized stack slot and the site's slot
    // cache. The reads are the core loop's: between two instructions,
    // running no code.
    unsafe {
        let (code, ext, pc) = (&*code, &*ext, pc as usize);
        let recv = &*at;
        if !droppable(recv) {
            return 1;
        }
        if let Some(v) = slot_hit(code, recv, pc, cache) {
            crate::drop_hot(std::mem::replace(&mut *at, v));
            return 0;
        }
        let arg = code.instructions[pc].arg;
        let v = match recv {
            Object::Instance(inst) if inst.cls_raw().native_kind.get() != 0 => {
                Interpreter::core_native_field(Some(ext), inst, arg, pc, code.instructions.len())
            }
            Object::Instance(inst) => match crate::field_slot_hit(ext, pc, inst) {
                Some(v) => Some(Interpreter::clone_operand(v)),
                None => {
                    let cls = inst.cls_raw();
                    match code.caches.get(pc as u32) {
                        IC::LoadAttrInstance { key_idx, ver } if cls.attr_version.get() == ver => {
                            match inst.attr_peek_index(key_idx as usize) {
                                Some((k, v)) if crate::slot_name_matches(code, arg, k) => {
                                    crate::field_slot_note(ext, code, pc, inst, key_idx);
                                    Some(Interpreter::clone_operand(v))
                                }
                                _ => None,
                            }
                        }
                        _ => None,
                    }
                    .or_else(|| Interpreter::leaf_fused_local_attr(code, recv, pc, arg))
                }
            },
            Object::Type(cls) if Rc::strong_count(cls) > 1 => type_attr(code, ext, cls, pc, arg),
            Object::Module(m) => Interpreter::core_module_attr(code, m, pc, arg)
                .or_else(|| Interpreter::leaf_fused_local_attr(code, recv, pc, arg)),
            Object::Code(_) | Object::Frame(_) => {
                let Some(v) = code
                    .names
                    .get(arg as usize)
                    .and_then(|name| Interpreter::frame_plain_attr(recv, name, None))
                else {
                    return 1;
                };
                crate::drop_hot(std::mem::replace(&mut *at, v));
                return 0;
            }
            _ => None,
        };
        let Some(v) = v else {
            return 1;
        };
        slot_note(code, recv, pc, cache);
        crate::drop_hot(std::mem::replace(&mut *at, v));
        0
    }
}

/// `LOAD_ATTR` (the `pc`th instruction of `code`, name `arg`) on the class
/// `cls`, as the core loop's arm reads it: the site's remembered value,
/// or the class's (or, under a metaclass that can't intercept the name,
/// its MRO's) entry, which a plain class's site then remembers. Runs no
/// code.
fn type_attr(
    code: &CodeObject,
    ext: &CodeConstObjects,
    cls: &Rc<crate::types::TypeObject>,
    pc: usize,
    arg: u32,
) -> Option<Object> {
    if let Some(v) = ext
        .stamp_slot(pc)
        .and_then(|s| crate::class_attr_hit(s, cls))
    {
        return Some(v);
    }
    let v = Interpreter::leaf_load_type_attr(code, cls, pc as u32, arg)?;
    // (The stamp is keyed by the class's version alone, which a
    // metaclass's changes don't move.)
    if Interpreter::plain_metaclass(cls) {
        crate::class_attr_fill(code, cls, pc, &v);
    }
    Some(v)
}

/// `LOAD_ATTR` (the `pc`th instruction of `code`) on the local at `recv`,
/// as the core loop's fused `LOAD_FAST x; LOAD_ATTR` runs it: `0` the
/// value written to the free slot `dst`, anything else declined
/// untouched.
unsafe extern "C" fn h_local_attr(
    dst: *mut Object,
    code: *const CodeObject,
    ext: *const CodeConstObjects,
    pc: u64,
    recv: *const Object,
    cache: *const SlotCache,
) -> u32 {
    // SAFETY: the code passes a free stack slot, its own code and
    // extension, one of its attribute loads, an initialized local and the
    // site's slot cache. The reads run no code.
    unsafe {
        let (code, ext, pc, recv) = (&*code, &*ext, pc as usize, &*recv);
        if let Some(v) = slot_hit(code, recv, pc, cache) {
            dst.write(v);
            return 0;
        }
        let arg = code.instructions[pc].arg;
        let v = match recv {
            Object::Instance(inst) if inst.cls_raw().native_kind.get() != 0 => {
                Interpreter::core_native_field(Some(ext), inst, arg, pc, code.instructions.len())
            }
            Object::Instance(_) | Object::Module(_) => {
                Interpreter::core_local_attr(Some(ext), code, recv, pc, arg)
            }
            Object::Type(cls) => type_attr(code, ext, cls, pc, arg),
            // A frame's or code object's plain field (a live frame's line
            // needs the full lane, which makes its `lasti` current).
            Object::Code(_) | Object::Frame(_) => {
                return match code
                    .names
                    .get(arg as usize)
                    .and_then(|name| Interpreter::frame_plain_attr(recv, name, None))
                {
                    Some(v) => {
                        dst.write(v);
                        0
                    }
                    None => 1,
                };
            }
            _ => None,
        };
        match v {
            Some(v) => {
                slot_note(code, recv, pc, cache);
                dst.write(v);
                0
            }
            None => 1,
        }
    }
}

/// `STORE_ATTR` (the `pc`th instruction of `code`) of the value at stack
/// slot `at` into the receiver above it, as the core loop's arm runs it:
/// `0` stored (both left the stack; the value moved), anything else
/// declined untouched.
unsafe extern "C" fn h_stack_store_attr(code: *const CodeObject, pc: u64, at: *mut Object) -> u32 {
    // SAFETY: the code passes its own code, one of its attribute stores,
    // and two initialized stack slots. The store runs no code.
    unsafe {
        let (code, pc) = (&*code, pc as usize);
        let (val, recv) = (&*at, &*at.add(1));
        let Object::Instance(inst) = recv else {
            return 1;
        };
        if !droppable(recv)
            || !Interpreter::core_store_attr(code, inst, pc, code.instructions[pc].arg, val)
        {
            return 1;
        }
        // The value moved in (its slot is a stand-in now).
        crate::drop_hot(at.add(1).read());
        0
    }
}

/// `BUILD_TUPLE n` (`list` false) or `BUILD_LIST n` of the `n` values
/// from stack slot `at` up, as the core loop's arms run them: `0` the
/// container at `at`, anything else declined untouched.
unsafe extern "C" fn h_build(st: *mut State, at: *mut Object, n: u64, list: u32) -> u32 {
    // SAFETY: the code passes its live state and `n` initialized stack
    // slots (or, for `n == 0`, a free one).
    unsafe {
        let st = &*st;
        let n = n as usize;
        if crate::stdlib::tracemalloc_real::is_tracking()
            || crate::stdlib::testinternalcapi_mod::reftrace_print_active()
        {
            return 1;
        }
        if list != 0 {
            if crate::gc_trace::auto_collect_due() {
                return 1;
            }
            let items: Vec<Object> = (0..n).map(|j| at.add(j).read()).collect();
            let obj = Object::new_list(items);
            crate::gc_trace::track(&obj);
            at.write(obj);
            return 0;
        }
        let interp = &*st.interp;
        let t = match n {
            1 => interp.alloc_tuple_array([at.read()]),
            2 => interp.alloc_tuple_array([at.read(), at.add(1).read()]),
            3 => interp.alloc_tuple_array([at.read(), at.add(1).read(), at.add(2).read()]),
            _ => return 1,
        };
        at.write(t);
        0
    }
}

/// `BUILD_MAP` of the `n` key/value pairs at `at` into a dict at `at`, as
/// the leaf arm builds it: only for keys whose hashing and equality are
/// native (`str`, `int`, `bool`, `None`). `0` built, anything else
/// declined with the slots untouched.
unsafe extern "C" fn h_build_map(at: *mut Object, n: u64) -> u32 {
    let n = n as usize;
    // SAFETY: the code passes `2n` initialized stack slots.
    unsafe {
        if crate::stdlib::tracemalloc_real::is_tracking()
            || crate::stdlib::testinternalcapi_mod::reftrace_print_active()
            || crate::gc_trace::auto_collect_due()
            || !(0..n).all(|j| {
                matches!(
                    &*at.add(2 * j),
                    Object::Str(_) | Object::Int(_) | Object::Bool(_) | Object::None
                )
            })
        {
            return 1;
        }
        let mut d =
            crate::object::DictData::with_capacity_and_hasher(n, crate::fasthash::FxBuildHasher);
        for j in 0..n {
            let k = at.add(2 * j).read();
            let v = at.add(2 * j + 1).read();
            d.insert(crate::object::DictKey(k), v);
        }
        let obj = Object::Dict(Rc::new(crate::sync::RefCell::new(d)));
        crate::gc_trace::track(&obj);
        at.write(obj);
    }
    0
}

/// How deep compiled callees may run inside their compiled callers' calls
/// (each level holds two native frames and the helpers between them).
const DIRECT_CALL_DEPTH: u32 = 48;

/// Run the activation a compiled caller's `CALL` just switched to, the
/// callee, through its own native code, from the caller's call helper:
/// `true` when it returned, the caller running again with the result at
/// `start` on its stack (its native code continues past the call).
/// `false` leaves whatever activation is running (the callee partway, or
/// one it called) synced for the core loop, which the caller's native code
/// then leaves for, as it did before the call ran anything.
///
/// # Safety
///
/// `st` is the caller's live state, its switch non-null, and the running
/// activation the callee its `CALL` just pushed.
unsafe fn run_callee_directly(st: &mut State, start: usize) -> bool {
    // SAFETY: the caller's contract.
    unsafe { run_switched_directly(st) && (*st.frame).stack.len() == start + 1 }
}

/// The pc a `FOR_ITER` at `pc` of `code` goes on at once its iterator is
/// exhausted and gone: past the loop's `END_FOR` / `POP_ITER` pair.
fn for_iter_exit(code: &CodeObject, pc: usize) -> usize {
    let mut to = pc + 1 + code.instructions[pc].arg as usize;
    let op = |pc: usize| code.instructions.get(pc).map(|i| i.op);
    if op(to) == Some(OpCode::EndFor) {
        to += 1;
        if matches!(op(to), Some(OpCode::PopIter | OpCode::PopTop)) {
            to += 1;
        }
    }
    to
}

/// [`run_callee_directly`] for any activation just switched to (a call's
/// callee, a resumed generator): `true` when its native code ran it until
/// it switched back to this code's activation with nothing pending (the
/// frame synced: the caller checks where it was left), `false` when the
/// core loop goes on (with whatever the run left synced).
///
/// # Safety
///
/// As [`run_callee_directly`]'s.
unsafe fn run_switched_directly(st: &mut State) -> bool {
    // SAFETY: the caller's contract.
    unsafe {
        let sw = &mut *st.sw;
        let interp = &*st.interp;
        let nested = interp.direct_calls.get();
        if nested >= DIRECT_CALL_DEPTH || crate::hot_gates::loop_gen() != st.snap_gen {
            return false;
        }
        let cframe = &mut *sw.cur;
        let Some(ext) = crate::code_vm_ext(&cframe.code) else {
            return false;
        };
        let locals: &mut Vec<Object> = &mut *cframe.locals.as_ptr();
        let nlocals = locals.len();
        let Some(native) = ext.frame_jit.get(nlocals) else {
            return false;
        };
        let pc = cframe.pc as usize;
        if pc >= ext.dispatch_len || !native.enters_at(pc) {
            return false;
        }
        let stack = &mut cframe.stack;
        if stack.capacity() < native.need() {
            stack.reserve(native.need() - stack.len());
        }
        let mut cst = State {
            locals: locals.as_mut_ptr(),
            stack: stack.as_mut_ptr(),
            len: stack.len(),
            cap: stack.capacity(),
            pc,
            last: *sw.last,
            countdown: st.countdown,
            snap_gen: st.snap_gen,
            maybe_dead: st.maybe_dead,
            interp: st.interp,
            out: 0,
            depth_cell: st.depth_cell,
            err: None,
            frame: sw.cur,
            sw: st.sw,
            gen_depth: 0,
            direct: false,
        };
        interp.direct_calls.set(nested + 1);
        let status = native.run(&mut cst);
        interp.direct_calls.set(nested);
        let sw = &mut *st.sw;
        if status == RELOAD {
            // Back in the caller, nothing pending: done. Otherwise whoever
            // switched last synced what runs next.
            return sw.pending.is_none() && std::ptr::eq(sw.cur, st.frame);
        }
        // The callee stopped partway: its state, synced as the core loop
        // syncs it after a native run, for the core loop to go on with.
        let cframe = &mut *cst.frame;
        cframe.stack.set_len(cst.len);
        cframe.pc = cst.pc as u32;
        *sw.last = cst.last;
        if status == MARKED {
            sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Marked));
        } else if let Some(e) = cst.err.take() {
            sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Raised(e)));
        }
        false
    }
}

/// Whether the call whose operands started at `start` finished in place
/// (a frameless `__init__`, a builtin's lane): nothing pending, the same
/// activation running, its result alone in their place. Then the native
/// code goes on (`st.len` past the result).
///
/// # Safety
///
/// `st` is a live state whose switch is non-null.
unsafe fn finished_in_place(st: &mut State, start: usize) -> bool {
    // SAFETY: the caller's contract.
    unsafe {
        let sw = &*st.sw;
        let done = sw.pending.is_none()
            && std::ptr::eq(sw.cur, st.frame)
            && (*st.frame).stack.len() == start + 1
            && crate::hot_gates::loop_gen() == st.snap_gen;
        if done {
            st.len = start + 1;
        }
        done
    }
}

/// A `CALL` site's direct-call cache (see [`direct_call`]): the code of
/// the plain function it calls directly (a counted reference, so the
/// address names one code object while cached) and that code's native
/// body, and whether the self slot is filled. A site whose callee failed
/// the checks keeps the code with no body and rechecks after `retry` more
/// calls.
#[repr(C)]
#[derive(Default)]
pub(crate) struct DirectSite {
    code: usize,
    native: usize,
    retry: u32,
    /// How many trailing parameters take the function's defaults.
    missing: u32,
    has_self: bool,
    /// A second callee the site calls directly (a polymorphic method
    /// call's other receiver class): its code (a counted reference, `0`
    /// none), body, defaults taken and self slot.
    alt_code: usize,
    alt_native: usize,
    alt_missing: u32,
    alt_has_self: bool,
}

const DS_NATIVE: i32 = std::mem::offset_of!(DirectSite, native) as i32;
const DS_RETRY: i32 = std::mem::offset_of!(DirectSite, retry) as i32;

impl Drop for DirectSite {
    fn drop(&mut self) {
        for code in [self.code, self.alt_code] {
            if code != 0 {
                // SAFETY: each holds the count a fill took.
                unsafe { drop(Rc::from_raw(code as *const CodeObject)) };
            }
        }
    }
}

impl DirectSite {
    /// Record a call of `code` (with or without its self slot): its native
    /// body if it qualifies, or none until `retry` more calls.
    fn fill(&mut self, code: &Rc<CodeObject>, has_self: bool, native: usize, missing: usize) {
        if Rc::as_ptr(code) as usize != self.code {
            let old = std::mem::replace(&mut self.code, Rc::into_raw(code.clone()) as usize);
            if old != 0 {
                // SAFETY: `old` held the count an earlier fill took.
                unsafe { drop(Rc::from_raw(old as *const CodeObject)) };
            }
        }
        self.has_self = has_self;
        self.missing = missing as u32;
        self.native = native;
        self.retry = if native == 0 { 64 } else { 0 };
    }

    /// Record a call of `code` with a native body as the site's first
    /// callee, the one it held before (if it had a body) becoming its
    /// second.
    fn promote(&mut self, code: &Rc<CodeObject>, has_self: bool, native: usize, missing: usize) {
        if self.native != 0 && self.code != 0 && self.code != Rc::as_ptr(code) as usize {
            let old = std::mem::replace(&mut self.alt_code, self.code);
            self.code = 0;
            if old != 0 {
                // SAFETY: `old` held the count an earlier fill took.
                unsafe { drop(Rc::from_raw(old as *const CodeObject)) };
            }
            self.alt_native = self.native;
            self.alt_missing = self.missing;
            self.alt_has_self = self.has_self;
        }
        self.fill(code, has_self, native, missing);
    }
}

/// Whether a call of `f` (running `code`) with `nargs` positional
/// arguments binds as [`Interpreter::core_call_plain`] binds it, and
/// `code`'s native body runs from its start: that body, and how many
/// trailing parameters take `f`'s defaults (with [`CELLS_BIT`] when the
/// callee has cells to bind).
fn direct_body<'a>(
    f: &crate::object::PyFunction,
    code: &'a CodeObject,
    nargs: usize,
) -> Option<(&'a Native, usize)> {
    let missing = (code.arg_count as usize).checked_sub(nargs)?;
    // A callee with cell or free variables binds its cells too (see
    // `Interpreter::bind_call_cells`).
    let cells = !code.cellvars.is_empty() || !code.freevars.is_empty() || !f.closure.is_empty();
    if code.is_generator
        || code.is_coroutine
        || code.is_async_generator
        || (cells && code.cellvars.is_empty() && f.lean_cells_ref(code).is_none())
        || missing >= CELLS_BIT as usize
        || Interpreter::has_extended_params(code)
        || (missing > 0 && !Interpreter::defaults_cover(f, missing))
        || code.wire.as_ref().is_some_and(|w| w.exec_error.is_some())
    {
        return None;
    }
    let ext = crate::code_vm_ext(code)?;
    if ext.dispatch_len == 0 {
        return None;
    }
    let packed = missing | if cells { CELLS_BIT as usize } else { 0 };
    Some((ext.frame_jit.get(code.varnames.len().max(nargs))?, packed))
}

/// A [`DirectSite`]'s `missing` flag for a callee with cells to bind.
const CELLS_BIT: u32 = 1 << 31;

/// `BINARY_OP` (the `pc`th instruction of `code`) over the instances at
/// stack slots `slot` and above it whose class's method runs alone (see
/// `Interpreter::binop_instance_dunder`): the method called directly
/// ([`direct_call`]) as `method(a, b)`, the result at `slot`. `0` done,
/// `CALL_DECLINED` declined untouched, `RAISED` or `RELOAD` as
/// [`h_call`]'s.
unsafe extern "C" fn h_binop_dunder(
    st: *mut State,
    code: *const CodeObject,
    pc: u64,
    slot: u64,
    site: *mut DirectSite,
) -> u32 {
    // SAFETY: the code passes its live state, its own code and one of its
    // `BINARY_OP`s, the operands' slot (with room for one more above
    // them, and nothing virtual below), and the site's cache.
    unsafe {
        let st = &mut *st;
        if st.sw.is_null() {
            return CALL_DECLINED;
        }
        let (pc, slot) = (pc as usize, slot as usize);
        let arg = (&(*code).instructions)[pc].arg;
        let op: BinOpKind = std::mem::transmute(arg as u8);
        let inplace = arg & weavepy_compiler::BINARY_OP_INPLACE_FLAG != 0;
        let at = st.stack.add(slot);
        let Some(f) = Interpreter::binop_instance_dunder(&*at, &*at.add(1), op, inplace) else {
            return CALL_DECLINED;
        };
        let Object::Instance(a) = &*at else {
            return CALL_DECLINED;
        };
        let cls = a.cls();
        // `[a, b]` becomes the method call `[method, a, b]`, `a` its self.
        at.add(2).write(at.add(1).read());
        at.add(1).write(at.read());
        at.write(Object::Function(f));
        match direct_call_out(
            st,
            pc,
            slot,
            slot + 3,
            &mut *site,
            Some((op, inplace, &cls)),
        ) {
            None => {
                let f = at.read();
                at.write(at.add(1).read());
                at.add(1).write(at.add(2).read());
                drop(f);
                CALL_DECLINED
            }
            Some(0) if (*at).is_same(&crate::vm_singletons::not_implemented()) => {
                crate::drop_hot(at.read());
                st.len = slot;
                st.pc = pc + 1;
                st.err = Some(crate::binop_type_error(op, inplace, &cls));
                RAISED
            }
            Some(r) => r,
        }
    }
}

/// `LOAD_ATTR` (the `pc`th instruction of `code`) the in-line and cached
/// reads declined: a `property` whose getter the site has cached for the
/// receiver's class (see `MethodSlot::getter`) called directly
/// ([`direct_call`]), or anything else as the full handler loads it
/// ([`attr_lane`]); the result at stack slot `slot`. The receiver is the
/// stack's at `slot` (`local` 0) or the local at `recv`, read in place.
/// `0` done, `CALL_DECLINED` declined untouched, `RAISED` or `RELOAD` as
/// [`h_call`]'s.
unsafe extern "C" fn h_property(
    st: *mut State,
    code: *const CodeObject,
    pc: u64,
    slot: u64,
    recv: *const Object,
    local: u64,
    site: *mut DirectSite,
) -> u32 {
    // SAFETY: the code passes its live state, its own code and one of its
    // `LOAD_ATTR`s, the result's slot (with room for one more above it,
    // and nothing virtual below), the receiver, and the site's cache.
    unsafe {
        let st = &mut *st;
        if st.sw.is_null() {
            return CALL_DECLINED;
        }
        let (pc, slot) = (pc as usize, slot as usize);
        // A frame's or code object's plain field, a live frame's line with
        // the running activation's own instruction.
        if matches!(&*recv, Object::Frame(_) | Object::Code(_)) && (local != 0 || droppable(&*recv))
        {
            let code = &*code;
            let running = (Rc::as_ptr(&(*st.frame).locals).cast::<()>(), pc as u32);
            let v = code
                .names
                .get(code.instructions[pc].arg as usize)
                .and_then(|name| Interpreter::frame_plain_attr(&*recv, name, Some(running)));
            if let Some(v) = v {
                let at = st.stack.add(slot);
                if local == 0 {
                    crate::drop_hot(at.read());
                }
                at.write(v);
                return 0;
            }
        }
        let Object::Instance(inst) = &*recv else {
            return attr_lane(st, pc, slot, recv, local);
        };
        let Some((getter, gcode)) = crate::code_vm_ext(&*code)
            .and_then(|ext| ext.method_slot(pc))
            .and_then(|ms| ms.getter(inst.cls_raw().attr_version.get()))
        else {
            return attr_lane(st, pc, slot, recv, local);
        };
        let at = st.stack.add(slot);
        // A pure leaf getter (no native body of its own: it never runs as
        // an activation) evaluates in place, as the core loop's arm does.
        if crate::code_is_pure_leaf(&gcode)
            && crate::pure_leaf_warm(&gcode)
            && crate::recursion::current_depth() < crate::recursion::recursion_limit()
            && (local != 0 || droppable(&*recv))
        {
            let interp = &*st.interp;
            if let Some(v) = interp.pure_leaf_eval::<false, false>(&gcode, &getter, &[recv]) {
                if local == 0 {
                    crate::drop_hot(at.read());
                }
                at.write(v);
                return 0;
            }
        }
        // `[recv]` becomes the method call `[getter, recv]`.
        if local != 0 {
            at.add(1).write(crate::clone_hot(&*recv));
        } else {
            at.add(1).write(at.read());
        }
        at.write(Object::Function(getter));
        match direct_call_out(st, pc, slot, slot + 2, &mut *site, None) {
            None => {
                let f = at.read();
                if local != 0 {
                    crate::drop_hot(at.add(1).read());
                } else {
                    at.write(at.add(1).read());
                }
                drop(f);
                attr_lane(st, pc, slot, recv, local)
            }
            Some(r) => r,
        }
    }
}

/// The `LOAD_ATTR` at `pc` (its receiver the stack's at `slot`, or the
/// local at `recv` when `local`) as the full handler runs it
/// (`Interpreter::core_attr_lane`), the frame synced and waiting on the
/// pending list (the load may run Python code): `0` the value at `slot`,
/// `RAISED` its error, `RELOAD` the core loop goes on, `CALL_DECLINED`
/// nothing done.
///
/// # Safety
///
/// As [`h_property`]'s.
unsafe fn attr_lane(
    st: &mut State,
    pc: usize,
    slot: usize,
    recv: *const Object,
    local: u64,
) -> u32 {
    // SAFETY: the caller's contract.
    unsafe {
        if st.sw.is_null() || !std::ptr::eq((*st.sw).cur, st.frame) {
            return CALL_DECLINED;
        }
        if local != 0 {
            st.stack.add(slot).write(crate::clone_hot(&*recv));
        }
        let frame = &mut *st.frame;
        frame.stack.set_len(slot + 1);
        frame.pc = pc as u32;
        let sw = &mut *st.sw;
        *sw.last = st.last;
        let interp = &mut *st.interp.cast_mut();
        match interp.core_attr_lane(sw, pc) {
            Ok(()) => {
                if finished_in_place(st, slot) {
                    0
                } else {
                    RELOAD
                }
            }
            Err(e) => {
                st.err = Some(e);
                st.len = slot;
                st.pc = pc + 1;
                RAISED
            }
        }
    }
}

/// A back edge's GIL countdown (at `countdown`) ran out: the fresh count
/// when nobody waits for the GIL (see `Interpreter::countdown_out`), or
/// `0` for the core loop's checkpoint.
unsafe extern "C" fn h_countdown(countdown: *mut u32) -> u32 {
    if crate::gil::handoff_idle() {
        // SAFETY: the code passes its state's countdown pointer.
        unsafe { *countdown = crate::gil::GIL_CHECK_INTERVAL };
        return crate::gil::GIL_CHECK_INTERVAL;
    }
    0
}

/// [`h_call`] with a direct call ([`direct_call`]) of the plain Python
/// function `site` caches tried first. (The native code calls `h_call`
/// itself for any other callee, and while a site without a body waits
/// out its retries.)
unsafe extern "C" fn h_call_direct(
    st: *mut State,
    code: *const CodeObject,
    ext: *const CodeConstObjects,
    pc: u64,
    len: u64,
    site: *mut DirectSite,
) -> u32 {
    // SAFETY: as `h_call`'s; `site` is this `CALL`'s own cache.
    unsafe {
        let s = &mut *st;
        let argc = (&(*code).instructions)[pc as usize].arg as usize;
        let len_u = len as usize;
        if len_u >= argc + 2 && !s.sw.is_null() {
            let start = len_u - argc - 2;
            if let Some(r) = direct_call(s, pc as usize, start, len_u, &mut *site, None) {
                return r;
            }
        }
        h_call(st, code, ext, pc, len)
    }
}

/// [`direct_call`] out of line, for the colder lanes (a property getter,
/// an instance operator's method), which share one copy of it.
#[inline(never)]
unsafe fn direct_call_out(
    st: &mut State,
    pc: usize,
    start: usize,
    len: usize,
    site: &mut DirectSite,
    binop: Option<(BinOpKind, bool, &Rc<crate::types::TypeObject>)>,
) -> Option<u32> {
    // SAFETY: the caller's contract.
    unsafe { direct_call(st, pc, start, len, site, binop) }
}

/// The `CALL` at `pc` (its operands from `start` up a `len`-deep stack) of
/// a plain Python function whose body has native code, run straight from
/// here: the callee's activation is bound as the core loop's arm binds it
/// ([`Interpreter::core_bind_plain`]), its native code runs, and its
/// return comes back here ([`h_return`]'s `DIRECT_RET`) to be finished
/// without the core loop's switch back. `Some(0)` leaves the result at
/// `start`, `Some(RELOAD)` leaves the rest to the core loop (the switch's
/// state synced), `None` touched nothing. An instance operator's method
/// (`binop`) that leaves the direct return to the core loop carries the
/// operator, whose `TypeError` its `NotImplemented` becomes there.
#[inline(always)]
unsafe fn direct_call(
    st: &mut State,
    pc: usize,
    start: usize,
    len: usize,
    site: &mut DirectSite,
    binop: Option<(BinOpKind, bool, &Rc<crate::types::TypeObject>)>,
) -> Option<u32> {
    // SAFETY: `h_call`'s contract, with a non-null switch.
    unsafe {
        let Object::Function(f) = &*st.stack.add(start) else {
            return None;
        };
        let fp = Rc::as_ptr(f);
        let has_self = !matches!(&*st.stack.add(start + 1), Object::Unbound);
        let code_rc: &Rc<CodeObject> = &*f.code.as_ptr();
        // (The site keys on the code: a fresh function made from the same
        // `def` binds the same way, and code without free variables takes
        // no closure.)
        let at = Rc::as_ptr(code_rc) as usize;
        let (native, packed) = if site.code == at && site.has_self == has_self && site.native != 0
        {
            (site.native, site.missing as usize)
        } else if site.alt_code == at && site.alt_has_self == has_self && site.alt_native != 0 {
            (site.alt_native, site.alt_missing as usize)
        } else {
            let nargs = len - start - 2 + usize::from(has_self);
            let (native, missing) = direct_body(f, code_rc, nargs).map_or((0, 0), |(n, m)| {
                (std::ptr::from_ref::<Native>(n) as usize, m)
            });
            if native == 0 {
                // A callee with no body (a frameless leaf) leaves the site's
                // compiled callees alone.
                if site.native == 0 {
                    site.fill(code_rc, has_self, 0, 0);
                }
                return None;
            }
            site.promote(code_rc, has_self, native, missing);
            (native, missing)
        };
        let (missing, cells) = (
            packed & !(CELLS_BIT as usize),
            packed & CELLS_BIT as usize != 0,
        );
        // (Another function running the same code may have rebound its
        // defaults, or, a closure, have cells the lean binding can't take.)
        if packed != 0
            && ((missing > 0 && !Interpreter::defaults_cover(f, missing))
                || (cells && code_rc.cellvars.is_empty() && f.lean_cells_ref(code_rc).is_none()))
        {
            return None;
        }
        let native = &*(native as *const Native);
        let interp = &mut *st.interp.cast_mut();
        let nested = interp.direct_calls.get();
        let code: &CodeObject = code_rc;
        // (Native code runs only in a burst, which free-threaded mode
        // never starts.)
        if nested >= DIRECT_CALL_DEPTH
            || crate::hot_gates::loop_gen() != st.snap_gen
            || !native.enters_at(0)
            || !interp.inline_calls_ok()
            || !(crate::tier2::jit_off_for_process()
                || code.jit_hint.is_not_jitable()
                || !code.jit_hint.is_compiled())
        {
            return None;
        }
        let act = interp.inline_pool.pop()?;
        let sw = &mut *st.sw;
        let crate::recursion::Enter::Ok(guard) = crate::recursion::enter_with(sw.depth_cell) else {
            interp.inline_pool.push(act);
            return None;
        };
        // Committed: the caller synced as the core loop's arm syncs it, and
        // the callee's activation made the running one.
        let caller = &mut *st.frame;
        caller.stack.set_len(len);
        caller.pc = pc as u32;
        *sw.last = st.last;
        let saved_last = sw.last;
        let callee = interp
            .core_bind_plain::<true>(sw, pc, start, has_self, missing, cells, fp, act, guard);
        crate::burst_stats::note_call(crate::burst_stats::CALL_DIRECT);
        let depth = (*sw.inl).len();
        Some(direct_run::<false>(
            st, pc, start, callee, depth, native, saved_last, nested, binop, 0,
        ))
    }
}

/// The activation a `CALL` at `pc` (its operands from `start`) just pushed
/// through the core loop's call paths (`Interpreter::core_call`,
/// `Interpreter::core_call_kw`) for a plain function, run as a direct
/// call's ([`direct_run`]) when its code has a native body entered where
/// it starts. `saved_last` is the caller's last-instruction slot before the
/// switch. `None` touched nothing.
///
/// # Safety
///
/// `st` is the caller's live state, its frame synced at the call and its
/// switch non-null.
#[inline(never)]
unsafe fn run_pushed_directly(
    st: &mut State,
    pc: usize,
    start: usize,
    saved_last: *mut usize,
) -> Option<u32> {
    // SAFETY: the caller's contract.
    unsafe {
        let sw = &mut *st.sw;
        if sw.pending.is_some() || std::ptr::eq(sw.cur, st.frame) {
            return stats::declined("pending or in place");
        }
        let inl = &mut *sw.inl;
        let depth = inl.len();
        let callee = sw.cur;
        let top = inl.last_mut()?;
        if top.gen.is_some()
            || top.binop.is_some()
            || top.discard
            || top.direct
            || !std::ptr::eq(&raw const *top.frame, callee)
        {
            return stats::declined("shape");
        }
        let interp = &*st.interp;
        let nested = interp.direct_calls.get();
        if nested >= DIRECT_CALL_DEPTH || crate::hot_gates::loop_gen() != st.snap_gen {
            return stats::declined("depth or generation");
        }
        let cframe = &*callee;
        let ext = crate::code_vm_ext(&cframe.code)?;
        let nlocals = (*cframe.locals.as_ptr()).len();
        let Some(native) = ext.frame_jit.get(nlocals) else {
            ext.frame_jit.warm_switched(&cframe.code, ext, nlocals);
            return stats::declined("no body");
        };
        let from = cframe.pc as usize;
        if from >= ext.dispatch_len || !native.enters_at(from) {
            return stats::declined("no entry");
        }
        top.direct = true;
        Some(direct_run::<true>(
            st, pc, start, callee, depth, native, saved_last, nested, None, from,
        ))
    }
}

/// Run a direct call's callee (the running activation, `callee`, at
/// `depth`, marked `direct`) through its native code from `from`, and
/// finish its return here: [`direct_call`]'s `Some` results. `PUSHED`
/// for an activation the core loop's call paths bound (see
/// [`run_pushed_directly`]), whose cells its clean return checks.
///
/// # Safety
///
/// As [`direct_call`]'s, with the callee bound and switched to.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
unsafe fn direct_run<const PUSHED: bool>(
    st: &mut State,
    pc: usize,
    start: usize,
    callee: *mut crate::Frame,
    depth: usize,
    native: &Native,
    saved_last: *mut usize,
    nested: u32,
    binop: Option<(BinOpKind, bool, &Rc<crate::types::TypeObject>)>,
    from: usize,
) -> u32 {
    // SAFETY: the caller's contract.
    unsafe {
        let interp = &mut *st.interp.cast_mut();
        let caller = &mut *st.frame;
        let cframe = &mut *callee;
        let stack = &mut cframe.stack;
        if stack.capacity() < native.need() {
            stack.reserve(native.need() - stack.len());
        }
        let mut cst = State {
            locals: (*cframe.locals.as_ptr()).as_mut_ptr(),
            stack: stack.as_mut_ptr(),
            len: stack.len(),
            cap: stack.capacity(),
            pc: from,
            last: usize::MAX,
            countdown: st.countdown,
            snap_gen: st.snap_gen,
            maybe_dead: st.maybe_dead,
            interp: st.interp,
            out: 0,
            depth_cell: st.depth_cell,
            err: None,
            frame: callee,
            sw: st.sw,
            gen_depth: 0,
            direct: true,
        };
        interp.direct_calls.set(nested + 1);
        let status = native.run_direct(&mut cst, from);
        interp.direct_calls.set(nested);
        let sw = &mut *st.sw;
        let inl = &mut *sw.inl;
        // (Only the callee's own `RETURN_VALUE` hands back `DIRECT_RET`,
        // with its activation the running one.)
        let ours = status == DIRECT_RET
            || (inl.len() == depth
                && inl
                    .last()
                    .is_some_and(|a| std::ptr::eq(&raw const *a.frame, callee)));
        debug_assert!(
            status != DIRECT_RET
                || (inl.len() == depth
                    && inl
                        .last()
                        .is_some_and(|a| a.direct && std::ptr::eq(&raw const *a.frame, callee)))
        );
        // The activation goes on as any other (see `InlineAct::binop`).
        let leave = |a: &mut crate::InlineAct| {
            a.direct = false;
            if let Some((op, inplace, cls)) = binop {
                a.binop = Some((op, inplace, cls.clone()));
            }
        };
        if status == DIRECT_RET {
            let clean = cst.len == 1
                && inl.last().is_some_and(|a| {
                    a.act.shell.is_none()
                        && (!PUSHED
                            || ((cframe.code.cellvars.is_empty() || a.owns_cells)
                                && (a.init_inst.is_none()
                                    || matches!(&*cst.stack, Object::None))))
                })
                && Rc::strong_count(&cframe.locals) == 1
                && !interp.countdown_out(2)
                && crate::hot_gates::loop_gen() == st.snap_gen;
            if clean {
                // `core_return`'s clean return, with the caller known (a
                // constructor's `__init__` returned `None`: the caller takes
                // the instance).
                let v = cst.stack.read();
                cframe.stack.set_len(0);
                let mut done = inl.pop().unwrap_unchecked();
                if depth == sw.entry_depth {
                    sw.entry_dead = true;
                }
                let v = if PUSHED {
                    done.init_inst.take().unwrap_or(v)
                } else {
                    v
                };
                crate::release_locals(&mut *cframe.locals.as_ptr());
                drop(done.guard.take());
                interp.lean_pending_exit(done.caller_pending);
                // SAFETY: moved out once; the parked slot treats the field
                // as stale (see `inline_deliver`).
                let callable = std::ptr::read(&raw const done.callable);
                if done.owns_cells {
                    done.clean = true;
                    interp.inline_park_clean(done);
                } else {
                    interp.inline_park_direct(done);
                }
                st.stack.add(start).write(v);
                caller.stack.set_len(start + 1);
                match callable {
                    Object::Function(f) if Rc::strong_count(&f) > 1 => drop(f),
                    callable => interp.release(callable),
                }
                sw.cur = st.frame;
                interp.gil_countdown -= 1;
                if (*sw.maybe_dead).get() {
                    let mut tmp = None;
                    let (cf, _, cshell) = sw.activation(depth - 1, &mut tmp);
                    interp.flush_lean(&mut *cf, &mut *cshell.cast::<crate::QuietShell<'_>>());
                    if interp.drain_if_maybe_dead() && crate::hot_gates::loop_gen() != st.snap_gen {
                        sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Breaker));
                    }
                }
                sw.last = saved_last;
                if std::ptr::eq(saved_last, &raw mut sw.scratch) {
                    sw.scratch = usize::MAX;
                }
                *sw.last = pc;
                if sw.pending.is_some() {
                    return RELOAD;
                }
                st.len = start + 1;
                return 0;
            }
            // Any other return is the core loop's arm's (as `h_return`'s).
            cframe.stack.set_len(cst.len);
            cframe.pc = cst.pc as u32;
            *sw.last = cst.last;
            leave(inl.last_mut().expect("checked above"));
            if !interp.core_return(sw, st.snap_gen) {
                sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Step));
            }
        } else if ours {
            leave(inl.last_mut().expect("checked above"));
        } else if let Some(a) = inl.get_mut(depth - 1) {
            if std::ptr::eq(&raw const *a.frame, callee) {
                leave(a);
            }
        }
        if status == RELOAD || status == DIRECT_RET {
            // Returned into the caller, nothing pending: done. Otherwise
            // whoever switched last synced what runs next.
            if sw.pending.is_none()
                && std::ptr::eq(sw.cur, st.frame)
                && caller.stack.len() == start + 1
            {
                st.len = start + 1;
                return 0;
            }
            return RELOAD;
        }
        // The callee stopped partway: its state, synced as the core loop
        // syncs it after a native run, for the core loop to go on with.
        let cframe = &mut *cst.frame;
        cframe.stack.set_len(cst.len);
        cframe.pc = cst.pc as u32;
        *sw.last = cst.last;
        if status == MARKED {
            sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Marked));
        } else if let Some(e) = cst.err.take() {
            sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Raised(e)));
        }
        RELOAD
    }
}

/// `CALL` (the `pc`th instruction of `code`) on a stack `len` deep, for
/// the shapes the core loop's arm runs in place: a pure leaf callee, a
/// natively served class's constructor or bound method, an exact list's
/// `append` / `pop`, and a registered leaf builtin. `0` the result in the
/// callee's slot (`st.len` past it), `RAISED` the builtin raised (the
/// operands released, `st.len` and `st.pc` as the arm leaves them),
/// anything else declined (a Python callee's activation is the core
/// loop's).
#[inline(never)]
unsafe extern "C" fn h_call(
    st: *mut State,
    code: *const CodeObject,
    ext: *const CodeConstObjects,
    pc: u64,
    len: u64,
) -> u32 {
    use crate::LeafKind;
    // SAFETY: the code passes its live state, its own code and extension,
    // one of its `CALL`s, and its stack depth. The shapes run no Python
    // code; every operand that leaves is checked to leave by a plain
    // decrement.
    unsafe {
        let st = &mut *st;
        let interp = &*st.interp;
        let (code, ext, pc, len) = (&*code, &*ext, pc as usize, len as usize);
        let argc = code.instructions[pc].arg as usize;
        if len < argc + 2 {
            return CALL_DECLINED;
        }
        let base = st.stack;
        let start = len - argc - 2;
        // The frame synced for a switch, as the core loop's arm syncs it.
        let sync = |st: &mut State| {
            let frame = &mut *st.frame;
            frame.stack.set_len(len);
            frame.pc = pc as u32;
            *(*st.sw).last = st.last;
        };
        // `gen.send(v)`: likewise, with the value sent in.
        if argc == 1
            && !st.sw.is_null()
            && Interpreter::gen_send_call(&*base.add(start), &*base.add(start + 1))
        {
            sync(st);
            let sw = &mut *st.sw;
            let interp = &mut *st.interp.cast_mut();
            if !interp.core_gen_resume(sw, pc, crate::InlineResume::SendCall) {
                sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Step));
            }
            return RELOAD;
        }
        // `next(gen)`: the generator resumes inline, as for `FOR_ITER`,
        // with the yield as the call's result.
        if argc == 1
            && matches!(
                (&*base.add(start), &*base.add(start + 1), &*base.add(start + 2)),
                (Object::Builtin(b), Object::Unbound, Object::Generator(_))
                    if Rc::as_ptr(b) as usize == interp.leaf_fns().next_ptr
            )
        {
            if st.sw.is_null() {
                return CALL_DECLINED;
            }
            sync(st);
            let sw = &mut *st.sw;
            let interp = &mut *st.interp.cast_mut();
            if !interp.core_gen_resume(sw, pc, crate::InlineResume::NextCall) {
                sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Step));
            }
            return RELOAD;
        }
        // `next(gen, default)`: likewise, the default the result of an
        // exhausted generator.
        if argc == 2
            && matches!(
                (&*base.add(start), &*base.add(start + 1), &*base.add(start + 2)),
                (Object::Builtin(b), Object::Unbound, Object::Generator(_))
                    if Rc::as_ptr(b) as usize == interp.leaf_fns().next_ptr
            )
        {
            if st.sw.is_null() {
                return CALL_DECLINED;
            }
            sync(st);
            let sw = &mut *st.sw;
            let interp = &mut *st.interp.cast_mut();
            if !interp.core_gen_resume(sw, pc, crate::InlineResume::NextDefault) {
                sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Step));
            }
            return RELOAD;
        }
        Interpreter::core_instance_callee(base.add(start));
        let done = |st: &mut State, r: Object| {
            for k in start..len {
                crate::drop_hot(base.add(k).read());
            }
            base.add(start).write(r);
            st.len = start + 1;
            0
        };
        // The commonest settled leaf builtins first (`isinstance(x, C)`, a
        // fast half), without the shape checks below; a decline falls
        // through to them untouched.
        if let Object::Builtin(b) = &*base.add(start) {
            let kind = ext.method_slot(pc).and_then(|s| s.get_leaf(b));
            if matches!(kind, Some(LeafKind::Isinstance | LeafKind::Fast(_)))
                && Rc::strong_count(b) > 1
                && !st.sw.is_null()
            {
                let first = if matches!(&*base.add(start + 1), Object::Unbound) {
                    start + 2
                } else {
                    start + 1
                };
                let args = std::slice::from_raw_parts(base.add(first), len - first);
                if Interpreter::core_all_droppable(args) {
                    let r = match kind {
                        Some(LeafKind::Fast(f)) => f(args),
                        _ => Interpreter::core_isinstance(args),
                    };
                    match r {
                        None => {}
                        Some(Ok(v)) => return done(st, v),
                        Some(Err(e)) => {
                            for k in start..len {
                                crate::drop_hot(base.add(k).read());
                            }
                            st.len = start;
                            st.pc = pc + 1;
                            st.err = Some(e);
                            return RAISED;
                        }
                    }
                }
            }
        }
        let ops = std::slice::from_raw_parts(base.add(start), argc + 2);
        let python = match &ops[0] {
            Object::Function(_) => 1,
            Object::BoundMethod(bm) => u8::from(matches!(bm.function, Object::Function(_))),
            Object::Type(ty) if !ty.flags.is_builtin => 2,
            _ => 0,
        };
        // A pure leaf callee: evaluated in place, no activation.
        if python == 1 && argc < 8 {
            if matches!(&ops[0], Object::Function(f) if crate::fn_is_leaf(f)) {
                if let Some(r) = interp.core_pure_call(code, pc, ops, st.depth_cell) {
                    return done(st, r);
                }
            }
        }
        // A natively served class's constructor or bound class method.
        if python != 1 {
            let r = match &ops[0] {
                Object::Type(_) => Interpreter::core_native_ctor_kw(ops, argc, false),
                Object::BoundMethod(_) => Interpreter::core_native_bound_call(ops),
                _ => None,
            };
            if let Some(r) = r {
                return done(st, r);
            }
        }
        if python != 0 {
            // A Python callee's activation, switched to as the arm does
            // (the frame synced first; a declined switch leaves the call
            // to the quiet loop).
            if st.sw.is_null() {
                return CALL_DECLINED;
            }
            // A generator function's call makes its generator in place
            // (`core_call` admits none).
            let gen_fn = matches!(&ops[0], Object::Function(f) if {
                let c = &*f.code.as_ptr();
                c.is_generator || c.is_coroutine || c.is_async_generator
            });
            sync(st);
            let sw = &mut *st.sw;
            let saved_last = sw.last;
            let interp = &mut *st.interp.cast_mut();
            let switched = if gen_fn {
                interp.core_gen_call(sw, pc)
            } else if python == 1 {
                interp.core_call(sw, pc) || interp.core_gen_call(sw, pc)
            } else {
                interp.core_new(sw, pc, st.snap_gen)
            };
            if !switched && sw.pending.is_none() {
                sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Step));
            }
            // A compiled callee runs from here, and its return continues
            // this code natively (anything else leaves for the core loop).
            if switched && sw.pending.is_none() && !std::ptr::eq(sw.cur, st.frame) {
                if let Some(r) = run_pushed_directly(st, pc, start, saved_last) {
                    return r;
                }
                if run_callee_directly(st, start) {
                    st.len = start + 1;
                    return 0;
                }
            }
            if switched && finished_in_place(st, start) {
                return 0;
            }
            return RELOAD;
        }
        // A builtin type's call (`range(a, b)`, `float(x)`, `list(xs)`), as
        // the core loop's arms run it: in place when nothing it does can
        // run Python code, else through its lane.
        if let Object::Type(t) = &ops[0] {
            if t.flags.is_builtin {
                if let Some(r) = interp.core_builtin_ctor(base, start, len) {
                    return done(st, r);
                }
                if argc == 1 && !st.sw.is_null() {
                    sync(st);
                    let sw = &mut *st.sw;
                    let interp = &mut *st.interp.cast_mut();
                    if interp.core_type_lane(sw) {
                        return if finished_in_place(st, start) {
                            0
                        } else {
                            RELOAD
                        };
                    }
                }
            }
        }
        // A builtin the site has settled as a leaf (one it hasn't runs
        // through the core loop's builtin lane).
        let Some(slot) = ext.method_slot(pc) else {
            return CALL_DECLINED;
        };
        // A builtin the site settled on running in the core loop's lane (a
        // builtin type's method, a common builtin function), whose body may
        // run Python code: through the lane, which reloads after.
        let body = match (&ops[0], &ops[1]) {
            (Object::Builtin(b), Object::Unbound) => Some((b, None)),
            (Object::Builtin(b), recv) => Some((b, Some(recv))),
            (Object::BoundMethod(bm), Object::Unbound) if !bm.redispatch_descriptor => {
                match &bm.function {
                    Object::Builtin(b) => Some((b, Some(&bm.receiver))),
                    _ => None,
                }
            }
            _ => None,
        };
        if let (Some((b, recv)), false) = (body, st.sw.is_null()) {
            let settled = slot.native_body(b).or_else(|| {
                if slot.get_leaf(b).is_some() {
                    return None;
                }
                let via_call = interp.builtin_lane(b, recv)?;
                slot.set_native_body(b, via_call);
                Some(via_call)
            });
            if let Some(via_call) = settled {
                sync(st);
                let sw = &mut *st.sw;
                let interp = &mut *st.interp.cast_mut();
                if interp.core_builtin_lane(sw, pc, via_call) {
                    if finished_in_place(st, start) {
                        return 0;
                    }
                    return RELOAD;
                }
            }
        }
        let Object::Builtin(b) = &ops[0] else {
            return leaf_call_lane(st, pc, start, len);
        };
        if Rc::strong_count(b) <= 1 {
            return CALL_DECLINED;
        }
        let Some(kind) = slot.get_leaf(b) else {
            return leaf_call_lane(st, pc, start, len);
        };
        if argc <= 1 && matches!(&ops[1], Object::List(_)) {
            // `lst.append(x)` / `lst.pop()` on an exact list: the operand
            // moves in, or the item out.
            let recv = &ops[1];
            let Object::List(l) = recv else {
                return CALL_DECLINED;
            };
            if !droppable(recv) {
                return CALL_DECLINED;
            }
            let Some(items) = l.peek_mut() else {
                return CALL_DECLINED;
            };
            let (result, top) = match (kind, argc) {
                (LeafKind::ListAppend, 1) => {
                    items.push(base.add(len - 1).read());
                    (Object::None, len - 1)
                }
                (LeafKind::ListPop, 0) => match items.pop() {
                    Some(v) => (v, len),
                    None => return CALL_DECLINED,
                },
                _ => return CALL_DECLINED,
            };
            crate::drop_hot(base.add(top - 1).read());
            crate::drop_hot(base.add(top - 2).read());
            base.add(top - 2).write(result);
            st.len = top - 1;
            return 0;
        }
        if !kind.runs_in_core() {
            return leaf_call_lane(st, pc, start, len);
        }
        let first = if matches!(&ops[1], Object::Unbound) {
            start + 2
        } else {
            start + 1
        };
        let args = std::slice::from_raw_parts(base.add(first), len - first);
        // (A generator's fast step can't raise from here.)
        if !args.iter().all(droppable) || st.sw.is_null() {
            return CALL_DECLINED;
        }
        let r = match kind {
            LeafKind::Fast(f) => f(args),
            LeafKind::Isinstance => Interpreter::core_isinstance(args),
            LeafKind::Opaque => Some(match b.call_kw.as_ref() {
                Some(ckw) => ckw(args, &[]),
                None => (b.call)(args),
            }),
            k => interp.leaf_builtin_call(k, b, args),
        };
        match r {
            None => leaf_call_lane(st, pc, start, len),
            Some(Ok(v)) => done(st, v),
            Some(Err(e)) => {
                for k in start..len {
                    crate::drop_hot(base.add(k).read());
                }
                st.len = start;
                st.pc = pc + 1;
                st.err = Some(e);
                RAISED
            }
        }
    }
}

/// `len(x)` of an exact container or string.
fn len_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    // SAFETY: reads between two instructions (the lengths run no code).
    let n = unsafe {
        match args {
            [Object::List(l)] => l.peek()?.len(),
            [Object::Tuple(t)] => t.len(),
            [Object::Str(s)] => crate::shared_value::SharedStr::char_count(s),
            [Object::Dict(d)] => d.peek()?.len(),
            [Object::Set(s)] => s.peek()?.len(),
            [Object::Bytes(b)] => b.len(),
            _ => return None,
        }
    };
    Some(Ok(Object::Int(n as i64)))
}

/// [`h_call`] at a site settled on a leaf builtin: a container's or
/// string's method through its [`Kernel`], or `isinstance`, `len`, a
/// registered fast half or a registered leaf body in place as `h_call`
/// runs them, from a small function (the general helper stays as it
/// is); anything else (another callee, a declined shape) through
/// `h_call`.
unsafe extern "C" fn h_call_leaf(
    st: *mut State,
    code: *const CodeObject,
    ext: *const CodeConstObjects,
    pc: u64,
    len: u64,
) -> u32 {
    use crate::LeafKind;
    // SAFETY: as `h_call`'s.
    unsafe {
        let s = &mut *st;
        let (c, x, pcu, lenu) = (&*code, &*ext, pc as usize, len as usize);
        let argc = c.instructions[pcu].arg as usize;
        if lenu >= argc + 2 && !s.sw.is_null() {
            let base = s.stack;
            let start = lenu - argc - 2;
            if let Object::Builtin(b) = &*base.add(start) {
                let kind = x.method_slot(pcu).and_then(|m| m.get_leaf(b));
                // A container's or string's method with a kernel, on the
                // receiver in the self slot.
                if let (Some(k), false) = (kind, matches!(&*base.add(start + 1), Object::Unbound)) {
                    let args = std::slice::from_raw_parts(base.add(start + 1), argc + 1);
                    if Rc::strong_count(b) > 1 && Interpreter::core_all_droppable(args) {
                        if let Some(v) = kernel_call(k, args, b) {
                            for j in start..lenu {
                                crate::drop_hot(base.add(j).read());
                            }
                            base.add(start).write(v);
                            s.len = start + 1;
                            return 0;
                        }
                    }
                }
                if matches!(
                    kind,
                    Some(
                        LeafKind::Isinstance
                            | LeafKind::Len
                            | LeafKind::Fast(_)
                            | LeafKind::Opaque
                            | LeafKind::Scalar
                    )
                ) && Rc::strong_count(b) > 1
                {
                    let first = if matches!(&*base.add(start + 1), Object::Unbound) {
                        start + 2
                    } else {
                        start + 1
                    };
                    let args = std::slice::from_raw_parts(base.add(first), lenu - first);
                    let r = if Interpreter::core_all_droppable(args) {
                        // (A body that runs no Python code for any
                        // arguments, or for scalar ones: as `h_call` and
                        // `leaf_builtin_call` run them.)
                        let body = || {
                            Some(match b.call_kw.as_ref() {
                                Some(ckw) => ckw(args, &[]),
                                None => (b.call)(args),
                            })
                        };
                        match kind {
                            Some(LeafKind::Fast(f)) => f(args),
                            Some(LeafKind::Len) => len_fast(args),
                            Some(LeafKind::Opaque) => body(),
                            Some(LeafKind::Scalar) => {
                                if args.iter().all(|a| {
                                    matches!(a, Object::Int(_) | Object::Float(_) | Object::Bool(_))
                                }) {
                                    body()
                                } else {
                                    None
                                }
                            }
                            _ => Interpreter::core_isinstance(args),
                        }
                    } else {
                        None
                    };
                    if let Some(r) = r {
                        for k in start..lenu {
                            crate::drop_hot(base.add(k).read());
                        }
                        return match r {
                            Ok(v) => {
                                base.add(start).write(v);
                                s.len = start + 1;
                                0
                            }
                            Err(e) => {
                                s.len = start;
                                s.pc = pcu + 1;
                                s.err = Some(e);
                                RAISED
                            }
                        };
                    }
                }
            }
        }
        h_call(st, code, ext, pc, len)
    }
}

/// The `CALL` at `pc` (its operands from `start` up a `len`-deep stack) of a
/// leaf builtin whose leaf half declined, or of an instance's
/// `__reduce_ex__`, through the core loop's lane for them
/// (`Interpreter::core_leaf_call_lane`), the frame synced first: `0` the
/// result at `start`, `RELOAD` the core loop goes on, `CALL_DECLINED`
/// nothing done.
///
/// # Safety
///
/// As [`h_call`]'s.
unsafe fn leaf_call_lane(st: &mut State, pc: usize, start: usize, len: usize) -> u32 {
    // SAFETY: the caller's contract.
    unsafe {
        if st.sw.is_null() {
            return CALL_DECLINED;
        }
        let frame = &mut *st.frame;
        frame.stack.set_len(len);
        frame.pc = pc as u32;
        let sw = &mut *st.sw;
        *sw.last = st.last;
        let interp = &mut *st.interp.cast_mut();
        if !interp.core_leaf_call_lane(sw) {
            return CALL_DECLINED;
        }
        if finished_in_place(st, start) {
            0
        } else {
            RELOAD
        }
    }
}

/// `CALL_KW` (the `pc`th instruction of `code`) on a stack `len` deep, as
/// the core loop's arm runs it: a natively served class's constructor, a
/// native method or builtin that takes keywords, or a pure leaf callee in
/// place (`0`, the result in the callee's slot); a plain Python function
/// as an inline activation switched to (`RELOAD`); anything else declined
/// untouched.
unsafe extern "C" fn h_call_kw(st: *mut State, code: *const CodeObject, pc: u64, len: u64) -> u32 {
    // SAFETY: the code passes its live state, its own code, one of its
    // `CALL_KW`s and its stack depth. The in-place shapes run no Python
    // code; every operand that leaves is checked to leave by a plain
    // decrement.
    unsafe {
        let st = &mut *st;
        let (code, pc, len) = (&*code, pc as usize, len as usize);
        let argc = code.instructions[pc].arg as usize;
        let base = st.stack;
        let Some(Object::Tuple(names)) = len.checked_sub(1).map(|k| &*base.add(k)) else {
            return CALL_DECLINED;
        };
        let kwc = names.len();
        if len < kwc + argc + 3 {
            return CALL_DECLINED;
        }
        let start = len - kwc - argc - 3;
        Interpreter::core_instance_callee(base.add(start));
        let ops = std::slice::from_raw_parts(base.add(start), len - start);
        let interp = &*st.interp;
        let r = match &ops[0] {
            Object::Type(ty) if ty.native_kind.get() != 0 => {
                Interpreter::core_native_ctor_kw(ops, argc, true)
            }
            Object::Builtin(_)
                if matches!(&ops[1], Object::Instance(i)
                    if crate::stdlib::decimal_native::is_decimal_instance(i)) =>
            {
                Interpreter::core_native_method_kw(ops, argc)
            }
            Object::BoundMethod(_) | Object::Builtin(_) => {
                Interpreter::core_native_kw_call(ops, argc)
            }
            _ if st.depth_cell.is_null() => None,
            _ => interp.core_pure_kw_call(code, pc, ops, argc, st.depth_cell),
        };
        let Some(r) = r else {
            // A plain Python callee switches in place, as `CALL`'s does.
            if !matches!(&ops[0], Object::Function(_)) || st.sw.is_null() {
                return CALL_DECLINED;
            }
            let frame = &mut *st.frame;
            frame.stack.set_len(len);
            frame.pc = pc as u32;
            let sw = &mut *st.sw;
            *sw.last = st.last;
            let saved_last = sw.last;
            let interp = &mut *st.interp.cast_mut();
            if !interp.core_call_kw(sw, pc) {
                if sw.pending.is_none() {
                    sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Step));
                }
                return RELOAD;
            }
            // A compiled callee runs from here, as for `CALL`.
            if let Some(r) = run_pushed_directly(st, pc, start, saved_last) {
                return r;
            }
            return RELOAD;
        };
        for k in start..len {
            crate::drop_hot(base.add(k).read());
        }
        base.add(start).write(r);
        0
    }
}

/// `CALL_FUNCTION_EX` (the `pc`th instruction of `code`) on a stack `len`
/// deep, as the core loop's arm runs it (`Interpreter::core_call_ex`, else
/// its lane), the frame synced first: `0` the result in place (a compiled
/// callee run from here, as for `CALL`, or a call finished in place),
/// `RELOAD` the core loop goes on.
unsafe extern "C" fn h_call_ex(st: *mut State, code: *const CodeObject, pc: u64, len: u64) -> u32 {
    // SAFETY: the code passes its live state, its own code, one of its
    // `CALL_FUNCTION_EX`s and its stack depth.
    unsafe {
        let _ = code;
        let st = &mut *st;
        let (pc, len) = (pc as usize, len as usize);
        if st.sw.is_null() || len < 4 {
            return CALL_DECLINED;
        }
        let start = len - 4;
        let frame = &mut *st.frame;
        frame.stack.set_len(len);
        frame.pc = pc as u32;
        let sw = &mut *st.sw;
        *sw.last = st.last;
        let saved_last = sw.last;
        let interp = &mut *st.interp.cast_mut();
        if !interp.core_call_ex(sw, pc, st.snap_gen) {
            if sw.pending.is_none() && !interp.core_call_lane(sw) {
                sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Step));
            }
            return if finished_in_place(st, start) { 0 } else { RELOAD };
        }
        if sw.pending.is_none() && !std::ptr::eq(sw.cur, st.frame) {
            if let Some(r) = run_pushed_directly(st, pc, start, saved_last) {
                return r;
            }
            if run_callee_directly(st, start) {
                st.len = start + 1;
                return 0;
            }
        }
        if finished_in_place(st, start) {
            return 0;
        }
        RELOAD
    }
}

/// `RETURN_VALUE` (the `pc`th instruction) of an inline activation with
/// the value atop a `len`-deep stack: the frame synced and the caller
/// switched back to as the core loop's arm does (`RELOAD`), the state
/// synced for a direct caller to finish it (`DIRECT_RET`, see
/// [`direct_call`]), or `0` for the root activation's, which the core
/// loop's quiet loop returns from.
unsafe extern "C" fn h_return(st: *mut State, pc: u64, len: u64) -> u32 {
    // SAFETY: the code passes its live state, whose switch is the core
    // loop's (non-null: generator bodies don't return through here), and
    // its stack depth.
    unsafe {
        let st = &mut *st;
        let sw = &mut *st.sw;
        let Some(top) = (*sw.inl).last() else {
            return 0;
        };
        // A direct call's activation: its caller finishes the return.
        if top.direct && std::ptr::eq(&raw const *top.frame, st.frame) {
            st.len = len as usize;
            st.pc = pc as usize;
            return DIRECT_RET;
        }
        let frame = &mut *st.frame;
        frame.stack.set_len(len as usize);
        frame.pc = pc as u32;
        *sw.last = st.last;
        let interp = &mut *st.interp.cast_mut();
        if !interp.core_return(sw, st.snap_gen) {
            sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Step));
        }
        RELOAD
    }
}

/// `GET_AWAITABLE` (`kind` 0) or `GET_YIELD_FROM_ITER` (`kind` 1) of the
/// value at `at` (the stack's top), as the core loop's arms run them: `0`
/// the delegate in place (a coroutine nothing has started, a generator,
/// or an instance's generator `__await__` / `__iter__`, made here),
/// anything else declined untouched.
unsafe extern "C" fn h_await_iter(st: *mut State, at: *mut Object, kind: u64) -> u32 {
    // SAFETY: the code passes its live state and its initialized top slot.
    unsafe {
        let st = &*st;
        let top = &*at;
        let ok = if kind == 0 {
            match top {
                // A read between instructions.
                Object::Coroutine(g) => g
                    .state
                    .peek()
                    .is_some_and(|s| matches!(s, crate::object::GeneratorState::Created(_))),
                // A `types.coroutine` generator is its own.
                Object::Generator(g) => {
                    matches!(&g.code, Object::Code(c) if c.is_iterable_coroutine)
                }
                _ => false,
            }
        } else {
            let code = &(*st.frame).code;
            match top {
                Object::Generator(_) => true,
                Object::Coroutine(_) => code.is_coroutine || code.is_iterable_coroutine,
                _ => false,
            }
        };
        if ok {
            return 0;
        }
        if matches!(top, Object::Instance(_)) && Interpreter::core_droppable(top) {
            let name = if kind == 0 { "__await__" } else { "__iter__" };
            if let Some(gen) = (*st.interp).instance_gen_call(top, name) {
                // The instance (droppable) is replaced in place.
                crate::drop_hot(std::mem::replace(&mut *at, gen));
                return 0;
            }
        }
        1
    }
}

/// `SEND` at `pc` of the running activation, its stack `len` deep: the
/// generator or coroutine below the top resumed and switched to as the
/// core loop's arm does (`RELOAD`), or, a compiled delegate run from here
/// that returned into this `SEND`, its result in the sent value's place
/// for the code to go on at the `END_SEND` (`0`); anything else (another
/// delegate, or a generator's fast step, which never switches) declined
/// untouched.
unsafe extern "C" fn h_send(st: *mut State, pc: u64, len: u64) -> u32 {
    // SAFETY: the code passes its live state and its stack depth (the
    // delegate and the sent value on top).
    unsafe {
        let st = &mut *st;
        let len = len as usize;
        if st.sw.is_null()
            || len < 2
            || !matches!(
                &*st.stack.add(len - 2),
                Object::Generator(_) | Object::Coroutine(_)
            )
        {
            return 1;
        }
        let sw = &mut *st.sw;
        let frame = &mut *st.frame;
        frame.stack.set_len(len);
        frame.pc = pc as u32;
        *sw.last = st.last;
        let interp = &mut *st.interp.cast_mut();
        if !interp.core_gen_resume(sw, pc as usize, crate::InlineResume::Send) {
            sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Step));
            return RELOAD;
        }
        if sw.pending.is_none() && !std::ptr::eq(sw.cur, st.frame) && run_switched_directly(st) {
            let frame = &*st.frame;
            let exit = pc as usize + 1 + frame.code.instructions[pc as usize].arg as usize;
            if frame.pc as usize == exit && frame.stack.len() == len {
                st.len = len;
                return 0;
            }
        }
        RELOAD
    }
}

/// `YIELD_VALUE` at `pc` of the running activation, an inline generator
/// resume (its stack `len` deep, the value on top), as the core loop's arm
/// runs it: the generator parks and its consumer, the value delivered, is
/// switched to (`RELOAD`); declined untouched for a generator's fast step,
/// a root activation (a draining consumer's fold is the arm's), or a yield
/// the switch doesn't take.
unsafe extern "C" fn h_gen_yield(st: *mut State, pc: u64, len: u64) -> u32 {
    // SAFETY: the code passes its live state and its stack depth.
    unsafe {
        let st = &mut *st;
        if st.sw.is_null() || (*(*st.sw).inl).is_empty() {
            return 1;
        }
        let sw = &mut *st.sw;
        let frame = &mut *st.frame;
        frame.stack.set_len(len as usize);
        frame.pc = pc as u32;
        *sw.last = st.last;
        let interp = &mut *st.interp.cast_mut();
        if !interp.core_gen_yield(sw, st.snap_gen) {
            sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Step));
        } else {
            // A `yield from` chain passes the value straight up.
            while sw.pending.is_none()
                && interp.core_yield_chains(sw)
                && interp.core_gen_yield(sw, st.snap_gen)
            {}
        }
        RELOAD
    }
}

/// `RETURN_VALUE` at `pc` of the running activation, an inline generator
/// resume, as the core loop's arm runs it (see [`h_return`]); declined
/// untouched for a generator's fast step or a root activation.
unsafe extern "C" fn h_gen_return(st: *mut State, pc: u64, len: u64) -> u32 {
    // SAFETY: the code passes its live state and its stack depth.
    unsafe {
        let st = &mut *st;
        if st.sw.is_null() || (*(*st.sw).inl).is_empty() {
            return 1;
        }
        let sw = &mut *st.sw;
        let frame = &mut *st.frame;
        frame.stack.set_len(len as usize);
        frame.pc = pc as u32;
        *sw.last = st.last;
        let interp = &mut *st.interp.cast_mut();
        if !interp.core_return(sw, st.snap_gen) {
            sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Step));
        }
        RELOAD
    }
}

/// `END_SEND` on the slots at `at` (the delegate) and above it (the
/// result): the result moves down over the delegate, which leaves by a
/// plain decrement (`0`); anything else declined untouched.
unsafe extern "C" fn h_end_send(at: *mut Object) -> u32 {
    // SAFETY: the code passes two initialized slots atop its stack.
    unsafe {
        if !Interpreter::core_droppable(&*at) {
            return 1;
        }
        let v = at.add(1).read();
        crate::drop_hot(std::mem::replace(&mut *at, v));
        0
    }
}

/// `LOAD_DEREF i` of the running frame, as the core loop's arm runs it:
/// `0` the cell's value written to the free slot `dst`, anything else
/// declined untouched.
unsafe extern "C" fn h_load_deref(st: *mut State, i: u64, dst: *mut Object) -> u32 {
    // SAFETY: the code passes its live state (whose frame is the running
    // one) and a free stack slot.
    unsafe {
        let frame = &*(*st).frame;
        let Some(cell) = frame.cells.get(i as usize) else {
            return 1;
        };
        let Some(v) = cell.peek() else {
            return 1;
        };
        if matches!(v, Object::Unbound) {
            return 1;
        }
        dst.write(Interpreter::clone_operand(v));
        0
    }
}

/// `RAISE_VARARGS n` (`raise e`, or `raise e from cause`; the `pc`th
/// instruction) over the operands atop a `len`-deep stack, as the core
/// loop's arm raises: an exception instance or builtin exception class,
/// and a cause of `None` or an exception instance. `RAISED` the exception
/// in `st` (the operands gone, `st` past the instruction); anything else
/// (`CALL_DECLINED`) is the core loop's, untouched.
unsafe extern "C" fn h_raise(st: *mut State, pc: u64, len: u64, n: u64) -> u32 {
    // SAFETY: the code passes its live state, one of its raises, and its
    // stack depth (the stack written out).
    unsafe {
        let st = &mut *st;
        let (len, n) = (len as usize, n as usize);
        if st.sw.is_null() || !(1..=2).contains(&n) || len < n {
            return CALL_DECLINED;
        }
        let bt = crate::builtin_types::builtin_types();
        let instance = |v: &Object| match v {
            Object::Instance(i) => {
                let cls = i.cls_raw();
                cls.flags.is_exception || cls.is_subclass_of(&bt.base_exception)
            }
            _ => false,
        };
        let exc_ok = match &*st.stack.add(len - n) {
            v @ Object::Instance(_) => instance(v),
            Object::Type(t) => {
                t.flags.is_builtin
                    && t.flags.is_exception
                    && !t.is_subclass_of(&bt.base_exception_group)
            }
            _ => false,
        };
        let cause_ok = n == 1 || {
            let c = &*st.stack.add(len - 1);
            matches!(c, Object::None) || instance(c)
        };
        if !exc_ok || !cause_ok {
            return CALL_DECLINED;
        }
        let cause = (n == 2).then(|| st.stack.add(len - 1).read());
        let arg = st.stack.add(len - n).read();
        let interp = &mut *st.interp.cast_mut();
        let globals = (*st.frame).globals.clone();
        let raised = interp
            .instantiate_raised_class(arg, &globals)
            .and_then(|arg| Interpreter::normalize_exception(arg, cause))
            .map(|mut exc| {
                interp.attach_implicit_context(&mut exc);
                Interpreter::sync_exc_attrs(&exc);
                crate::RuntimeError::PyException(exc)
            });
        st.err = Some(match raised {
            Ok(e) | Err(e) => e,
        });
        st.len = len - n;
        st.pc = pc as usize + 1;
        RAISED
    }
}

/// `DELETE_FAST i` of the running frame's bound local: `0` the local
/// emptied and its value released, anything else (an unbound local, whose
/// `UnboundLocalError` is the core loop's) declined untouched.
unsafe extern "C" fn h_delete_fast(st: *mut State, i: u64) -> u32 {
    // SAFETY: the code passes its live state and one of its locals.
    unsafe {
        let st = &*st;
        let slot = st.locals.add(i as usize);
        if matches!(&*slot, Object::Unbound) {
            return 1;
        }
        let old = std::mem::replace(&mut *slot, Object::Unbound);
        (*st.interp.cast_mut()).release(old);
        0
    }
}

/// The exception a helper raised (`RAISED`, `st` synced past the raising
/// instruction) caught by a handler of the running activation, as the
/// quiet loop catches it (`Interpreter::quiet_catch`): `0` the state at
/// the handler (its stack set up), `RELOAD` caught but the core loop goes
/// on (an eval breaker came due), `RAISED` uncaught (the error back in
/// `st`).
unsafe extern "C" fn h_catch(st: *mut State) -> u32 {
    // SAFETY: the code passes its live state.
    unsafe {
        let st = &mut *st;
        if st.sw.is_null() {
            return RAISED;
        }
        let Some(err) = st.err.take() else {
            return RAISED;
        };
        let frame = &mut *st.frame;
        frame.stack.set_len(st.len);
        frame.pc = st.pc as u32;
        let sw = &mut *st.sw;
        *sw.last = st.last;
        if !std::ptr::eq(sw.cur, st.frame) {
            st.err = Some(err);
            return RAISED;
        }
        let interp = &mut *st.interp.cast_mut();
        let depth = (*sw.inl).len();
        let mut tmp = None;
        let (_, _, shell) = sw.activation(depth, &mut tmp);
        let raise_pc = st.pc - 1;
        match interp.quiet_catch(frame, &mut *shell.cast::<crate::QuietShell<'_>>(), err) {
            Ok(()) => {
                *sw.last = raise_pc;
                st.pc = frame.pc as usize;
                st.len = frame.stack.len();
                st.last = raise_pc;
                if interp.quiet_caught_yields(st.snap_gen) {
                    return RELOAD;
                }
                0
            }
            Err(e) => {
                st.err = Some(e);
                RAISED
            }
        }
    }
}

/// `LOAD_CLOSURE i` of the running frame: `0` the cell written to the
/// free slot `dst`, anything else declined untouched.
unsafe extern "C" fn h_load_closure(st: *mut State, i: u64, dst: *mut Object) -> u32 {
    // SAFETY: the code passes its live state and a free stack slot.
    unsafe {
        let frame = &*(*st).frame;
        let Some(cell) = frame.cells.get(i as usize) else {
            return 1;
        };
        dst.write(Object::Cell(cell.clone()));
        0
    }
}

/// `STORE_DEREF i` of the value at stack slot `at` into the running
/// frame's cell, as the core loop's arm runs it: `0` stored (the value
/// moved in; the displaced one released), anything else declined
/// untouched.
unsafe extern "C" fn h_store_deref(st: *mut State, i: u64, at: *mut Object) -> u32 {
    // SAFETY: the code passes its live state and an initialized slot.
    unsafe {
        let frame = &*(*st).frame;
        let Some(cell) = frame.cells.get(i as usize) else {
            return 1;
        };
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return 1;
        };
        let old = std::mem::replace(&mut *slot, at.read());
        drop(slot);
        drop(old);
        0
    }
}

/// `LOAD_COMMON_CONSTANT arg`: `0` the constant written to the free slot
/// `dst`, anything else declined untouched.
unsafe extern "C" fn h_common_const(st: *mut State, arg: u64, dst: *mut Object) -> u32 {
    // SAFETY: the code passes its live state and a free stack slot.
    unsafe {
        match (*(*st).interp).common_constant(arg as u32) {
            Some(v) => {
                dst.write(v);
                0
            }
            None => 1,
        }
    }
}

/// `MAKE_FUNCTION flags` over the operands from stack slot `at` up (the
/// code object on top): `0` the function at `at`, anything else declined
/// untouched (a function watcher's event is the full handler's).
unsafe extern "C" fn h_make_function(st: *mut State, at: *mut Object, flags: u64) -> u32 {
    // SAFETY: the code passes its live state and the instruction's
    // initialized operand slots.
    unsafe {
        let frame = &*(*st).frame;
        let n = 1 + (flags & 0xf).count_ones() as usize;
        if crate::capi_watchers::funcs_active() || !matches!(&*at.add(n - 1), Object::Code(_)) {
            return 1;
        }
        let mut k = n;
        let f = crate::make_function(
            flags as u32,
            || {
                k -= 1;
                Ok(at.add(k).read())
            },
            &frame.globals,
            &frame.builtins,
        )
        .expect("a code object on top makes a function");
        at.write(f);
        0
    }
}

/// `SET_FUNCTION_ATTRIBUTE flag` of the value at stack slot `at` on the
/// fresh function above it: `0` the function at `at`, `RAISED` its error
/// (`st` synced past the instruction at `pc`), anything else declined
/// untouched.
unsafe extern "C" fn h_set_function_attr(
    st: *mut State,
    at: *mut Object,
    flag: u64,
    pc: u64,
) -> u32 {
    // SAFETY: the code passes its live state and two initialized slots,
    // with nothing virtual below them.
    unsafe {
        if !matches!(&*at.add(1), Object::Function(_)) {
            return 1;
        }
        let (value, func) = (at.read(), at.add(1).read());
        match crate::set_function_attribute(func, value, flag as u32) {
            Ok(f) => {
                at.write(f);
                0
            }
            Err(e) => {
                let st = &mut *st;
                st.len = at.offset_from(st.stack) as usize;
                st.pc = pc as usize + 1;
                st.err = Some(e);
                RAISED
            }
        }
    }
}

/// `LIST_TO_TUPLE` of the list at stack slot `at` only the stack holds:
/// `0` its items' tuple in its place, anything else declined untouched.
unsafe extern "C" fn h_list_to_tuple(at: *mut Object) -> u32 {
    // SAFETY: the code passes an initialized slot.
    unsafe {
        let Object::List(l) = &*at else {
            return 1;
        };
        if Rc::strong_count(l) != 1 || !droppable(&*at) {
            return 1;
        }
        let Some(items) = l.peek_mut() else {
            return 1;
        };
        let t = Object::new_tuple(std::mem::take(items));
        crate::drop_hot(std::mem::replace(&mut *at, t));
        0
    }
}

/// `GET_ITER` of the value at stack slot `at`, as the core loop's arm
/// runs it: `0` the iterator in its place, anything else declined
/// untouched.
unsafe extern "C" fn h_get_iter(at: *mut Object) -> u32 {
    // SAFETY: the code passes an initialized stack slot.
    unsafe {
        let v = &*at;
        if matches!(
            v,
            Object::Iter(_) | Object::LazyIter(_) | Object::Generator(_)
        ) {
            return 0;
        }
        if !matches!(
            v,
            Object::List(_)
                | Object::Tuple(_)
                | Object::Range(_)
                | Object::Dict(_)
                | Object::DictView(_)
                | Object::Str(_)
                | Object::Bytes(_)
                | Object::Set(_)
                | Object::FrozenSet(_)
        ) || !droppable(v)
        {
            return 1;
        }
        let Ok(it) = v.make_iter() else {
            return 1;
        };
        let it = Object::Iter(Rc::new(crate::sync::RefCell::new(it)));
        crate::drop_hot(std::mem::replace(&mut *at, it));
        0
    }
}

// ---- the compiler ---------------------------------------------------------

/// The process's frame compiler. Code it emits lives as long as the
/// process (code objects are shared between threads, and their native
/// code is never freed).
struct Engine {
    module: JITModule,
    ctx: cranelift_codegen::Context,
    fbctx: FunctionBuilderContext,
    ptr: types::Type,
    tags: Tags,
    next: u32,
}

// SAFETY: the engine is only used under its mutex.
unsafe impl Send for Engine {}

static ENGINE: std::sync::Mutex<Option<Option<Engine>>> = std::sync::Mutex::new(None);

impl Engine {
    fn new() -> Option<Engine> {
        let tags = tags()?;
        let mut flags = settings::builder();
        flags.set("use_colocated_libcalls", "false").ok()?;
        flags.set("is_pic", "false").ok()?;
        // The lowering already does the optimizing that pays here (operands
        // kept unboxed and in registers): Cranelift's mid-end costs more to
        // run than it saves. Its register allocator pays for itself (the
        // single-pass one spills around every helper call).
        flags.set("opt_level", "none").ok()?;
        if let Ok(a) = std::env::var("WEAVEPY_FRAME_JIT_REGALLOC") {
            flags.set("regalloc_algorithm", &a).ok()?;
        }
        if !cfg!(debug_assertions) && !verify() {
            flags.set("enable_verifier", "false").ok()?;
        }
        let isa = cranelift_native::builder()
            .ok()?
            .finish(settings::Flags::new(flags))
            .ok()?;
        let module = JITModule::new(JITBuilder::with_isa(
            isa,
            cranelift_module::default_libcall_names(),
        ));
        let ptr = module.target_config().pointer_type();
        let ctx = module.make_context();
        Some(Engine {
            module,
            ctx,
            fbctx: FunctionBuilderContext::new(),
            ptr,
            tags,
            next: 0,
        })
    }
}

/// Whether to verify the IR of every compiled frame and report failures
/// (`WEAVEPY_FRAME_JIT_VERIFY`, a debugging aid).
fn verify() -> bool {
    static VERIFY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *VERIFY.get_or_init(|| std::env::var_os("WEAVEPY_FRAME_JIT_VERIFY").is_some())
}

/// Compile `code`; `None` leaves it to the core loop. A compiler panic
/// turns frame compilation off for the process rather than taking the
/// interpreter down.
fn compile(code: &CodeObject, ext: &CodeConstObjects, nlocals: usize) -> Option<Native> {
    if code.instructions.is_empty() || code.instructions.len() > MAX_INSTRUCTIONS {
        return None;
    }
    crate::tier2::ensure_obj_layout();
    let depths = weavepy_compiler::cpython_code::compute_startdepths(code);
    let mut guard = ENGINE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let engine = guard.get_or_insert_with(Engine::new).as_mut()?;
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        compile_with(engine, code, ext, nlocals, &depths)
    }));
    match built {
        Ok(native) => native,
        Err(_) => {
            *guard = Some(None);
            None
        }
    }
}

fn compile_with(
    engine: &mut Engine,
    code: &CodeObject,
    ext: &CodeConstObjects,
    nlocals: usize,
    depths: &[i64],
) -> Option<Native> {
    engine.module.clear_context(&mut engine.ctx);
    let ptr = engine.ptr;
    let sig = &mut engine.ctx.func.signature;
    sig.params.push(AbiParam::new(ptr));
    sig.returns.push(AbiParam::new(types::I32));
    let signature = sig.clone();
    let name = format!("wpframe_{}", engine.next);
    engine.next += 1;
    let id = engine
        .module
        .declare_function(&name, Linkage::Local, &engine.ctx.func.signature)
        .ok()?;
    let ninstrs = code.instructions.len();
    // The attribute sites' field caches (allocated here if no read has
    // recorded one yet: the code addresses them).
    let field_slots = ext.alloc_field_sites(code);
    let mut entries: Vec<bool>;
    let mut globals: Vec<Box<GlobalCache>>;
    let mut slots: Vec<Box<SlotCache>>;
    let mut methods: Vec<Box<MethodCache>>;
    let mut direct: Vec<Box<DirectSite>>;
    let mut held: Vec<Rc<crate::object::BuiltinFn>>;
    let mut mods: Vec<Box<ModCache>>;
    let t0 = stats::enabled().then(std::time::Instant::now);
    // A body whose code comes out large is lowered again lean: compiling
    // costs about the same per IR instruction, and a large body's time
    // goes to its calls more than to the shortcuts in line.
    let mut lean = false;
    let built = loop {
        let b = FunctionBuilder::new(&mut engine.ctx.func, &mut engine.fbctx);
        let mut lower = Lower::new(b, ptr, engine.tags, code, ext, nlocals, depths, field_slots);
        lower.cold = lean;
        let done = lower.lower();
        entries = (0..ninstrs).map(|pc| lower.enters_at(pc)).collect();
        globals = std::mem::take(&mut lower.global_caches);
        slots = std::mem::take(&mut lower.slot_caches);
        methods = std::mem::take(&mut lower.method_caches);
        direct = std::mem::take(&mut lower.direct_sites);
        held = std::mem::take(&mut lower.held);
        mods = std::mem::take(&mut lower.mod_caches);
        if done {
            lower.b.seal_all_blocks();
            lower.b.finalize();
        }
        if done && !lean && engine.ctx.func.dfg.num_insts() > lean_above() {
            lean = true;
            engine.fbctx = FunctionBuilderContext::new();
            engine.module.clear_context(&mut engine.ctx);
            engine.ctx.func.signature = signature.clone();
            continue;
        }
        break done;
    };
    if !built {
        // An unfinished function leaves the builder's state behind.
        engine.fbctx = FunctionBuilderContext::new();
        engine.module.clear_context(&mut engine.ctx);
        return None;
    }
    if std::env::var("WEAVEPY_FRAME_JIT_DUMP").is_ok_and(|n| code.qualname.contains(&n)) {
        eprintln!("{}", engine.ctx.func.display());
    }
    let t1 = stats::enabled().then(std::time::Instant::now);
    let insts = engine.ctx.func.dfg.num_insts();
    let blocks = engine.ctx.func.dfg.num_blocks();
    let defined = engine.module.define_function(id, &mut engine.ctx);
    if let (Some(t0), Some(t1)) = (t0, t1) {
        let bytes = engine
            .ctx
            .compiled_code()
            .map_or(0, |c| c.code_buffer().len());
        eprintln!(
            "frame jit: {} lowered in {:?} ({insts} IR instructions, {blocks} blocks{}, {bytes} bytes), compiled in {:?}",
            code.qualname,
            t1 - t0,
            if lean { ", lean" } else { "" },
            t1.elapsed()
        );
    }
    if let (Err(e), true) = (&defined, verify()) {
        eprintln!(
            "weavepy: frame {:?} failed to compile: {e:?}",
            code.qualname
        );
    }
    engine.module.clear_context(&mut engine.ctx);
    defined.ok()?;
    engine.module.finalize_definitions().ok()?;
    let func = engine.module.get_finalized_function(id);
    // SAFETY: `func` is a finalized function with exactly the signature
    // declared above, and the module (never dropped) keeps it alive.
    let func = unsafe { std::mem::transmute::<*const u8, NativeFn>(func) };
    Some(Native {
        func,
        nlocals,
        fails: entries.iter().map(|_| AtomicU8::new(0)).collect(),
        entries: entries.into_iter().map(AtomicBool::new).collect(),
        name: code.qualname.clone(),
        ops: code.instructions.iter().map(|i| i.op).collect(),
        depths: depths.into(),
        need: stack_need(depths),
        _globals: globals,
        _slots: slots,
        _methods: methods,
        _direct: direct,
        _held: held,
        _mods: mods,
    })
}

const FLAGS: MemFlags = MemFlags::trusted();

/// The stack capacity the code needs: the deepest point plus an
/// instruction's own pushes (it writes stack slots at fixed offsets).
fn stack_need(depths: &[i64]) -> usize {
    depths.iter().copied().max().unwrap_or(0).max(0) as usize + 4
}

/// A value on the virtual operand stack: where it lives until something
/// needs it on the frame's stack.
#[derive(Clone, Copy)]
enum Item {
    /// On the frame's stack, at this slot (owned there).
    Mem(usize),
    /// The value of this local, not copied yet (no reference taken).
    Local(u32),
    /// This constant, not copied yet.
    Const(u32),
    /// An `int`, unboxed.
    Int(Value),
    /// A `float`, unboxed.
    Float(Value),
    /// A `bool`, unboxed (an `i8`, `0` or `1`).
    Bool(Value),
    /// A scalar (`None`, `bool`, `int` or `float`) known only at run time:
    /// its tag, word payload, and `bool` byte.
    Dyn(Value, Value, Value),
}

/// An operand's type, as far as the code knows it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Int,
    Float,
    Bool,
    None,
    /// Read at run time.
    Unknown,
}

/// An operand ready for a scalar operation: its kind, and its tag and
/// payload values (the tag only for an unknown kind).
#[derive(Clone, Copy)]
struct Opnd {
    kind: Kind,
    tag: Option<Value>,
    /// The `i64` word (an `int`, a `float`'s bits), or a `bool`'s `i8`.
    word: Value,
    /// A `bool`'s byte (unknown kinds).
    byte: Option<Value>,
}

struct Lower<'a> {
    b: FunctionBuilder<'a>,
    ptr: types::Type,
    tags: Tags,
    layout: Option<&'static weavepy_jit::ObjLayout>,
    code: &'a CodeObject,
    ext: &'a CodeConstObjects,
    nlocals: usize,
    depths: &'a [i64],
    field_slots: crate::Sites<'a, FieldSlot>,
    st: Value,
    locals: Value,
    stack: Value,
    /// The instruction that last ran, when known here; `None` at a block's
    /// start, whose `st.last` the entry or the jump in wrote.
    last_pc: Option<usize>,
    /// Locals that may be unbound when a block starts (parameters no
    /// `DELETE_FAST` names never are), and the ones this block has seen
    /// bound.
    maybe_unbound: Vec<bool>,
    bound: Vec<bool>,
    /// Each block start's block (instruction index → block).
    blocks: Vec<Option<Block>>,
    /// Dispatches on the state's pc and depth (the entry, and helpers that
    /// move the pc by a run-time amount).
    dispatch: Block,
    /// Whether copies are on an exit path (one helper call each, not the
    /// scalar test in line: exits are rare, and code size costs compile
    /// time), or the whole body is lowered lean (see `compile_with`):
    /// global reads and stack operands' arithmetic through their helpers
    /// too.
    cold: bool,
    /// The virtual stack above the frame's, and the logical depth.
    vs: Vec<Item>,
    depth: usize,
    sigs: Vec<(Vec<Type>, Option<Type>, SigRef)>,
    /// Side exits waiting for their blocks to be filled (after the body:
    /// the builder fills one block at a time).
    side_exits: Vec<SideExit>,
    /// The `LOAD_GLOBAL` and `LOAD_ATTR` sites' caches, which the code
    /// addresses.
    #[allow(clippy::vec_box)]
    global_caches: Vec<Box<GlobalCache>>,
    #[allow(clippy::vec_box)]
    slot_caches: Vec<Box<SlotCache>>,
    method_caches: Vec<Box<MethodCache>>,
    #[allow(clippy::vec_box)]
    direct_sites: Vec<Box<DirectSite>>,
    /// The builtins the method kernels' calls address.
    held: Vec<Rc<crate::object::BuiltinFn>>,
    /// The method loads' module-attribute caches.
    #[allow(clippy::vec_box)]
    mod_caches: Vec<Box<ModCache>>,
    /// The block a helper's raise goes to (see [`Self::leave`]), made on
    /// first use.
    catch: Option<Block>,
}

/// An exit block of [`Lower::exit_with`], filled after the body from the
/// lowering state at its branch.
struct SideExit {
    block: Block,
    vs: Vec<Item>,
    depth: usize,
    last_pc: Option<usize>,
    restore: Vec<Item>,
    status: u32,
    pc: usize,
}

/// Whether the native code runs `op` itself (the rest are handed to the
/// core loop: their successors start blocks).
fn native_op(op: OpCode) -> bool {
    matches!(
        op,
        OpCode::Nop
            | OpCode::NotTaken
            | OpCode::Resume
            | OpCode::CopyFreeVars
            | OpCode::LoadFast
            | OpCode::LoadFastBorrow
            | OpCode::LoadConst
            | OpCode::LoadSmallInt
            | OpCode::StoreFast
            | OpCode::PopTop
            | OpCode::BinaryOp
            | OpCode::CompareOp
            | OpCode::ToBool
            | OpCode::PopJumpIfFalse
            | OpCode::PopJumpIfTrue
            | OpCode::PopJumpIfNone
            | OpCode::PopJumpIfNotNone
            | OpCode::JumpForward
            | OpCode::JumpBackward
            | OpCode::ForIter
            | OpCode::LoadAttr
            | OpCode::IsOp
            | OpCode::CopyTop
            | OpCode::Swap
            | OpCode::BinarySubscr
            | OpCode::BinarySlice
            | OpCode::StoreSubscr
            | OpCode::ListAppend
            | OpCode::SetAdd
            | OpCode::MapAdd
            | OpCode::UnpackSequence
            | OpCode::ContainsOp
            | OpCode::LoadGlobal
            | OpCode::PushNull
            | OpCode::LoadMethodAttr
            | OpCode::StoreAttr
            | OpCode::BuildTuple
            | OpCode::BuildList
            | OpCode::LoadDeref
            | OpCode::GetIter
            | OpCode::UnaryOp
            | OpCode::LoadFastAndClear
            | OpCode::LoadClosure
            | OpCode::LoadClosureBorrow
            | OpCode::StoreDeref
            | OpCode::LoadCommonConstant
            | OpCode::MakeFunction
            | OpCode::SetFunctionAttribute
            | OpCode::ListToTuple
            | OpCode::UnpackEx
            | OpCode::LoadSuperAttr
            | OpCode::LoadSpecial
            | OpCode::PushExcInfo
            | OpCode::CheckExcMatch
            | OpCode::PopExcept
            | OpCode::DeleteFast
            | OpCode::StoreSlice
            | OpCode::DeleteSubscr
            | OpCode::GetAwaitable
            | OpCode::GetYieldFromIter
            | OpCode::Send
            | OpCode::EndSend
    )
}

/// Whether the native code runs the instruction at `pc` itself (as
/// [`native_op`], with the operands some instructions need).
fn native_at(code: &CodeObject, pc: usize) -> bool {
    let ins = code.instructions[pc];
    match ins.op {
        OpCode::BuildTuple => (1..=3).contains(&ins.arg),
        OpCode::BuildMap => (0..=8).contains(&ins.arg),
        OpCode::UnaryOp => ins.arg <= 3,
        OpCode::RaiseVarargs => matches!(ins.arg, 1 | 2),
        // A prologue `MAKE_CELL`: the activation's cells are built with it.
        OpCode::MakeCell => prologue_cell(code, pc),
        // (A slot shared with a cell saves the cell itself.)
        OpCode::LoadFastAndClear => {
            Interpreter::shared_cell_index(code, ins.arg as usize).is_none()
        }
        op => native_op(op),
    }
}

/// Whether the `MAKE_CELL` at `pc` is in the code's prologue, which every
/// activation's cells satisfy as they are built (the core loop runs it as
/// a no-op too); a later one is an inlined comprehension's fresh cell.
fn prologue_cell(code: &CodeObject, pc: usize) -> bool {
    code.instructions[..pc]
        .iter()
        .all(|i| matches!(i.op, OpCode::MakeCell | OpCode::CopyFreeVars))
}

impl<'a> Lower<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        mut b: FunctionBuilder<'a>,
        ptr: types::Type,
        tags: Tags,
        code: &'a CodeObject,
        ext: &'a CodeConstObjects,
        nlocals: usize,
        depths: &'a [i64],
        field_slots: crate::Sites<'a, FieldSlot>,
    ) -> Self {
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let st = b.block_params(entry)[0];
        let load = |b: &mut FunctionBuilder<'_>, ty, off| b.ins().load(ty, FLAGS, st, off);
        let locals = load(&mut b, ptr, S_LOCALS);
        let stack = load(&mut b, ptr, S_STACK);
        Lower {
            b,
            ptr,
            tags,
            layout: crate::tier2::published_obj_layout(),
            code,
            ext,
            nlocals,
            depths,
            field_slots,
            st,
            locals,
            stack,
            last_pc: None,
            maybe_unbound: {
                let params = code.arg_count as usize
                    + code.kwonly_count as usize
                    + usize::from(code.has_varargs)
                    + usize::from(code.has_varkeywords);
                let mut m: Vec<bool> = (0..nlocals).map(|i| i >= params).collect();
                for ins in &code.instructions {
                    if matches!(ins.op, OpCode::DeleteFast | OpCode::LoadFastAndClear) {
                        if let Some(x) = m.get_mut(ins.arg as usize) {
                            *x = true;
                        }
                    }
                }
                m
            },
            bound: vec![false; nlocals],
            blocks: Vec::new(),
            dispatch: Block::from_u32(0),
            cold: false,
            vs: Vec::new(),
            depth: 0,
            sigs: Vec::new(),
            side_exits: Vec::new(),
            global_caches: Vec::new(),
            slot_caches: Vec::new(),
            method_caches: Vec::new(),
            direct_sites: Vec::new(),
            held: Vec::new(),
            mod_caches: Vec::new(),
            catch: None,
        }
    }

    // ---- building blocks ----

    fn sig(&mut self, params: &[Type], ret: Option<Type>) -> SigRef {
        if let Some((_, _, s)) = self.sigs.iter().find(|(p, r, _)| p == params && *r == ret) {
            return *s;
        }
        let mut sig = Signature::new(self.b.func.signature.call_conv);
        for &t in params {
            sig.params.push(AbiParam::new(t));
        }
        if let Some(t) = ret {
            sig.returns.push(AbiParam::new(t));
        }
        let s = self.b.import_signature(sig);
        self.sigs.push((params.to_vec(), ret, s));
        s
    }

    /// Call the helper at `addr`: its `u32` result, if it has one.
    fn call(&mut self, addr: usize, args: &[Value], ret: bool) -> Option<Value> {
        self.call_typed(addr, args, ret.then_some(types::I32))
    }

    /// Call the helper at `addr`: its result of type `ret`, if any.
    fn call_typed(&mut self, addr: usize, args: &[Value], ret: Option<Type>) -> Option<Value> {
        let params: Vec<Type> = args
            .iter()
            .map(|&a| self.b.func.dfg.value_type(a))
            .collect();
        let sig = self.sig(&params, ret);
        let callee = self.b.ins().iconst(self.ptr, addr as i64);
        let inst = self.b.ins().call_indirect(sig, callee, args);
        ret.map(|_| self.b.inst_results(inst)[0])
    }

    fn set_last(&mut self, pc: usize) {
        self.last_pc = Some(pc);
    }

    /// Write the last instruction to the state (unless the state has it).
    fn store_last(&mut self) {
        if let Some(pc) = self.last_pc {
            let v = self.b.ins().iconst(types::I64, pc as i64);
            self.b.ins().store(FLAGS, v, self.st, S_LAST);
        }
    }

    /// The running thread's queued-finalizer flag (read where checked: a
    /// value live through the whole function costs the register allocator
    /// more than the load).
    fn dead_flag(&mut self) -> Value {
        let dead = self.b.ins().load(self.ptr, FLAGS, self.st, S_DEAD);
        self.b.ins().uload8(types::I32, FLAGS, dead, 0)
    }

    fn slot_addr(&mut self, slot: usize) -> Value {
        self.b.ins().iadd_imm(self.stack, 16 * slot as i64)
    }

    fn local_addr(&mut self, i: u32) -> Value {
        self.b.ins().iadd_imm(self.locals, 16 * i64::from(i))
    }

    fn const_addr(&mut self, k: u32) -> Value {
        let at = std::ptr::from_ref(&self.ext.objects[k as usize]) as i64;
        self.b.ins().iconst(self.ptr, at)
    }

    fn tag_at(&mut self, addr: Value) -> Value {
        self.b.ins().uload8(types::I64, FLAGS, addr, 0)
    }

    fn is_scalar(&mut self, tag: Value) -> Value {
        let one = self.b.ins().iconst(types::I64, 1);
        let bit = self.b.ins().ishl(one, tag);
        let m = self.b.ins().band_imm(bit, self.tags.scalar_mask());
        self.b.ins().icmp_imm(IntCC::NotEqual, m, 0)
    }

    fn write_tag(&mut self, dst: Value, t: u8) {
        let t = self.b.ins().iconst(types::I8, i64::from(t));
        self.b.ins().store(FLAGS, t, dst, 0);
    }

    fn copy16(&mut self, dst: Value, src: Value) {
        let a = self.b.ins().load(types::I64, FLAGS, src, 0);
        let w = self.b.ins().load(types::I64, FLAGS, src, 8);
        self.b.ins().store(FLAGS, a, dst, 0);
        self.b.ins().store(FLAGS, w, dst, 8);
    }

    /// Branch to `out` when `cond`; continue in a fresh block otherwise.
    fn branch_out(&mut self, cond: Value, out: Block) {
        let ok = self.b.create_block();
        self.b.ins().brif(cond, out, &[], ok, &[]);
        self.b.switch_to_block(ok);
    }

    // ---- the virtual stack ----

    fn push(&mut self, item: Item) {
        self.vs.push(item);
        self.depth += 1;
    }

    fn pop(&mut self) -> Item {
        self.depth -= 1;
        self.vs.pop().unwrap_or(Item::Mem(self.depth))
    }

    /// Write `item` to the frame's stack at `slot` (taking a reference for
    /// a heap value it copies).
    fn materialize(&mut self, item: Item, slot: usize) {
        match item {
            Item::Mem(s) => debug_assert_eq!(s, slot),
            Item::Local(i) => {
                let src = self.local_addr(i);
                let dst = self.slot_addr(slot);
                self.copy_value(dst, src);
            }
            Item::Const(k) => {
                let dst = self.slot_addr(slot);
                match self.ext.objects[k as usize] {
                    Object::Int(v) => {
                        let v = self.b.ins().iconst(types::I64, v);
                        self.write_tag(dst, self.tags.int);
                        self.b.ins().store(FLAGS, v, dst, 8);
                    }
                    Object::Float(x) => {
                        let v = self.b.ins().iconst(types::I64, x.to_bits() as i64);
                        self.write_tag(dst, self.tags.float);
                        self.b.ins().store(FLAGS, v, dst, 8);
                    }
                    Object::Bool(x) => {
                        let v = self.b.ins().iconst(types::I8, i64::from(x));
                        self.write_tag(dst, self.tags.boolean);
                        self.b.ins().store(FLAGS, v, dst, 1);
                    }
                    Object::None => self.write_tag(dst, self.tags.none),
                    _ => {
                        let src = self.const_addr(k);
                        self.call(h_clone as *const () as usize, &[dst, src], false);
                    }
                }
            }
            Item::Int(v) => {
                let dst = self.slot_addr(slot);
                self.write_tag(dst, self.tags.int);
                self.b.ins().store(FLAGS, v, dst, 8);
            }
            Item::Float(v) => {
                let dst = self.slot_addr(slot);
                self.write_tag(dst, self.tags.float);
                let bits = self.b.ins().bitcast(types::I64, MemFlags::new(), v);
                self.b.ins().store(FLAGS, bits, dst, 8);
            }
            Item::Bool(v) => {
                let dst = self.slot_addr(slot);
                self.write_tag(dst, self.tags.boolean);
                self.b.ins().store(FLAGS, v, dst, 1);
            }
            Item::Dyn(tag, word, byte) => {
                let dst = self.slot_addr(slot);
                let t = self.b.ins().ireduce(types::I8, tag);
                let b8 = self.b.ins().ireduce(types::I8, byte);
                self.b.ins().store(FLAGS, t, dst, 0);
                self.b.ins().store(FLAGS, b8, dst, 1);
                self.b.ins().store(FLAGS, word, dst, 8);
            }
        }
    }

    /// `*dst = (*src).clone()`: a scalar by value, anything else through
    /// its clone.
    fn copy_value(&mut self, dst: Value, src: Value) {
        if self.cold {
            self.call(h_clone as *const () as usize, &[dst, src], false);
            return;
        }
        let tag = self.tag_at(src);
        let scalar = self.is_scalar(tag);
        let by_value = self.b.create_block();
        let by_clone = self.b.create_block();
        let done = self.b.create_block();
        if self.tags.rc_counted == 0 && self.tags.rc_counted_hdr == 0 {
            self.b.ins().brif(scalar, by_value, &[], by_clone, &[]);
        } else {
            // An instance, list, dict, class, string or tuple while one
            // thread owns every count: a plain increment of its count.
            let counted = self.b.create_block();
            let heap = self.b.create_block();
            self.b.ins().brif(scalar, by_value, &[], heap, &[]);
            self.b.switch_to_block(heap);
            let one = self.b.ins().iconst(types::I64, 1);
            let bit = self.b.ins().ishl(one, tag);
            let any = self
                .b
                .ins()
                .band_imm(bit, self.tags.rc_counted | self.tags.rc_counted_hdr);
            let rc = self.b.ins().icmp_imm(IntCC::NotEqual, any, 0);
            let flag = self.b.ins().iconst(
                self.ptr,
                std::ptr::from_ref(crate::sync::rc_shared_flag()) as i64,
            );
            let shared = self.b.ins().load(types::I8, FLAGS, flag, 0);
            let solo = self.b.ins().icmp_imm(IntCC::Equal, shared, 0);
            let ok = self.b.ins().band(rc, solo);
            self.b.ins().brif(ok, counted, &[], by_clone, &[]);
            self.b.switch_to_block(counted);
            let at = self.count_addr(src, bit);
            let c = self.b.ins().load(types::I64, FLAGS, at, 0);
            let c1 = self.b.ins().iadd_imm(c, 1);
            self.b.ins().store(FLAGS, c1, at, 0);
            self.b.ins().jump(by_value, &[]);
        }
        self.b.switch_to_block(by_value);
        self.copy16(dst, src);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(by_clone);
        self.call(h_clone as *const () as usize, &[dst, src], false);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
    }

    /// The strong count's address of the counted heap value at `src`
    /// whose tag bit is `bit`: its payload word, or two words before it
    /// for a [`Tags::rc_counted_hdr`] variant.
    fn count_addr(&mut self, src: Value, bit: Value) -> Value {
        let w = self.b.ins().load(self.ptr, FLAGS, src, 8);
        if self.tags.rc_counted_hdr == 0 {
            return w;
        }
        let hdr = self.b.ins().band_imm(bit, self.tags.rc_counted_hdr);
        let hdr = self.b.ins().icmp_imm(IntCC::NotEqual, hdr, 0);
        let before = self.b.ins().iadd_imm(w, -16);
        self.b.ins().select(hdr, before, w)
    }

    /// Write the whole virtual stack to the frame's.
    fn flush(&mut self) {
        let base = self.depth - self.vs.len();
        let items = std::mem::take(&mut self.vs);
        for (k, item) in items.into_iter().enumerate() {
            self.materialize(item, base + k);
        }
    }

    /// Write the virtual stack's copies of local `i` to the frame's stack
    /// (before the local changes).
    fn flush_local(&mut self, i: u32) {
        let base = self.depth - self.vs.len();
        for k in 0..self.vs.len() {
            if let Item::Local(j) = self.vs[k] {
                if j == i {
                    self.materialize(Item::Local(j), base + k);
                    self.vs[k] = Item::Mem(base + k);
                }
            }
        }
    }

    /// Return `status` with the virtual stack written out and `pc` next.
    fn exit(&mut self, status: u32, pc: usize) {
        let cold = std::mem::replace(&mut self.cold, true);
        self.flush();
        self.cold = cold;
        let l = self.b.ins().iconst(types::I64, self.depth as i64);
        self.b.ins().store(FLAGS, l, self.st, S_LEN);
        self.store_last();
        let pcv = self.b.ins().iconst(types::I64, pc as i64);
        self.b.ins().store(FLAGS, pcv, self.st, S_PC);
        let s = self.b.ins().iconst(types::I32, i64::from(status));
        self.b.ins().return_(&[s]);
    }

    /// Leave for the core loop at `pc` (the instruction about to run),
    /// with `restore` (the operands it popped) back on the stack, from a
    /// side block; lowering continues in the current block afterwards.
    fn exit_with(&mut self, pc: usize, restore: &[Item], status: u32) -> Block {
        let block = self.b.create_block();
        self.side_exits.push(SideExit {
            block,
            vs: self.vs.clone(),
            depth: self.depth,
            last_pc: self.last_pc,
            restore: restore.to_vec(),
            status,
            pc,
        });
        block
    }

    /// Return a helper's status `r` from the native code, a raise first
    /// trying the running activation's handlers (see [`h_catch`]).
    fn leave(&mut self, r: Value) {
        let catch = *self.catch.get_or_insert_with(|| self.b.create_block());
        let ret = self.b.create_block();
        let raised = self.b.ins().icmp_imm(IntCC::Equal, r, i64::from(RAISED));
        self.b.ins().brif(raised, catch, &[], ret, &[]);
        self.b.switch_to_block(ret);
        self.b.ins().return_(&[r]);
    }

    /// Fill the side exits' blocks, and the catch block: a raise its
    /// handler caught goes on through the dispatch (at the handler).
    fn side_exits(&mut self) {
        if let Some(catch) = self.catch {
            self.b.switch_to_block(catch);
            let r = self
                .call(h_catch as *const () as usize, &[self.st], true)
                .expect("returns");
            let ret = self.b.create_block();
            self.b.ins().brif(r, ret, &[], self.dispatch, &[]);
            self.b.switch_to_block(ret);
            self.b.ins().return_(&[r]);
        }
        while let Some(x) = self.side_exits.pop() {
            self.b.switch_to_block(x.block);
            self.vs = x.vs;
            self.depth = x.depth;
            self.last_pc = x.last_pc;
            for item in x.restore {
                self.push(item);
            }
            self.exit(x.status, x.pc);
        }
    }

    /// Stop for a queued finalizer (with `pc` next), from the current
    /// state.
    fn check_released(&mut self, next: usize) {
        let f = self.dead_flag();
        let out = self.exit_with(next, &[], MARKED);
        self.branch_out(f, out);
    }

    /// Jump to the block starting at `pc`, the virtual stack written out.
    fn goto(&mut self, pc: usize) {
        self.flush();
        match self.blocks.get(pc).copied().flatten() {
            Some(b) => {
                // The block finds the last instruction in the state.
                self.store_last();
                self.b.ins().jump(b, &[]);
            }
            None => self.exit(INTERP, pc),
        }
    }

    // ---- operands ----

    /// `item` as a scalar operand. An unknown-kind operand's tag still has
    /// to be checked.
    fn operand(&mut self, item: Item) -> Option<Opnd> {
        let at = |s: &mut Self, addr: Value| -> Opnd {
            let tag = s.tag_at(addr);
            let word = s.b.ins().load(types::I64, FLAGS, addr, 8);
            let byte = s.b.ins().uload8(types::I64, FLAGS, addr, 1);
            Opnd {
                kind: Kind::Unknown,
                tag: Some(tag),
                word,
                byte: Some(byte),
            }
        };
        Some(match item {
            Item::Int(v) => Opnd {
                kind: Kind::Int,
                tag: None,
                word: v,
                byte: None,
            },
            Item::Float(v) => Opnd {
                kind: Kind::Float,
                tag: None,
                word: self.b.ins().bitcast(types::I64, MemFlags::new(), v),
                byte: None,
            },
            Item::Bool(v) => Opnd {
                kind: Kind::Bool,
                tag: None,
                word: v,
                byte: None,
            },
            Item::Const(k) => match self.ext.objects[k as usize] {
                Object::Int(v) => Opnd {
                    kind: Kind::Int,
                    tag: None,
                    word: self.b.ins().iconst(types::I64, v),
                    byte: None,
                },
                Object::Float(x) => Opnd {
                    kind: Kind::Float,
                    tag: None,
                    word: self.b.ins().iconst(types::I64, x.to_bits() as i64),
                    byte: None,
                },
                Object::Bool(x) => Opnd {
                    kind: Kind::Bool,
                    tag: None,
                    word: self.b.ins().iconst(types::I8, i64::from(x)),
                    byte: None,
                },
                Object::None => Opnd {
                    kind: Kind::None,
                    tag: None,
                    word: self.b.ins().iconst(types::I64, 0),
                    byte: None,
                },
                _ => return None,
            },
            Item::Local(i) => {
                let a = self.local_addr(i);
                at(self, a)
            }
            Item::Mem(s) => {
                let a = self.slot_addr(s);
                at(self, a)
            }
            Item::Dyn(tag, word, byte) => Opnd {
                kind: Kind::Unknown,
                tag: Some(tag),
                word,
                byte: Some(byte),
            },
        })
    }

    /// Whether `o` is of `kind`: a static answer, or a run-time test.
    fn is_kind(&mut self, o: Opnd, kind: Kind, tag: u8) -> Result<bool, Value> {
        match o.kind {
            Kind::Unknown => {
                let t = o.tag.expect("unknown kinds carry a tag");
                Err(self.b.ins().icmp_imm(IntCC::Equal, t, i64::from(tag)))
            }
            k => Ok(k == kind),
        }
    }

    /// `o`'s value as an `f64`, given it is an `int` or a `float`
    /// (`is_float` says which, when unknown).
    fn as_f64(&mut self, o: Opnd, is_float: Result<bool, Value>) -> Value {
        match is_float {
            Ok(true) => self.b.ins().bitcast(types::F64, MemFlags::new(), o.word),
            Ok(false) => self.b.ins().fcvt_from_sint(types::F64, o.word),
            Err(f) => {
                let x = self.b.ins().bitcast(types::F64, MemFlags::new(), o.word);
                let c = self.b.ins().fcvt_from_sint(types::F64, o.word);
                self.b.ins().select(f, x, c)
            }
        }
    }

    /// The truth of a scalar operand (`None`, `bool`, `int`; and `float`
    /// with `floats`), as an `i8`; anything else branches to `other`.
    fn truth(&mut self, o: Opnd, floats: bool, other: Block) -> Value {
        match o.kind {
            Kind::Bool => o.word,
            Kind::Int => self.b.ins().icmp_imm(IntCC::NotEqual, o.word, 0),
            Kind::None => self.b.ins().iconst(types::I8, 0),
            Kind::Float if floats => {
                let x = self.b.ins().bitcast(types::F64, MemFlags::new(), o.word);
                let z = self.b.ins().f64const(0.0);
                self.b.ins().fcmp(FloatCC::NotEqual, x, z)
            }
            Kind::Float => {
                self.b.ins().jump(other, &[]);
                let dead = self.b.create_block();
                self.b.switch_to_block(dead);
                self.b.ins().iconst(types::I8, 0)
            }
            Kind::Unknown => {
                let t = o.tag.expect("unknown kinds carry a tag");
                let byte = o.byte.expect("unknown kinds carry a byte");
                let tb = self.tags;
                let is_bool = self
                    .b
                    .ins()
                    .icmp_imm(IntCC::Equal, t, i64::from(tb.boolean));
                let is_int = self.b.ins().icmp_imm(IntCC::Equal, t, i64::from(tb.int));
                let is_none = self.b.ins().icmp_imm(IntCC::Equal, t, i64::from(tb.none));
                let mut ok = self.b.ins().bor(is_bool, is_int);
                ok = self.b.ins().bor(ok, is_none);
                let fnz = if floats {
                    let is_float = self.b.ins().icmp_imm(IntCC::Equal, t, i64::from(tb.float));
                    ok = self.b.ins().bor(ok, is_float);
                    let x = self.b.ins().bitcast(types::F64, MemFlags::new(), o.word);
                    let z = self.b.ins().f64const(0.0);
                    let nz = self.b.ins().fcmp(FloatCC::NotEqual, x, z);
                    Some((is_float, nz))
                } else {
                    None
                };
                let bad = self.b.ins().bxor_imm(ok, 1);
                self.branch_out(bad, other);
                let bnz = self.b.ins().icmp_imm(IntCC::NotEqual, byte, 0);
                let wnz = self.b.ins().icmp_imm(IntCC::NotEqual, o.word, 0);
                let mut r = self.b.ins().select(is_bool, bnz, wnz);
                let not_none = self.b.ins().bxor_imm(is_none, 1);
                r = self.b.ins().band(r, not_none);
                if let Some((is_float, nz)) = fnz {
                    r = self.b.ins().select(is_float, nz, r);
                }
                r
            }
        }
    }

    // ---- the instructions ----

    /// Whether entering at `pc` gets anywhere: a block start whose first
    /// instruction the native code runs (not one it hands straight back).
    fn enters_at(&self, pc: usize) -> bool {
        let instrs = &self.code.instructions;
        if pc >= instrs.len() || self.blocks[pc].is_none() {
            return false;
        }
        native_at(self.code, pc)
    }

    /// Lower the whole function; `false` when it can't be compiled.
    fn lower(&mut self) -> bool {
        let code = self.code;
        let n = code.instructions.len();
        if self.depths.len() != n {
            return false;
        }
        // Block starts: the entry, jump targets, and every instruction
        // after one that ends a block or that the core loop runs.
        let mut starts = vec![false; n];
        starts[0] = true;
        for (pc, ins) in code.instructions.iter().enumerate() {
            let next = pc + 1;
            let target = match ins.op {
                OpCode::JumpForward
                | OpCode::PopJumpIfFalse
                | OpCode::PopJumpIfTrue
                | OpCode::PopJumpIfNone
                | OpCode::PopJumpIfNotNone
                | OpCode::ForIter => Some(next + ins.arg as usize),
                OpCode::JumpBackward => Some(next.saturating_sub(ins.arg as usize)),
                _ => None,
            };
            // A `SEND`'s exit (`END_SEND`), where a delegate's return
            // resumes the activation.
            if ins.op == OpCode::Send && next + (ins.arg as usize) < n {
                starts[next + ins.arg as usize] = true;
            }
            if let Some(t) = target {
                if t < n {
                    starts[t] = true;
                }
                if ins.op == OpCode::ForIter {
                    // The loop exit past `END_FOR` / `POP_ITER`.
                    let mut to = t;
                    if code.instructions.get(to).map(|i| i.op) == Some(OpCode::EndFor) {
                        to += 1;
                        if matches!(
                            code.instructions.get(to).map(|i| i.op),
                            Some(OpCode::PopIter | OpCode::PopTop)
                        ) {
                            to += 1;
                        }
                    }
                    if to < n {
                        starts[to] = true;
                    }
                }
            }
            if next < n && (target.is_some() || !native_at(code, pc)) {
                starts[next] = true;
            }
            // A fused attribute store continues after its `STORE_ATTR`.
            if ins.op == OpCode::LoadFast
                && code.instructions.get(next).map(|i| i.op) == Some(OpCode::StoreAttr)
                && pc + 2 < n
            {
                starts[pc + 2] = true;
            }
            // A global's fused class-constant read continues after its
            // `LOAD_ATTR`.
            if ins.op == OpCode::LoadGlobal
                && code.instructions.get(next).map(|i| i.op) == Some(OpCode::LoadAttr)
                && pc + 2 < n
            {
                starts[pc + 2] = true;
            }
            // A fused method call continues after its `CALL`.
            if ins.op == OpCode::LoadFast
                && code.instructions.get(next).map(|i| i.op) == Some(OpCode::LoadMethodAttr)
            {
                if let Some(call) =
                    (pc + 2..n.min(pc + 12)).find(|&k| code.instructions[k].op == OpCode::Call)
                {
                    if call + 1 < n {
                        starts[call + 1] = true;
                    }
                }
            }
        }
        // A handler, entered from a helper's raise (see `h_catch`).
        for h in &code.exception_table {
            if (h.handler as usize) < n {
                starts[h.handler as usize] = true;
            }
        }
        for (pc, s) in starts.iter_mut().enumerate() {
            if *s && self.depths[pc] < 0 {
                // Unreachable on the ordinary paths (a handler's): no entry.
                *s = false;
            }
        }
        self.blocks = starts
            .iter()
            .map(|&s| s.then(|| self.b.create_block()))
            .collect();
        // The entry: to the block at the state's pc, when the stack has the
        // block's depth.
        self.dispatch = self.b.create_block();
        self.b.ins().jump(self.dispatch, &[]);
        self.b.switch_to_block(self.dispatch);
        let pc = self.b.ins().load(types::I64, FLAGS, self.st, S_PC);
        let len = self.b.ins().load(types::I64, FLAGS, self.st, S_LEN);
        let pc32 = self.b.ins().ireduce(types::I32, pc);
        let out = self.b.create_block();
        // The code writes stack slots at fixed offsets: the frame's stack
        // must have room for the deepest point plus an instruction's own
        // pushes (a pooled stack can be smaller; the core loop grows it).
        let need = stack_need(self.depths) as i64;
        let cap = self.b.ins().load(types::I64, FLAGS, self.st, S_CAP);
        let short = self.b.ins().icmp_imm(IntCC::SignedLessThan, cap, need);
        let roomy = self.b.create_block();
        self.b.ins().brif(short, out, &[], roomy, &[]);
        self.b.switch_to_block(roomy);
        let mut tramps = Vec::new();
        let mut calls = Vec::with_capacity(n);
        for pc in 0..n {
            let target = match self.blocks[pc] {
                Some(blk) => {
                    let t = self.b.create_block();
                    tramps.push((t, blk, self.depths[pc]));
                    t
                }
                None => out,
            };
            calls.push(self.b.func.dfg.block_call(target, &[]));
        }
        let def = self.b.func.dfg.block_call(out, &[]);
        let jt = self.b.create_jump_table(JumpTableData::new(def, &calls));
        self.b.ins().br_table(pc32, jt);
        self.b.switch_to_block(out);
        let s = self.b.ins().iconst(types::I32, i64::from(INTERP));
        self.b.ins().return_(&[s]);
        for (t, blk, depth) in tramps {
            self.b.switch_to_block(t);
            let wrong = self.b.ins().icmp_imm(IntCC::NotEqual, len, depth);
            self.b.ins().brif(wrong, out, &[], blk, &[]);
        }
        // The blocks: each from its start until it ends or the next block
        // starts.
        for start in 0..n {
            if let Some(blk) = self.blocks[start] {
                self.lower_block(start, blk);
            }
        }
        self.side_exits();
        true
    }

    /// Lower `blk`, from the instruction at `start` until the block ends or
    /// the next one starts.
    fn lower_block(&mut self, start: usize, blk: Block) {
        let n = self.code.instructions.len();
        self.b.switch_to_block(blk);
        self.vs.clear();
        self.depth = self.depths[start] as usize;
        self.last_pc = None;
        self.bound.iter_mut().for_each(|b| *b = false);
        let mut pc = start;
        loop {
            if !self.instruction(pc) {
                break;
            }
            pc += 1;
            if pc >= n {
                self.exit(INTERP, pc);
                break;
            }
            if self.blocks[pc].is_some() {
                self.goto(pc);
                break;
            }
        }
    }

    /// Lower the instruction at `pc` into the current block: `true` when
    /// the block continues with the next instruction, `false` when it ended
    /// (a jump, an exit).
    fn instruction(&mut self, pc: usize) -> bool {
        let ins = self.code.instructions[pc];
        let next = pc + 1;
        if op_disabled(ins.op) {
            self.exit(INTERP, pc);
            return false;
        }
        let cont = match ins.op {
            OpCode::Nop | OpCode::NotTaken | OpCode::Resume | OpCode::CopyFreeVars => true,
            OpCode::MakeCell if prologue_cell(self.code, pc) => true,
            OpCode::LoadFast | OpCode::LoadFastBorrow => self.load_fast(pc, ins.arg),
            OpCode::LoadConst if (ins.arg as usize) < self.ext.objects.len() => {
                self.push(Item::Const(ins.arg));
                true
            }
            OpCode::LoadSmallInt => {
                let v = self.b.ins().iconst(types::I64, i64::from(ins.arg));
                self.push(Item::Int(v));
                true
            }
            OpCode::StoreFast if (ins.arg as usize) < self.nlocals => self.store_fast(pc, ins.arg),
            OpCode::PopTop => {
                match self.pop() {
                    Item::Mem(s) => {
                        let at = self.slot_addr(s);
                        let tag = self.tag_at(at);
                        let scalar = self.is_scalar(tag);
                        let heap = self.b.create_block();
                        let done = self.b.create_block();
                        // (A shared counted value by a plain decrement.)
                        self.release_or(scalar, at, tag, done, heap);
                        self.b.switch_to_block(heap);
                        let r = self
                            .call(h_pop as *const () as usize, &[at], true)
                            .expect("returns");
                        let out = self.exit_with(pc, &[Item::Mem(s)], INTERP);
                        self.branch_out(r, out);
                        self.set_last(pc);
                        self.check_released(next);
                        self.b.ins().jump(done, &[]);
                        self.b.switch_to_block(done);
                    }
                    // A copy never taken has nothing to release.
                    _ => {}
                }
                true
            }
            OpCode::BinaryOp => return self.binary(pc, ins.arg),
            OpCode::CompareOp => return self.compare(pc, ins.arg),
            OpCode::ToBool => return self.truth_of(pc),
            OpCode::PopJumpIfFalse | OpCode::PopJumpIfTrue => {
                let item = self.pop();
                let Some(o) = self.operand(item) else {
                    self.push(item);
                    self.exit(INTERP, pc);
                    return false;
                };
                let other = self.exit_with(pc, &[item], INTERP);
                let t = self.truth(o, false, other);
                self.set_last(pc);
                let taken = next + ins.arg as usize;
                self.branch(t, ins.op == OpCode::PopJumpIfTrue, taken, next);
                return false;
            }
            OpCode::PopJumpIfNone | OpCode::PopJumpIfNotNone => {
                let item = self.pop();
                let is_none = match item {
                    Item::Mem(s) => {
                        let at = self.slot_addr(s);
                        let tag = self.tag_at(at);
                        let none =
                            self.b
                                .ins()
                                .icmp_imm(IntCC::Equal, tag, i64::from(self.tags.none));
                        let scalar = self.is_scalar(tag);
                        let heap = self.b.create_block();
                        let done = self.b.create_block();
                        self.b.ins().brif(scalar, done, &[], heap, &[]);
                        self.b.switch_to_block(heap);
                        let r = self
                            .call(h_pop as *const () as usize, &[at], true)
                            .expect("returns");
                        let out = self.exit_with(pc, &[Item::Mem(s)], INTERP);
                        self.branch_out(r, out);
                        self.b.ins().jump(done, &[]);
                        self.b.switch_to_block(done);
                        none
                    }
                    Item::Local(i) => {
                        let at = self.local_addr(i);
                        let tag = self.tag_at(at);
                        self.b
                            .ins()
                            .icmp_imm(IntCC::Equal, tag, i64::from(self.tags.none))
                    }
                    Item::Const(k) => {
                        let none = matches!(self.ext.objects[k as usize], Object::None);
                        self.b.ins().iconst(types::I8, i64::from(none))
                    }
                    Item::Dyn(tag, ..) => {
                        self.b
                            .ins()
                            .icmp_imm(IntCC::Equal, tag, i64::from(self.tags.none))
                    }
                    Item::Int(_) | Item::Float(_) | Item::Bool(_) => {
                        self.b.ins().iconst(types::I8, 0)
                    }
                };
                self.set_last(pc);
                let taken = next + ins.arg as usize;
                self.branch(is_none, ins.op == OpCode::PopJumpIfNone, taken, next);
                return false;
            }
            OpCode::IsOp => return self.is_op(pc, ins.arg),
            OpCode::CopyTop => {
                let n = (ins.arg as usize).max(1);
                if n > self.depth {
                    self.exit(INTERP, pc);
                    return false;
                }
                let k = self.vs.len();
                let item = if n <= k {
                    self.vs[k - n]
                } else {
                    Item::Mem(self.depth - n)
                };
                match item {
                    Item::Mem(s) => {
                        // A second reference to a value on the stack.
                        let dst = self.slot_addr(self.depth);
                        let src = self.slot_addr(s);
                        self.copy_value(dst, src);
                        let d = self.depth;
                        self.push(Item::Mem(d));
                    }
                    other => self.push(other),
                }
                true
            }
            OpCode::Swap => {
                let n = ins.arg as usize;
                if n < 2 || n > self.depth {
                    self.exit(INTERP, pc);
                    return false;
                }
                let k = self.vs.len();
                let in_registers = n <= k
                    && !matches!(self.vs[k - 1], Item::Mem(_))
                    && !matches!(self.vs[k - n], Item::Mem(_));
                if in_registers {
                    self.vs.swap(k - 1, k - n);
                } else {
                    // A stack value's slot can't move on the virtual stack:
                    // swap on the frame's.
                    self.flush();
                    let a = self.slot_addr(self.depth - 1);
                    let b = self.slot_addr(self.depth - n);
                    let (a0, a1) = (
                        self.b.ins().load(types::I64, FLAGS, a, 0),
                        self.b.ins().load(types::I64, FLAGS, a, 8),
                    );
                    self.copy16(a, b);
                    self.b.ins().store(FLAGS, a0, b, 0);
                    self.b.ins().store(FLAGS, a1, b, 8);
                }
                true
            }

            OpCode::JumpForward => {
                self.set_last(pc);
                self.goto(next + ins.arg as usize);
                return false;
            }
            OpCode::JumpBackward => {
                self.flush();
                // The back edge is the eval breaker (the core loop's arm,
                // which the exits below hand the instruction to).
                let countdown = self.b.ins().load(self.ptr, FLAGS, self.st, S_COUNTDOWN);
                let c = self.b.ins().load(types::I32, FLAGS, countdown, 0);
                let low = self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, c, 1);
                let gen_addr = self
                    .b
                    .ins()
                    .iconst(self.ptr, crate::hot_gates::loop_gen_ptr() as i64);
                let g = self.b.ins().load(types::I64, FLAGS, gen_addr, 0);
                let snap = self.b.ins().load(types::I64, FLAGS, self.st, S_SNAP);
                let moved = self.b.ins().icmp(IntCC::NotEqual, g, snap);
                let out = self.exit_with(pc, &[], INTERP);
                self.branch_out(moved, out);
                // A countdown that ran out with nobody waiting for the GIL
                // starts over here (see `Interpreter::countdown_out`).
                let refill = self.b.create_block();
                let go = self.b.create_block();
                self.b.append_block_param(go, types::I32);
                self.b.set_cold_block(refill);
                self.b.ins().brif(low, refill, &[], go, &[c.into()]);
                self.b.switch_to_block(refill);
                let n = self
                    .call(h_countdown as *const () as usize, &[countdown], true)
                    .expect("returns");
                self.b.ins().brif(n, go, &[n.into()], out, &[]);
                self.b.switch_to_block(go);
                let c = self.b.block_params(go)[0];
                let c1 = self.b.ins().iadd_imm(c, -1);
                self.b.ins().store(FLAGS, c1, countdown, 0);
                self.set_last(pc);
                self.goto(next.saturating_sub(ins.arg as usize));
                return false;
            }
            OpCode::ForIter => return self.for_iter(pc, ins.arg),
            OpCode::BinarySubscr if self.seq_index(pc) => true,
            OpCode::BinarySubscr if self.subscr_ref(pc) => true,
            OpCode::UnpackSequence if self.unpack_ref(pc, ins.arg as usize) => true,
            OpCode::StoreSubscr if self.store_subscr_ref(pc) => true,
            OpCode::BinarySubscr
            | OpCode::BinarySlice
            | OpCode::StoreSubscr
            | OpCode::UnpackSequence
            | OpCode::UnpackEx
            | OpCode::LoadSuperAttr
            | OpCode::LoadSpecial
            | OpCode::PushExcInfo
            | OpCode::CheckExcMatch
            | OpCode::PopExcept
            | OpCode::StoreSlice
            | OpCode::DeleteSubscr => return self.container(pc),
            OpCode::RaiseVarargs if matches!(ins.arg, 1 | 2) && self.depth >= ins.arg as usize => {
                self.flush();
                self.store_last();
                let pcv = self.b.ins().iconst(types::I64, pc as i64);
                let len = self.b.ins().iconst(types::I64, self.depth as i64);
                let n = self.b.ins().iconst(types::I64, i64::from(ins.arg));
                let r = self
                    .call(h_raise as *const () as usize, &[self.st, pcv, len, n], true)
                    .expect("returns");
                let out = self.exit_with(pc, &[], INTERP);
                let raised = self.b.create_block();
                let declined = self
                    .b
                    .ins()
                    .icmp_imm(IntCC::Equal, r, i64::from(CALL_DECLINED));
                self.b.ins().brif(declined, out, &[], raised, &[]);
                self.b.switch_to_block(raised);
                self.leave(r);
                return false;
            }
            OpCode::DeleteFast if (ins.arg as usize) < self.nlocals => {
                let i = ins.arg;
                self.flush_local(i);
                let iv = self.b.ins().iconst(types::I64, i64::from(i));
                let r = self
                    .call(h_delete_fast as *const () as usize, &[self.st, iv], true)
                    .expect("returns");
                let out = self.exit_with(pc, &[], INTERP);
                self.branch_out(r, out);
                self.bound[i as usize] = false;
                self.set_last(pc);
                self.check_released(pc + 1);
                true
            }
            OpCode::ListAppend if ins.arg != 0 && (ins.arg as usize) < self.depth => {
                return self.list_append(pc, ins.arg as usize)
            }
            OpCode::ListAppend | OpCode::SetAdd | OpCode::MapAdd => return self.container(pc),
            OpCode::ContainsOp => return self.contains(pc, ins.arg),
            OpCode::LoadAttr => return self.load_attr(pc, ins.arg),
            OpCode::LoadGlobal => return self.load_global(pc),
            OpCode::PushNull => {
                let s = self.depth;
                let dst = self.slot_addr(s);
                self.write_tag(dst, self.tags.unbound);
                self.push(Item::Mem(s));
                true
            }
            OpCode::LoadMethodAttr => return self.load_method(pc),
            OpCode::StoreAttr => return self.stack_store_attr(pc),
            OpCode::BuildTuple if (1..=3).contains(&ins.arg) => {
                return self.build(pc, ins.arg as usize, false)
            }
            OpCode::BuildList => return self.build(pc, ins.arg as usize, true),
            OpCode::BuildMap if (0..=8).contains(&ins.arg) => {
                return self.build_map(pc, ins.arg as usize)
            }
            // PEP 709: the local's value (`Unbound` included) moves onto
            // the stack and the local empties (a cell-sharing local is the
            // core loop's).
            OpCode::LoadFastAndClear
                if (ins.arg as usize) < self.nlocals
                    && Interpreter::shared_cell_index(self.code, ins.arg as usize).is_none() =>
            {
                let i = ins.arg;
                self.flush_local(i);
                let s = self.depth;
                let dst = self.slot_addr(s);
                let src = self.local_addr(i);
                self.copy16(dst, src);
                self.write_tag(src, self.tags.unbound);
                self.bound[i as usize] = false;
                self.push(Item::Mem(s));
                true
            }
            OpCode::LoadClosure | OpCode::LoadClosureBorrow | OpCode::LoadCommonConstant => {
                let s = self.depth;
                let dst = self.slot_addr(s);
                let i = self.b.ins().iconst(types::I64, i64::from(ins.arg));
                let helper = if ins.op == OpCode::LoadCommonConstant {
                    h_common_const as *const () as usize
                } else {
                    h_load_closure as *const () as usize
                };
                let r = self
                    .call(helper, &[self.st, i, dst], true)
                    .expect("returns");
                let out = self.exit_with(pc, &[], INTERP);
                self.branch_out(r, out);
                self.push(Item::Mem(s));
                true
            }
            OpCode::StoreDeref => {
                if self.depth == 0 {
                    self.exit(INTERP, pc);
                    return false;
                }
                let s = self.top_to_mem();
                let at = self.slot_addr(s);
                let i = self.b.ins().iconst(types::I64, i64::from(ins.arg));
                let r = self
                    .call(h_store_deref as *const () as usize, &[self.st, i, at], true)
                    .expect("returns");
                let out = self.exit_with(pc, &[], INTERP);
                self.branch_out(r, out);
                self.pop();
                // (The displaced value's release may queue a finalizer.)
                self.set_last(pc);
                self.check_released(pc + 1);
                true
            }
            OpCode::MakeFunction | OpCode::SetFunctionAttribute => {
                let n = if ins.op == OpCode::MakeFunction {
                    1 + (ins.arg & 0xf).count_ones() as usize
                } else {
                    2
                };
                if self.depth < n {
                    self.exit(INTERP, pc);
                    return false;
                }
                self.flush();
                let s = self.depth - n;
                let at = self.slot_addr(s);
                let arg = self.b.ins().iconst(types::I64, i64::from(ins.arg));
                let r = if ins.op == OpCode::MakeFunction {
                    self.call(
                        h_make_function as *const () as usize,
                        &[self.st, at, arg],
                        true,
                    )
                } else {
                    let pcv = self.b.ins().iconst(types::I64, pc as i64);
                    self.call(
                        h_set_function_attr as *const () as usize,
                        &[self.st, at, arg, pcv],
                        true,
                    )
                }
                .expect("returns");
                let out = self.exit_with(pc, &[], INTERP);
                let other = self.b.create_block();
                let ok = self.b.create_block();
                self.b.ins().brif(r, other, &[], ok, &[]);
                self.b.switch_to_block(other);
                let raised = self.b.ins().icmp_imm(IntCC::Equal, r, i64::from(RAISED));
                let leave = self.b.create_block();
                self.b.ins().brif(raised, leave, &[], out, &[]);
                self.b.switch_to_block(leave);
                self.leave(r);
                self.b.switch_to_block(ok);
                for _ in 0..n {
                    self.pop();
                }
                self.push(Item::Mem(s));
                true
            }
            OpCode::ListToTuple => {
                if self.depth == 0 {
                    self.exit(INTERP, pc);
                    return false;
                }
                let s = self.top_to_mem();
                let at = self.slot_addr(s);
                let r = self
                    .call(h_list_to_tuple as *const () as usize, &[at], true)
                    .expect("returns");
                let out = self.exit_with(pc, &[], INTERP);
                self.branch_out(r, out);
                true
            }
            OpCode::LoadDeref => {
                let s = self.depth;
                let dst = self.slot_addr(s);
                let i = self.b.ins().iconst(types::I64, i64::from(ins.arg));
                let r = self
                    .call(h_load_deref as *const () as usize, &[self.st, i, dst], true)
                    .expect("returns");
                let out = self.exit_with(pc, &[], INTERP);
                self.branch_out(r, out);
                self.push(Item::Mem(s));
                true
            }
            OpCode::GetIter => {
                if self.depth == 0 {
                    self.exit(INTERP, pc);
                    return false;
                }
                let s = self.top_to_mem();
                let at = self.slot_addr(s);
                let r = self
                    .call(h_get_iter as *const () as usize, &[at], true)
                    .expect("returns");
                let out = self.exit_with(pc, &[], INTERP);
                self.branch_out(r, out);
                self.set_last(pc);
                self.check_released(pc + 1);
                true
            }
            OpCode::UnaryOp if ins.arg <= 3 => return self.unary(pc, ins.arg),
            // (A generator's fast step never switches or raises from a call
            // helper; a generator body's return isn't a switch.)
            OpCode::Call => return self.call_op(pc, ins.arg as usize),
            OpCode::CallKw => return self.call_kw(pc),
            OpCode::CallEx => return self.call_ex(pc),
            // `await`'s and `yield from`'s delegate, through `h_await_iter`.
            OpCode::GetAwaitable | OpCode::GetYieldFromIter if self.depth > 0 => {
                let s = self.top_to_mem();
                let at = self.slot_addr(s);
                let k = self
                    .b
                    .ins()
                    .iconst(types::I64, i64::from(ins.op == OpCode::GetYieldFromIter));
                let r = self
                    .call(h_await_iter as *const () as usize, &[self.st, at, k], true)
                    .expect("returns");
                let out = self.exit_with(pc, &[], INTERP);
                self.branch_out(r, out);
                // (A replaced instance's release may queue a finalizer.)
                self.set_last(pc);
                self.check_released(pc + 1);
                true
            }
            // The delegate resumes as the running activation (`h_send`):
            // the block ends either way.
            OpCode::Send if self.depth >= 2 => {
                self.flush();
                self.store_last();
                let pcv = self.b.ins().iconst(types::I64, pc as i64);
                let len = self.b.ins().iconst(types::I64, self.depth as i64);
                let r = self
                    .call(h_send as *const () as usize, &[self.st, pcv, len], true)
                    .expect("returns");
                let out = self.exit_with(pc, &[], INTERP);
                let switched = self.b.create_block();
                let other = self.b.create_block();
                let returned = self.b.create_block();
                let reload = self.b.ins().icmp_imm(IntCC::Equal, r, i64::from(RELOAD));
                self.b.ins().brif(reload, switched, &[], other, &[]);
                self.b.switch_to_block(switched);
                self.b.ins().return_(&[r]);
                self.b.switch_to_block(other);
                self.b.ins().brif(r, out, &[], returned, &[]);
                // The delegate returned into this `SEND`: its result took the
                // sent value's place.
                self.b.switch_to_block(returned);
                self.set_last(pc);
                self.goto(next + ins.arg as usize);
                return false;
            }
            // A generator body's yield and return switch to its consumer
            // (`h_gen_yield`, `h_gen_return`); in a fast step (no switch)
            // they're the step's own.
            OpCode::YieldValue | OpCode::ReturnValue if self.is_gen() && self.depth > 0 => {
                self.flush();
                let sw = self.b.ins().load(self.ptr, FLAGS, self.st, S_SW);
                let out = self.exit_with(pc, &[], INTERP);
                let none = self.b.ins().icmp_imm(IntCC::Equal, sw, 0);
                self.branch_out(none, out);
                self.store_last();
                let pcv = self.b.ins().iconst(types::I64, pc as i64);
                let len = self.b.ins().iconst(types::I64, self.depth as i64);
                let helper = if ins.op == OpCode::YieldValue {
                    h_gen_yield as *const () as usize
                } else {
                    h_gen_return as *const () as usize
                };
                let r = self
                    .call(helper, &[self.st, pcv, len], true)
                    .expect("returns");
                let switched = self.b.create_block();
                let reload = self.b.ins().icmp_imm(IntCC::Equal, r, i64::from(RELOAD));
                self.b.ins().brif(reload, switched, &[], out, &[]);
                self.b.switch_to_block(switched);
                self.b.ins().return_(&[r]);
                return false;
            }
            OpCode::EndSend if self.depth >= 2 => {
                self.flush();
                let at = self.slot_addr(self.depth - 2);
                let r = self
                    .call(h_end_send as *const () as usize, &[at], true)
                    .expect("returns");
                let out = self.exit_with(pc, &[], INTERP);
                self.branch_out(r, out);
                self.depth -= 1;
                self.set_last(pc);
                self.check_released(pc + 1);
                true
            }
            OpCode::ReturnValue if !self.is_gen() && self.depth > 0 => {
                self.flush();
                self.store_last();
                let pcv = self.b.ins().iconst(types::I64, pc as i64);
                let len = self.b.ins().iconst(types::I64, self.depth as i64);
                // A direct call's callee hands its return to the caller as
                // it stands (`h_return`'s `DIRECT_RET`, in line).
                let direct = self.b.ins().uload8(types::I32, FLAGS, self.st, S_DIRECT);
                let handed = self.b.create_block();
                let other = self.b.create_block();
                self.b.ins().brif(direct, handed, &[], other, &[]);
                self.b.switch_to_block(handed);
                self.b.ins().store(FLAGS, len, self.st, S_LEN);
                self.b.ins().store(FLAGS, pcv, self.st, S_PC);
                let r = self.b.ins().iconst(types::I32, i64::from(DIRECT_RET));
                self.b.ins().return_(&[r]);
                self.b.switch_to_block(other);
                let r = self
                    .call(h_return as *const () as usize, &[self.st, pcv, len], true)
                    .expect("returns");
                let out = self.exit_with(pc, &[], INTERP);
                let switched = self.b.create_block();
                self.b.ins().brif(r, switched, &[], out, &[]);
                // Switched back (`RELOAD`), or a direct call's return.
                self.b.switch_to_block(switched);
                self.b.ins().return_(&[r]);
                return false;
            }
            _ => {
                self.exit(INTERP, pc);
                return false;
            }
        };
        if cont {
            self.set_last(pc);
        }
        cont
    }

    /// Branch to `taken` when `cond` (an `i8` truth) is `want`.
    fn branch(&mut self, cond: Value, want: bool, taken: usize, fall: usize) {
        self.flush();
        let (t, f) = if want { (taken, fall) } else { (fall, taken) };
        let tb = self.b.create_block();
        let fb = self.b.create_block();
        self.b.ins().brif(cond, tb, &[], fb, &[]);
        let depth = self.depth;
        self.b.switch_to_block(tb);
        self.goto(t);
        self.depth = depth;
        self.b.switch_to_block(fb);
        self.goto(f);
    }

    fn load_fast(&mut self, pc: usize, i: u32) -> bool {
        if i as usize >= self.nlocals {
            self.exit(INTERP, pc);
            return false;
        }
        // The core loop's fused receiver shapes (`x.m(...)`, `x.attr = v`)
        // start here.
        // A method call whose callee doesn't run in place loads the method
        // here and leaves at the `CALL`.
        match self.code.instructions.get(pc + 1).map(|i| i.op) {
            Some(OpCode::LoadMethodAttr)
                if !self.is_gen() && crate::method_site_in_place(self.ext, pc + 1) =>
            {
                self.method_kernel(pc);
                self.method_call(pc)
            }
            Some(OpCode::StoreAttr) => return self.store_attr(pc),
            _ => {}
        }
        // An unbound local raises in the core loop (a parameter nothing
        // deletes is always bound, as is a local this block bound or tested).
        let ix = i as usize;
        if self.maybe_unbound[ix] && !self.bound[ix] {
            let at = self.local_addr(i);
            let tag = self.tag_at(at);
            let unbound = self
                .b
                .ins()
                .icmp_imm(IntCC::Equal, tag, i64::from(self.tags.unbound));
            let out = self.exit_with(pc, &[], INTERP);
            self.branch_out(unbound, out);
            self.bound[ix] = true;
        }
        self.push(Item::Local(i));
        true
    }

    /// Whether the code is a generator's, coroutine's or async generator's
    /// body: its resume can't raise from here (its calls are the general
    /// loop's).
    fn is_gen(&self) -> bool {
        self.code.is_generator || self.code.is_coroutine || self.code.is_async_generator
    }

    /// `LOAD_FAST x; x.m(<up to two local, constant or small-int
    /// arguments>); CALL` on an exact `list`, `dict`, `set` or `str` whose
    /// method the site settled as a leaf with a [`Kernel`]: the receiver's
    /// tag checked in line and the kernel called on the operands in place
    /// (no method object, receiver copy or argument copies), continuing
    /// after the `CALL`. Lowering continues (at the general fused call)
    /// where the receiver is something else or the kernel declines;
    /// nothing is emitted for a site without a kernel.
    fn method_kernel(&mut self, pc: usize) {
        #[derive(Clone, Copy)]
        enum Arg {
            Local(u32),
            Const(u32),
            Small(u32),
        }
        let code = self.code;
        let ins = &code.instructions;
        let n = ins.len();
        let mut args: Vec<Arg> = Vec::new();
        let mut k = pc + 2;
        let call_pc = loop {
            let Some(i) = ins.get(k) else {
                return;
            };
            match i.op {
                OpCode::LoadFast | OpCode::LoadFastBorrow if (i.arg as usize) < self.nlocals => {
                    args.push(Arg::Local(i.arg));
                }
                OpCode::LoadConst if (i.arg as usize) < self.ext.objects.len() => {
                    args.push(Arg::Const(i.arg));
                }
                OpCode::LoadSmallInt => args.push(Arg::Small(i.arg)),
                OpCode::Call if i.arg as usize == args.len() => break k,
                _ => return,
            }
            if args.len() > 2 {
                return;
            }
            k += 1;
        };
        if call_pc + 1 >= n || self.blocks.get(call_pc + 1).copied().flatten().is_none() {
            return;
        }
        let Some((tag, kind, f)) = crate::builtin_method_site(self.ext, pc + 1, call_pc) else {
            return;
        };
        let Some(kernel) = kernel_for(tag, kind, args.len()) else {
            return;
        };
        let want = match tag {
            1 => self.tags.list,
            2 => self.tags.dict,
            3 => self.tags.set,
            _ => self.tags.str,
        };
        self.flush();
        let slot = self.depth;
        let declined = self.b.create_block();
        let recv = self.local_addr(self.code.instructions[pc].arg);
        let t = self.tag_at(recv);
        let other = self.b.ins().icmp_imm(IntCC::NotEqual, t, i64::from(want));
        self.branch_out(other, declined);
        let mut addrs = Vec::with_capacity(2);
        for (j, arg) in args.iter().enumerate() {
            let at = match *arg {
                Arg::Local(i) => {
                    let at = self.local_addr(i);
                    if self.maybe_unbound[i as usize] && !self.bound[i as usize] {
                        let t = self.tag_at(at);
                        let unbound =
                            self.b
                                .ins()
                                .icmp_imm(IntCC::Equal, t, i64::from(self.tags.unbound));
                        self.branch_out(unbound, declined);
                    }
                    at
                }
                Arg::Const(c) => self.const_addr(c),
                // A scalar in the slot the argument would take.
                Arg::Small(v) => {
                    let at = self.slot_addr(slot + 2 + j);
                    self.write_tag(at, self.tags.int);
                    let v = self.b.ins().iconst(types::I64, i64::from(v));
                    self.b.ins().store(FLAGS, v, at, 8);
                    at
                }
            };
            addrs.push(at);
        }
        let null = self.b.ins().iconst(self.ptr, 0);
        let a = addrs.first().copied().unwrap_or(null);
        let b = addrs.get(1).copied().unwrap_or(null);
        let fp = self.b.ins().iconst(self.ptr, Rc::as_ptr(&f) as i64);
        self.held.push(f);
        let out = self.slot_addr(slot);
        let r = self
            .call(kernel as *const () as usize, &[out, recv, a, b, fp], true)
            .expect("returns");
        let ran = self.b.create_block();
        self.b.ins().brif(r, declined, &[], ran, &[]);
        // Ran: the result in the receiver's slot, as after the `CALL`.
        self.b.switch_to_block(ran);
        let (depth, last) = (self.depth, self.last_pc);
        self.push(Item::Mem(slot));
        self.set_last(call_pc);
        self.goto(call_pc + 1);
        self.depth = depth;
        self.last_pc = last;
        self.vs.clear();
        self.b.switch_to_block(declined);
    }

    /// `LOAD_FAST x; x.m(<simple arguments>)` through [`h_local_method`],
    /// continuing at whatever follows the call (through the dispatch).
    /// Lowering continues where the helper declines (the plain load).
    fn method_call(&mut self, pc: usize) {
        self.flush();
        let code = self
            .b
            .ins()
            .iconst(self.ptr, std::ptr::from_ref(self.code) as i64);
        let ext = self
            .b
            .ins()
            .iconst(self.ptr, std::ptr::from_ref(self.ext) as i64);
        let pcv = self.b.ins().iconst(types::I64, pc as i64);
        let nl = self.b.ins().iconst(types::I64, self.nlocals as i64);
        let len = self.b.ins().iconst(types::I64, self.depth as i64);
        self.store_last();
        let r = self
            .call(
                h_local_method as *const () as usize,
                &[self.st, code, ext, pcv, nl, len],
                true,
            )
            .expect("returns");
        let ran = self.b.create_block();
        let other = self.b.create_block();
        self.b.ins().brif(r, other, &[], ran, &[]);
        // Ran: the state holds the pc, depth and last instruction after the
        // call.
        self.b.switch_to_block(ran);
        let saved = self.last_pc.take();
        self.check_released_dynamic();
        self.b.ins().jump(self.dispatch, &[]);
        // Raised: the core loop raises it.
        self.b.switch_to_block(other);
        let raised = self.b.ins().icmp_imm(IntCC::Equal, r, i64::from(RAISED));
        let raise_b = self.b.create_block();
        let declined = self.b.create_block();
        self.b.ins().brif(raised, raise_b, &[], declined, &[]);
        self.b.switch_to_block(raise_b);
        let s = self.b.ins().iconst(types::I32, i64::from(RAISED));
        self.leave(s);
        self.b.switch_to_block(declined);
        self.last_pc = saved;
    }

    /// Stop for a queued finalizer with the state as a helper left it (pc
    /// and depth already stored).
    fn check_released_dynamic(&mut self) {
        let f = self.dead_flag();
        let out = self.b.create_block();
        let ok = self.b.create_block();
        self.b.ins().brif(f, out, &[], ok, &[]);
        self.b.switch_to_block(out);
        let s = self.b.ins().iconst(types::I32, i64::from(MARKED));
        self.b.ins().return_(&[s]);
        self.b.switch_to_block(ok);
    }

    /// `LOAD_FAST x; x.attr = v` through [`h_store_attr`] (the value on
    /// the stack's top).
    fn store_attr(&mut self, pc: usize) -> bool {
        if self.depth == 0 {
            self.exit(INTERP, pc);
            return false;
        }
        self.flush();
        let slot = self.slot_addr(self.depth - 1);
        let code = self
            .b
            .ins()
            .iconst(self.ptr, std::ptr::from_ref(self.code) as i64);
        let pcv = self.b.ins().iconst(types::I64, pc as i64);
        let r = self
            .call(
                h_store_attr as *const () as usize,
                &[self.st, code, pcv, slot],
                true,
            )
            .expect("returns");
        let out = self.exit_with(pc, &[], INTERP);
        self.branch_out(r, out);
        self.depth -= 1;
        self.set_last(pc + 1);
        self.check_released(pc + 2);
        self.goto(pc + 2);
        false
    }

    fn store_fast(&mut self, pc: usize, i: u32) -> bool {
        self.flush_local(i);
        self.bound[i as usize] = true;
        let item = self.pop();
        if let Item::Local(j) = item {
            if j == i {
                return true;
            }
        }
        let local = self.local_addr(i);
        // The new value's words, for a store that needs no release.
        let words: Option<(Value, Value, Value)> = match item {
            Item::Int(v) => Some((
                self.b.ins().iconst(types::I64, i64::from(self.tags.int)),
                v,
                self.b.ins().iconst(types::I8, 0),
            )),
            Item::Float(v) => Some((
                self.b.ins().iconst(types::I64, i64::from(self.tags.float)),
                self.b.ins().bitcast(types::I64, MemFlags::new(), v),
                self.b.ins().iconst(types::I8, 0),
            )),
            Item::Bool(v) => Some((
                self.b
                    .ins()
                    .iconst(types::I64, i64::from(self.tags.boolean)),
                self.b.ins().iconst(types::I64, 0),
                v,
            )),
            Item::Dyn(t, w, b) => Some((t, w, self.b.ins().ireduce(types::I8, b))),
            Item::Const(k)
                if matches!(
                    self.ext.objects[k as usize],
                    Object::Int(_) | Object::Float(_) | Object::Bool(_) | Object::None
                ) =>
            {
                let o = self.operand(item).expect("a scalar constant");
                let tag = match o.kind {
                    Kind::Int => self.tags.int,
                    Kind::Float => self.tags.float,
                    Kind::Bool => self.tags.boolean,
                    _ => self.tags.none,
                };
                let (w, b) = if o.kind == Kind::Bool {
                    (self.b.ins().iconst(types::I64, 0), o.word)
                } else {
                    (o.word, self.b.ins().iconst(types::I8, 0))
                };
                Some((self.b.ins().iconst(types::I64, i64::from(tag)), w, b))
            }
            _ => None,
        };
        let old = self.tag_at(local);
        let plain_old = {
            let s = self.is_scalar(old);
            let u = self
                .b
                .ins()
                .icmp_imm(IntCC::Equal, old, i64::from(self.tags.unbound));
            self.b.ins().bor(s, u)
        };
        // The value's slot (it left the virtual stack).
        let slot = self.depth;
        match words {
            Some((tag, word, byte)) => {
                // A scalar over a scalar (or nothing): three stores.
                let slow = self.b.create_block();
                let done = self.b.create_block();
                let fast = self.b.create_block();
                self.release_or(plain_old, local, old, fast, slow);
                self.b.switch_to_block(fast);
                let t8 = self.b.ins().ireduce(types::I8, tag);
                self.b.ins().store(FLAGS, t8, local, 0);
                self.b.ins().store(FLAGS, byte, local, 1);
                self.b.ins().store(FLAGS, word, local, 8);
                self.b.ins().jump(done, &[]);
                // Over a heap value: through the stack slot, released by
                // the helper.
                self.b.switch_to_block(slow);
                self.materialize(item, slot);
                self.store_from_slot(pc, i, slot);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
            }
            None => {
                // The value into its slot (a copy taken), then moved in.
                self.materialize(item, slot);
                let src = self.slot_addr(slot);
                let vt = self.tag_at(src);
                // The PEP 709 cell restore stays on the full path.
                let cell = self
                    .b
                    .ins()
                    .icmp_imm(IntCC::Equal, vt, i64::from(self.tags.cell));
                let out = self.exit_with(pc, &[Item::Mem(slot)], INTERP);
                self.branch_out(cell, out);
                let slow = self.b.create_block();
                let done = self.b.create_block();
                let fast = self.b.create_block();
                self.release_or(plain_old, local, old, fast, slow);
                self.b.switch_to_block(fast);
                self.copy16(local, src);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(slow);
                self.store_from_slot(pc, i, slot);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
            }
        }
        true
    }

    /// Branch to `fast` when the value at `local` (tagged `old`) can be
    /// overwritten in place: `plain` (a scalar, or nothing), or a shared
    /// instance, list, dict or class whose reference is released here by
    /// a plain decrement (one thread owns every count, and no watcher or
    /// tracer wants the release). Anything else goes to `slow`.
    fn release_or(&mut self, plain: Value, local: Value, old: Value, fast: Block, slow: Block) {
        if self.cold || (self.tags.rc_counted == 0 && self.tags.rc_counted_hdr == 0) {
            self.b.ins().brif(plain, fast, &[], slow, &[]);
            return;
        }
        let heap = self.b.create_block();
        let counted = self.b.create_block();
        let dec = self.b.create_block();
        self.b.ins().brif(plain, fast, &[], heap, &[]);
        self.b.switch_to_block(heap);
        let one = self.b.ins().iconst(types::I64, 1);
        let bit = self.b.ins().ishl(one, old);
        let rc = self
            .b
            .ins()
            .band_imm(bit, self.tags.rc_counted | self.tags.rc_counted_hdr);
        let rc = self.b.ins().icmp_imm(IntCC::NotEqual, rc, 0);
        // The single-thread bias, and the release observers, all off.
        let addr = self
            .b
            .ins()
            .iconst(self.ptr, crate::plain_release_gate() as i64);
        let on = self.b.ins().load(types::I32, FLAGS, addr, 0);
        let off = self.b.ins().icmp_imm(IntCC::Equal, on, 0);
        let quiet = self.b.ins().band(rc, off);
        self.b.ins().brif(quiet, counted, &[], slow, &[]);
        self.b.switch_to_block(counted);
        let w = self.count_addr(local, bit);
        let c = self.b.ins().load(types::I64, FLAGS, w, 0);
        let shared = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThan, c, 1);
        self.b.ins().brif(shared, dec, &[], slow, &[]);
        self.b.switch_to_block(dec);
        let c1 = self.b.ins().iadd_imm(c, -1);
        self.b.ins().store(FLAGS, c1, w, 0);
        self.b.ins().jump(fast, &[]);
    }

    /// `STORE_FAST i` of the value at stack `slot` (just above the
    /// virtual stack) over a heap value, through the helper.
    fn store_from_slot(&mut self, pc: usize, i: u32, slot: usize) {
        let src = self.slot_addr(slot);
        let iv = self.b.ins().iconst(types::I64, i64::from(i));
        let r = self
            .call(h_store as *const () as usize, &[self.st, src, iv], true)
            .expect("returns");
        // Declined: the value is on the stack, as the core loop expects.
        let out = self.exit_with(pc, &[Item::Mem(slot)], INTERP);
        self.branch_out(r, out);
        let saved = self.last_pc;
        self.set_last(pc);
        self.check_released(pc + 1);
        self.last_pc = saved;
    }

    fn binary(&mut self, pc: usize, arg: u32) -> bool {
        let kind = arg as u8;
        let is = |k: BinOpKind| kind == k as u8;
        let bi = self.pop();
        let ai = self.pop();
        // A value on the stack (a call's result, an element) is as likely
        // a heap value as a number: the helper takes every shape. So does
        // a site the core loop has run on a natively served operand
        // (`datetime`'s; see `crate::native_site`).
        let native = crate::native_site(self.ext, pc);
        if native
            && matches!(ai, Item::Local(_) | Item::Const(_))
            && matches!(bi, Item::Local(_) | Item::Const(_))
        {
            return self.binary_borrowed(pc, kind, ai, bi);
        }
        // A site that has run on instances keeps its operands and result
        // on the stack, where an operator method's direct call leaves it
        // (with the whole stack on the frame's, see `binop_declined`).
        if crate::instance_site(self.ext, pc) {
            self.flush();
            return self.binary_helper(pc, kind, ai, bi);
        }
        if native {
            return self.binary_helper(pc, kind, ai, bi);
        }
        // A value on the stack (a call's result, an element, a field) is as
        // likely a heap value as a number: numbers in line, the result
        // onto the stack, anything else through the helper.
        if matches!(ai, Item::Mem(_)) || matches!(bi, Item::Mem(_)) {
            if self.cold {
                return self.binary_helper(pc, kind, ai, bi);
            }
            return self.binary_on_stack(pc, kind, ai, bi);
        }
        let (Some(a), Some(b)) = (self.operand(ai), self.operand(bi)) else {
            // A string or other heap constant: the core loop's out-of-line
            // shapes, through the helper.
            return self.binary_helper(pc, kind, ai, bi);
        };
        let slow = self.exit_with(pc, &[ai, bi], INTERP);
        if matches!(a.kind, Kind::Bool | Kind::None) || matches!(b.kind, Kind::Bool | Kind::None) {
            self.b.ins().jump(slow, &[]);
            return false;
        }
        let int_kind = [
            BinOpKind::Add,
            BinOpKind::Sub,
            BinOpKind::Mult,
            BinOpKind::BitAnd,
            BinOpKind::BitOr,
            BinOpKind::BitXor,
            BinOpKind::RShift,
            BinOpKind::LShift,
            BinOpKind::FloorDiv,
            BinOpKind::Mod,
        ]
        .into_iter()
        .any(&is);
        let float_kind =
            is(BinOpKind::Add) || is(BinOpKind::Sub) || is(BinOpKind::Mult) || is(BinOpKind::Div);
        let ia = self.is_kind(a, Kind::Int, self.tags.int);
        let ib = self.is_kind(b, Kind::Int, self.tags.int);
        let fa = self.is_kind(a, Kind::Float, self.tags.float);
        let fb = self.is_kind(b, Kind::Float, self.tags.float);
        // Both statically known: one path.
        let static_int = matches!((ia, ib), (Ok(true), Ok(true)));
        let static_num = matches!(
            (ia, ib, fa, fb),
            (Ok(x), Ok(y), Ok(p), Ok(q)) if (x || p) && (y || q)
        );
        if static_int {
            let Some(r) = self.int_op(kind, a.word, b.word, slow) else {
                self.b.ins().jump(slow, &[]);
                return false;
            };
            self.push(Item::Int(r));
            self.set_last(pc);
            return true;
        }
        if static_num {
            if !float_kind {
                self.b.ins().jump(slow, &[]);
                return false;
            }
            let x = self.as_f64(a, fa);
            let y = self.as_f64(b, fb);
            let r = self.float_op(kind, x, y, slow);
            self.push(Item::Float(r));
            self.set_last(pc);
            return true;
        }
        // At run time: two ints, or two numbers with a float; any other
        // shape through the helper.
        let other = self.b.create_block();
        let to_v = |s: &mut Self, r: Result<bool, Value>| match r {
            Ok(x) => s.b.ins().iconst(types::I8, i64::from(x)),
            Err(v) => v,
        };
        let (iav, ibv, fav, fbv) = (
            to_v(self, ia),
            to_v(self, ib),
            to_v(self, fa),
            to_v(self, fb),
        );
        let done = self.b.create_block();
        for _ in 0..3 {
            self.b.append_block_param(done, types::I64);
        }
        let zero = self.b.ins().iconst(types::I64, 0);
        let both_int = self.b.ins().band(iav, ibv);
        let int_b = self.b.create_block();
        let not_int = self.b.create_block();
        self.b.ins().brif(both_int, int_b, &[], not_int, &[]);
        self.b.switch_to_block(int_b);
        if int_kind {
            match self.int_op(kind, a.word, b.word, other) {
                Some(r) => {
                    let t = self.b.ins().iconst(types::I64, i64::from(self.tags.int));
                    self.b.ins().jump(done, &[t.into(), r.into(), zero.into()]);
                }
                None => {
                    self.b.ins().jump(other, &[]);
                }
            }
        } else {
            self.b.ins().jump(other, &[]);
        }
        self.b.switch_to_block(not_int);
        if float_kind {
            let na = self.b.ins().bor(iav, fav);
            let nb = self.b.ins().bor(ibv, fbv);
            let nums = self.b.ins().band(na, nb);
            let bad = self.b.ins().bxor_imm(nums, 1);
            self.branch_out(bad, other);
            let x = self.as_f64(a, Err(fav));
            let y = self.as_f64(b, Err(fbv));
            let r = self.float_op(kind, x, y, other);
            let bits = self.b.ins().bitcast(types::I64, MemFlags::new(), r);
            let t = self.b.ins().iconst(types::I64, i64::from(self.tags.float));
            self.b
                .ins()
                .jump(done, &[t.into(), bits.into(), zero.into()]);
        } else {
            self.b.ins().jump(other, &[]);
        }
        self.binary_fallback(pc, kind, ai, bi, other, done);
        self.b.switch_to_block(done);
        let p = self.b.block_params(done);
        let (t, w, byte) = (p[0], p[1], p[2]);
        self.push(Item::Dyn(t, w, byte));
        self.set_last(pc);
        true
    }

    /// `BINARY_OP` with an operand on the stack: two numbers in line, the
    /// result (a scalar) written to the lower operand's slot; anything
    /// else through [`h_binop`], whose result takes that slot too.
    fn binary_on_stack(&mut self, pc: usize, kind: u8, ai: Item, bi: Item) -> bool {
        let (Some(a), Some(b)) = (self.operand(ai), self.operand(bi)) else {
            return self.binary_helper(pc, kind, ai, bi);
        };
        let is = |k: BinOpKind| kind == k as u8;
        let int_kind = [
            BinOpKind::Add,
            BinOpKind::Sub,
            BinOpKind::Mult,
            BinOpKind::BitAnd,
            BinOpKind::BitOr,
            BinOpKind::BitXor,
            BinOpKind::RShift,
            BinOpKind::LShift,
            BinOpKind::FloorDiv,
            BinOpKind::Mod,
        ]
        .into_iter()
        .any(&is);
        let float_kind =
            is(BinOpKind::Add) || is(BinOpKind::Sub) || is(BinOpKind::Mult) || is(BinOpKind::Div);
        let slot = self.depth;
        let dst = self.slot_addr(slot);
        let other = self.b.create_block();
        let done = self.b.create_block();
        let to_v = |s: &mut Self, o: Opnd, kind: Kind, tag: u8| match s.is_kind(o, kind, tag) {
            Ok(x) => s.b.ins().iconst(types::I8, i64::from(x)),
            Err(v) => v,
        };
        let (int_t, float_t) = (self.tags.int, self.tags.float);
        let iav = to_v(self, a, Kind::Int, int_t);
        let ibv = to_v(self, b, Kind::Int, int_t);
        let fav = to_v(self, a, Kind::Float, float_t);
        let fbv = to_v(self, b, Kind::Float, float_t);
        let both_int = self.b.ins().band(iav, ibv);
        let int_b = self.b.create_block();
        let not_int = self.b.create_block();
        self.b.ins().brif(both_int, int_b, &[], not_int, &[]);
        self.b.switch_to_block(int_b);
        match int_kind
            .then(|| self.int_op(kind, a.word, b.word, other))
            .flatten()
        {
            Some(r) => {
                self.write_tag(dst, int_t);
                self.b.ins().store(FLAGS, r, dst, 8);
                self.b.ins().jump(done, &[]);
            }
            None => {
                self.b.ins().jump(other, &[]);
            }
        }
        self.b.switch_to_block(not_int);
        if float_kind {
            let na = self.b.ins().bor(iav, fav);
            let nb = self.b.ins().bor(ibv, fbv);
            let nums = self.b.ins().band(na, nb);
            let bad = self.b.ins().bxor_imm(nums, 1);
            self.branch_out(bad, other);
            let x = self.as_f64(a, Err(fav));
            let y = self.as_f64(b, Err(fbv));
            let r = self.float_op(kind, x, y, other);
            let bits = self.b.ins().bitcast(types::I64, MemFlags::new(), r);
            self.write_tag(dst, float_t);
            self.b.ins().store(FLAGS, bits, dst, 8);
            self.b.ins().jump(done, &[]);
        } else {
            self.b.ins().jump(other, &[]);
        }
        // Any other shape: the operands onto the stack and through the
        // helper (the operands it releases are the stack's own).
        self.b.switch_to_block(other);
        let cold = std::mem::replace(&mut self.cold, true);
        if !matches!(ai, Item::Mem(_)) {
            self.materialize(ai, slot);
        }
        if !matches!(bi, Item::Mem(_)) {
            self.materialize(bi, slot + 1);
        }
        self.cold = cold;
        let k = self.b.ins().iconst(types::I32, i64::from(kind));
        let r = self
            .call(h_binop as *const () as usize, &[dst, k], true)
            .expect("returns");
        let out = self.exit_with(pc, &[Item::Mem(slot), Item::Mem(slot + 1)], INTERP);
        self.binop_declined(pc, slot, r, out, done);
        self.b.switch_to_block(done);
        self.push(Item::Mem(slot));
        self.set_last(pc);
        true
    }

    /// The other shapes of a `BINARY_OP` whose in-line arithmetic declined
    /// (at `other`): the operands onto the stack and through [`h_binop`].
    /// A scalar result joins the in-line ones at `done` (its tag, word and
    /// `bool` byte); any other leaves for the core loop after the
    /// instruction, and a decline before it.
    fn binary_fallback(
        &mut self,
        pc: usize,
        kind: u8,
        ai: Item,
        bi: Item,
        other: Block,
        done: Block,
    ) {
        self.b.switch_to_block(other);
        let slot = self.depth;
        let cold = std::mem::replace(&mut self.cold, true);
        self.materialize(ai, slot);
        self.materialize(bi, slot + 1);
        self.cold = cold;
        let at = self.slot_addr(slot);
        let k = self.b.ins().iconst(types::I32, i64::from(kind));
        let r = self
            .call(h_binop as *const () as usize, &[at, k], true)
            .expect("returns");
        let out = self.exit_with(pc, &[Item::Mem(slot), Item::Mem(slot + 1)], INTERP);
        let ok = self.b.create_block();
        self.binop_declined(pc, slot, r, out, ok);
        self.b.switch_to_block(ok);
        // (The operands were a local's, a constant's or scalars: their
        // copies' releases free nothing.)
        let saved = self.last_pc.replace(pc);
        let after = self.exit_with(pc + 1, &[Item::Mem(slot)], INTERP);
        self.last_pc = saved;
        let tag = self.tag_at(at);
        let scalar = self.is_scalar(tag);
        let ok = self.b.create_block();
        self.b.ins().brif(scalar, ok, &[], after, &[]);
        self.b.switch_to_block(ok);
        let w = self.b.ins().load(types::I64, FLAGS, at, 8);
        let byte = self.b.ins().uload8(types::I64, FLAGS, at, 1);
        self.b
            .ins()
            .jump(done, &[tag.into(), w.into(), byte.into()]);
    }

    /// After [`h_binop`] returned `r` for the operands at `slot` and above:
    /// done (`r` zero) at `ok`; otherwise two instances whose class's
    /// method runs alone through [`h_binop_dunder`] (its result at `slot`,
    /// at `ok` too), and anything else at `out`. The method's call can
    /// raise or switch, which leaves the whole stack on the frame's: only
    /// a stack with nothing virtual below the operands tries it.
    fn binop_declined(&mut self, pc: usize, slot: usize, r: Value, out: Block, ok: Block) {
        if !self.vs.iter().all(|i| matches!(i, Item::Mem(_))) {
            self.b.ins().brif(r, out, &[], ok, &[]);
            return;
        }
        let dunder = self.b.create_block();
        self.b.ins().brif(r, dunder, &[], ok, &[]);
        self.b.switch_to_block(dunder);
        let site: Box<DirectSite> = Box::default();
        let site_at = self
            .b
            .ins()
            .iconst(self.ptr, std::ptr::from_ref::<DirectSite>(&site) as i64);
        self.direct_sites.push(site);
        let code = self.code_ptr();
        let pcv = self.b.ins().iconst(types::I64, pc as i64);
        let slotv = self.b.ins().iconst(types::I64, slot as i64);
        let r = self
            .call(
                h_binop_dunder as *const () as usize,
                &[self.st, code, pcv, slotv, site_at],
                true,
            )
            .expect("returns");
        let other = self.b.create_block();
        self.b.ins().brif(r, other, &[], ok, &[]);
        self.b.switch_to_block(other);
        let declined = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, r, i64::from(CALL_DECLINED));
        let leave = self.b.create_block();
        self.b.ins().brif(declined, out, &[], leave, &[]);
        self.b.switch_to_block(leave);
        self.leave(r);
    }

    /// `BINARY_OP` through [`h_binop`]: the operands onto the stack, and
    /// the result in the lower one's slot.
    fn binary_helper(&mut self, pc: usize, kind: u8, ai: Item, bi: Item) -> bool {
        let slot = self.depth;
        self.materialize(ai, slot);
        self.materialize(bi, slot + 1);
        let at = self.slot_addr(slot);
        let k = self.b.ins().iconst(types::I32, i64::from(kind));
        let r = self
            .call(h_binop as *const () as usize, &[at, k], true)
            .expect("returns");
        let out = self.exit_with(pc, &[Item::Mem(slot), Item::Mem(slot + 1)], INTERP);
        let ok = self.b.create_block();
        self.binop_declined(pc, slot, r, out, ok);
        self.b.switch_to_block(ok);
        self.push(Item::Mem(slot));
        self.set_last(pc);
        true
    }

    /// The address of a local's or a constant's value (`None` for any
    /// other item).
    fn item_addr(&mut self, item: Item) -> Option<Value> {
        match item {
            Item::Local(i) => Some(self.local_addr(i)),
            Item::Const(k) => Some(self.const_addr(k)),
            _ => None,
        }
    }

    /// `BINARY_OP` of two locals or constants through [`h_binop_ref`],
    /// which reads them in place (no references taken or released): the
    /// result in the next free slot.
    fn binary_borrowed(&mut self, pc: usize, kind: u8, ai: Item, bi: Item) -> bool {
        let (Some(a), Some(b)) = (self.item_addr(ai), self.item_addr(bi)) else {
            return self.binary_helper(pc, kind, ai, bi);
        };
        let slot = self.depth;
        let dst = self.slot_addr(slot);
        let k = self.b.ins().iconst(types::I32, i64::from(kind));
        let r = self
            .call(h_binop_ref as *const () as usize, &[dst, a, b, k], true)
            .expect("returns");
        let out = self.exit_with(pc, &[ai, bi], INTERP);
        self.branch_out(r, out);
        self.push(Item::Mem(slot));
        self.set_last(pc);
        true
    }

    /// `COMPARE_OP` through [`h_compare`]: the operands onto the stack,
    /// and the result in the lower one's slot.
    fn compare_helper(&mut self, pc: usize, arg: u32, ai: Item, bi: Item) -> bool {
        let slot = self.depth;
        self.materialize(ai, slot);
        self.materialize(bi, slot + 1);
        let at = self.slot_addr(slot);
        let k = self.b.ins().iconst(types::I32, i64::from(arg));
        let r = self
            .call(h_compare as *const () as usize, &[at, k], true)
            .expect("returns");
        let out = self.exit_with(pc, &[Item::Mem(slot), Item::Mem(slot + 1)], INTERP);
        self.branch_out(r, out);
        self.push(Item::Mem(slot));
        self.set_last(pc);
        true
    }

    /// A container instruction through [`h_container`] (the operands on
    /// the stack; the depth after is the static one).
    fn container(&mut self, pc: usize) -> bool {
        let after = self.depths.get(pc + 1).copied().unwrap_or(-1);
        if after < 0 {
            self.exit(INTERP, pc);
            return false;
        }
        self.flush();
        let ins = self.b.ins().iconst(
            self.ptr,
            std::ptr::from_ref(&self.code.instructions[pc]) as i64,
        );
        let code = self
            .b
            .ins()
            .iconst(self.ptr, std::ptr::from_ref(self.code) as i64);
        let pcv = self.b.ins().iconst(types::I64, pc as i64);
        let len = self.b.ins().iconst(types::I64, self.depth as i64);
        let can_raise = self.b.ins().iconst(types::I64, 1);
        let n = self
            .call_typed(
                h_container as *const () as usize,
                &[self.st, ins, code, pcv, len, can_raise],
                Some(types::I64),
            )
            .expect("returns");
        let declined = self.b.ins().icmp_imm(IntCC::Equal, n, -1);
        let out = self.exit_with(pc, &[], INTERP);
        self.branch_out(declined, out);
        let raised = self.b.ins().icmp_imm(IntCC::Equal, n, -2);
        let raise_b = self.b.create_block();
        let ok = self.b.create_block();
        self.b.ins().brif(raised, raise_b, &[], ok, &[]);
        self.b.switch_to_block(raise_b);
        let s = self.b.ins().iconst(types::I32, i64::from(RAISED));
        self.leave(s);
        self.b.switch_to_block(ok);
        self.depth = after as usize;
        self.set_last(pc);
        self.check_released(pc + 1);
        true
    }

    /// `local[i]` of a list or tuple local and an `int` index, in line:
    /// the item is read in place (the local gives no reference to take or
    /// release) and copied to the stack. Any other operands (a `str` or a
    /// dict, a borrowed list, an index out of range) take the in-place
    /// helper, as [`Self::subscr_ref`] does. `false`, emitting nothing, for
    /// other operand shapes.
    fn seq_index(&mut self, pc: usize) -> bool {
        let (Some(list), Some((tlen, titems))) = (self.tags.list_layout, self.tags.tuple_layout)
        else {
            return false;
        };
        let n = self.vs.len();
        if self.cold || n < 2 || self.depths.get(pc + 1).copied().unwrap_or(-1) < 0 {
            return false;
        }
        let (cont, key) = (self.vs[n - 2], self.vs[n - 1]);
        let Item::Local(l) = cont else {
            return false;
        };
        let int_key = match key {
            Item::Int(_) | Item::Local(_) | Item::Dyn(..) => true,
            Item::Const(k) => matches!(self.ext.objects[k as usize], Object::Int(_)),
            _ => false,
        };
        if !int_key {
            return false;
        }
        self.pop();
        self.pop();
        // Both paths join with the rest of the stack written out.
        self.flush();
        let slot = self.depth;
        let slow = self.b.create_block();
        let join = self.b.create_block();
        let idx = self.operand(key).expect("an int-shaped key");
        if let Some(t) = idx.tag {
            let other = self
                .b
                .ins()
                .icmp_imm(IntCC::NotEqual, t, i64::from(self.tags.int));
            self.branch_out(other, slow);
        }
        let addr = self.local_addr(l);
        let tag = self.tag_at(addr);
        let word = self.b.ins().load(self.ptr, FLAGS, addr, 8);
        let as_list = self.b.create_block();
        let not_list = self.b.create_block();
        let as_tuple = self.b.create_block();
        let found = self.b.create_block();
        self.b.append_block_param(found, types::I64);
        self.b.append_block_param(found, self.ptr);
        let is_list = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, tag, i64::from(self.tags.list));
        self.b.ins().brif(is_list, as_list, &[], not_list, &[]);
        // A list: its cell unborrowed, and cells unshared.
        self.b.switch_to_block(as_list);
        let flag = self
            .b
            .ins()
            .iconst(self.ptr, crate::sync::cells_unguarded_flag() as i64);
        let shared = self.b.ins().uload8(types::I32, FLAGS, flag, 0);
        self.branch_out(shared, slow);
        let borrow = self.b.ins().load(types::I32, FLAGS, word, list.borrow);
        let held = self.b.ins().icmp_imm(IntCC::SignedLessThan, borrow, 0);
        self.branch_out(held, slow);
        let len = self.b.ins().load(types::I64, FLAGS, word, list.len);
        let items = self.b.ins().load(self.ptr, FLAGS, word, list.ptr);
        self.b.ins().jump(found, &[len.into(), items.into()]);
        self.b.switch_to_block(not_list);
        let is_tuple = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, tag, i64::from(self.tags.tuple));
        self.b.ins().brif(is_tuple, as_tuple, &[], slow, &[]);
        self.b.switch_to_block(as_tuple);
        let len = self.b.ins().load(types::I64, FLAGS, word, tlen);
        let items = self.b.ins().iadd_imm(word, i64::from(titems));
        self.b.ins().jump(found, &[len.into(), items.into()]);
        self.b.switch_to_block(found);
        let p = self.b.block_params(found);
        let (len, items) = (p[0], p[1]);
        let i = idx.word;
        let neg = self.b.ins().icmp_imm(IntCC::SignedLessThan, i, 0);
        let wrapped = self.b.ins().iadd(i, len);
        let i = self.b.ins().select(neg, wrapped, i);
        let outside = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, i, len);
        self.branch_out(outside, slow);
        let off = self.b.ins().ishl_imm(i, 4);
        let item = self.b.ins().iadd(items, off);
        let dst = self.slot_addr(slot);
        self.copy_value(dst, item);
        self.b.ins().jump(join, &[]);
        // Anything else (a dict, a string, a borrowed list, an index out of
        // range) through the in-place helper, which reads the local
        // container where it lies (see `Self::subscr_ref`).
        self.b.switch_to_block(slow);
        let (pcont, own_c) = self.ref_or_own(cont, slot);
        let (pkey, own_k) = self.ref_or_own(key, slot + 1);
        let owned = u8::from(own_c) | (u8::from(own_k) << 1);
        self.ref_call(
            pc,
            h_subscr_ref as *const () as usize,
            slot,
            2,
            &[pcont, pkey],
            owned,
        );
        // (The helper released the operands; a finalizer it queued runs
        // before the next instruction, as after `Self::container`.)
        self.depth = slot + 1;
        self.set_last(pc);
        self.check_released(pc + 1);
        self.depth = slot;
        self.b.ins().jump(join, &[]);
        self.b.switch_to_block(join);
        self.push(Item::Mem(slot));
        self.set_last(pc);
        true
    }

    /// A container instruction through one of the in-place helpers
    /// ([`h_subscr_ref`], [`h_unpack_ref`], [`h_store_subscr_ref`]), its
    /// `n` operands from `slot` up (the owned ones there, by the bits of
    /// `owned`; `ops` their addresses and any further arguments): the
    /// helper's new depth, a decline leaving for the core loop with the
    /// operands on the stack, a raise (only with nothing virtual below
    /// the operands) taking the handlers.
    fn ref_call(
        &mut self,
        pc: usize,
        helper: usize,
        slot: usize,
        n: usize,
        ops: &[Value],
        owned: u8,
    ) {
        let ins = self.b.ins().iconst(
            self.ptr,
            std::ptr::from_ref(&self.code.instructions[pc]) as i64,
        );
        let code = self.code_ptr();
        let pcv = self.b.ins().iconst(types::I64, pc as i64);
        let lenv = self.b.ins().iconst(types::I64, (slot + n) as i64);
        let raisable = self.vs.iter().all(|i| matches!(i, Item::Mem(_)));
        let flags = self
            .b
            .ins()
            .iconst(types::I64, i64::from(owned) | (i64::from(raisable) << 8));
        let mut args = vec![self.st, ins, code, pcv, lenv];
        args.extend_from_slice(ops);
        args.push(flags);
        let r = self
            .call_typed(helper, &args, Some(types::I64))
            .expect("returns");
        let declined = self.b.ins().icmp_imm(IntCC::Equal, r, -1);
        let restore: Vec<Item> = (slot..slot + n).map(Item::Mem).collect();
        let out = self.exit_with(pc, &restore, INTERP);
        self.branch_out(declined, out);
        if raisable {
            let raised = self.b.ins().icmp_imm(IntCC::Equal, r, -2);
            let raise_b = self.b.create_block();
            let ok = self.b.create_block();
            self.b.ins().brif(raised, raise_b, &[], ok, &[]);
            self.b.switch_to_block(raise_b);
            let s = self.b.ins().iconst(types::I32, i64::from(RAISED));
            self.leave(s);
            self.b.switch_to_block(ok);
        }
    }

    /// `BINARY_SUBSCR` through [`h_subscr_ref`], which reads a local's or
    /// constant's operand in place (no reference taken or released): the
    /// item in the container's slot. `false`, emitting nothing, where the
    /// depth after isn't known.
    fn subscr_ref(&mut self, pc: usize) -> bool {
        if self.depth < 2 || self.depths.get(pc + 1).copied().unwrap_or(-1) < 0 {
            return false;
        }
        let key = self.pop();
        let cont = self.pop();
        let slot = self.depth;
        let (pcont, own_c) = self.ref_or_own(cont, slot);
        let (pkey, own_k) = self.ref_or_own(key, slot + 1);
        let owned = u8::from(own_c) | (u8::from(own_k) << 1);
        self.ref_call(
            pc,
            h_subscr_ref as *const () as usize,
            slot,
            2,
            &[pcont, pkey],
            owned,
        );
        self.push(Item::Mem(slot));
        self.set_last(pc);
        // (The helper released the owned operands; a finalizer that queued
        // runs before the next instruction.)
        self.check_released(pc + 1);
        true
    }

    /// `UNPACK_SEQUENCE n` through [`h_unpack_ref`], which reads a local's
    /// or constant's sequence in place. `false`, emitting nothing, where
    /// the depth after isn't known.
    fn unpack_ref(&mut self, pc: usize, n: usize) -> bool {
        if n == 0 || self.depth == 0 || self.depths.get(pc + 1).copied().unwrap_or(-1) < 0 {
            return false;
        }
        let item = self.pop();
        let slot = self.depth;
        let (src, owned) = self.ref_or_own(item, slot);
        let nv = self.b.ins().iconst(types::I64, n as i64);
        self.ref_call(
            pc,
            h_unpack_ref as *const () as usize,
            slot,
            1,
            &[src, nv],
            u8::from(owned),
        );
        for k in 0..n {
            self.push(Item::Mem(slot + k));
        }
        self.set_last(pc);
        self.check_released(pc + 1);
        true
    }

    /// `STORE_SUBSCR` through [`h_store_subscr_ref`], which reads a local's
    /// or constant's operand in place. `false`, emitting nothing, where
    /// the depth after isn't known.
    fn store_subscr_ref(&mut self, pc: usize) -> bool {
        if self.depth < 3 || self.depths.get(pc + 1).copied().unwrap_or(-1) < 0 {
            return false;
        }
        let key = self.pop();
        let cont = self.pop();
        let val = self.pop();
        let slot = self.depth;
        let (pv, own_v) = self.ref_or_own(val, slot);
        let (pcont, own_c) = self.ref_or_own(cont, slot + 1);
        let (pkey, own_k) = self.ref_or_own(key, slot + 2);
        let owned = u8::from(own_v) | (u8::from(own_c) << 1) | (u8::from(own_k) << 2);
        self.ref_call(
            pc,
            h_store_subscr_ref as *const () as usize,
            slot,
            3,
            &[pv, pcont, pkey],
            owned,
        );
        self.set_last(pc);
        self.check_released(pc + 1);
        true
    }

    /// `LIST_APPEND` through [`h_list_append`] (the value leaves the stack
    /// into the list `arg` below it).
    fn list_append(&mut self, pc: usize, arg: usize) -> bool {
        self.flush();
        let list = self.slot_addr(self.depth - 1 - arg);
        let value = self.slot_addr(self.depth - 1);
        let r = self
            .call(h_list_append as *const () as usize, &[list, value], true)
            .expect("returns");
        let out = self.exit_with(pc, &[], INTERP);
        self.branch_out(r, out);
        self.depth -= 1;
        self.set_last(pc);
        true
    }

    /// An operand for a helper that reads it in place: a local's or a
    /// constant's own value (borrowed, `false`), or anything else on the
    /// stack at `slot` (owned there, `true`).
    fn ref_or_own(&mut self, item: Item, slot: usize) -> (Value, bool) {
        match item {
            Item::Local(i) => (self.local_addr(i), false),
            Item::Const(k) => (self.const_addr(k), false),
            other => {
                self.materialize(other, slot);
                (self.slot_addr(slot), true)
            }
        }
    }

    /// `CONTAINS_OP` through [`h_contains_ref`], which reads a local or
    /// constant operand in place (any other through [`h_contains`]): an
    /// unboxed `bool`.
    fn contains(&mut self, pc: usize, arg: u32) -> bool {
        let bi = self.pop();
        let ai = self.pop();
        let slot = self.depth;
        let (pa, own_a) = self.ref_or_own(ai, slot);
        let (pb, own_b) = self.ref_or_own(bi, slot + 1);
        let owned = self.b.ins().iconst(
            types::I32,
            i64::from(u8::from(own_a) | (u8::from(own_b) << 1)),
        );
        let at = self.slot_addr(slot);
        let r = self
            .call(
                h_contains_ref as *const () as usize,
                &[pa, pb, owned, at],
                true,
            )
            .expect("returns");
        let declined = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThan, r, 1);
        let out = self.exit_with(pc, &[Item::Mem(slot), Item::Mem(slot + 1)], INTERP);
        self.branch_out(declined, out);
        let found = self.b.ins().icmp_imm(IntCC::NotEqual, r, 0);
        let found = if arg == 1 {
            self.b.ins().bxor_imm(found, 1)
        } else {
            found
        };
        self.push(Item::Bool(found));
        self.set_last(pc);
        self.check_released(pc + 1);
        true
    }

    /// Two ints, by `kind`; a result that doesn't fit (or an operand the
    /// core loop's arm declines) branches to `slow`. `None` for a kind
    /// with no `int` arm.
    fn int_op(&mut self, kind: u8, x: Value, y: Value, slow: Block) -> Option<Value> {
        let is = |k: BinOpKind| kind == k as u8;
        Some(if is(BinOpKind::Add) {
            let r = self.b.ins().iadd(x, y);
            let axr = self.b.ins().bxor(x, r);
            let bxr = self.b.ins().bxor(y, r);
            let and = self.b.ins().band(axr, bxr);
            let ovf = self.b.ins().icmp_imm(IntCC::SignedLessThan, and, 0);
            self.branch_out(ovf, slow);
            r
        } else if is(BinOpKind::Sub) {
            let r = self.b.ins().isub(x, y);
            let axb = self.b.ins().bxor(x, y);
            let axr = self.b.ins().bxor(x, r);
            let and = self.b.ins().band(axb, axr);
            let ovf = self.b.ins().icmp_imm(IntCC::SignedLessThan, and, 0);
            self.branch_out(ovf, slow);
            r
        } else if is(BinOpKind::Mult) {
            let lo = self.b.ins().imul(x, y);
            let hi = self.b.ins().smulhi(x, y);
            let sign = self.b.ins().sshr_imm(lo, 63);
            let ovf = self.b.ins().icmp(IntCC::NotEqual, hi, sign);
            self.branch_out(ovf, slow);
            lo
        } else if is(BinOpKind::BitAnd) {
            self.b.ins().band(x, y)
        } else if is(BinOpKind::BitOr) {
            self.b.ins().bor(x, y)
        } else if is(BinOpKind::BitXor) {
            self.b.ins().bxor(x, y)
        } else if is(BinOpKind::RShift) {
            let out = self
                .b
                .ins()
                .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, y, 64);
            self.branch_out(out, slow);
            self.b.ins().sshr(x, y)
        } else if is(BinOpKind::LShift) {
            let out = self
                .b
                .ins()
                .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, y, 63);
            self.branch_out(out, slow);
            let r = self.b.ins().ishl(x, y);
            let back = self.b.ins().sshr(r, y);
            let lost = self.b.ins().icmp(IntCC::NotEqual, back, x);
            self.branch_out(lost, slow);
            r
        } else if is(BinOpKind::FloorDiv) || is(BinOpKind::Mod) {
            let yneg = self.b.ins().icmp_imm(IntCC::SignedLessThanOrEqual, y, 0);
            let xneg = self.b.ins().icmp_imm(IntCC::SignedLessThan, x, 0);
            let bad = self.b.ins().bor(yneg, xneg);
            self.branch_out(bad, slow);
            if is(BinOpKind::FloorDiv) {
                self.b.ins().udiv(x, y)
            } else {
                self.b.ins().urem(x, y)
            }
        } else {
            return None;
        })
    }

    /// Two floats, by `kind` (one of add, subtract, multiply, divide); a
    /// zero divisor or a NaN result branches to `slow` (the core loop
    /// raises, or tags the NaN).
    fn float_op(&mut self, kind: u8, x: Value, y: Value, slow: Block) -> Value {
        let is = |k: BinOpKind| kind == k as u8;
        let r = if is(BinOpKind::Add) {
            self.b.ins().fadd(x, y)
        } else if is(BinOpKind::Sub) {
            self.b.ins().fsub(x, y)
        } else if is(BinOpKind::Mult) {
            self.b.ins().fmul(x, y)
        } else {
            let zero = self.b.ins().f64const(0.0);
            let z = self.b.ins().fcmp(FloatCC::Equal, y, zero);
            self.branch_out(z, slow);
            self.b.ins().fdiv(x, y)
        };
        let nan = self.b.ins().fcmp(FloatCC::Unordered, r, r);
        self.branch_out(nan, slow);
        r
    }

    fn compare(&mut self, pc: usize, arg: u32) -> bool {
        let kind = (arg & !COMPARE_OP_TO_BOOL_FLAG) as u8;
        let (icc, fcc) = match kind {
            k if k == CompareKind::Lt as u8 => (IntCC::SignedLessThan, FloatCC::LessThan),
            k if k == CompareKind::LtE as u8 => {
                (IntCC::SignedLessThanOrEqual, FloatCC::LessThanOrEqual)
            }
            k if k == CompareKind::Eq as u8 => (IntCC::Equal, FloatCC::Equal),
            k if k == CompareKind::NotEq as u8 => (IntCC::NotEqual, FloatCC::NotEqual),
            k if k == CompareKind::Gt as u8 => (IntCC::SignedGreaterThan, FloatCC::GreaterThan),
            k if k == CompareKind::GtE as u8 => {
                (IntCC::SignedGreaterThanOrEqual, FloatCC::GreaterThanOrEqual)
            }
            _ => {
                self.exit(INTERP, pc);
                return false;
            }
        };
        let bi = self.pop();
        let ai = self.pop();
        // A site the core loop has run on natively served operands: through
        // the helper (see `binary`).
        if crate::native_site(self.ext, pc) {
            return self.compare_helper(pc, arg, ai, bi);
        }
        let (Some(a), Some(b)) = (self.operand(ai), self.operand(bi)) else {
            // A string or other heap constant: through the helper.
            let other = self.b.create_block();
            let done = self.b.create_block();
            self.b.append_block_param(done, types::I8);
            self.b.ins().jump(other, &[]);
            self.compare_fallback(pc, arg, ai, bi, other, done);
            self.b.switch_to_block(done);
            let r = self.b.block_params(done)[0];
            self.push(Item::Bool(r));
            self.set_last(pc);
            return true;
        };
        if matches!(a.kind, Kind::Bool | Kind::None) || matches!(b.kind, Kind::Bool | Kind::None) {
            let slow = self.exit_with(pc, &[ai, bi], INTERP);
            self.b.ins().jump(slow, &[]);
            return false;
        }
        let ia = self.is_kind(a, Kind::Int, self.tags.int);
        let ib = self.is_kind(b, Kind::Int, self.tags.int);
        let fa = self.is_kind(a, Kind::Float, self.tags.float);
        let fb = self.is_kind(b, Kind::Float, self.tags.float);
        let int_cmp = |s: &mut Self| s.b.ins().icmp(icc, a.word, b.word);
        // Two numbers as floats (an unordered pair takes `other`).
        let float_cmp = |s: &mut Self, x: Value, y: Value, other: Block| {
            let uno = s.b.ins().fcmp(FloatCC::Unordered, x, y);
            s.branch_out(uno, other);
            s.b.ins().fcmp(fcc, x, y)
        };
        let r = match (ia, ib, fa, fb) {
            (Ok(true), Ok(true), ..) => int_cmp(self),
            (_, _, Ok(true), Ok(true)) => {
                let slow = self.exit_with(pc, &[ai, bi], INTERP);
                let x = self.b.ins().bitcast(types::F64, MemFlags::new(), a.word);
                let y = self.b.ins().bitcast(types::F64, MemFlags::new(), b.word);
                float_cmp(self, x, y, slow)
            }
            _ => {
                // At run time: two ints; two numbers with a float (an int
                // within 2**53 converts exactly); anything else through
                // the helper.
                let other = self.b.create_block();
                let to_v = |s: &mut Self, r: Result<bool, Value>| match r {
                    Ok(x) => s.b.ins().iconst(types::I8, i64::from(x)),
                    Err(v) => v,
                };
                let (iav, ibv, fav, fbv) = (
                    to_v(self, ia),
                    to_v(self, ib),
                    to_v(self, fa),
                    to_v(self, fb),
                );
                let done = self.b.create_block();
                self.b.append_block_param(done, types::I8);
                let both_int = self.b.ins().band(iav, ibv);
                let int_b = self.b.create_block();
                let not_int = self.b.create_block();
                self.b.ins().brif(both_int, int_b, &[], not_int, &[]);
                self.b.switch_to_block(int_b);
                let r = int_cmp(self);
                self.b.ins().jump(done, &[r.into()]);
                self.b.switch_to_block(not_int);
                let na = self.b.ins().bor(iav, fav);
                let nb = self.b.ins().bor(ibv, fbv);
                let nums = self.b.ins().band(na, nb);
                let bad = self.b.ins().bxor_imm(nums, 1);
                self.branch_out(bad, other);
                let exact = |s: &mut Self, word: Value, is_int: Value| {
                    let t = s.b.ins().iadd_imm(word, 1 << 53);
                    let ok =
                        s.b.ins()
                            .icmp_imm(IntCC::UnsignedLessThanOrEqual, t, 1 << 54);
                    let not_int = s.b.ins().bxor_imm(is_int, 1);
                    s.b.ins().bor(ok, not_int)
                };
                let ea = exact(self, a.word, iav);
                let eb = exact(self, b.word, ibv);
                let both = self.b.ins().band(ea, eb);
                let inexact = self.b.ins().bxor_imm(both, 1);
                self.branch_out(inexact, other);
                let x = self.as_f64(a, Err(fav));
                let y = self.as_f64(b, Err(fbv));
                let r = float_cmp(self, x, y, other);
                self.b.ins().jump(done, &[r.into()]);
                self.compare_fallback(pc, arg, ai, bi, other, done);
                self.b.switch_to_block(done);
                self.b.block_params(done)[0]
            }
        };
        self.push(Item::Bool(r));
        self.set_last(pc);
        true
    }

    /// The other shapes of a `COMPARE_OP` whose in-line compare declined
    /// (at `other`): the operands onto the stack and through
    /// [`h_compare_bool`], whose result joins the in-line ones at `done`
    /// (a decline leaves for the core loop before the instruction).
    fn compare_fallback(
        &mut self,
        pc: usize,
        arg: u32,
        ai: Item,
        bi: Item,
        other: Block,
        done: Block,
    ) {
        self.b.switch_to_block(other);
        let slot = self.depth;
        let cold = std::mem::replace(&mut self.cold, true);
        self.materialize(ai, slot);
        self.materialize(bi, slot + 1);
        self.cold = cold;
        let at = self.slot_addr(slot);
        let k = self.b.ins().iconst(types::I32, i64::from(arg));
        let r = self
            .call(h_compare_bool as *const () as usize, &[at, k], true)
            .expect("returns");
        let declined = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThan, r, 1);
        let out = self.exit_with(pc, &[Item::Mem(slot), Item::Mem(slot + 1)], INTERP);
        self.branch_out(declined, out);
        // (The helper releases only numbers and strings: no finalizer.)
        let t = self.b.ins().icmp_imm(IntCC::NotEqual, r, 0);
        self.b.ins().jump(done, &[t.into()]);
    }

    /// An item's tag, word and `bool` byte, for an identity test (`None`
    /// for a stack value, whose release the test would owe).
    fn identity(&mut self, item: Item) -> Option<(Value, Value, Value)> {
        let c = |s: &mut Self, v: i64| s.b.ins().iconst(types::I64, v);
        Some(match item {
            Item::Mem(_) => return None,
            Item::Local(i) => {
                let at = self.local_addr(i);
                let t = self.tag_at(at);
                let w = self.b.ins().load(types::I64, FLAGS, at, 8);
                let b = self.b.ins().uload8(types::I64, FLAGS, at, 1);
                (t, w, b)
            }
            Item::Const(k) => {
                let obj = &self.ext.objects[k as usize];
                let p = std::ptr::from_ref(obj).cast::<u8>();
                // SAFETY: a constant's tag, payload byte and word (the
                // constants live as long as the code).
                let (t, b, w) = unsafe { (*p, *p.add(1), p.add(8).cast::<u64>().read_unaligned()) };
                let (t, w, b) = (i64::from(t), w as i64, i64::from(b));
                (c(self, t), c(self, w), c(self, b))
            }
            Item::Int(v) => {
                let t = c(self, i64::from(self.tags.int));
                let z = c(self, 0);
                (t, v, z)
            }
            Item::Float(v) => {
                let t = c(self, i64::from(self.tags.float));
                let w = self.b.ins().bitcast(types::I64, MemFlags::new(), v);
                let z = c(self, 0);
                (t, w, z)
            }
            Item::Bool(v) => {
                let t = c(self, i64::from(self.tags.boolean));
                let z = c(self, 0);
                let b = self.b.ins().uextend(types::I64, v);
                (t, z, b)
            }
            Item::Dyn(t, w, b) => (t, w, b),
        })
    }

    /// `IS_OP` of two values the code holds no reference to: the
    /// variants' identity rules (`Object::is_same`) in line, for the
    /// scalars and the pointer-identity heap variants.
    fn is_op(&mut self, pc: usize, arg: u32) -> bool {
        let bi = self.pop();
        let ai = self.pop();
        let (Some((ta, wa, ba)), Some((tb, wb, bb))) = (self.identity(ai), self.identity(bi))
        else {
            // A stack value (whose release the test owes): through the
            // helper, the operands onto the stack.
            let slot = self.depth;
            self.materialize(ai, slot);
            self.materialize(bi, slot + 1);
            let at = self.slot_addr(slot);
            let k = self.b.ins().iconst(types::I32, i64::from(arg));
            let r = self
                .call(h_is as *const () as usize, &[at, k], true)
                .expect("returns");
            let declined = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThan, r, 1);
            let out = self.exit_with(pc, &[Item::Mem(slot), Item::Mem(slot + 1)], INTERP);
            self.branch_out(declined, out);
            // (The result onto the stack for a finalizer's stop.)
            let saved = self.last_pc.replace(pc);
            let marked = self.exit_with(pc + 1, &[Item::Mem(slot)], MARKED);
            self.last_pc = saved;
            let f = self.dead_flag();
            let marked_b = self.b.create_block();
            let fine = self.b.create_block();
            self.b.ins().brif(f, marked_b, &[], fine, &[]);
            self.b.switch_to_block(marked_b);
            let dst = self.slot_addr(slot);
            self.write_tag(dst, self.tags.boolean);
            let r8 = self.b.ins().ireduce(types::I8, r);
            self.b.ins().store(FLAGS, r8, dst, 1);
            self.b.ins().jump(marked, &[]);
            self.b.switch_to_block(fine);
            let t = self.b.ins().icmp_imm(IntCC::NotEqual, r, 0);
            self.push(Item::Bool(t));
            self.set_last(pc);
            return true;
        };
        let slow = self.exit_with(pc, &[ai, bi], INTERP);
        let tags = self.tags;
        let same_tag = self.b.ins().icmp(IntCC::Equal, ta, tb);
        // Which rule the (shared) tag takes.
        let one = self.b.ins().iconst(types::I64, 1);
        let bit = self.b.ins().ishl(one, ta);
        let words_mask = tags.by_pointer | (1i64 << tags.int) | (1i64 << tags.float);
        let by_word = self.b.ins().band_imm(bit, words_mask);
        let by_word = self.b.ins().icmp_imm(IntCC::NotEqual, by_word, 0);
        let is_bool = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, ta, i64::from(tags.boolean));
        let unit_mask = (1i64 << tags.none) | (1i64 << tags.unbound);
        let unit = self.b.ins().band_imm(bit, unit_mask);
        let unit = self.b.ins().icmp_imm(IntCC::NotEqual, unit, 0);
        // A tag no rule here covers (a long, a complex, …) is the core
        // loop's, when the tags match.
        let known = self.b.ins().bor(by_word, is_bool);
        let known = self.b.ins().bor(known, unit);
        let unknown = self.b.ins().bxor_imm(known, 1);
        let bad = self.b.ins().band(same_tag, unknown);
        self.branch_out(bad, slow);
        let weq = self.b.ins().icmp(IntCC::Equal, wa, wb);
        let beq = self.b.ins().icmp(IntCC::Equal, ba, bb);
        let r = self.b.ins().select(is_bool, beq, weq);
        let r = self.b.ins().select(unit, same_tag, r);
        let r = self.b.ins().band(r, same_tag);
        let r = if arg == 1 {
            self.b.ins().bxor_imm(r, 1)
        } else {
            r
        };
        self.push(Item::Bool(r));
        self.set_last(pc);
        true
    }

    fn for_iter(&mut self, pc: usize, arg: u32) -> bool {
        let before = self.last_pc;
        self.flush();
        // (A switch to a generator, or a raise, leaves `st.last` as the
        // instruction before.)
        self.store_last();
        let it_slot = self.depth - 1;
        let it = self.slot_addr(it_slot);
        let out_slot = self.slot_addr(self.depth);
        let pcv = self.b.ins().iconst(types::I64, pc as i64);
        let r = self
            .call(
                h_for_iter as *const () as usize,
                &[self.st, it, out_slot, pcv],
                true,
            )
            .expect("returns");
        let int_b = self.b.create_block();
        let other = self.b.create_block();
        self.b.ins().brif(r, other, &[], int_b, &[]);
        // An int: unboxed.
        self.b.switch_to_block(int_b);
        let v = self.b.ins().load(types::I64, FLAGS, self.st, S_OUT);
        let depth = self.depth;
        self.push(Item::Int(v));
        self.set_last(pc);
        self.goto(pc + 1);
        self.depth = depth;
        // Another value, on the stack; or the end; or the core loop's.
        self.b.switch_to_block(other);
        let pushed = self.b.ins().icmp_imm(IntCC::Equal, r, 1);
        let pushed_b = self.b.create_block();
        let rest = self.b.create_block();
        self.b.ins().brif(pushed, pushed_b, &[], rest, &[]);
        self.b.switch_to_block(pushed_b);
        self.depth += 1;
        self.set_last(pc);
        self.goto(pc + 1);
        self.depth = depth;
        self.b.switch_to_block(rest);
        let retired = self.b.ins().icmp_imm(IntCC::Equal, r, 2);
        let ret_b = self.b.create_block();
        let decl = self.b.create_block();
        self.b.ins().brif(retired, ret_b, &[], decl, &[]);
        self.b.switch_to_block(ret_b);
        // Past the loop's `END_FOR` / `POP_ITER` pair.
        let mut to = pc + 1 + arg as usize;
        let op = |s: &Self, pc: usize| s.code.instructions.get(pc).map(|i| i.op);
        if op(self, to) == Some(OpCode::EndFor) {
            to += 1;
            if matches!(op(self, to), Some(OpCode::PopIter | OpCode::PopTop)) {
                to += 1;
            }
        }
        self.depth -= 1;
        self.set_last(pc);
        self.goto(to);
        self.depth = depth;
        // Switched to a generator, or raised: the core loop takes it from
        // the state; anything else is the core loop's.
        self.b.switch_to_block(decl);
        let switched = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, r, i64::from(FOR_SWITCHED));
        let raised = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, r, i64::from(FOR_RAISED));
        let leave = self.b.create_block();
        let declined = self.b.create_block();
        let either = self.b.ins().bor(switched, raised);
        self.b.ins().brif(either, leave, &[], declined, &[]);
        self.b.switch_to_block(leave);
        let reload = self.b.ins().iconst(types::I32, i64::from(RELOAD));
        let raise = self.b.ins().iconst(types::I32, i64::from(RAISED));
        let s = self.b.ins().select(switched, reload, raise);
        self.leave(s);
        self.b.switch_to_block(declined);
        self.last_pc = before;
        self.exit(INTERP, pc);
        false
    }

    /// `x.attr` of a local's or a stack value: the site's field shortcut
    /// in line, then the core loop's cached reads through
    /// [`h_local_attr`] / [`h_stack_attr`]. The value goes onto the stack
    /// (a scalar copied, anything else with a reference taken); a stack
    /// receiver is released after. A miss is the core loop's.
    fn load_attr(&mut self, pc: usize, _arg: u32) -> bool {
        // A property's getter may run here (see `h_property`), which needs
        // the whole stack on the frame's.
        if crate::property_site(self.ext, pc) {
            self.flush();
        }
        let item = self.pop();
        let at = match item {
            Item::Local(i) => self.local_addr(i),
            Item::Mem(s) => self.slot_addr(s),
            _ => {
                self.push(item);
                self.exit(INTERP, pc);
                return false;
            }
        };
        let exit = self.exit_with(pc, &[item], INTERP);
        let slot = self.depth;
        let dst = self.slot_addr(slot);
        let miss = self.b.create_block();
        let done = self.b.create_block();
        // (A site with no field shortcut yet goes straight to the helper.)
        match (self.layout, self.field_slots.get(pc)) {
            (Some(l), Some(cache_slot)) if cache_slot.get().0 != 0 => {
                let l = *l;
                let otag = self.tag_at(at);
                let not_inst =
                    self.b
                        .ins()
                        .icmp_imm(IntCC::NotEqual, otag, i64::from(self.tags.instance));
                self.branch_out(not_inst, miss);
                let ptr = self.ptr;
                let inst = self.b.ins().load(ptr, FLAGS, at, 8);
                let cls = self.b.ins().load(ptr, FLAGS, inst, l.inst_class);
                let ver = self
                    .b
                    .ins()
                    .load(types::I64, FLAGS, cls, l.type_attr_version);
                let cache = self
                    .b
                    .ins()
                    .iconst(ptr, std::ptr::from_ref(cache_slot).cast::<u8>() as i64);
                let want = self.b.ins().load(types::I64, FLAGS, cache, FIELD_VER);
                let idx = self.b.ins().uload32(FLAGS, cache, FIELD_IDX);
                let lazy = self.b.ins().load(ptr, FLAGS, inst, l.inst_dict_lazy);
                let flag = self.b.ins().iconst(ptr, l.cells_unguarded as i64);
                let shared = self.b.ins().uload8(types::I64, FLAGS, flag, 0);
                let borrow = self.b.ins().sload32(FLAGS, inst, l.inst_split_borrow);
                let block = self.b.ins().load(ptr, FLAGS, inst, l.inst_split_block);
                let unset = self.b.ins().icmp_imm(IntCC::Equal, want, 0);
                let stale = self.b.ins().icmp(IntCC::NotEqual, ver, want);
                let busy = self.b.ins().icmp_imm(IntCC::SignedLessThan, borrow, 0);
                let empty = self.b.ins().icmp_imm(IntCC::Equal, block, 0);
                let other = self.b.ins().bor(lazy, shared);
                let other = self.b.ins().icmp_imm(IntCC::NotEqual, other, 0);
                let bad = self.b.ins().bor(unset, stale);
                let bad = self.b.ins().bor(bad, busy);
                let bad = self.b.ins().bor(bad, empty);
                let bad = self.b.ins().bor(bad, other);
                self.branch_out(bad, miss);
                let keys = self.b.ins().load(ptr, FLAGS, block, l.split_keys);
                let ckeys = self.b.ins().load(ptr, FLAGS, cls, l.type_shared_keys);
                let len = self.b.ins().uload32(FLAGS, block, l.split_len);
                let foreign = self.b.ins().icmp(IntCC::NotEqual, keys, ckeys);
                let absent = self
                    .b
                    .ins()
                    .icmp(IntCC::UnsignedGreaterThanOrEqual, idx, len);
                let bad = self.b.ins().bor(foreign, absent);
                self.branch_out(bad, miss);
                let off = self.b.ins().ishl_imm(idx, 4);
                let v = self.b.ins().iadd(block, off);
                let v = self.b.ins().iadd_imm(v, i64::from(l.split_values));
                match item {
                    // The receiver is the local's: the value just takes a
                    // copy.
                    Item::Local(_) => {
                        self.copy_value(dst, v);
                        self.b.ins().jump(done, &[]);
                    }
                    _ => {
                        // The receiver is the stack's: the value goes above
                        // it (copied) until the receiver is released, then
                        // down.
                        let above = self.slot_addr(slot + 1);
                        self.copy_value(above, v);
                        let r = self
                            .call(h_pop as *const () as usize, &[dst], true)
                            .expect("returns");
                        let undo = self.b.create_block();
                        let released = self.b.create_block();
                        self.b.ins().brif(r, undo, &[], released, &[]);
                        // Declined: the copy goes, and the core loop takes
                        // the instruction from the start.
                        self.b.switch_to_block(undo);
                        // (A fresh copy has another owner: its release is a
                        // plain decrement.)
                        self.call(h_pop as *const () as usize, &[above], true);
                        self.b.ins().jump(exit, &[]);
                        self.b.switch_to_block(released);
                        self.copy16(dst, above);
                        self.b.ins().jump(done, &[]);
                    }
                }
            }
            _ => {
                self.b.ins().jump(miss, &[]);
            }
        }
        // The core loop's cached reads.
        self.b.switch_to_block(miss);
        let code = self.code_ptr();
        let ext = self.ext_ptr();
        let pcv = self.b.ins().iconst(types::I64, pc as i64);
        let cache: Box<SlotCache> = Box::default();
        let c = self
            .b
            .ins()
            .iconst(self.ptr, std::ptr::from_ref::<SlotCache>(&cache) as i64);
        self.slot_caches.push(cache);
        let r = match item {
            Item::Local(_) => self.call(
                h_local_attr as *const () as usize,
                &[dst, code, ext, pcv, at, c],
                true,
            ),
            _ => self.call(
                h_stack_attr as *const () as usize,
                &[code, ext, pcv, at, c],
                true,
            ),
        }
        .expect("returns");
        // A property whose getter the site has cached runs directly, when
        // nothing virtual lies below (its call can raise or switch).
        if self.vs.iter().all(|i| matches!(i, Item::Mem(_))) {
            let prop = self.b.create_block();
            self.b.ins().brif(r, prop, &[], done, &[]);
            self.b.switch_to_block(prop);
            let site: Box<DirectSite> = Box::default();
            let site_at = self
                .b
                .ins()
                .iconst(self.ptr, std::ptr::from_ref::<DirectSite>(&site) as i64);
            self.direct_sites.push(site);
            let code = self.code_ptr();
            let slotv = self.b.ins().iconst(types::I64, slot as i64);
            let local = self
                .b
                .ins()
                .iconst(types::I64, i64::from(matches!(item, Item::Local(_))));
            let r = self
                .call(
                    h_property as *const () as usize,
                    &[self.st, code, pcv, slotv, at, local, site_at],
                    true,
                )
                .expect("returns");
            let other = self.b.create_block();
            self.b.ins().brif(r, other, &[], done, &[]);
            self.b.switch_to_block(other);
            let declined = self
                .b
                .ins()
                .icmp_imm(IntCC::Equal, r, i64::from(CALL_DECLINED));
            let leave = self.b.create_block();
            self.b.ins().brif(declined, exit, &[], leave, &[]);
            self.b.switch_to_block(leave);
            self.leave(r);
        } else {
            self.b.ins().brif(r, exit, &[], done, &[]);
        }
        self.b.switch_to_block(done);
        self.push(Item::Mem(slot));
        self.set_last(pc);
        if matches!(item, Item::Mem(_)) {
            self.check_released(pc + 1);
        }
        true
    }

    fn code_ptr(&mut self) -> Value {
        self.b
            .ins()
            .iconst(self.ptr, std::ptr::from_ref(self.code) as i64)
    }

    fn ext_ptr(&mut self) -> Value {
        self.b
            .ins()
            .iconst(self.ptr, std::ptr::from_ref(self.ext) as i64)
    }

    /// Make the top of the virtual stack a value on the frame's stack: its
    /// slot.
    fn top_to_mem(&mut self) -> usize {
        let item = self.pop();
        let s = self.depth;
        if !matches!(item, Item::Mem(_)) {
            self.materialize(item, s);
        }
        self.push(Item::Mem(s));
        s
    }

    /// `LOAD_GLOBAL`: the site's cached value in line while the frame's
    /// namespaces and their stamps match it, anything else through
    /// [`h_load_global`] (which fills the cache); a fused shape continues
    /// at whatever follows it (through the dispatch).
    fn load_global(&mut self, pc: usize) -> bool {
        let next = self.code.instructions.get(pc + 1).map(|i| i.op);
        let may_fuse = matches!(
            next,
            Some(OpCode::PushNull | OpCode::LoadMethodAttr | OpCode::LoadAttr)
        );
        // A fused shape leaves through the dispatch, which finds the whole
        // stack on the frame's.
        if may_fuse {
            self.flush();
        }
        let s = self.depth;
        let dst = self.slot_addr(s);
        let cache: Box<GlobalCache> = Box::default();
        let cache_at = std::ptr::from_ref::<GlobalCache>(&cache) as i64;
        self.global_caches.push(cache);
        let helper = self.b.create_block();
        let done = self.b.create_block();
        // (In line even in a lean body: the cached read is a few loads
        // and compares, the helper's a full call.)
        {
            let ptr = self.ptr;
            let c = self.b.ins().iconst(ptr, cache_at);
            let frame = self.b.ins().load(ptr, FLAGS, self.st, S_FRAME);
            let g = self
                .b
                .ins()
                .load(types::I64, FLAGS, frame, F_GLOBALS as i32);
            let cg = self.b.ins().load(types::I64, FLAGS, c, GC_GLOBALS);
            let gd = self.b.ins().load(ptr, FLAGS, c, GC_GDATA);
            let other = self.b.ins().icmp(IntCC::NotEqual, g, cg);
            self.branch_out(other, helper);
            let gs = self.b.ins().load(types::I64, FLAGS, gd, DICT_STAMP);
            let cgs = self.b.ins().load(types::I64, FLAGS, c, GC_GSTAMP);
            // (A frame with rare state, a custom builtins mapping among
            // it, takes the helper.)
            let rare = self.b.ins().load(types::I64, FLAGS, frame, F_RARE as i32);
            let moved = self.b.ins().icmp(IntCC::NotEqual, gs, cgs);
            let custom = self.b.ins().icmp_imm(IntCC::NotEqual, rare, 0);
            let bad = self.b.ins().bor(moved, custom);
            self.branch_out(bad, helper);
            let cb = self.b.ins().load(types::I64, FLAGS, c, GC_BUILTINS);
            let builtin = self.b.create_block();
            let read = self.b.create_block();
            self.b.ins().brif(cb, builtin, &[], read, &[]);
            // A builtin: the builtins too, and no globals `__missing__`.
            self.b.switch_to_block(builtin);
            let b = self
                .b
                .ins()
                .load(types::I64, FLAGS, frame, F_BUILTINS as i32);
            let bd = self.b.ins().load(ptr, FLAGS, c, GC_BDATA);
            let other = self.b.ins().icmp(IntCC::NotEqual, b, cb);
            self.branch_out(other, helper);
            let bs = self.b.ins().load(types::I64, FLAGS, bd, DICT_STAMP);
            let cbs = self.b.ins().load(types::I64, FLAGS, c, GC_BSTAMP);
            let interp = self.b.ins().load(ptr, FLAGS, self.st, S_INTERP);
            let missing = self
                .b
                .ins()
                .uload8(types::I64, FLAGS, interp, I_MISSING as i32);
            let moved = self.b.ins().icmp(IntCC::NotEqual, bs, cbs);
            let missing = self.b.ins().icmp_imm(IntCC::NotEqual, missing, 0);
            let bad = self.b.ins().bor(moved, missing);
            self.branch_out(bad, helper);
            self.b.ins().jump(read, &[]);
            self.b.switch_to_block(read);
            let v = self.b.ins().load(ptr, FLAGS, c, GC_VALUE);
            self.copy_value(dst, v);
            self.b.ins().jump(done, &[]);
        }
        self.b.switch_to_block(helper);
        let code = self.code_ptr();
        let ext = self.ext_ptr();
        let pcv = self.b.ins().iconst(types::I64, pc as i64);
        let nl = self.b.ins().iconst(types::I64, self.nlocals as i64);
        let len = self.b.ins().iconst(types::I64, self.depth as i64);
        let c = self.b.ins().iconst(self.ptr, cache_at);
        let r = self
            .call(
                h_load_global as *const () as usize,
                &[self.st, code, ext, pcv, nl, len, c],
                true,
            )
            .expect("returns");
        let out = self.exit_with(pc, &[], INTERP);
        if may_fuse {
            let other = self.b.create_block();
            self.b.ins().brif(r, other, &[], done, &[]);
            self.b.switch_to_block(other);
            let fused = self.b.ins().icmp_imm(IntCC::Equal, r, i64::from(FUSED));
            let fb = self.b.create_block();
            self.b.ins().brif(fused, fb, &[], out, &[]);
            // Fused: the state holds the pc, depth and last instruction
            // after it.
            self.b.switch_to_block(fb);
            self.check_released_dynamic();
            self.b.ins().jump(self.dispatch, &[]);
        } else {
            self.b.ins().brif(r, out, &[], done, &[]);
        }
        self.b.switch_to_block(done);
        self.push(Item::Mem(s));
        self.set_last(pc);
        true
    }

    /// `LOAD_ATTR` with the method flag through [`h_load_method`]: the
    /// method and its self slot in place of the receiver.
    fn load_method(&mut self, pc: usize) -> bool {
        if self.depth == 0 {
            self.exit(INTERP, pc);
            return false;
        }
        self.flush();
        let helper = self.b.create_block();
        let done = self.b.create_block();
        let fn_counted = self.tags.rc_counted & (1i64 << self.tags.function) != 0;
        let cache: Box<MethodCache> = Box::default();
        let cache_at = std::ptr::from_ref::<MethodCache>(&cache) as i64;
        self.method_caches.push(cache);
        match self.layout {
            // An instance of the class the site last saw, which holds no
            // dictionary and fewer values than the name's position among
            // its class's names: the cached function, the receiver above
            // it.
            Some(l) if fn_counted => {
                let l = *l;
                let ptr = self.ptr;
                let recv = self.slot_addr(self.depth - 1);
                let above = self.slot_addr(self.depth);
                let c = self.b.ins().iconst(ptr, cache_at);
                let tag = self.tag_at(recv);
                let not_inst =
                    self.b
                        .ins()
                        .icmp_imm(IntCC::NotEqual, tag, i64::from(self.tags.instance));
                self.branch_out(not_inst, helper);
                let inst = self.b.ins().load(ptr, FLAGS, recv, 8);
                let cls = self.b.ins().load(ptr, FLAGS, inst, l.inst_class);
                let ver = self
                    .b
                    .ins()
                    .load(types::I64, FLAGS, cls, l.type_attr_version);
                let want = self.b.ins().load(types::I64, FLAGS, c, MC_VER);
                let lazy = self.b.ins().load(ptr, FLAGS, inst, l.inst_dict_lazy);
                let flag = self.b.ins().iconst(ptr, l.cells_unguarded as i64);
                let unguarded = self.b.ins().uload8(types::I64, FLAGS, flag, 0);
                let rflag = self.b.ins().iconst(
                    ptr,
                    std::ptr::from_ref(crate::sync::rc_shared_flag()) as i64,
                );
                let rshared = self.b.ins().uload8(types::I64, FLAGS, rflag, 0);
                let borrow = self.b.ins().sload32(FLAGS, inst, l.inst_split_borrow);
                let unset = self.b.ins().icmp_imm(IntCC::Equal, want, 0);
                let stale = self.b.ins().icmp(IntCC::NotEqual, ver, want);
                let busy = self.b.ins().icmp_imm(IntCC::SignedLessThan, borrow, 0);
                let other = self.b.ins().bor(lazy, unguarded);
                let other = self.b.ins().bor(other, rshared);
                let other = self.b.ins().icmp_imm(IntCC::NotEqual, other, 0);
                let bad = self.b.ins().bor(unset, stale);
                let bad = self.b.ins().bor(bad, busy);
                let bad = self.b.ins().bor(bad, other);
                self.branch_out(bad, helper);
                let block = self.b.ins().load(ptr, FLAGS, inst, l.inst_split_block);
                let check = self.b.create_block();
                let hit = self.b.create_block();
                self.b.ins().brif(block, check, &[], hit, &[]);
                self.b.switch_to_block(check);
                let n = self.b.ins().uload32(FLAGS, block, l.split_len);
                let keys = self.b.ins().load(types::I64, FLAGS, block, l.split_keys);
                let ckeys = self.b.ins().load(types::I64, FLAGS, c, MC_KEYS);
                let before = self.b.ins().uload32(FLAGS, c, MC_BEFORE);
                let empty = self.b.ins().icmp_imm(IntCC::Equal, n, 0);
                let same = self.b.ins().icmp(IntCC::Equal, keys, ckeys);
                let short = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, n, before);
                let held = self.b.ins().band(same, short);
                let ok = self.b.ins().bor(empty, held);
                self.b.ins().brif(ok, hit, &[], helper, &[]);
                self.b.switch_to_block(hit);
                // The function's count, then the receiver up and the
                // function in its place.
                let f = self.b.ins().load(types::I64, FLAGS, c, MC_FUNC);
                let count = self.b.ins().load(types::I64, FLAGS, f, 0);
                let count = self.b.ins().iadd_imm(count, 1);
                self.b.ins().store(FLAGS, count, f, 0);
                self.copy16(above, recv);
                self.write_tag(recv, self.tags.function);
                self.b.ins().store(FLAGS, f, recv, 8);
                self.b.ins().jump(done, &[]);
            }
            _ => {
                self.b.ins().jump(helper, &[]);
            }
        }
        self.b.switch_to_block(helper);
        let code = self.code_ptr();
        let ext = self.ext_ptr();
        let pcv = self.b.ins().iconst(types::I64, pc as i64);
        let len = self.b.ins().iconst(types::I64, self.depth as i64);
        let c = self.b.ins().iconst(self.ptr, cache_at);
        let mcache: Box<ModCache> = Box::default();
        let mc = self
            .b
            .ins()
            .iconst(self.ptr, std::ptr::from_ref::<ModCache>(&mcache) as i64);
        self.mod_caches.push(mcache);
        let r = self
            .call(
                h_load_method as *const () as usize,
                &[self.st, code, ext, pcv, len, c, mc],
                true,
            )
            .expect("returns");
        let out = self.exit_with(pc, &[], INTERP);
        self.branch_out(r, out);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
        self.depth += 1;
        self.set_last(pc);
        self.check_released(pc + 1);
        true
    }

    /// `STORE_ATTR` of a stack receiver through [`h_stack_store_attr`].
    fn stack_store_attr(&mut self, pc: usize) -> bool {
        if self.depth < 2 {
            self.exit(INTERP, pc);
            return false;
        }
        self.flush();
        let code = self.code_ptr();
        let pcv = self.b.ins().iconst(types::I64, pc as i64);
        let at = self.slot_addr(self.depth - 2);
        let r = self
            .call(
                h_stack_store_attr as *const () as usize,
                &[code, pcv, at],
                true,
            )
            .expect("returns");
        let out = self.exit_with(pc, &[], INTERP);
        self.branch_out(r, out);
        self.depth -= 2;
        self.set_last(pc);
        self.check_released(pc + 1);
        true
    }

    /// `TO_BOOL`: a scalar's truth in line, any other value's through
    /// [`h_truth`].
    fn truth_of(&mut self, pc: usize) -> bool {
        let item = self.pop();
        let o = self.operand(item);
        if o.is_some_and(|o| o.kind == Kind::Bool) {
            self.push(item);
            self.set_last(pc);
            return true;
        }
        let other = self.b.create_block();
        let done = self.b.create_block();
        self.b.append_block_param(done, types::I8);
        match o {
            Some(o) => {
                let t = self.truth(o, true, other);
                self.b.ins().jump(done, &[t.into()]);
            }
            None => {
                self.b.ins().jump(other, &[]);
            }
        }
        // Any other value through the helper (the operand onto the stack).
        self.b.switch_to_block(other);
        let slot = self.depth;
        let cold = std::mem::replace(&mut self.cold, true);
        self.materialize(item, slot);
        self.cold = cold;
        let at = self.slot_addr(slot);
        let r = self
            .call(h_truth as *const () as usize, &[self.st, at], true)
            .expect("returns");
        let declined = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThan, r, 1);
        let out = self.exit_with(pc, &[Item::Mem(slot)], INTERP);
        self.branch_out(declined, out);
        let saved = self.last_pc.replace(pc);
        let marked = self.exit_with(pc + 1, &[Item::Mem(slot)], MARKED);
        self.last_pc = saved;
        let f = self.dead_flag();
        let marked_b = self.b.create_block();
        let fine = self.b.create_block();
        self.b.ins().brif(f, marked_b, &[], fine, &[]);
        // (The result goes onto the stack for the core loop.)
        self.b.switch_to_block(marked_b);
        let dst = self.slot_addr(slot);
        self.write_tag(dst, self.tags.boolean);
        let r8 = self.b.ins().ireduce(types::I8, r);
        self.b.ins().store(FLAGS, r8, dst, 1);
        self.b.ins().jump(marked, &[]);
        self.b.switch_to_block(fine);
        let t = self.b.ins().icmp_imm(IntCC::NotEqual, r, 0);
        self.b.ins().jump(done, &[t.into()]);
        self.b.switch_to_block(done);
        let t = self.b.block_params(done)[0];
        self.push(Item::Bool(t));
        self.set_last(pc);
        true
    }

    /// `CALL` through [`h_call`]: a callee that runs in place leaves its
    /// result on the stack; anything else is the core loop's (the next
    /// instruction starts a block, where the native code resumes once
    /// the callee returns).
    fn call_op(&mut self, pc: usize, argc: usize) -> bool {
        if self.depth < argc + 2 {
            self.exit(INTERP, pc);
            return false;
        }
        self.flush();
        // (A raise leaves `st.last` as the instruction before.)
        self.store_last();
        let code = self.code_ptr();
        let ext = self.ext_ptr();
        let pcv = self.b.ins().iconst(types::I64, pc as i64);
        let len = self.b.ins().iconst(types::I64, self.depth as i64);
        // A Python function's call tries a direct call first, unless its
        // site found the callee without a native body and has retries
        // left to count down.
        let site: Box<DirectSite> = Box::default();
        let site_at = self
            .b
            .ins()
            .iconst(self.ptr, std::ptr::from_ref::<DirectSite>(&site) as i64);
        self.direct_sites.push(site);
        let callee = self.slot_addr(self.depth - argc - 2);
        let tag = self.tag_at(callee);
        let is_fn = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, tag, i64::from(self.tags.function));
        let direct = self.b.create_block();
        let general = self.b.create_block();
        let called = self.b.create_block();
        let fresh = self.b.create_block();
        let waiting = self.b.create_block();
        let counted = self.b.create_block();
        self.b.append_block_param(called, types::I32);
        self.b.ins().brif(is_fn, fresh, &[], general, &[]);
        self.b.switch_to_block(fresh);
        let native = self.b.ins().load(types::I64, FLAGS, site_at, DS_NATIVE);
        self.b.ins().brif(native, direct, &[], waiting, &[]);
        self.b.switch_to_block(waiting);
        let retry = self.b.ins().load(types::I32, FLAGS, site_at, DS_RETRY);
        self.b.ins().brif(retry, counted, &[], direct, &[]);
        self.b.switch_to_block(counted);
        let left = self.b.ins().iadd_imm(retry, -1);
        self.b.ins().store(FLAGS, left, site_at, DS_RETRY);
        self.b.ins().jump(general, &[]);
        self.b.switch_to_block(direct);
        let r = self
            .call(
                h_call_direct as *const () as usize,
                &[self.st, code, ext, pcv, len, site_at],
                true,
            )
            .expect("returns");
        self.b.ins().jump(called, &[r.into()]);
        self.b.switch_to_block(general);
        // (A site settled on a leaf builtin tries it first, in a smaller
        // helper.)
        let leaf = crate::call_site_leaf(self.ext, pc).is_some();
        let helper = if leaf {
            h_call_leaf as *const () as usize
        } else {
            h_call as *const () as usize
        };
        let r = self
            .call(helper, &[self.st, code, ext, pcv, len], true)
            .expect("returns");
        self.b.ins().jump(called, &[r.into()]);
        self.b.switch_to_block(called);
        let r = self.b.block_params(called)[0];
        let out = self.exit_with(pc, &[], INTERP);
        let ran = self.b.create_block();
        let other = self.b.create_block();
        self.b.ins().brif(r, other, &[], ran, &[]);
        // Raised or switched: the core loop takes it from the state.
        self.b.switch_to_block(other);
        let declined = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, r, i64::from(CALL_DECLINED));
        let leave = self.b.create_block();
        self.b.ins().brif(declined, out, &[], leave, &[]);
        self.b.switch_to_block(leave);
        self.leave(r);
        self.b.switch_to_block(ran);
        self.depth -= argc + 1;
        self.set_last(pc);
        self.check_released(pc + 1);
        true
    }

    /// `CALL_KW` through [`h_call_kw`] (as [`Self::call_op`]; the depth
    /// after is the static one).
    fn call_kw(&mut self, pc: usize) -> bool {
        self.call_with(pc, h_call_kw as *const () as usize)
    }

    /// `CALL_FUNCTION_EX` through [`h_call_ex`] (as [`Self::call_kw`]).
    fn call_ex(&mut self, pc: usize) -> bool {
        self.call_with(pc, h_call_ex as *const () as usize)
    }

    /// A call instruction through `helper` (`h_call_kw`'s signature): the
    /// result in place when it ran, anything else the core loop's.
    fn call_with(&mut self, pc: usize, helper: usize) -> bool {
        let after = self.depths.get(pc + 1).copied().unwrap_or(-1);
        if after < 0 || self.depth == 0 {
            self.exit(INTERP, pc);
            return false;
        }
        self.flush();
        self.store_last();
        let code = self.code_ptr();
        let pcv = self.b.ins().iconst(types::I64, pc as i64);
        let len = self.b.ins().iconst(types::I64, self.depth as i64);
        let r = self
            .call(helper, &[self.st, code, pcv, len], true)
            .expect("returns");
        let out = self.exit_with(pc, &[], INTERP);
        let ran = self.b.create_block();
        let other = self.b.create_block();
        self.b.ins().brif(r, other, &[], ran, &[]);
        self.b.switch_to_block(other);
        let declined = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, r, i64::from(CALL_DECLINED));
        let leave = self.b.create_block();
        self.b.ins().brif(declined, out, &[], leave, &[]);
        self.b.switch_to_block(leave);
        self.leave(r);
        self.b.switch_to_block(ran);
        self.depth = after as usize;
        self.set_last(pc);
        self.check_released(pc + 1);
        true
    }

    /// `BUILD_TUPLE n` / `BUILD_LIST n` through [`h_build`].
    fn build(&mut self, pc: usize, n: usize, list: bool) -> bool {
        if n > self.depth {
            self.exit(INTERP, pc);
            return false;
        }
        self.flush();
        let at = self.slot_addr(self.depth - n);
        let nv = self.b.ins().iconst(types::I64, n as i64);
        let lv = self.b.ins().iconst(types::I32, i64::from(list));
        let r = self
            .call(h_build as *const () as usize, &[self.st, at, nv, lv], true)
            .expect("returns");
        let out = self.exit_with(pc, &[], INTERP);
        self.branch_out(r, out);
        self.depth = self.depth - n + 1;
        self.set_last(pc);
        true
    }

    /// `BUILD_MAP` of `n` pairs through [`h_build_map`].
    fn build_map(&mut self, pc: usize, n: usize) -> bool {
        if 2 * n > self.depth {
            self.exit(INTERP, pc);
            return false;
        }
        self.flush();
        let at = self.slot_addr(self.depth - 2 * n);
        let nv = self.b.ins().iconst(types::I64, n as i64);
        let r = self
            .call(h_build_map as *const () as usize, &[at, nv], true)
            .expect("returns");
        let out = self.exit_with(pc, &[], INTERP);
        self.branch_out(r, out);
        self.depth = self.depth - 2 * n + 1;
        self.set_last(pc);
        true
    }

    /// `UNARY_OP` of a scalar (the leaf arm's shapes): `not` of a `bool`,
    /// `int` or `None`; `-`, `+` of an `int` or `float`; `~` of an `int`.
    fn unary(&mut self, pc: usize, arg: u32) -> bool {
        let item = self.pop();
        let Some(o) = self.operand(item) else {
            self.push(item);
            self.exit(INTERP, pc);
            return false;
        };
        let slow = self.exit_with(pc, &[item], INTERP);
        let (pos, neg, not) = (0, 1, 2);
        if arg == not {
            let t = self.truth(o, false, slow);
            let r = self.b.ins().bxor_imm(t, 1);
            self.push(Item::Bool(r));
            self.set_last(pc);
            return true;
        }
        let int_r = |s: &mut Self, x: Value| -> Value {
            if arg == neg {
                let min = s.b.ins().icmp_imm(IntCC::Equal, x, i64::MIN);
                s.branch_out(min, slow);
                s.b.ins().ineg(x)
            } else if arg == pos {
                x
            } else {
                s.b.ins().bnot(x)
            }
        };
        let float_r = |s: &mut Self, bits: Value| -> Value {
            let x = s.b.ins().bitcast(types::F64, MemFlags::new(), bits);
            if arg == neg {
                s.b.ins().fneg(x)
            } else {
                x
            }
        };
        let floats = arg == neg || arg == pos;
        match o.kind {
            Kind::Int => {
                let r = int_r(self, o.word);
                self.push(Item::Int(r));
            }
            Kind::Float if floats => {
                let r = float_r(self, o.word);
                self.push(Item::Float(r));
            }
            Kind::Unknown => {
                let t = o.tag.expect("unknown kinds carry a tag");
                let is_int = self
                    .b
                    .ins()
                    .icmp_imm(IntCC::Equal, t, i64::from(self.tags.int));
                let done = self.b.create_block();
                self.b.append_block_param(done, types::I64);
                self.b.append_block_param(done, types::I64);
                let int_b = self.b.create_block();
                let not_int = self.b.create_block();
                self.b.ins().brif(is_int, int_b, &[], not_int, &[]);
                self.b.switch_to_block(int_b);
                let r = int_r(self, o.word);
                let it = self.b.ins().iconst(types::I64, i64::from(self.tags.int));
                self.b.ins().jump(done, &[it.into(), r.into()]);
                self.b.switch_to_block(not_int);
                if floats {
                    let is_float =
                        self.b
                            .ins()
                            .icmp_imm(IntCC::Equal, t, i64::from(self.tags.float));
                    let bad = self.b.ins().bxor_imm(is_float, 1);
                    self.branch_out(bad, slow);
                    let r = float_r(self, o.word);
                    let bits = self.b.ins().bitcast(types::I64, MemFlags::new(), r);
                    let ft = self.b.ins().iconst(types::I64, i64::from(self.tags.float));
                    self.b.ins().jump(done, &[ft.into(), bits.into()]);
                } else {
                    self.b.ins().jump(slow, &[]);
                }
                self.b.switch_to_block(done);
                let (t, w) = (self.b.block_params(done)[0], self.b.block_params(done)[1]);
                let zero = self.b.ins().iconst(types::I64, 0);
                self.push(Item::Dyn(t, w, zero));
            }
            _ => {
                self.b.ins().jump(slow, &[]);
                return false;
            }
        }
        self.set_last(pc);
        true
    }
}

/// Where a field cache's version and position sit.
const FIELD_VER: i32 = std::mem::offset_of!((u64, u32), 0) as i32;
const FIELD_IDX: i32 = std::mem::offset_of!((u64, u32), 1) as i32;
