//! The call protocol between compiled functions: a per-thread stack of
//! records, one a nesting level.
//!
//! A record is what a direct call's callee runs on: its [`State`], which
//! compiled code gets a pointer to, and a parked activation slot
//! ([`crate::InlineAct`]) whose frame has its locals and operand stack. A
//! record keeps both between calls, so most of what a callee needs is
//! already in place: the state's per-thread fields, the frame's address and
//! its buffers' addresses. A call fills in what differs: the arguments move
//! into the locals, the frame takes its code and namespaces (uncounted, the
//! function holds them), and the record notes the call.
//!
//! What only matters if something looks stays undone until something does.
//! The function stays in its slot on the caller's stack, which also holds
//! the callee's handles; the callee's code takes no count; the caller's pc
//! and depth stay unsynced; nothing is on the switch, the pending list or
//! the spine. [`commit`] does all of it, from [`super::register_lazy`], when
//! a helper needs the core loop's view, a stack walk or a newer activation
//! comes along (`Interpreter::settle_lazy`). The activation slot then leaves
//! with the switch, and the record takes another for its next call. A
//! reassigned `__code__` settles first, so the frame's uncounted code can't
//! go away under it.
//!
//! Compiled code makes the common call in three steps, none of which needs
//! a frame of its own: [`h_direct_enter`] checks the callee (a compiled body
//! records its direct entry on its own code, so a call site caches nothing,
//! and any number of callees cost it the same) and binds it,
//! the code calls the callee's native code straight, and [`h_direct_leave`]
//! finishes a clean return, moving the result into the function's slot and
//! leaving the record as it found it. Both helpers are leaves on those
//! paths, ending in a tail call for anything else: a push and a pop of the
//! callee-saved registers would cost more than most of what they do.

use std::cell::Cell;

use crate::object::Object;
use crate::sync::Rc;
use crate::Interpreter;
use weavepy_compiler::CodeObject;

use super::{State, DIRECT_CALL_DEPTH};

/// An instance operator's call (see [`super::direct_call`]).
pub(super) type Binop<'a> = Option<(
    weavepy_compiler::BinOpKind,
    bool,
    &'a Rc<crate::types::TypeObject>,
    &'a Rc<crate::types::TypeObject>,
)>;

/// One nesting level of direct calls (see the module docs). The state comes
/// first: a callee's state pointer is its record's.
#[repr(C)]
pub(crate) struct DirectRec {
    pub(super) st: State,
    /// The parked slot's frame and locals vector (`st.lazy` is the slot).
    frame: *mut crate::Frame,
    locals: *mut Vec<Object>,
    /// The count of the handle on the locals (see `Frame::locals`).
    locals_count: *const std::sync::atomic::AtomicUsize,
    /// The slots the locals and the operand stack each have room for.
    room: u32,
    /// A return has more to do: cells of its own, a pending `lru_cache`
    /// store.
    extra: bool,
    /// The running call's pc and its operands' start on the caller's stack.
    call: CallAt,
    /// The caller's state.
    caller: *mut State,
    /// `Interpreter::lazy_top` when the call began.
    outer: usize,
    /// `Interpreter::exc_info_len` when the call began.
    exc_depth: usize,
    /// The cells of a function without any.
    no_cells: *const Vec<Rc<crate::sync::RefCell<Object>>>,
}

/// A `CALL`'s pc, its operands' start on the stack and its argument count,
/// in one word (a constant of the compiled code).
#[derive(Clone, Copy, Default)]
#[repr(transparent)]
pub(super) struct CallAt(u64);

impl CallAt {
    /// `last` is the instruction before, if the caller's state doesn't
    /// have it.
    #[inline(always)]
    pub(super) fn new(pc: usize, start: usize, argc: usize, last: Option<usize>) -> Self {
        debug_assert!(pc < 1 << 13 && start < 1 << 18);
        let last = last.map_or(0, |l| l as u64 + 1);
        CallAt(pc as u64 | last << 13 | (start as u64) << 26 | (argc as u64) << 44)
    }

    #[inline(always)]
    pub(super) fn bits(self) -> i64 {
        self.0 as i64
    }

    #[inline(always)]
    fn pc(self) -> u32 {
        (self.0 & 0x1fff) as u32
    }

    #[inline(always)]
    fn start(self) -> usize {
        (self.0 >> 26 & 0x3_ffff) as usize
    }

    /// The instruction before, if the caller's state doesn't have it.
    #[inline(always)]
    fn last(self) -> Option<usize> {
        match self.0 >> 13 & 0x1fff {
            0 => None,
            l => Some(l as usize - 1),
        }
    }

    #[inline(always)]
    fn argc(self) -> usize {
        (self.0 >> 44) as usize
    }
}

/// A thread's records: one per [`DIRECT_CALL_DEPTH`] level, and one more
/// that never has a slot (so that a full stack's top reads as a record
/// without one).
pub(crate) struct DirectRecs {
    base: *mut DirectRec,
    /// The next level's record.
    top: Cell<*mut DirectRec>,
}

// SAFETY: an interpreter moves to another thread only while idle, when every
// record rests with a parked slot (see `InlineAct`'s `Send`), and `sync`
// rewrites the thread's own addresses before the records are used again.
unsafe impl Send for DirectRecs {}

const RECS: usize = DIRECT_CALL_DEPTH as usize + 1;

