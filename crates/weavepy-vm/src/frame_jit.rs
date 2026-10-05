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
//! An instruction the native code doesn't take, and any shape an in-line
//! one doesn't settle (an overflow, a heap operand, a cache miss), returns
//! to the core loop with the frame exactly as the loop would have it
//! before that instruction: the loop runs it, and enters the native code
//! again at the next block start it reaches. The core loop's fused shapes
//! (a local receiver's method call or attribute store) start at their
//! `LOAD_FAST`, so the native code hands those over at it.
//!
//! Compiling costs far more than interpreting a few iterations, and every
//! hand-over to the core loop costs a round trip, so a code object
//! compiles only once a loop of it is hot and that loop runs mostly in
//! native code (see `worth_compiling`); a loop of calls and global loads
//! stays with the core loop. Code a tier-2 loop may still take waits for
//! tier 2 to settle first.
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
use crate::{CodeConstObjects, FieldSlot, Interpreter};

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

/// Activations and back edges before a code object is compiled
/// (`WEAVEPY_FRAME_JIT_HOT` overrides it, a tuning aid).
fn hot() -> u32 {
    static HOT: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *HOT.get_or_init(|| {
        std::env::var("WEAVEPY_FRAME_JIT_HOT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000)
    })
}

/// How many times [`hot`] a code object's heat must reach: compiling
/// costs about the same for every instruction of the code, while what it
/// saves comes from the loop that got hot alone, so a long body (a
/// driver's setup around a short loop) waits longer to earn its compile.
#[inline(always)]
fn size_factor(code: &CodeObject) -> u32 {
    (code.instructions.len() / SIZE_UNIT).max(1) as u32
}

/// The code length [`hot`] alone covers.
const SIZE_UNIT: usize = 16;

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
const S_FRAME: i32 = std::mem::offset_of!(State, frame) as i32;
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
    /// The `LOAD_GLOBAL` sites' caches, which the code addresses.
    #[allow(clippy::vec_box)]
    _globals: Vec<Box<GlobalCache>>,
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
            stats::note(&self.name, &self.ops, from, st.pc, status, bail);
        }
        status
    }
}

