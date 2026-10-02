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
use std::sync::atomic::{AtomicU32, Ordering};

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
    /// the native code runs.
    entries: Box<[bool]>,
    /// The code's name and opcodes (for `WEAVEPY_FRAME_JIT_STATS`).
    name: String,
    ops: Box<[OpCode]>,
}

// SAFETY: the code only touches the state it is handed; it is only
// entered with the GIL held (never in the free-threaded build).
unsafe impl Send for Native {}
// SAFETY: as above.
unsafe impl Sync for Native {}

impl Native {
    /// Run from `st.pc` until an instruction the core loop must run.
    ///
    /// # Safety
    ///
    /// `st` is the running activation of the code this was compiled
    /// from, as the core loop holds it, with `nlocals` locals.
    /// Whether entering at `pc` gets anywhere.
    #[inline(always)]
    pub(crate) fn enters_at(&self, pc: usize) -> bool {
        self.entries.get(pc).copied().unwrap_or(false)
    }

    #[inline(always)]
    pub(crate) unsafe fn run(&self, st: &mut State) -> u32 {
        let from = st.pc;
        // SAFETY: the caller's contract.
        let status = unsafe { (self.func)(st) };
        if stats::enabled() {
            stats::note(&self.name, &self.ops, from, st.pc, status);
        }
        status
    }
}

/// A code object's compilation state, kept in its extension.
#[derive(Default)]
pub(crate) struct Slot {
    native: std::sync::OnceLock<Option<Box<Native>>>,
    heat: AtomicU32,
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

    /// Count one activation or back edge of code without native code,
    /// compiling it once it is hot.
    #[inline(always)]
    /// Count a call (`at` 0) or a back edge (`at` its pc) toward
    /// compiling `code`.
    pub(crate) fn warm(&self, code: &CodeObject, ext: &CodeConstObjects, nlocals: usize, at: usize) {
        if self.native.get().is_some() {
            return;
        }
        // (A racing thread losing a count is harmless.)
        let h = self.heat.load(Ordering::Relaxed) + 1;
        self.heat.store(h, Ordering::Relaxed);
        if h >= hot() {
            self.heat.store(0, Ordering::Relaxed);
            self.try_compile(code, ext, nlocals, at);
        }
    }