impl DirectRecs {
    pub(crate) fn new() -> Self {
        let no_cells = Rc::as_ptr(crate::object::empty_cells_ref());
        let recs: Box<[DirectRec]> = (0..RECS)
            .map(|_| DirectRec {
                st: State {
                    locals: std::ptr::null_mut(),
                    stack: std::ptr::null_mut(),
                    len: 0,
                    cap: 0,
                    pc: 0,
                    last: usize::MAX,
                    countdown: std::ptr::null_mut(),
                    snap_gen: 0,
                    maybe_dead: std::ptr::null(),
                    interp: std::ptr::null(),
                    out: 0,
                    depth_cell: std::ptr::null(),
                    err: None,
                    frame: std::ptr::null_mut(),
                    sw: std::ptr::null_mut(),
                    gen_depth: 0,
                    direct: true,
                    gen_lazy: false,
                    lazy: std::ptr::null_mut(),
                    lazy_depth: 0,
                    lazy_last: std::ptr::null_mut(),
                    inl_call: std::ptr::null(),
                    inl_gone: false,
                    no_direct: false,
                    in_rec: true,
                    at_start: true,
                    clean: true,
                    wait_pc: usize::MAX,
                    wait_pending: None,
                },
                frame: std::ptr::null_mut(),
                locals: std::ptr::null_mut(),
                locals_count: std::ptr::null(),
                room: 0,
                extra: false,
                call: CallAt::default(),
                caller: std::ptr::null_mut(),
                outer: 0,
                exc_depth: 0,
                no_cells,
            })
            .collect();
        let base: *mut DirectRec = Box::into_raw(recs).cast();
        DirectRecs {
            base,
            top: Cell::new(base),
        }
    }

    /// The next level's record.
    #[inline(always)]
    pub(crate) fn top(&self) -> *mut DirectRec {
        self.top.get()
    }

    /// Make `top` the next level's record.
    #[inline(always)]
    pub(crate) fn set_top(&self, top: *mut DirectRec) {
        self.top.set(top);
    }

    /// Whether `top` is past the last level.
    #[inline(always)]
    pub(crate) fn full_at(&self, top: *mut DirectRec) -> bool {
        // SAFETY: within the allocation.
        std::ptr::eq(top, unsafe { self.base.add(DIRECT_CALL_DEPTH as usize) })
    }

    /// Whether every level is taken.
    #[inline(always)]
    pub(crate) fn full(&self) -> bool {
        self.full_at(self.top.get())
    }

    /// Point the records' states at this thread's interpreter and cells
    /// (when a burst starts: the interpreter may have moved since the last).
    #[inline]
    pub(crate) fn sync(
        &mut self,
        interp: *const Interpreter,
        countdown: *mut u32,
        maybe_dead: *const Cell<bool>,
        depth_cell: *const Cell<usize>,
    ) {
        // SAFETY: the first record is within the allocation.
        let first = unsafe { &(*self.base).st };
        if std::ptr::eq(first.interp, interp)
            && std::ptr::eq(first.maybe_dead, maybe_dead)
            && std::ptr::eq(first.depth_cell, depth_cell)
        {
            return;
        }
        for k in 0..RECS {
            // SAFETY: within the allocation; nothing runs on them.
            let st = unsafe { &mut (*self.base.add(k)).st };
            st.interp = interp;
            st.countdown = countdown;
            st.maybe_dead = maybe_dead;
            st.depth_cell = depth_cell;
        }
    }
}

impl Drop for DirectRecs {
    fn drop(&mut self) {
        // SAFETY: the allocation `new` made; a record at rest owns its
        // parked slot.
        unsafe {
            let recs = Box::from_raw(std::ptr::slice_from_raw_parts_mut(self.base, RECS));
            for rec in &*recs {
                if !rec.st.lazy.is_null() {
                    // (The locals' values went at the last return.)
                    (*rec.locals).set_len(0);
                    drop(Box::from_raw(rec.st.lazy));
                }
            }
        }
    }
}

impl DirectRec {
    /// Give the record a parked slot (it has none at first, or once its
    /// last one left with the switch).
    ///
    /// # Safety
    ///
    /// The record is at rest, without a slot.
    #[cold]
    #[inline(never)]
    unsafe fn take_slot(&mut self, interp: &mut Interpreter) {
        let mut act = interp.inline_slot();
        // SAFETY: the slot's own frame; a parked one holds nothing of these.
        unsafe {
            let fr: &mut crate::Frame = &mut act.frame;
            self.frame = fr;
            self.locals = fr.locals.storage();
            self.locals_count = crate::rc::strong_word(Rc::as_ptr(fr.locals.shared()));
            self.st.frame = fr;
            self.st.lazy = Box::into_raw(act);
            self.st.clean = true;
            self.note_buffers();
        }
    }

    /// Note where the slot's buffers are, and how much they hold.
    ///
    /// # Safety
    ///
    /// The record has a slot.
    #[inline]
    unsafe fn note_buffers(&mut self) {
        // SAFETY: the caller's contract.
        unsafe {
            let locals = &mut *self.locals;
            let stack = &mut (*self.frame).stack;
            self.st.locals = locals.as_mut_ptr();
            self.st.stack = stack.as_mut_ptr();
            self.st.cap = stack.capacity();
            self.room = locals.capacity().min(stack.capacity()) as u32;
        }
    }

    /// Make room for `room` locals and stack slots.
    ///
    /// # Safety
    ///
    /// The record has a slot, its locals and stack empty.
    #[cold]
    #[inline(never)]
    unsafe fn grow(&mut self, room: usize) {
        // SAFETY: the caller's contract.
        unsafe {
            (*self.locals).reserve(room);
            (*self.frame).stack.reserve(room);
            self.note_buffers();
        }
    }