/// A code object's compilation state, kept in its extension.
#[derive(Default)]
pub(crate) struct Slot {
    native: std::sync::OnceLock<Option<Box<Native>>>,
    heat: AtomicU32,
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
        // (A racing thread losing a count is harmless.)
        let h = self.heat.load(Ordering::Relaxed) + 1;
        self.heat.store(h, Ordering::Relaxed);
        let mut need = hot().saturating_mul(size_factor(code));
        if matches!(at, Heat::Call) && code.jit_hint.loop_free(code) {
            need = need.saturating_mul(tuning().call_factor);
        }
        if h >= need {
            self.heat.store(0, Ordering::Relaxed);
            self.try_compile(code, ext, nlocals, at);
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
/// little from compiling, and a body's entry costs about what a few
/// natively run instructions save.
fn worth_compiling(code: &CodeObject, ext: &CodeConstObjects, at: Heat) -> bool {
    use weavepy_compiler::OpCode;
    let ins = &code.instructions;
    // A method call on a local runs through a helper without leaving, when
    // its callee runs in place.
    let mut helped = vec![false; ins.len()];
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
    let t = tuning();
    // What running an instruction natively saves, in halves of an
    // in-line instruction's: one run in line saves its whole dispatch and
    // operand traffic, one run through a helper (as the core loop's arm
    // runs it) about half of that, and one handed to the core loop costs
    // a round trip.
    let gain = |k: usize| -> Option<usize> {
        let op = ins[k].op;
        if helped[k] {
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
            | OpCode::LoadDeref
            | OpCode::GetIter
            | OpCode::BinarySubscr
            | OpCode::BinarySlice
            | OpCode::StoreSubscr
            | OpCode::ListAppend
            | OpCode::UnpackSequence
            | OpCode::ContainsOp => false,
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
        for k in top..=pc {
            match gain(k) {
                Some(g) => n += g,
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
    /// The natively run instructions a body needs beyond its exits' cost.
    body_min: usize,
    /// How many times [`hot`] a loop-free body's calls must reach.
    call_factor: u32,
}

fn tuning() -> Tuning {
    static T: std::sync::OnceLock<Tuning> = std::sync::OnceLock::new();
    *T.get_or_init(|| {
        let mut v = [8usize, 1, 3, 8, 4];
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
            call_factor: v[4] as u32,
        }
    })
}

/// The pcs a call runs through on its ordinary paths: from the entry
/// along fall-throughs and jumps (not into the exception handlers).
fn body_pcs(code: &CodeObject) -> Vec<usize> {
    let ins = &code.instructions;
    let n = ins.len();
    let mut seen = vec![false; n];
    let mut work = vec![0usize];
    let mut out = Vec::new();
    while let Some(pc) = work.pop() {
        if pc >= n || seen[pc] {
            continue;
        }
        seen[pc] = true;
        out.push(pc);
        let i = ins[pc];
        let next = pc + 1;
        match i.op {
            OpCode::ReturnValue | OpCode::RaiseVarargs | OpCode::Reraise => {}
            OpCode::JumpForward => work.push(next + i.arg as usize),
            OpCode::JumpBackward => work.push(next.saturating_sub(i.arg as usize)),
            OpCode::PopJumpIfFalse
            | OpCode::PopJumpIfTrue
            | OpCode::PopJumpIfNone
            | OpCode::PopJumpIfNotNone
            | OpCode::ForIter => {
                work.push(next + i.arg as usize);
                work.push(next);
            }
            _ => work.push(next),
        }
    }
    out
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

    #[cold]
    pub(super) fn note(
        name: &str,
        ops: &[OpCode],
        from: usize,
        at: usize,
        status: u32,
        bail: &str,
    ) {
        let op = match (status, ops.get(at)) {
            (0, Some(op)) => format!("{op:?}{bail}"),
            (0, None) => "<end>".to_owned(),
            (super::RELOAD, _) => "<switched>".to_owned(),
            (super::RAISED, _) => "<raised>".to_owned(),
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
    /// `Option::<Object>::None`'s first byte, when the option keeps
    /// `Object`'s size (its tag byte's niche).
    opt_none: Option<u8>,
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
        opt_none: {
            let none: Option<Object> = None;
            // SAFETY: reading the first byte of a 16-byte value.
            (std::mem::size_of::<Option<Object>>() == 16)
                .then(|| unsafe { *std::ptr::from_ref(&none).cast::<u8>() })
        },
        by_pointer: samples
            .iter()
            .fold(0, |m, &t| if t < 64 { m | (1i64 << t) } else { m }),
    };
    if samples.iter().any(|&t| t >= 64) {
        return None;
    }
    let all = [
        t.none, t.unbound, t.boolean, t.int, t.float, t.cell, t.instance,
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
/// stack), anything else declined.
unsafe extern "C" fn h_for_iter(st: *mut State, it: *mut Object, out: *mut Object) -> u32 {
    // SAFETY: the code passes its live state and an initialized slot with
    // a free one above it.
    let (st, top) = unsafe { (&mut *st, &*it) };
    let rc = match top {
        Object::Iter(rc) => rc,
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
                None if unique && owner.is_none() => {
                    // SAFETY: as above.
                    drop(unsafe { it.read() });
                    return 2;
                }
                None => return 3,
            }
        }
        PyIterator::Tuple { items, index } => {
            let Some(v) = items.get(*index).cloned() else {
                return 3;
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
            Object::new_tuple_array([Object::Int(i), x])
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

/// A container instruction (`BINARY_SUBSCR`, `BINARY_SLICE`, `STORE_SUBSCR`,
/// `LIST_APPEND`, `UNPACK_SEQUENCE`; the one at `ins`, the `pc`th of
/// `code`) on the top of a `len`-deep stack, as the core loop's arm runs
/// it: the new depth, or `u64::MAX` declined untouched.
unsafe extern "C" fn h_container(
    st: *mut State,
    ins: *const weavepy_compiler::Instruction,
    code: *const CodeObject,
    pc: u64,
    len: u64,
) -> u64 {
    // SAFETY: the code passes its live state, one of its own instructions
    // and its code, and its stack depth.
    unsafe {
        let st = &mut *st;
        let (ins, len) = (*ins, len as usize);
        if let Some(n) = Interpreter::core_container_op(ins, st.stack, len, st.cap) {
            return n as u64;
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

/// `BINARY_OP` of `kind` over the values at `at` and above it, for the
/// shapes the core loop's arm runs (two numbers; strings' concatenation,
/// repetition and `%` formatting; a natively served instance's operator):
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
            Object::List(l) => match l.try_borrow() {
                Ok(l) => !l.is_empty(),
                Err(_) => return 2,
            },
            Object::Tuple(t) => !t.is_empty(),
            Object::Dict(d) => match d.try_borrow() {
                Ok(d) => !d.is_empty(),
                Err(_) => return 2,
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
        if frame.builtins_obj.is_some() {
            return DECLINED;
        }
        let Some(slot) = ext.stamp_slots.get().and_then(|s| s.get(pc)) else {
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
                    .method_slots
                    .get()
                    .and_then(|s| s.get(pc + 1))
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
                    .stamp_slots
                    .get()
                    .and_then(|s| s.get(pc + 1))
                    .and_then(|s| crate::class_attr_hit(s, cls))
                {
                    return fused(st, c, pc + 1);
                }
            }
            // A value no fused shape takes: the native code reads it in
            // line while the dicts' stamps hold (a stamp changes with every
            // mutation, so the value stays where it is).
            _ => {
                let c = &mut *cache;
                let field = |off: usize| st.frame.cast::<u8>().add(off).cast::<u64>().read();
                c.value = 0;
                c.globals = field(F_GLOBALS);
                c.gstamp = g_stamp;
                c.gdata = gdict as u64;
                (c.builtins, c.bstamp, c.bdata) = if builtin {
                    (field(F_BUILTINS), b_stamp, bdict as u64)
                } else {
                    (0, 0, 0)
                };
                c.value = std::ptr::from_ref(v) as u64;
            }
        }
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
const F_BUILTINS_OBJ: usize = std::mem::offset_of!(crate::Frame, builtins_obj);
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
) -> u32 {
    // SAFETY: the code passes its live state, its own code and extension,
    // one of its method loads, and its stack depth (at least one, with
    // room for a push). Nothing here runs code.
    unsafe {
        let st = &mut *st;
        let (code, ext, pc, len) = (&*code, &*ext, pc as usize, len as usize);
        let Some(ms) = ext.method_slots.get().and_then(|s| s.get(pc)) else {
            return 1;
        };
        if len == 0 || len >= st.cap {
            return 1;
        }
        let name = code.instructions[pc].arg;
        let top = st.stack.add(len - 1);
        let f = match &*top {
            Object::Instance(inst) => {
                let cls = inst.cls_raw();
                let ver = cls.attr_version.get();
                if !Interpreter::default_getattribute(cls)
                    || crate::inst_may_shadow(inst, code, name)
                {
                    return 1;
                }
                let f = match ms.get_held(ver) {
                    Some(f) => Object::Function(f),
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

/// `LOAD_ATTR` (the `pc`th instruction of `code`) on the receiver at
/// stack slot `at`, as the core loop's arm runs it (and, for an instance
/// or module, the leaf arm's cached reads): `0` the value in place of the
/// receiver (released), anything else declined untouched.
unsafe extern "C" fn h_stack_attr(
    code: *const CodeObject,
    ext: *const CodeConstObjects,
    pc: u64,
    at: *mut Object,
) -> u32 {
    use weavepy_compiler::InlineCache as IC;
    // SAFETY: the code passes its own code and extension, one of its
    // attribute loads, and an initialized stack slot. The reads are the
    // core loop's: between two instructions, running no code.
    unsafe {
        let (code, ext, pc) = (&*code, &*ext, pc as usize);
        let recv = &*at;
        if !droppable(recv) {
            return 1;
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
                                    crate::field_slot_note(
                                        ext,
                                        code.instructions.len(),
                                        pc,
                                        inst,
                                        key_idx,
                                    );
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
            Object::Type(cls) if Rc::strong_count(cls) > 1 => ext
                .stamp_slots
                .get()
                .and_then(|s| s.get(pc))
                .and_then(|s| crate::class_attr_hit(s, cls)),
            Object::Module(m) => Interpreter::core_module_attr(code, m, pc, arg)
                .or_else(|| Interpreter::leaf_fused_local_attr(code, recv, pc, arg)),
            _ => None,
        };
        let Some(v) = v else {
            return 1;
        };
        crate::drop_hot(std::mem::replace(&mut *at, v));
        0
    }
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
) -> u32 {
    // SAFETY: the code passes a free stack slot, its own code and
    // extension, one of its attribute loads, and an initialized local.
    // The reads run no code.
    unsafe {
        let (code, ext, pc, recv) = (&*code, &*ext, pc as usize, &*recv);
        let arg = code.instructions[pc].arg;
        let v = match recv {
            Object::Instance(inst) if inst.cls_raw().native_kind.get() != 0 => {
                Interpreter::core_native_field(Some(ext), inst, arg, pc, code.instructions.len())
            }
            Object::Instance(_) | Object::Module(_) => {
                Interpreter::core_local_attr(Some(ext), code, recv, pc, arg)
            }
            _ => None,
        };
        match v {
            Some(v) => {
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

/// `CALL` (the `pc`th instruction of `code`) on a stack `len` deep, for
/// the shapes the core loop's arm runs in place: a pure leaf callee, a
/// natively served class's constructor or bound method, an exact list's
/// `append` / `pop`, and a registered leaf builtin. `0` the result in the
/// callee's slot (`st.len` past it), `RAISED` the builtin raised (the
/// operands released, `st.len` and `st.pc` as the arm leaves them),
/// anything else declined (a Python callee's activation is the core
/// loop's).
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
        // `next(gen)` resumes the generator inline: the core loop's.
        if argc == 1
            && matches!(
                (&*base.add(start), &*base.add(start + 2)),
                (Object::Builtin(b), Object::Generator(_))
                    if Rc::as_ptr(b) as usize == interp.leaf_fns().next_ptr
            )
        {
            return CALL_DECLINED;
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
            let sw = &mut *st.sw;
            let frame = &mut *st.frame;
            frame.stack.set_len(len);
            frame.pc = pc as u32;
            *sw.last = st.last;
            let interp = &mut *st.interp.cast_mut();
            let switched = if python == 1 {
                interp.core_call(sw, pc)
            } else {
                interp.core_new(sw, pc, st.snap_gen)
            };
            if !switched && sw.pending.is_none() {
                sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Step));
            }
            return RELOAD;
        }
        // A builtin the site has settled as a leaf (one it hasn't runs
        // through the core loop's builtin lane).
        let Some(slot) = ext.method_slots.get().and_then(|s| s.get(pc)) else {
            return CALL_DECLINED;
        };
        let Object::Builtin(b) = &ops[0] else {
            return CALL_DECLINED;
        };
        if Rc::strong_count(b) <= 1 {
            return CALL_DECLINED;
        }
        let Some(kind) = slot.get_leaf(b) else {
            return CALL_DECLINED;
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
            return CALL_DECLINED;
        }
        let first = if matches!(&ops[1], Object::Unbound) {
            start + 2
        } else {
            start + 1
        };
        let args = std::slice::from_raw_parts(base.add(first), len - first);
        if !args.iter().all(droppable) {
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
            None => CALL_DECLINED,
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

/// `RETURN_VALUE` (the `pc`th instruction) of an inline activation with
/// the value atop a `len`-deep stack: the frame synced and the caller
/// switched back to as the core loop's arm does (`1`), or `0` for the
/// root activation's, which the core loop's quiet loop returns from.
unsafe extern "C" fn h_return(st: *mut State, pc: u64, len: u64) -> u32 {
    // SAFETY: the code passes its live state, whose switch is the core
    // loop's (non-null: generator bodies don't return through here), and
    // its stack depth.
    unsafe {
        let st = &mut *st;
        let sw = &mut *st.sw;
        if (*sw.inl).is_empty() {
            return 0;
        }
        let frame = &mut *st.frame;
        frame.stack.set_len(len as usize);
        frame.pc = pc as u32;
        *sw.last = st.last;
        let interp = &mut *st.interp.cast_mut();
        if !interp.core_return(sw, st.snap_gen) {
            sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Step));
        }
        1
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
        let Ok(v) = cell.try_borrow() else {
            return 1;
        };
        if matches!(*v, Object::Unbound) {
            return 1;
        }
        dst.write(Interpreter::clone_operand(&v));
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
    let name = format!("wpframe_{}", engine.next);
    engine.next += 1;
    let id = engine
        .module
        .declare_function(&name, Linkage::Local, &engine.ctx.func.signature)
        .ok()?;
    let ninstrs = code.instructions.len();
    // The attribute sites' field caches (allocated here if no read has
    // recorded one yet: the code addresses them).
    let field_slots: &[FieldSlot] = ext
        .field_slots
        .get_or_init(|| (0..ninstrs).map(|_| FieldSlot::empty()).collect());
    let entries: Vec<bool>;
    let globals: Vec<Box<GlobalCache>>;
    let built = {
        let b = FunctionBuilder::new(&mut engine.ctx.func, &mut engine.fbctx);
        let mut lower = Lower::new(b, ptr, engine.tags, code, ext, nlocals, depths, field_slots);
        let done = lower.lower();
        entries = (0..ninstrs).map(|pc| lower.enters_at(pc)).collect();
        globals = std::mem::take(&mut lower.global_caches);
        if done {
            lower.b.seal_all_blocks();
            lower.b.finalize();
        }
        done
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
    let defined = engine.module.define_function(id, &mut engine.ctx);
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
    field_slots: &'a [FieldSlot],
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
    /// The re-entry copies of the blocks' rests (see `lower`).
    resume: Vec<Option<Block>>,
    /// Dispatches on the state's pc and depth (the entry, and helpers that
    /// move the pc by a run-time amount).
    dispatch: Block,
    /// Whether copies are on an exit path (one helper call each, not the
    /// scalar test in line: exits are rare, and code size costs compile
    /// time).
    cold: bool,
    /// The virtual stack above the frame's, and the logical depth.
    vs: Vec<Item>,
    depth: usize,
    sigs: Vec<(Vec<Type>, Option<Type>, SigRef)>,
    /// Side exits waiting for their blocks to be filled (after the body:
    /// the builder fills one block at a time).
    side_exits: Vec<SideExit>,
    /// The `LOAD_GLOBAL` sites' caches, which the code addresses.
    #[allow(clippy::vec_box)]
    global_caches: Vec<Box<GlobalCache>>,
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
    )
}

/// Whether the native code may hand the instruction at `pc` back to the
/// core loop for a shape its helpers don't settle (the next instruction is
/// worth re-entering at).
fn may_decline(code: &CodeObject, pc: usize) -> bool {
    native_at(code, pc)
        && matches!(
            code.instructions[pc].op,
            OpCode::LoadGlobal
                | OpCode::LoadAttr
                | OpCode::LoadMethodAttr
                | OpCode::StoreAttr
                | OpCode::BuildTuple
                | OpCode::BuildList
                | OpCode::LoadDeref
                | OpCode::GetIter
                | OpCode::BinarySubscr
                | OpCode::BinarySlice
                | OpCode::StoreSubscr
                | OpCode::ListAppend
                | OpCode::UnpackSequence
                | OpCode::ContainsOp
                | OpCode::BinaryOp
                | OpCode::CompareOp
                | OpCode::ToBool
                | OpCode::UnaryOp
        )
}

/// Whether the native code runs the instruction at `pc` itself (as
/// [`native_op`], with the operands some instructions need).
fn native_at(code: &CodeObject, pc: usize) -> bool {
    let ins = code.instructions[pc];
    match ins.op {
        OpCode::BuildTuple => (1..=3).contains(&ins.arg),
        OpCode::UnaryOp => ins.arg <= 3,
        op => native_op(op),
    }
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
        field_slots: &'a [FieldSlot],
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
                    if ins.op == OpCode::DeleteFast {
                        if let Some(x) = m.get_mut(ins.arg as usize) {
                            *x = true;
                        }
                    }
                }
                m
            },
            bound: vec![false; nlocals],
            blocks: Vec::new(),
            resume: Vec::new(),
            dispatch: Block::from_u32(0),
            cold: false,
            vs: Vec::new(),
            depth: 0,
            sigs: Vec::new(),
            side_exits: Vec::new(),
            global_caches: Vec::new(),
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
        self.b.ins().brif(scalar, by_value, &[], by_clone, &[]);
        self.b.switch_to_block(by_value);
        self.copy16(dst, src);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(by_clone);
        self.call(h_clone as *const () as usize, &[dst, src], false);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
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

    /// Fill the side exits' blocks.
    fn side_exits(&mut self) {
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
        if pc >= instrs.len() || (self.blocks[pc].is_none() && self.resume[pc].is_none()) {
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
        // Re-entry points after an instruction the helpers may hand back:
        // the core loop runs that one instruction and enters again at a
        // copy of the rest of its block (within a code size budget).
        let mut budget = n;
        self.resume = vec![None; n];
        for pc in 0..n.saturating_sub(1) {
            if starts[pc + 1] || self.depths[pc + 1] < 0 || !may_decline(code, pc) {
                continue;
            }
            let rest = (pc + 1..n).take_while(|&k| !starts[k]).count();
            if rest < 2 || rest > budget {
                continue;
            }
            budget -= rest;
            self.resume[pc + 1] = Some(self.b.create_block());
        }
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
            let target = match self.blocks[pc].or(self.resume[pc]) {
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
        // starts; then the re-entry copies.
        for start in 0..n {
            if let Some(blk) = self.blocks[start] {
                self.lower_block(start, blk);
            }
        }
        for start in 0..n {
            if let Some(blk) = self.resume[start] {
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
                        self.b.ins().brif(scalar, done, &[], heap, &[]);
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
            OpCode::ToBool => return self.to_bool(pc),
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
                let trip = self.b.ins().bor(low, moved);
                let out = self.exit_with(pc, &[], INTERP);
                self.branch_out(trip, out);
                let c1 = self.b.ins().iadd_imm(c, -1);
                self.b.ins().store(FLAGS, c1, countdown, 0);
                self.set_last(pc);
                self.goto(next.saturating_sub(ins.arg as usize));
                return false;
            }
            OpCode::ForIter => return self.for_iter(pc, ins.arg),
            OpCode::BinarySubscr
            | OpCode::BinarySlice
            | OpCode::StoreSubscr
            | OpCode::ListAppend
            | OpCode::UnpackSequence => return self.container(pc),
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
            // (A generator body's raise can't leave from here, nor its
            // return switch.)
            OpCode::Call if !self.is_gen() => return self.call_op(pc, ins.arg as usize),
            OpCode::ReturnValue if !self.is_gen() && self.depth > 0 => {
                self.flush();
                self.store_last();
                let pcv = self.b.ins().iconst(types::I64, pc as i64);
                let len = self.b.ins().iconst(types::I64, self.depth as i64);
                let r = self
                    .call(h_return as *const () as usize, &[self.st, pcv, len], true)
                    .expect("returns");
                let out = self.exit_with(pc, &[], INTERP);
                let switched = self.b.create_block();
                self.b.ins().brif(r, switched, &[], out, &[]);
                self.b.switch_to_block(switched);
                let s = self.b.ins().iconst(types::I32, i64::from(RELOAD));
                self.b.ins().return_(&[s]);
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
        self.b.ins().return_(&[s]);
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
                self.b.ins().brif(plain_old, fast, &[], slow, &[]);
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
                self.b.ins().brif(plain_old, fast, &[], slow, &[]);
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
        if matches!(ai, Item::Mem(_)) || matches!(bi, Item::Mem(_)) || native {
            return self.binary_helper(pc, kind, ai, bi);
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
        self.branch_out(r, out);
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
        self.branch_out(r, out);
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
        let n = self
            .call_typed(
                h_container as *const () as usize,
                &[self.st, ins, code, pcv, len],
                Some(types::I64),
            )
            .expect("returns");
        let declined = self.b.ins().icmp_imm(IntCC::Equal, n, -1);
        let out = self.exit_with(pc, &[], INTERP);
        self.branch_out(declined, out);
        self.depth = after as usize;
        self.set_last(pc);
        self.check_released(pc + 1);
        true
    }

    /// `CONTAINS_OP` through [`h_contains`]: an unboxed `bool`.
    fn contains(&mut self, pc: usize, arg: u32) -> bool {
        let bi = self.pop();
        let ai = self.pop();
        let slot = self.depth;
        self.materialize(ai, slot);
        self.materialize(bi, slot + 1);
        let at = self.slot_addr(slot);
        let r = self
            .call(h_contains as *const () as usize, &[at], true)
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
            self.push(ai);
            self.push(bi);
            self.exit(INTERP, pc);
            return false;
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
        let it_slot = self.depth - 1;
        let it = self.slot_addr(it_slot);
        let out_slot = self.slot_addr(self.depth);
        let r = self
            .call(
                h_for_iter as *const () as usize,
                &[self.st, it, out_slot],
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
        self.b.switch_to_block(decl);
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
        let r = match item {
            Item::Local(_) => self.call(
                h_local_attr as *const () as usize,
                &[dst, code, ext, pcv, at],
                true,
            ),
            _ => self.call(
                h_stack_attr as *const () as usize,
                &[code, ext, pcv, at],
                true,
            ),
        }
        .expect("returns");
        self.b.ins().brif(r, exit, &[], done, &[]);
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
        if let Some(none_tag) = self.tags.opt_none {
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
            let bo = self
                .b
                .ins()
                .uload8(types::I64, FLAGS, frame, F_BUILTINS_OBJ as i32);
            let moved = self.b.ins().icmp(IntCC::NotEqual, gs, cgs);
            let custom = self
                .b
                .ins()
                .icmp_imm(IntCC::NotEqual, bo, i64::from(none_tag));
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
        } else {
            self.b.ins().jump(helper, &[]);
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
        let code = self.code_ptr();
        let ext = self.ext_ptr();
        let pcv = self.b.ins().iconst(types::I64, pc as i64);
        let len = self.b.ins().iconst(types::I64, self.depth as i64);
        let r = self
            .call(
                h_load_method as *const () as usize,
                &[self.st, code, ext, pcv, len],
                true,
            )
            .expect("returns");
        let out = self.exit_with(pc, &[], INTERP);
        self.branch_out(r, out);
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
    fn to_bool(&mut self, pc: usize) -> bool {
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
        let r = self
            .call(
                h_call as *const () as usize,
                &[self.st, code, ext, pcv, len],
                true,
            )
            .expect("returns");
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
        self.b.ins().return_(&[r]);
        self.b.switch_to_block(ran);
        self.depth -= argc + 1;
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