    #[cold]
    #[inline(never)]
    fn try_compile(&self, code: &CodeObject, ext: &CodeConstObjects, nlocals: usize, at: usize) {
        if !enabled() {
            let _ = self.native.set(None);
            return;
        }
        // Loops the tier-2 compiler may still take keep their back edges'
        // consultation in the core loop.
        let hint = &code.jit_hint;
        if !(hint.is_not_jitable()
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

/// Whether the loop of `code` whose back edge is at `at` (any loop, for
/// a call) runs mostly in native code: each
/// instruction the core loop runs costs a round trip out of the native
/// code and back, so a loop of calls and global loads gains nothing from
/// compiling (nor does a loop-free body, whose entry costs as much as the
/// little it would save).
fn worth_compiling(code: &CodeObject, ext: &CodeConstObjects, at: usize) -> bool {
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
            if let Some(call) = (pc + 2..ins.len().min(pc + 12)).find(|&k| ins[k].op == OpCode::Call) {
                helped[call] = true;
            }
        }
    }
    let at_loop = ins.get(at).is_some_and(|i| i.op == OpCode::JumpBackward);
    ins.iter().enumerate().any(|(pc, i)| {
        if i.op != OpCode::JumpBackward || (at_loop && pc != at) {
            return false;
        }
        let top = (pc + 1).saturating_sub(i.arg as usize);
        let (mut native, mut exits) = (0usize, 0usize);
        for k in top..=pc {
            if native_op(ins[k].op) || helped[k] {
                native += 1;
            } else {
                exits += 1;
            }
        }
        if stats::enabled() {
            eprintln!("frame jit: {} loop at {top}: {native} native, {exits} exits", code.qualname);
        }
        exits * EXIT_WEIGHT <= native
    })
}

/// How many natively run instructions a loop needs per instruction that
/// leaves to the core loop before compiling it pays.
const EXIT_WEIGHT: usize = 8;

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
                }
                // SAFETY: registering a plain function.
                unsafe { libc::atexit(dump) };
            }
            on
        })
    }

    #[cold]
    pub(super) fn note(name: &str, ops: &[OpCode], from: usize, at: usize, status: u32) {
        let op = match (status, ops.get(at)) {
            (0, Some(op)) => format!("{op:?}"),
            (0, None) => "<end>".to_owned(),
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
        tag(&Object::Type(crate::builtin_types::builtin_types().object_.clone())),
    ];
    let t = Tags {
        none: tag(&Object::None),
        unbound: tag(&Object::Unbound),
        boolean: tag(&Object::Bool(true)),
        int: tag(&Object::Int(1)),
        float: tag(&Object::Float(1.0)),
        cell: tag(&cell),
        instance: tag(&inst),
        by_pointer: samples
            .iter()
            .fold(0, |m, &t| if t < 64 { m | (1i64 << t) } else { m }),
    };
    if samples.iter().any(|&t| t >= 64) {
        return None;
    }
    let all = [t.none, t.unbound, t.boolean, t.int, t.float, t.cell, t.instance];
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
    let Object::Iter(rc) = top else {
        return 3;
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
        u32::from(!Interpreter::core_store_local_attr(code, local, pc + 1, name, &*slot))
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
        // kept unboxed and in registers); Cranelift's own passes cost more
        // to run than they save. `WEAVEPY_FRAME_JIT_OPT` turns them on.
        if std::env::var_os("WEAVEPY_FRAME_JIT_OPT").is_some() {
            flags.set("opt_level", "speed").ok()?;
        } else {
            flags.set("opt_level", "none").ok()?;
            flags.set("regalloc_algorithm", "single_pass").ok()?;
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
    let built = {
        let b = FunctionBuilder::new(&mut engine.ctx.func, &mut engine.fbctx);
        let mut lower = Lower::new(
            b,
            ptr,
            engine.tags,
            code,
            ext,
            nlocals,
            depths,
            field_slots,
        );
        let done = lower.lower();
        entries = (0..ninstrs).map(|pc| lower.enters_at(pc)).collect();
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
        entries: entries.into_boxed_slice(),
        name: code.qualname.clone(),
        ops: code.instructions.iter().map(|i| i.op).collect(),
    })
}

const FLAGS: MemFlags = MemFlags::trusted();

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
    snap: Value,
    countdown: Value,
    dead: Value,
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
    /// time).
    cold: bool,
    /// The virtual stack above the frame's, and the logical depth.
    vs: Vec<Item>,
    depth: usize,
    sigs: Vec<(Vec<Type>, Option<Type>, SigRef)>,
    /// Side exits waiting for their blocks to be filled (after the body:
    /// the builder fills one block at a time).
    side_exits: Vec<SideExit>,
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
    )
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
        let snap = load(&mut b, types::I64, S_SNAP);
        let countdown = load(&mut b, ptr, S_COUNTDOWN);
        let dead = load(&mut b, ptr, S_DEAD);
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
            snap,
            countdown,
            dead,
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
            dispatch: Block::from_u32(0),
            cold: false,
            vs: Vec::new(),
            depth: 0,
            sigs: Vec::new(),
            side_exits: Vec::new(),
        }
    }

    // ---- building blocks ----

    fn sig(&mut self, params: &[Type], ret: Option<Type>) -> SigRef {
        if let Some((_, _, s)) = self
            .sigs
            .iter()
            .find(|(p, r, _)| p == params && *r == ret)
        {
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
        let f = self.b.ins().uload8(types::I32, FLAGS, self.dead, 0);
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
                let is_bool = self.b.ins().icmp_imm(IntCC::Equal, t, i64::from(tb.boolean));
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
        if !self.blocks.get(pc).is_some_and(Option::is_some) {
            return false;
        }
        native_op(instrs[pc].op)
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
            if next < n && (target.is_some() || !native_op(ins.op)) {
                starts[next] = true;
            }
            // A fused attribute store continues after its `STORE_ATTR`.
            if ins.op == OpCode::LoadFast
                && code.instructions.get(next).map(|i| i.op) == Some(OpCode::StoreAttr)
                && pc + 2 < n
            {
                starts[pc + 2] = true;
            }
            // A fused method call continues after its `CALL`.
            if ins.op == OpCode::LoadFast
                && code.instructions.get(next).map(|i| i.op) == Some(OpCode::LoadMethodAttr)
            {
                if let Some(call) = (pc + 2..n.min(pc + 12))
                    .find(|&k| code.instructions[k].op == OpCode::Call)
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
        let need = self.depths.iter().copied().max().unwrap_or(0).max(0) + 4;
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
            let Some(blk) = self.blocks[start] else {
                continue;
            };
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
        self.side_exits();
        true
    }

    /// Lower the instruction at `pc` into the current block: `true` when
    /// the block continues with the next instruction, `false` when it ended
    /// (a jump, an exit).
    fn instruction(&mut self, pc: usize) -> bool {
        let ins = self.code.instructions[pc];
        let next = pc + 1;
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
            OpCode::StoreFast if (ins.arg as usize) < self.nlocals => {
                self.store_fast(pc, ins.arg)
            }
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
            OpCode::ToBool => {
                let item = self.pop();
                let Some(o) = self.operand(item) else {
                    self.push(item);
                    self.exit(INTERP, pc);
                    return false;
                };
                if o.kind == Kind::Bool {
                    self.push(item);
                } else {
                    let other = self.exit_with(pc, &[item], INTERP);
                    let t = self.truth(o, true, other);
                    self.push(Item::Bool(t));
                }
                true
            }
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
                        let none = self.b.ins().icmp_imm(IntCC::Equal, tag, i64::from(self.tags.none));
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
                        self.b.ins().icmp_imm(IntCC::Equal, tag, i64::from(self.tags.none))
                    }
                    Item::Const(k) => {
                        let none = matches!(self.ext.objects[k as usize], Object::None);
                        self.b.ins().iconst(types::I8, i64::from(none))
                    }
                    Item::Dyn(tag, ..) => {
                        self.b.ins().icmp_imm(IntCC::Equal, tag, i64::from(self.tags.none))
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
                let c = self.b.ins().load(types::I32, FLAGS, self.countdown, 0);
                let low = self.b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, c, 1);
                let gen_addr = self
                    .b
                    .ins()
                    .iconst(self.ptr, crate::hot_gates::loop_gen_ptr() as i64);
                let g = self.b.ins().load(types::I64, FLAGS, gen_addr, 0);
                let moved = self.b.ins().icmp(IntCC::NotEqual, g, self.snap);
                let trip = self.b.ins().bor(low, moved);
                let out = self.exit_with(pc, &[], INTERP);
                self.branch_out(trip, out);
                let c1 = self.b.ins().iadd_imm(c, -1);
                self.b.ins().store(FLAGS, c1, self.countdown, 0);
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
        match self.code.instructions.get(pc + 1).map(|i| i.op) {
            Some(OpCode::LoadMethodAttr) => return self.method_call(pc),
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

    /// `LOAD_FAST x; x.m(<simple arguments>)` through [`h_local_method`],
    /// continuing at whatever follows the call (through the dispatch). A
    /// generator body's resume can't raise from here: its calls are the
    /// general loop's.
    fn method_call(&mut self, pc: usize) -> bool {
        if self.code.is_generator || self.code.is_coroutine || self.code.is_async_generator {
            self.exit(INTERP, pc);
            return false;
        }
        self.flush();
        let code = self.b.ins().iconst(self.ptr, std::ptr::from_ref(self.code) as i64);
        let ext = self.b.ins().iconst(self.ptr, std::ptr::from_ref(self.ext) as i64);
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
        self.exit(INTERP, pc);
        false
    }

    /// Stop for a queued finalizer with the state as a helper left it (pc
    /// and depth already stored).
    fn check_released_dynamic(&mut self) {
        let f = self.b.ins().uload8(types::I32, FLAGS, self.dead, 0);
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
        let code = self.b.ins().iconst(self.ptr, std::ptr::from_ref(self.code) as i64);
        let pcv = self.b.ins().iconst(types::I64, pc as i64);
        let r = self
            .call(h_store_attr as *const () as usize, &[self.st, code, pcv, slot], true)
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
                self.b.ins().iconst(types::I64, i64::from(self.tags.boolean)),
                self.b.ins().iconst(types::I64, 0),
                v,
            )),
            Item::Dyn(t, w, b) => Some((t, w, self.b.ins().ireduce(types::I8, b))),
            Item::Const(k) if matches!(
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
        // a heap value as a number: the helper takes every shape.
        if matches!(ai, Item::Mem(_)) || matches!(bi, Item::Mem(_)) {
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
        .any(|k| is(k));
        let float_kind = is(BinOpKind::Add) || is(BinOpKind::Sub) || is(BinOpKind::Mult) || is(BinOpKind::Div);
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
        if matches!((ia, ib), (Ok(false), _) | (_, Ok(false))) && !float_kind {
            self.b.ins().jump(slow, &[]);
            return false;
        }
        // At run time: two ints, or two numbers with a float.
        let to_v = |s: &mut Self, r: Result<bool, Value>| match r {
            Ok(x) => s.b.ins().iconst(types::I8, i64::from(x)),
            Err(v) => v,
        };
        let (iav, ibv, fav, fbv) = (to_v(self, ia), to_v(self, ib), to_v(self, fa), to_v(self, fb));
        let done = self.b.create_block();
        self.b.append_block_param(done, types::I64);
        self.b.append_block_param(done, types::I64);
        let both_int = self.b.ins().band(iav, ibv);
        let int_b = self.b.create_block();
        let not_int = self.b.create_block();
        self.b.ins().brif(both_int, int_b, &[], not_int, &[]);
        self.b.switch_to_block(int_b);
        if int_kind {
            match self.int_op(kind, a.word, b.word, slow) {
                Some(r) => {
                    let t = self.b.ins().iconst(types::I64, i64::from(self.tags.int));
                    self.b.ins().jump(done, &[t.into(), r.into()]);
                }
                None => {
                    self.b.ins().jump(slow, &[]);
                }
            }
        } else {
            self.b.ins().jump(slow, &[]);
        }
        self.b.switch_to_block(not_int);
        if float_kind {
            let na = self.b.ins().bor(iav, fav);
            let nb = self.b.ins().bor(ibv, fbv);
            let nums = self.b.ins().band(na, nb);
            let bad = self.b.ins().bxor_imm(nums, 1);
            self.branch_out(bad, slow);
            let x = self.as_f64(a, Err(fav));
            let y = self.as_f64(b, Err(fbv));
            let r = self.float_op(kind, x, y, slow);
            let bits = self.b.ins().bitcast(types::I64, MemFlags::new(), r);
            let t = self.b.ins().iconst(types::I64, i64::from(self.tags.float));
            self.b.ins().jump(done, &[t.into(), bits.into()]);
        } else {
            self.b.ins().jump(slow, &[]);
        }
        self.b.switch_to_block(done);
        let (t, w) = (self.b.block_params(done)[0], self.b.block_params(done)[1]);
        let zero = self.b.ins().iconst(types::I64, 0);
        self.push(Item::Dyn(t, w, zero));
        self.set_last(pc);
        true
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

    /// A container instruction through [`h_container`] (the operands on
    /// the stack; the depth after is the static one).
    fn container(&mut self, pc: usize) -> bool {
        let after = self.depths.get(pc + 1).copied().unwrap_or(-1);
        if after < 0 {
            self.exit(INTERP, pc);
            return false;
        }
        self.flush();
        let ins = self
            .b
            .ins()
            .iconst(self.ptr, std::ptr::from_ref(&self.code.instructions[pc]) as i64);
        let code = self.b.ins().iconst(self.ptr, std::ptr::from_ref(self.code) as i64);
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
            let out = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThanOrEqual, y, 64);
            self.branch_out(out, slow);
            self.b.ins().sshr(x, y)
        } else if is(BinOpKind::LShift) {
            let out = self.b.ins().icmp_imm(IntCC::UnsignedGreaterThanOrEqual, y, 63);
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
        let (Some(a), Some(b)) = (self.operand(ai), self.operand(bi)) else {
            self.push(ai);
            self.push(bi);
            self.exit(INTERP, pc);
            return false;
        };
        let slow = self.exit_with(pc, &[ai, bi], INTERP);
        if matches!(a.kind, Kind::Bool | Kind::None) || matches!(b.kind, Kind::Bool | Kind::None) {
            self.b.ins().jump(slow, &[]);
            return false;
        }
        let ia = self.is_kind(a, Kind::Int, self.tags.int);
        let ib = self.is_kind(b, Kind::Int, self.tags.int);
        let fa = self.is_kind(a, Kind::Float, self.tags.float);
        let fb = self.is_kind(b, Kind::Float, self.tags.float);
        let int_cmp = |s: &mut Self| s.b.ins().icmp(icc, a.word, b.word);
        let float_cmp = |s: &mut Self| {
            let x = s.b.ins().bitcast(types::F64, MemFlags::new(), a.word);
            let y = s.b.ins().bitcast(types::F64, MemFlags::new(), b.word);
            // An unordered pair takes the core loop's arm.
            let uno = s.b.ins().fcmp(FloatCC::Unordered, x, y);
            s.branch_out(uno, slow);
            s.b.ins().fcmp(fcc, x, y)
        };
        let r = match (ia, ib, fa, fb) {
            (Ok(true), Ok(true), ..) => int_cmp(self),
            (_, _, Ok(true), Ok(true)) => float_cmp(self),
            _ => {
                let to_v = |s: &mut Self, r: Result<bool, Value>| match r {
                    Ok(x) => s.b.ins().iconst(types::I8, i64::from(x)),
                    Err(v) => v,
                };
                let (iav, ibv, fav, fbv) =
                    (to_v(self, ia), to_v(self, ib), to_v(self, fa), to_v(self, fb));
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
                let both_float = self.b.ins().band(fav, fbv);
                let bad = self.b.ins().bxor_imm(both_float, 1);
                self.branch_out(bad, slow);
                let r = float_cmp(self);
                self.b.ins().jump(done, &[r.into()]);
                self.b.switch_to_block(done);
                self.b.block_params(done)[0]
            }
        };
        self.push(Item::Bool(r));
        self.set_last(pc);
        true
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
                let (t, b, w) = unsafe {
                    (*p, *p.add(1), p.add(8).cast::<u64>().read_unaligned())
                };
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
        let is_bool = self.b.ins().icmp_imm(IntCC::Equal, ta, i64::from(tags.boolean));
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
            .call(h_for_iter as *const () as usize, &[self.st, it, out_slot], true)
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

    /// `x.attr` of an instance (a local's, or one on the stack): the
    /// site's field shortcut in line. The value goes onto the stack (a
    /// scalar copied, anything else with a reference taken); a stack
    /// receiver is released after. Anything else, and a miss, is the core
    /// loop's.
    fn load_attr(&mut self, pc: usize, _arg: u32) -> bool {
        let (Some(l), Some(cache_slot)) = (self.layout, self.field_slots.get(pc)) else {
            self.exit(INTERP, pc);
            return false;
        };
        let l = *l;
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
        let miss = self.exit_with(pc, &[item], INTERP);
        let otag = self.tag_at(at);
        let not_inst = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, otag, i64::from(self.tags.instance));
        self.branch_out(not_inst, miss);
        let ptr = self.ptr;
        let inst = self.b.ins().load(ptr, FLAGS, at, 8);
        let cls = self.b.ins().load(ptr, FLAGS, inst, l.inst_class);
        let ver = self.b.ins().load(types::I64, FLAGS, cls, l.type_attr_version);
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
        let absent = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, idx, len);
        let bad = self.b.ins().bor(foreign, absent);
        self.branch_out(bad, miss);
        let off = self.b.ins().ishl_imm(idx, 4);
        let v = self.b.ins().iadd(block, off);
        let v = self.b.ins().iadd_imm(v, i64::from(l.split_values));
        let slot = self.depth;
        let dst = self.slot_addr(slot);
        match item {
            Item::Local(_) => {
                // The receiver is the local's: the value just takes a copy.
                self.copy_value(dst, v);
            }
            _ => {
                // The receiver is the stack's: the value goes above it
                // (copied) until the receiver is released, then down.
                let above = self.slot_addr(slot + 1);
                self.copy_value(above, v);
                let r = self
                    .call(h_pop as *const () as usize, &[dst], true)
                    .expect("returns");
                let undo = self.b.create_block();
                let released = self.b.create_block();
                self.b.ins().brif(r, undo, &[], released, &[]);
                // Declined: the copy goes, and the core loop takes the
                // instruction from the start.
                self.b.switch_to_block(undo);
                // (A fresh copy has another owner: its release is a plain
                // decrement.)
                self.call(h_pop as *const () as usize, &[above], true);
                self.b.ins().jump(miss, &[]);
                self.b.switch_to_block(released);
                self.copy16(dst, above);
            }
        }
        self.push(Item::Mem(slot));
        self.set_last(pc);
        if matches!(item, Item::Mem(_)) {
            self.check_released(pc + 1);
        }
        true
    }
}

/// Where a field cache's version and position sit.
const FIELD_VER: i32 = std::mem::offset_of!((u64, u32), 0) as i32;
const FIELD_IDX: i32 = std::mem::offset_of!((u64, u32), 1) as i32;