    /// The call's fields every call writes (the arguments bound and the
    /// frame's handles written), and the record made the thread's innermost
    /// lazily running one.
    ///
    /// # Safety
    ///
    /// The record has a slot; `st` is the caller's live state.
    #[inline(always)]
    unsafe fn arm(&mut self, st: *mut State, call: CallAt, interp: &Interpreter) {
        // SAFETY: the caller's contract.
        unsafe {
            self.call = call;
            self.caller = st;
            self.exc_depth = interp.exc_info_len();
            self.st.sw = (*st).sw;
            self.st.snap_gen = (*st).snap_gen;
            self.st.len = 0;
            self.st.pc = 0;
            self.st.last = usize::MAX;
            self.outer = interp.lazy_top.get();
            interp
                .lazy_top
                .set(std::ptr::from_mut(&mut self.st) as usize);
        }
    }

    /// The call's frame takes the function's handles, uncounted.
    ///
    /// # Safety
    ///
    /// The record has a slot; the frame's fields are stale.
    #[inline(always)]
    unsafe fn write_handles(
        &mut self,
        fp: *const crate::object::PyFunction,
        code_rc: &Rc<CodeObject>,
    ) {
        // SAFETY: the caller's contract; the function outlives the call.
        unsafe {
            let fr = &mut *self.frame;
            std::ptr::write(&raw mut fr.code, crate::borrowed_rc(code_rc));
            std::ptr::write(&raw mut fr.globals, crate::borrowed_rc(&(*fp).globals));
            std::ptr::write(&raw mut fr.builtins, crate::borrowed_rc(&(*fp).builtins));
            fr.pc = 0;
        }
    }
}

/// What [`h_direct_enter`] needs of a callee whose code has a direct entry
/// (`JitHint::set_direct`): the parameters arguments bind to, the locals to
/// bind, the record's room it takes and whether it reads its function's
/// closure, packed in a word.
#[derive(Clone, Copy)]
struct Shape(u32);

impl Shape {
    const CLOSURE: u32 = 1 << 31;

    #[inline(always)]
    fn params(self) -> usize {
        (self.0 & 0xf) as usize
    }

    #[inline(always)]
    fn room(self) -> u32 {
        self.0 >> 4 & 0x3fff
    }

    #[inline(always)]
    fn total(self) -> usize {
        (self.0 >> 18 & 0x1fff) as usize
    }

    #[inline(always)]
    fn closure(self) -> bool {
        self.0 & Self::CLOSURE != 0
    }
}

/// The most arguments [`h_direct_enter`] moves.
const FAST_ARGS: usize = 8;

/// Record `code`'s direct entry (`native`, its body) if a call of it binds
/// without allocating and runs the body whole from its start, and it's past
/// the warm-up hooks only the general path runs (see [`enter`]). `packed` is
/// the binding of [`super::direct_body`].
#[inline(never)]
pub(super) fn note_direct(code: &CodeObject, native: &super::Native, packed: usize) {
    let hint = &code.jit_hint;
    let nlocals = code.varnames.len();
    let room = nlocals.max(native.need);
    if packed & super::EXTENDED_BIT as usize != 0
        || !code.cellvars.is_empty()
        || !native.chunks.is_empty()
        || !native.enters_at(0)
        || native.nlocals != nlocals
        || (code.arg_count as usize) > FAST_ARGS
        || room > 0x1fff
        || hint.lean_entries() <= crate::tier2::LEAN_WARM_COMPILE_THRESHOLD_CAP
        || (hint.is_compiled() && !hint.is_not_jitable() && !crate::tier2::jit_off_for_process())
    {
        return;
    }
    let closure = packed & super::CELLS_BIT as usize != 0;
    let shape = code.arg_count
        | (room as u32) << 4
        | (nlocals as u32) << 18
        | if closure { Shape::CLOSURE } else { 0 };
    hint.set_direct(native.func as usize, shape);
}

/// Move `n` (at most [`FAST_ARGS`]) values, without a call to `memcpy`.
///
/// # Safety
///
/// `src` holds `n` values the caller forgets, and `dst` has room for them.
#[inline(always)]
unsafe fn move_args(src: *const Object, dst: *mut Object, n: usize) {
    // SAFETY: the caller's contract. (The nesting keeps this a chain of
    // compares: the compiler makes a call of a plain loop.)
    unsafe {
        macro_rules! step {
            ($k:expr, $rest:block) => {
                if n > $k {
                    let (a, b) = (src.add($k).cast::<u64>(), dst.add($k).cast::<u64>());
                    let (x, y) = (a.read(), a.add(1).read());
                    b.write(x);
                    b.add(1).write(y);
                    $rest
                }
            };
        }
        step!(0, {
            step!(1, {
                step!(2, {
                    step!(3, {
                        step!(4, {
                            step!(5, {
                                step!(6, {
                                    step!(7, {});
                                });
                            });
                        });
                    });
                });
            });
        });
    }
}

/// What [`h_direct_enter`] gives compiled code, in two registers: the
/// callee's state (null: nothing done), and the native code to call with it.
#[repr(C)]
pub(super) struct Entered {
    st: *mut State,
    entry: usize,
}

const NOT_ENTERED: Entered = Entered {
    st: std::ptr::null_mut(),
    entry: 0,
};

/// A `CALL`'s callee entered from compiled code, the function at `slot` on
/// the stack and `call` the `CALL`'s own: its arguments bound and its record
/// armed, for the code to call the native code and pass the status to
/// [`h_direct_leave`]. Null touched nothing (the code then goes on to the
/// general paths, which settle anything this doesn't).
///
/// # Safety
///
/// `st` is the running code's live state, whose stack holds the `CALL`'s
/// operands from `slot` on.
pub(super) unsafe extern "C" fn h_direct_enter(
    st: *mut State,
    slot: *mut Object,
    call: CallAt,
) -> Entered {
    // SAFETY: the caller's contract.
    unsafe {
        let s = &*st;
        let Object::Function(f) = &*slot else {
            return NOT_ENTERED;
        };
        let fp = Rc::as_ptr(f);
        let code_rc: &Rc<CodeObject> = &*(*fp).code.as_ptr();
        let entry = code_rc.jit_hint.direct_entry();
        if entry == 0 {
            return NOT_ENTERED;
        }
        let shape = Shape(code_rc.jit_hint.direct_shape());
        let interp = &*s.interp;
        let rec = &mut *interp.recs.top();
        // (A full stack's top has no slot.)
        if rec.st.lazy.is_null()
            || s.no_direct
            || rec.room < shape.room()
            || crate::hot_gates::loop_gen() != s.snap_gen
        {
            return NOT_ENTERED;
        }
        let dc = &*s.depth_cell;
        let depth = dc.get() + 1;
        if depth > crate::recursion::recursion_limit() {
            return NOT_ENTERED;
        }
        let has_self = !matches!(&*slot.add(1), Object::Unbound);
        let n = call.argc() + usize::from(has_self);
        if n != shape.params() || shape.closure() {
            return enter_more(st, slot, call, entry);
        }
        // Committed. (The native code's entry marks the other locals unbound
        // before anything can see them.)
        dc.set(depth);
        move_args(slot.add(2 - usize::from(has_self)), rec.st.locals, n);
        (*rec.locals).set_len(shape.total());
        rec.write_handles(fp, code_rc);
        std::ptr::write(&raw mut (*rec.frame).cells, Rc::from_raw(rec.no_cells));
        rec.arm(st, call, interp);
        interp.recs.set_top(std::ptr::from_mut(rec).add(1));
        Entered {
            st: &raw mut rec.st,
            entry,
        }
    }
}

/// [`h_direct_enter`] for a callee `entry` with defaults to take or a closure
/// to read, or with the wrong number of arguments, the other checks passed.
///
/// # Safety
///
/// As `h_direct_enter`'s, with a function at `slot`.
#[inline(never)]
unsafe extern "C" fn enter_more(
    st: *mut State,
    slot: *mut Object,
    call: CallAt,
    entry: usize,
) -> Entered {
    // SAFETY: the caller's contract.
    unsafe {
        let s = &*st;
        let Object::Function(f) = &*slot else {
            std::hint::unreachable_unchecked()
        };
        let fp = Rc::as_ptr(f);
        let code_rc: &Rc<CodeObject> = &*(*fp).code.as_ptr();
        let shape = Shape(code_rc.jit_hint.direct_shape());
        let has_self = !matches!(&*slot.add(1), Object::Unbound);
        let n = call.argc() + usize::from(has_self);
        let interp = &*s.interp;
        let rec = &mut *interp.recs.top();
        let params = shape.params();
        if n > params {
            return NOT_ENTERED;
        }
        let missing = params - n;
        if missing != 0
            && (crate::sync::release_gates() != 0
                || (*fp).defaults.len() < missing
                || (*fp).defaults_maybe_overridden())
        {
            return NOT_ENTERED;
        }
        let cells = if shape.closure() {
            match (*fp).closure_cells.get() {
                Some(cells) => Rc::as_ptr(cells),
                None => return NOT_ENTERED,
            }
        } else {
            rec.no_cells
        };
        // Committed.
        let dc = &*s.depth_cell;
        dc.set(dc.get() + 1);
        let dst = rec.st.locals;
        move_args(slot.add(2 - usize::from(has_self)), dst, n);
        if missing != 0 {
            let defaults = (*fp).defaults.as_ptr().add((*fp).defaults.len() - missing);
            for k in 0..missing {
                let d = defaults.add(k);
                // While the gates are off this thread alone updates counts.
                if let Some(word) = (*d).strong_word() {
                    let w = &*word;
                    w.store(
                        w.load(std::sync::atomic::Ordering::Relaxed) + 1,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                }
                let (a, b) = (d.cast::<u64>(), dst.add(n + k).cast::<u64>());
                let (x, y) = (a.read(), a.add(1).read());
                b.write(x);
                b.add(1).write(y);
            }
        }
        (*rec.locals).set_len(shape.total());
        rec.write_handles(fp, code_rc);
        std::ptr::write(&raw mut (*rec.frame).cells, Rc::from_raw(cells));
        rec.arm(st, call, interp);
        interp.recs.set_top(std::ptr::from_mut(rec).add(1));
        Entered {
            st: &raw mut rec.st,
            entry,
        }
    }
}

/// `CALL` (`call`, atop a `len`-deep stack) of a Python class whose memoized
/// plan runs a plain `__init__` that has a native body, as
/// `Interpreter::core_new_n` runs one: the instance allocated and that body
/// run on a record, as a direct call of it, the caller taking the instance
/// when it returns `None`. `None` touched nothing.
///
/// # Safety
///
/// As `h_call`'s, with a non-null switch.
#[inline(never)]
pub(super) unsafe fn ctor_direct(st: &mut State, call: CallAt, len: usize) -> Option<u32> {
    // SAFETY: the caller's contract.
    unsafe {
        let start = call.start();
        let argc = call.argc();
        let slot = st.stack.add(start);
        let (Object::Type(ty), Object::Unbound) = (&*slot, &*slot.add(1)) else {
            return None;
        };
        let interp = &mut *st.interp.cast_mut();
        let nested = interp.recs.top();
        if st.no_direct
            || interp.recs.full_at(nested)
            || ty.flags.is_builtin
            || crate::hot_gates::loop_gen() != st.snap_gen
            || (*st.maybe_dead).get()
            || crate::stdlib::tracemalloc_real::is_tracking()
            || crate::stdlib::testinternalcapi_mod::reftrace_print_active()
            || crate::stdlib::testinternalcapi_mod::nomem_alloc_fails()
            || !Interpreter::metaclass_calls_like_type(ty)
        {
            return None;
        }
        // SAFETY: a guard-free read of the memoized plan; nothing runs
        // before its last use.
        let (ver, plan) = ty.instance_plan.peek().and_then(Option::as_ref)?;
        if *ver != ty.attr_version.get() || plan.user_new.is_some() || plan.seeds_exception_args {
            return None;
        }
        // What runs on the frame alone (a store-only or leaf `__init__`, the
        // bare allocation): a lazily running activation stays lazy.
        {
            let frame = &mut *st.frame;
            frame.stack.set_len(len);
            frame.pc = call.pc();
            let (ty, plan) = (ty.clone(), plan.clone());
            if interp.core_init_on_frame(frame, &ty, &plan, argc, start, st.depth_cell) {
                st.len = start + 1;
                return Some(0);
            }
        }
        let Object::Type(ty) = &*slot else {
            return None;
        };
        let (_, plan) = ty.instance_plan.peek().and_then(Option::as_ref)?;
        let (init, code) = plan.lean_init.as_ref()?;
        // SAFETY: GIL-serialized raw read of the function's code cell; only
        // the pointer is compared.
        if !std::ptr::eq(Rc::as_ptr(&*init.code.as_ptr()), Rc::as_ptr(code))
            || !Interpreter::lean_code_ok(code)
            || Interpreter::has_extended_params(code)
        {
            return None;
        }
        let missing = Interpreter::missing_defaults(init, code, argc + 1)?;
        let cells = !code.cellvars.is_empty() || !code.freevars.is_empty();
        if cells && code.cellvars.is_empty() && init.lean_cells_ref(code).is_none() {
            return None;
        }
        let ext = crate::code_vm_ext(code)?;
        let native: &super::Native = ext.frame_jit.get(code.varnames.len())?;
        if !native.enters_at(0) {
            return None;
        }
        let dc = &*st.depth_cell;
        if dc.get() + 1 > crate::recursion::recursion_limit() {
            return None;
        }
        // Committed: the frame as the core loop's arm has it at the call,
        // the instance made (a collection it sets off may run finalizers).
        let frame = &mut *st.frame;
        frame.stack.set_len(len);
        frame.pc = call.pc();
        let (ty, plan) = (ty.clone(), plan.clone());
        let (init, _) = plan.lean_init.as_ref().expect("checked above");
        let (inst, tracked) = interp.alloc_lean_instance_obj(&ty, &plan.native);
        if tracked && crate::gc_trace::maybe_auto_collect() {
            interp.run_pending_finalizers();
        }
        // The function takes the class's slot, and the instance the empty
        // self slot.
        let init = Rc::clone(init);
        slot.add(1).write(inst.clone());
        let cls = slot.replace(Object::Function(init));
        drop(plan);
        drop(ty);
        drop(cls);
        let Object::Function(f) = &*slot else {
            std::hint::unreachable_unchecked()
        };
        let code_rc: &Rc<CodeObject> = &*f.code.as_ptr();
        let bound = enter(
            st,
            call.pc() as usize,
            start,
            len,
            Rc::as_ptr(f),
            code_rc,
            true,
            BindArgs::Positional {
                missing,
                extended: false,
            },
            cells,
            native.need(),
            interp.recs.top(),
            None,
        );
        assert!(bound, "the depth was checked above");
        let rec_at = interp.recs.top();
        let rec = &mut *rec_at;
        std::ptr::write(&raw mut (*rec.st.lazy).init_inst, Some(inst));
        rec.extra = true;
        rec.st.clean = false;
        Some(run(st, rec_at, native, None))
    }
}

/// How a direct call's arguments bind (see [`enter`]).
#[derive(Clone, Copy)]
pub(super) enum BindArgs {
    /// Positionally, the last `missing` parameters taking defaults; with
    /// `extended`, the code's `*args`, `**kwargs` and keyword-only
    /// parameters too (see `Interpreter::fill_extended_raw`).
    Positional { missing: usize, extended: bool },
    /// A `CALL_KW`'s, through the site's verified `CallPyKwNames`
    /// permutation (see [`super::direct_call_kw`]): `argc` positionals and
    /// `kwc` keyword values, the names tuple atop the stack.
    Keywords {
        perm: u32,
        covered: u32,
        kwc: usize,
        argc: usize,
    },
}

/// A direct call's callee (the plain function `fp` running `code_rc`, the
/// call's operands from `start` up a `len`-deep stack) bound on the
/// record `rec_at`, its checks passed: its arguments as `args` says, its
/// cells when `cells`, a `functools.lru_cache` miss's store (`lru`). `need`
/// is the stack its native code takes. `false` touched nothing.
///
/// # Safety
///
/// As `h_call`'s, with a non-null switch, and `rec_at` the next level's
/// record.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn enter(
    st: &mut State,
    pc: usize,
    start: usize,
    len: usize,
    fp: *const crate::object::PyFunction,
    code_rc: &Rc<CodeObject>,
    has_self: bool,
    args: BindArgs,
    cells: bool,
    need: usize,
    rec_at: *mut DirectRec,
    lru: Option<&mut Option<crate::stdlib::functools_mod::LruPending>>,
) -> bool {
    // SAFETY: the caller's contract.
    unsafe {
        let interp = &mut *st.interp.cast_mut();
        let dc = &*st.depth_cell;
        let depth = dc.get() + 1;
        if depth > crate::recursion::recursion_limit() {
            return false;
        }
        // Committed: the callee bound on the record, the caller left as it
        // stands.
        dc.set(depth);
        let rec = &mut *rec_at;
        if rec.st.lazy.is_null() {
            rec.take_slot(interp);
        }
        let locals: &mut Vec<Object> = &mut *rec.locals;
        // (Its values went at the last return.)
        locals.set_len(0);
        let room = need.max(code_rc.varnames.len());
        if (rec.room as usize) < room {
            rec.grow(room);
        }
        let base = st.stack;
        match args {
            BindArgs::Positional {
                missing,
                extended: true,
            } => {
                // `*args`, `**kwargs` or keyword-only parameters: the
                // arguments move in as `fill_extended_raw` binds them.
                let first = if has_self { start + 1 } else { start + 2 };
                Interpreter::fill_extended_raw(
                    &*fp,
                    code_rc,
                    base.add(first),
                    len - first,
                    missing,
                    locals,
                );
                crate::fill_unbound(locals, code_rc.varnames.len());
            }
            BindArgs::Positional { missing, .. } => {
                // The arguments move into the locals, the defaults follow
                // as copies, and the other locals start unbound.
                let first = if has_self { start + 1 } else { start + 2 };
                let nargs = len - first;
                let total = code_rc.varnames.len().max(nargs + missing);
                if locals.capacity() < total {
                    locals.reserve(total);
                }
                let dst = locals.as_mut_ptr();
                crate::move_objects(base.add(first), dst, nargs);
                let mut k = nargs;
                if missing > 0 {
                    let defaults = &(*fp).defaults;
                    for d in &defaults[defaults.len() - missing..] {
                        dst.add(k).write(crate::clone_hot(d));
                        k += 1;
                    }
                }
                crate::fill_unbound_raw(dst.add(k), total - k);
                locals.set_len(total);
            }
            BindArgs::Keywords {
                perm,
                covered,
                kwc,
                argc,
            } => {
                // The keywords take the slots the permutation names, the
                // positionals the first ones, the rest their defaults.
                let caller = &mut *st.frame;
                caller.stack.set_len(len);
                let names = caller.stack.pop().expect("the names tuple");
                let items: &[Object] = match &names {
                    Object::Tuple(t) => t,
                    _ => &[],
                };
                Interpreter::kw_names_fill_locals(
                    locals,
                    &mut caller.stack,
                    items,
                    &*fp,
                    code_rc,
                    perm,
                    covered,
                    kwc,
                    argc + usize::from(has_self),
                );
                crate::drop_hot(names);
            }
        }
        rec.write_handles(fp, code_rc);
        let fr = &mut *rec.frame;
        if cells {
            let act = &mut *rec.st.lazy;
            act.owns_cells = !code_rc.cellvars.is_empty();
            let frame_cells = if act.owns_cells {
                // (Its parameters that are cells move into them.)
                rec.extra = true;
                rec.st.clean = false;
                crate::fresh_cells(&*fp, code_rc, locals)
            } else {
                crate::borrowed_rc((*fp).lean_cells_ref(code_rc).expect("checked by the call"))
            };
            std::ptr::write(&raw mut fr.cells, frame_cells);
        } else {
            std::ptr::write(&raw mut fr.cells, Rc::from_raw(rec.no_cells));
        }
        // (A binding that grew them moved them.)
        rec.note_buffers();
        // As for any inline activation: a hot body is compiled once.
        let n = code_rc.jit_hint.bump_lean_entries();
        if n <= crate::tier2::LEAN_WARM_COMPILE_THRESHOLD_CAP
            && n == crate::tier2::lean_warm_at()
            && !crate::tier2::jit_off_for_process()
            && !code_rc.jit_hint.is_not_jitable()
        {
            crate::tier2::warm_compile(interp, &mut *rec.frame);
        }
        // A `functools.lru_cache` miss's store goes with the activation.
        if let Some(lru) = lru {
            if lru.is_some() {
                (*rec.st.lazy).lru = lru.take();
                rec.extra = true;
                rec.st.clean = false;
            }
        }
        rec.arm(st, CallAt::new(pc, start, 0, None), interp);
        crate::burst_stats::note_call(crate::burst_stats::CALL_DIRECT);
        true
    }
}

/// What [`enter`] and [`h_direct_enter`] put off (see the module docs),
/// done for a callee about to register: `act` is its record's slot.
///
/// # Safety
///
/// `st` is a record's state, armed and still lazy (its `lazy` just taken),
/// with `act` that value.
pub(super) unsafe fn commit(st: &mut State, act: *mut crate::InlineAct) {
    // SAFETY: the caller's contract; the function is still in its slot, and
    // the caller's frame is live.
    unsafe {
        let rec = &mut *std::ptr::from_mut(st).cast::<DirectRec>();
        let act = &mut *act;
        let caller = &mut *rec.caller;
        if let Some(last) = rec.call.last() {
            caller.last = last;
        }
        let start = rec.call.start();
        std::ptr::write(&raw mut act.callable, caller.stack.add(start).read());
        let frame = &mut *caller.frame;
        frame.stack.set_len(start);
        frame.pc = rec.call.pc() + 1;
        act.call_pc = rec.call.pc() as usize;
        act.exc_depth = rec.exc_depth;
        act.guard = Some(crate::recursion::Guard::adopt(st.depth_cell));
        act.lazy_caller = rec.caller;
        act.parked = false;
        act.direct = true;
        // The frame owns its code from here.
        std::mem::forget(Rc::clone(&(*rec.frame).code));
        rec.extra = false;
        st.clean = false;
    }
}

/// [`h_direct_enter`]'s callee's native code returned `status`: its return
/// finished here when clean (`0`, the result in the function's slot, any
/// finalizers it queued for the code's own check), or whatever it left
/// synced for the core loop. A leaf that releases what costs nothing to
/// release: anything else goes on in a tail call.
///
/// # Safety
///
/// `st` is the caller's live state, and `cst` what `h_direct_enter` gave
/// it, whose native code just returned `status`.
pub(super) unsafe extern "C" fn h_direct_leave(
    st: *mut State,
    cst: *mut State,
    status: u32,
) -> u32 {
    // SAFETY: the caller's contract.
    unsafe {
        let s = &mut *st;
        let rec = &mut *cst.cast::<DirectRec>();
        let interp = &mut *s.interp.cast_mut();
        if status as u8 != super::REC_RET as u8
            || (*rec.locals_count).load(std::sync::atomic::Ordering::Relaxed) > 1
            || interp.gil_countdown <= 2
            || crate::hot_gates::loop_gen() != rec.st.snap_gen
            || crate::sync::release_gates() != 0
        {
            return left_slowly(s, rec, status);
        }
        interp.lazy_top.set(rec.outer);
        (*rec.frame).stack.set_len(0);
        let n = (*rec.locals).len();
        let dc = &*s.depth_cell;
        dc.set(dc.get() - 1);
        // (The last first, as CPython releases them.)
        let locals = rec.st.locals;
        let mut k = n;
        while k != 0 {
            k -= 1;
            if let Some(word) = (*locals.add(k)).strong_word() {
                // While the gates are off this thread alone updates counts.
                let w = &*word;
                let c = w.load(std::sync::atomic::Ordering::Relaxed);
                if c == 1 {
                    return release_rest(s, rec, k + 1);
                }
                w.store(c - 1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        let slot = s.stack.add(rec.call.start());
        if !matches!(&*slot, Object::Function(_)) {
            return finish_rest(s, rec);
        }
        // (A function's second word is its count's address.)
        let word = &**slot.cast::<*const std::sync::atomic::AtomicUsize>().add(1);
        let c = word.load(std::sync::atomic::Ordering::Relaxed);
        if c == 1 {
            return finish_rest(s, rec);
        }
        word.store(c - 1, std::sync::atomic::Ordering::Relaxed);
        let (a, b) = (rec.st.stack.cast::<u64>(), slot.cast::<u64>());
        let (x, y) = (a.read(), a.add(1).read());
        b.write(x);
        b.add(1).write(y);
        interp.gil_countdown -= 1;
        interp.recs.set_top(std::ptr::from_mut(rec));
        if (*s.maybe_dead).get() {
            return super::left_marked(st, rec.call.pc(), rec.call.start() + 1);
        }
        0
    }
}

/// [`h_direct_leave`] for a local whose last reference goes, the first `n`
/// still to release.
#[cold]
#[inline(never)]
unsafe fn release_rest(s: &mut State, rec: &mut DirectRec, n: usize) -> u32 {
    // SAFETY: as `h_direct_leave`'s; the first `n` values are still owned.
    unsafe {
        crate::release_run(rec.st.locals, n);
        finish_rest(s, rec)
    }
}

/// [`h_direct_leave`] from past the locals, the function's release going
/// the general way.
#[inline(never)]
unsafe fn finish_rest(s: &mut State, rec: &mut DirectRec) -> u32 {
    // SAFETY: as `h_direct_leave`'s.
    unsafe {
        let interp = &mut *s.interp.cast_mut();
        let start = rec.call.start();
        let slot = s.stack.add(start);
        let callable = slot.read();
        slot.write(rec.st.stack.read());
        // (What the release runs may nest; nothing reads the record after.)
        interp.release(callable);
        interp.recs.set_top(std::ptr::from_mut(rec));
        interp.gil_countdown -= 1;
        // (An inlined body's caller checks for itself, with its frames built
        // by the exit that takes.)
        if (*s.maybe_dead).get() && s.inl_call.is_null() {
            return finalizers_after(s, rec.call, interp);
        }
        s.len = start + 1;
        0
    }
}

/// A clean return's queued finalizers, which may run Python code: the
/// caller's view first.
#[cold]
#[inline(never)]
unsafe fn finalizers_after(st: &mut State, call: CallAt, interp: &mut Interpreter) -> u32 {
    // SAFETY: as `h_direct_leave`'s.
    unsafe {
        let start = call.start();
        let frame = &mut *st.frame;
        frame.stack.set_len(start + 1);
        frame.pc = call.pc() + 1;
        super::settle(st);
        let sw = &mut *st.sw;
        let mut tmp = None;
        let (cf, _, cshell) = sw.activation((*sw.inl).len(), &mut tmp);
        interp.flush_lean(&mut *cf, &mut *cshell.cast::<crate::QuietShell<'_>>());
        if interp.drain_if_maybe_dead() && crate::hot_gates::loop_gen() != st.snap_gen {
            sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Breaker));
        }
        if sw.pending.is_some() {
            return super::RELOAD;
        }
        st.len = start + 1;
        0
    }
}

/// [`h_direct_leave`] for anything but its common case.
#[inline(never)]
unsafe fn left_slowly(s: &mut State, rec: &mut DirectRec, status: u32) -> u32 {
    // SAFETY: as `h_direct_leave`'s.
    unsafe {
        let status = rec.unpack(status);
        if status == super::INTERP {
            // An entry that got nowhere counts against the entry.
            if let Some(native) = crate::code_vm_ext(&(*rec.frame).code)
                .and_then(|ext| ext.frame_jit.get((*rec.locals).len()))
            {
                native.ran(&rec.st, 0, 0, rec.st.cap, usize::MAX, status);
            }
        }
        leave(s, rec, status, None)
    }
}

/// The end of a call on `rec`, whose native code returned `status`: its
/// return finished here when clean, or whatever it left synced for the
/// core loop. `binop` is an instance operator's (see
/// [`super::direct_call`]).
///
/// # Safety
///
/// As `h_direct_leave`'s.
#[inline(always)]
unsafe fn leave(st: &mut State, rec: &mut DirectRec, status: u32, binop: Binop<'_>) -> u32 {
    // SAFETY: the caller's contract.
    unsafe {
        let interp = &mut *st.interp.cast_mut();
        let status = rec.unpack(status);
        interp.lazy_top.set(rec.outer);
        let rec_at = std::ptr::from_mut(rec);
        if status == super::DIRECT_RET
            && !rec.st.lazy.is_null()
            && rec.st.len == 1
            && !(*rec.frame).locals.is_shared()
            && (!rec.extra || rec.extra_allows_clean())
            && !interp.countdown_out(2)
            && crate::hot_gates::loop_gen() == rec.st.snap_gen
        {
            return finish_clean(st, rec, interp);
        }
        // Anything else is the core loop's to finish: the callee registers
        // first. (The record stays this level's until that's done: anything
        // the core loop's paths run nests above it.)
        let r = left_uncleanly(st, rec, status, binop);
        interp.recs.set_top(rec_at);
        r
    }
}

/// A clean return, never registered, with its result alone on its stack:
/// the result in the function's slot on the caller's stack (`0`), the
/// record back at rest.
#[inline(always)]
unsafe fn finish_clean(st: &mut State, rec: &mut DirectRec, interp: &mut Interpreter) -> u32 {
    // SAFETY: as `leave`'s.
    unsafe {
        (*rec.frame).stack.set_len(0);
        let owns_cells = rec.extra && (*rec.st.lazy).owns_cells;
        crate::release_activation_locals(&mut *rec.frame, owns_cells);
        let dc = &*st.depth_cell;
        dc.set(dc.get() - 1);
        if rec.extra {
            rec.finish_extra(interp);
        }
        finish_rest(st, rec)
    }
}

impl DirectRec {
    /// What a clean return has to do past the common case (see
    /// [`DirectRec::extra`]).
    #[cold]
    #[inline(never)]
    unsafe fn finish_extra(&mut self, interp: &mut Interpreter) {
        // SAFETY: the record is armed, its slot its own.
        unsafe {
            let act = &mut *self.st.lazy;
            // A constructor's `__init__` returned `None`: the caller takes
            // the instance.
            if let Some(inst) = act.init_inst.take() {
                self.st.stack.write(inst);
            }
            // A `functools.lru_cache` miss's function returned: its result
            // is stored before the caller takes it.
            if act.lru.is_some() {
                interp.lru_finish(&mut act.lru, &*self.st.stack);
            }
            if act.owns_cells {
                act.owns_cells = false;
                drop(std::ptr::read(&raw const (*self.frame).cells));
            }
            self.extra = false;
            self.st.clean = true;
        }
    }

    /// Whether a return with [`DirectRec::extra`] set can finish cleanly: a
    /// constructor's `__init__` that returned anything but `None` raises.
    ///
    /// # Safety
    ///
    /// The record is armed, unregistered, its result atop its stack.
    #[cold]
    #[inline(never)]
    unsafe fn extra_allows_clean(&self) -> bool {
        // SAFETY: the caller's contract.
        unsafe { (*self.st.lazy).init_inst.is_none() || matches!(&*self.st.stack, Object::None) }
    }

    /// `status` as [`leave`] takes it: a clean return's ([`super::REC_RET`]
    /// and what the native code's return left out of the state) written
    /// out as any other direct return's.
    #[inline(always)]
    fn unpack(&mut self, status: u32) -> u32 {
        if status as u8 != super::REC_RET as u8 {
            return status;
        }
        self.st.len = 1;
        self.st.pc = (status >> 8 & 0xfff) as usize;
        let last = status >> 20;
        if last != 0 {
            self.st.last = last as usize - 1;
        }
        super::DIRECT_RET
    }
}

/// [`leave`] when the return isn't clean.
#[cold]
#[inline(never)]
unsafe fn left_uncleanly(
    st: &mut State,
    rec: &mut DirectRec,
    status: u32,
    binop: Binop<'_>,
) -> u32 {
    // SAFETY: as `leave`'s.
    unsafe {
        super::register_lazy(&mut rec.st);
        let (pc, start) = (rec.call.pc(), rec.call.start());
        let callee = rec.st.frame;
        super::direct_done::<false>(
            st,
            pc as usize,
            start,
            callee,
            0,
            std::ptr::null_mut(),
            binop,
            status,
            &mut rec.st,
        )
    }
}

/// Run the callee bound on `rec_at` through its native body `native`, from
/// its start, and finish its return (see [`leave`]).
///
/// # Safety
///
/// As [`enter`]'s, with `rec_at` what it bound.
#[inline(always)]
pub(super) unsafe fn run(
    st: &mut State,
    rec_at: *mut DirectRec,
    native: &super::Native,
    binop: Binop<'_>,
) -> u32 {
    // SAFETY: the caller's contract.
    unsafe {
        let rec = &mut *rec_at;
        (*st.interp).recs.set_top(rec_at.add(1));
        let status = native.run_direct(&mut rec.st, 0);
        leave(st, rec, status, binop)
    }
}
