//! Native code for leaf plans.
//!
//! A plan that keeps running is compiled, once per effect mode, into a
//! function over the same registers: each register is a pair of SSA
//! variables holding the [`V`] tag and payload words; moves, constants,
//! jumps, and scalar compares, truth tests and arithmetic run in line; so
//! do the common hits of a site's caches (an instance field, a method, a
//! class constant, a module global), which their helpers fill; and every
//! other operation calls a helper that runs the interpreter's own
//! implementation of it. Whatever the native code or a helper can't
//! settle declines the evaluation exactly where the interpreter's runner
//! would, so a native plan keeps the runner's contract: a result, or
//! nothing observable.
//!
//! A method call whose site last resolved a pure leaf is compiled with the
//! callee's plan in line, behind a guard on the function and its code; any
//! other call goes through [`Interpreter::leaf_eval_nested`] (and so runs
//! natively too once the callee's own plan is compiled).

use super::{
    binary, compare, is_same, new_container, norm, plan_store, subscr, truth, unary, LeafPlan,
    LeafRet, Op, Owned, Pending, NEST, V,
};
use crate::object::{Object, PyFunction};
use crate::sync::Rc;
use crate::{CodeConstObjects, Interpreter};
use cranelift_codegen::ir::{
    condcodes::{FloatCC, IntCC},
    types, AbiParam, Block, InstBuilder, MemFlags, SigRef, Signature, StackSlotData, StackSlotKind,
    Value,
};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};
use weavepy_compiler::{BinOpKind, CodeObject, CompareKind};

/// Evaluations of a plan (per effect mode) before it is compiled. A
/// compile costs millions of instructions and saves tens to hundreds per
/// evaluation, so a plan compiles once interpreting it has cost about
/// what compiling would (the rent-or-buy rule): a short run doesn't pay
/// for code it never amortizes, and a long one loses little.
const WARM_RUNS: u32 = 20_000;

/// How deep callee plans are compiled in line.
const INLINE_DEPTH: u8 = 2;

/// The `repr(u64)` discriminants of [`V`]'s variants, in declaration order.
const T_FN: u64 = 0;
const T_BI: u64 = 1;
const T_NULL: u64 = 2;
const T_R: u64 = 3;
const T_I: u64 = 4;
const T_F: u64 = 5;
const T_B: u64 = 6;
const T_N: u64 = 7;

/// A compiled plan: `(ctx, argument words) -> status`, where `0` returned
/// (the result's words in `ctx.out`) and anything else declined.
type NativeFn = unsafe extern "C" fn(*mut Ctx<'static>, *const u64) -> u32;

/// A plan's native code and what it was compiled against.
pub(super) struct Native {
    func: NativeFn,
    /// The deepest in-line callee (the evaluation needs that much room
    /// below the recursion limit).
    depth: u8,
    /// The in-line callees' frame descriptors, which the code addresses,
    /// and what keeps their identity guards sound: a function's
    /// allocation stays reserved (so no other function takes its
    /// address), and its code stays alive.
    // Boxed: the code embeds each descriptor's address.
    #[allow(clippy::vec_box)]
    _frames: Vec<Box<Frame>>,
    _keep: Vec<(crate::sync::Weak<PyFunction>, Rc<CodeObject>)>,
    /// The attribute and global sites' caches, which the code addresses.
    #[allow(clippy::vec_box)]
    _fields: Vec<Box<FieldCache>>,
    #[allow(clippy::vec_box)]
    _globals: Vec<Box<GlobalCache>>,
    #[allow(clippy::vec_box)]
    _methods: Vec<Box<MethodCache>>,
}

/// An attribute site's split-field positions by receiver class version:
/// a site several classes reach (methods a base class shares) keeps one
/// for each. A version names one class in one state, and a class's shared
/// names never move, so a hit needs only the instance to hold the field.
///
/// An entry may instead name a `__slots__` member's position in its
/// class's slot layout (see [`SLOT_FIELD`]), with the layout's word in
/// the parallel `layouts` entry.
#[derive(Default)]
pub(super) struct FieldCache {
    entries: [std::cell::Cell<(u64, u32)>; 4],
    layouts: [std::cell::Cell<usize>; 4],
    next: std::cell::Cell<u8>,
}

/// A module global site's value as register words, with the globals
/// stamp it was read under (process-unique, so it names the dict and its
/// key layout: the entry stays put) and the global value epoch (which an
/// in-place rebinding advances). Native code reads it in line.
#[derive(Default)]
#[repr(C)]
pub(super) struct GlobalCache {
    stamp: std::cell::Cell<u64>,
    epoch: std::cell::Cell<u64>,
    tag: std::cell::Cell<u64>,
    pay: std::cell::Cell<u64>,
}

const GC_STAMP: i32 = std::mem::offset_of!(GlobalCache, stamp) as i32;
const GC_EPOCH: i32 = std::mem::offset_of!(GlobalCache, epoch) as i32;
const GC_TAG: i32 = std::mem::offset_of!(GlobalCache, tag) as i32;
const GC_PAY: i32 = std::mem::offset_of!(GlobalCache, pay) as i32;
/// Where native code finds the evaluation's own function.
const TOP_F_OFFSET: i32 = TOP_OFFSET + std::mem::offset_of!(Frame, f) as i32;

/// A class-attribute stamp's scalar tags (see `class_attr_fill`) and the
/// register tags of the values they hold.
const CLASS_ATTR_SCALARS: [(u64, u64); 4] = [
    (crate::CLASS_ATTR_INT, T_I),
    (crate::CLASS_ATTR_FLOAT, T_F),
    (crate::CLASS_ATTR_BOOL, T_B),
    (crate::CLASS_ATTR_NONE, T_N),
];

/// A method site's latest resolutions, which native code reads in line:
/// for an instance receiver, its class's version, the function, and how
/// many of the class's shared names (from the first) come before the
/// method's name, so an instance holding no more fields than that can't
/// shadow it; for a class receiver, its version and the function called
/// with an empty self slot. Versions are process-unique (one class in one
/// state), so an equal one means the same resolution.
#[derive(Default)]
#[repr(C)]
pub(super) struct MethodCache {
    inst_ver: std::cell::Cell<u64>,
    inst_func: std::cell::Cell<u64>,
    inst_limit: std::cell::Cell<u64>,
    type_ver: std::cell::Cell<u64>,
    type_func: std::cell::Cell<u64>,
}

const MC_INST_VER: i32 = std::mem::offset_of!(MethodCache, inst_ver) as i32;
const MC_INST_FUNC: i32 = std::mem::offset_of!(MethodCache, inst_func) as i32;
const MC_INST_LIMIT: i32 = std::mem::offset_of!(MethodCache, inst_limit) as i32;
const MC_TYPE_VER: i32 = std::mem::offset_of!(MethodCache, type_ver) as i32;
const MC_TYPE_FUNC: i32 = std::mem::offset_of!(MethodCache, type_func) as i32;

// SAFETY: the descriptors name immutable code objects and reserved
// function allocations; native code is only entered with the GIL held.
unsafe impl Send for Native {}
// SAFETY: as above.
unsafe impl Sync for Native {}

/// `v` as its tag and payload words (the unused payload bytes zeroed).
#[inline(always)]
fn words(v: V) -> [u64; 2] {
    match v {
        V::Fn(p) => [T_FN, p as u64],
        V::Bi(p) => [T_BI, p as u64],
        V::Null => [T_NULL, 0],
        V::R(p) => [T_R, p as u64],
        V::I(i) => [T_I, i as u64],
        V::F(x) => [T_F, x.to_bits()],
        V::B(b) => [T_B, u64::from(b)],
        V::N => [T_N, 0],
    }
}

/// The value with tag `t` and payload `p`.
#[inline(always)]
fn value(t: u64, p: u64) -> V {
    match t {
        T_FN => V::Fn(p as *const PyFunction),
        T_BI => V::Bi(p as *const crate::object::BuiltinFn),
        T_NULL => V::Null,
        T_R => V::R(p as *const Object),
        T_I => V::I(p as i64),
        T_F => V::F(f64::from_bits(p)),
        T_B => V::B((p & 0xff) != 0),
        _ => V::N,
    }
}

/// Whether the discriminants above are `V`'s (checked once, before any
/// plan compiles).
fn tags_match() -> bool {
    let tag = |v: V| {
        // SAFETY: `V` is `repr(u64)`: its first word is the discriminant,
        // always initialized.
        unsafe { *std::ptr::from_ref(&v).cast::<u64>() }
    };
    tag(V::Fn(std::ptr::null())) == T_FN
        && tag(V::Bi(std::ptr::null())) == T_BI
        && tag(V::Null) == T_NULL
        && tag(V::R(std::ptr::null())) == T_R
        && tag(V::I(0)) == T_I
        && tag(V::F(0.0)) == T_F
        && tag(V::B(false)) == T_B
        && tag(V::N) == T_N
        && std::mem::size_of::<V>() == 16
}

/// What one frame of an evaluation runs: the code (its caches), its
/// constants and namespaces' function.
#[repr(C)]
pub(super) struct Frame {
    code: *const CodeObject,
    ext: *const CodeConstObjects,
    f: *const PyFunction,
}

impl Frame {
    /// # Safety
    ///
    /// The frame's code and function are alive (the evaluation's own, or
    /// an in-line callee whose guard passed).
    #[inline(always)]
    unsafe fn parts<'a>(&self) -> (&'a CodeObject, &'a CodeConstObjects, &'a PyFunction) {
        // SAFETY: forwarded contract.
        unsafe { (&*self.code, &*self.ext, &*self.f) }
    }
}

/// One native evaluation's state, shared with the helpers.
#[repr(C)]
pub(super) struct Ctx<'a> {
    /// A helper's result words (a method load's two values; a return's
    /// first).
    out: [u64; 4],
    /// The evaluation's own frame.
    top: Frame,
    interp: &'a Interpreter,
    args: &'a [*const Object],
    owned: Owned,
    pend: Pending,
    nest: u8,
}

impl Ctx<'_> {
    #[inline(always)]
    fn put(&mut self, v: V) {
        let [t, p] = words(v);
        self.out[0] = t;
        self.out[1] = p;
    }
}

/// `ctx.top`'s offset (native code passes the evaluation's own frame by
/// address).
const TOP_OFFSET: i32 = 32;
/// Where native code finds the count of buffered stores, and a field
/// cache's first entry's version and position.
const PEND_N_OFFSET: i32 =
    (std::mem::offset_of!(Ctx<'static>, pend) + std::mem::offset_of!(Pending, n)) as i32;
const FIELD_VER_OFFSET: i32 =
    (std::mem::offset_of!(FieldCache, entries) + std::mem::offset_of!((u64, u32), 0)) as i32;
const FIELD_IDX_OFFSET: i32 =
    (std::mem::offset_of!(FieldCache, entries) + std::mem::offset_of!((u64, u32), 1)) as i32;
const FIELD_LAYOUT_OFFSET: i32 = std::mem::offset_of!(FieldCache, layouts) as i32;
const _: () = assert!(std::mem::offset_of!(Ctx<'static>, top) == TOP_OFFSET as usize);

/// Run `plan`'s native code for one evaluation, compiling it once it is
/// warm: `None` when it has none (the interpreter's runner evaluates),
/// otherwise the evaluation's outcome.
#[inline]
#[allow(clippy::too_many_arguments)]
pub(super) fn run<const EFFECT: bool>(
    interp: &Interpreter,
    code: &CodeObject,
    ext: &CodeConstObjects,
    plan: &LeafPlan,
    f: &PyFunction,
    args: &[*const Object],
    nest: u8,
) -> Option<Option<LeafRet>> {
    let slot = &plan.native[usize::from(EFFECT)];
    let native = match slot.get() {
        Some(Some(native)) => native,
        Some(None) => return None,
        None => {
            if plan.runs.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < WARM_RUNS
                || crate::tier2::jit_off_for_process()
                || crate::gil::free_threading_enabled()
            {
                return None;
            }
            slot.get_or_init(|| {
                // SAFETY: a function value is always an `Rc` allocation
                // (`Object::Function`), alive for this evaluation: the
                // count taken here is released with the handle.
                let f = unsafe {
                    Rc::increment_strong_count(std::ptr::from_ref(f));
                    Rc::from_raw(std::ptr::from_ref(f))
                };
                compile(code, ext, plan, &f, EFFECT)
            })
            .as_ref()?
        }
    };
    enter::<EFFECT>(
        interp,
        native,
        (code, ext, plan, f),
        args,
        nest,
        crate::recursion::current_depth(),
    )
}

/// [`run`] for a plan whose native code is already compiled, with the
/// caller's recursion `depth` in hand: `None` when there is no native
/// code (yet), otherwise the evaluation's outcome.
#[inline]
pub(super) fn run_compiled<const EFFECT: bool>(
    interp: &Interpreter,
    code: &CodeObject,
    ext: &CodeConstObjects,
    plan: &LeafPlan,
    f: &PyFunction,
    args: &[*const Object],
    depth: usize,
) -> Option<Option<LeafRet>> {
    let native = plan.native[usize::from(EFFECT)].get()?.as_ref()?;
    enter::<EFFECT>(interp, native, (code, ext, plan, f), args, 0, depth)
}

/// One evaluation of `native` (see [`run`]), at recursion depth `depth`.
#[inline(always)]
fn enter<const EFFECT: bool>(
    interp: &Interpreter,
    native: &Native,
    (code, ext, plan, f): (&CodeObject, &CodeConstObjects, &LeafPlan, &PyFunction),
    args: &[*const Object],
    nest: u8,
    depth: usize,
) -> Option<Option<LeafRet>> {
    // In-line callees run where the interpreter would nest frames: near
    // the recursion limit, it decides.
    if depth + usize::from(nest) + usize::from(native.depth) + 1
        >= crate::recursion::recursion_limit()
    {
        return None;
    }
    let mut aw = [0u64; 16];
    for (k, &a) in args.iter().enumerate() {
        let [t, p] = words(norm(a));
        aw[2 * k] = t;
        aw[2 * k + 1] = p;
    }
    let mut ctx = Ctx {
        out: [0; 4],
        top: Frame { code, ext, f },
        interp,
        args,
        owned: Owned::new(),
        pend: Pending::new(),
        nest,
    };
    // SAFETY: the code was compiled from `plan` with this signature; the
    // context and the argument words outlive the call, and the lifetime
    // is only erased for its duration.
    let status = unsafe {
        (native.func)(
            std::ptr::from_mut(&mut ctx).cast::<Ctx<'static>>(),
            aw.as_ptr(),
        )
    };
    if status != 0 {
        return Some(None);
    }
    let v = value(ctx.out[0], ctx.out[1]);
    // In place: registers point into the scratch, which must not move.
    Some(interp.plan_finish::<EFFECT>(code, ext, plan, &ctx.owned, &mut ctx.pend, v))
}

// ---- helpers (the interpreter's operations, for native code) ----------

/// The context behind a native plan's first parameter.
///
/// # Safety
///
/// `ctx` is the live context [`run`] passed to the native code.
#[inline(always)]
unsafe fn cx<'a>(ctx: *mut Ctx<'static>) -> &'a mut Ctx<'a> {
    // SAFETY: forwarded contract.
    unsafe { &mut *ctx.cast::<Ctx<'a>>() }
}

/// A frame's stamp slots (its global and class attribute caches).
#[inline(always)]
fn stamps(ext: &CodeConstObjects) -> &[crate::StampSlot] {
    ext.stamp_slots.get().map_or(&[], |s| &s[..])
}

unsafe extern "C" fn h_global(
    ctx: *mut Ctx<'static>,
    fr: *const Frame,
    slot: *const crate::StampSlot,
    cache: *const GlobalCache,
    pc: u32,
) -> u32 {
    // SAFETY: called by native code with its context, a live frame, the
    // site's stamp slot (its code keeps the table), and its cache.
    let (c, (code, _, f), slot, cache) = unsafe { (cx(ctx), (*fr).parts(), &*slot, &*cache) };
    let Some(v) = c.interp.plan_global_at(code, slot, f, pc as u16) else {
        return 1;
    };
    // A module global's value is remembered for the in-line read (a
    // builtin's also depends on the globals not gaining the name, which
    // the helper checks).
    if matches!(
        code.caches.get(pc),
        weavepy_compiler::InlineCache::LoadGlobalModule { .. }
    ) {
        // SAFETY (raw dict read): nothing runs code here.
        let gs = unsafe { (*f.globals.as_ptr()).mutation_stamp() };
        let [t, p] = words(v);
        cache.stamp.set(gs);
        cache.epoch.set(crate::object::global_value_epoch());
        cache.tag.set(t);
        cache.pay.set(p);
    }
    c.put(v);
    0
}

unsafe extern "C" fn h_deref(ctx: *mut Ctx<'static>, fr: *const Frame, idx: u32) -> u32 {
    // SAFETY: called by native code with its context and a live frame.
    let (c, (_, _, f)) = unsafe { (cx(ctx), (*fr).parts()) };
    match super::free_var(f, idx as u8) {
        Some(v) => {
            c.put(v);
            0
        }
        None => 1,
    }
}

unsafe extern "C" fn h_attr<const EFFECT: bool>(
    ctx: *mut Ctx<'static>,
    fr: *const Frame,
    t: u64,
    p: u64,
    pc_name: u32,
) -> u32 {
    // SAFETY: called by native code with its context and a live frame.
    let (c, (code, ext, _)) = unsafe { (cx(ctx), (*fr).parts()) };
    let (pc, name) = (pc_name as u16, (pc_name >> 16) as u16);
    match c.interp.plan_attr::<false, EFFECT>(
        code,
        ext,
        stamps(ext),
        &mut c.owned,
        &c.pend,
        value(t, p),
        pc,
        name,
    ) {
        Some(v) => {
            c.put(v);
            0
        }
        None => 1,
    }
}

/// [`h_attr`] through the site's [`FieldCache`]: a split field of a plain
/// instance whose class the site has seen answers without the general
/// read; a general read that found the instance's own field remembers it.
unsafe extern "C" fn h_field<const EFFECT: bool>(
    ctx: *mut Ctx<'static>,
    fr: *const Frame,
    cache: *const FieldCache,
    t: u64,
    p: u64,
    pc_name: u32,
) -> u32 {
    // SAFETY: called by native code with its context and the site's
    // cache.
    let (c, cache) = unsafe { (cx(ctx), &*cache) };
    // SAFETY: as `norm`.
    let inst = match (t == T_R).then(|| unsafe { &*(p as *const Object) }) {
        Some(Object::Instance(inst)) => Some(inst),
        _ => None,
    };
    if let Some(inst) = inst.filter(|_| !EFFECT || c.pend.n == 0) {
        let ver = inst.cls_raw().attr_version.get();
        for e in &cache.entries {
            let (v, idx) = e.get();
            if v == ver && v != 0 {
                // SAFETY: the receiver is rooted and nothing runs code
                // while the view is read.
                let hit = unsafe {
                    if idx & SLOT_FIELD != 0 {
                        inst.laid_out_slot((idx & !SLOT_FIELD) as usize)
                    } else {
                        inst.split_field(idx as usize)
                    }
                };
                if let Some(v) = hit {
                    c.put(norm(v));
                    return 0;
                }
                break;
            }
        }
    }
    // SAFETY: forwarded.
    let status = unsafe { h_attr::<EFFECT>(ctx, fr, t, p, pc_name) };
    if let (0, Some(inst)) = (status, inst) {
        // SAFETY: a live frame.
        let (code, _, _) = unsafe { (*fr).parts() };
        let ver = inst.cls_raw().attr_version.get();
        // The instance's own field (not a class value or descriptor),
        // from its split layout.
        let found = match Interpreter::leaf_resolve_instance_attr_ix(code, inst, pc_name >> 16) {
            Some((_, Some(idx))) => inst.dict.published().is_none().then_some((idx, 0)),
            // A `__slots__` member laid out over its class's names.
            _ => laid_out_member(code, inst, pc_name >> 16, ver).map(|(i, l)| (i | SLOT_FIELD, l)),
        };
        if let Some((idx, layout)) = found.filter(|_| ver != 0) {
            let k = usize::from(cache.next.get()) % cache.entries.len();
            cache.entries[k].set((ver, idx));
            cache.layouts[k].set(layout);
            cache.next.set(cache.next.get().wrapping_add(1));
        }
    }
    status
}

/// A [`FieldCache`] position naming a `__slots__` member's place in its
/// class's slot layout (see [`crate::types::PyInstance::laid_out_slot`])
/// rather than a split field's.
const SLOT_FIELD: u32 = 1 << 31;

/// Where `inst` keeps the attribute `code.names[name]` when reading it
/// means reading a `__slots__` member its class (at version `ver`) lays
/// out (see [`crate::types::TypeObject::fresh_slots`]): the member's
/// position in the layout, and the layout's word.
#[cold]
fn laid_out_member(
    code: &CodeObject,
    inst: &Rc<crate::types::PyInstance>,
    name: u32,
    ver: u64,
) -> Option<(u32, usize)> {
    use weavepy_compiler::InlineCache as IC;
    inst.cls_raw().slot_layout.get()?.as_ref()?;
    let name = code.names.get(name as usize)?;
    let obj = Object::Instance(inst.clone());
    match crate::specialize::attempt_specialize_load_attr(&obj, name) {
        IC::LoadAttrSlot { ver: v, .. } if v == ver => inst.laid_out_position(name),
        _ => None,
    }
}

unsafe extern "C" fn h_method(
    ctx: *mut Ctx<'static>,
    fr: *const Frame,
    ms: *const crate::MethodSlot,
    cache: *const MethodCache,
    t: u64,
    p: u64,
    name: u32,
) -> u32 {
    // SAFETY: called by native code with its context, a live frame, the
    // site's method slot (its code keeps the table), and its cache.
    let (c, (code, ext, _), ms, cache) = unsafe { (cx(ctx), (*fr).parts(), &*ms, &*cache) };
    let Some((func, recv)) = c.interp.plan_method_at(code, ms, value(t, p), name as u16) else {
        return 1;
    };
    let [a, b] = words(func);
    let [d, e] = words(recv);
    c.out = [a, b, d, e];
    // Remember the resolution for the in-line check.
    // SAFETY: the method load succeeded on a heap receiver (as `norm`).
    match (func, unsafe { &*(p as *const Object) }) {
        (V::Fn(fp), Object::Instance(inst)) if inst.dict.published().is_none() => {
            let cls = inst.cls_raw();
            let i = name as usize;
            if let (Some(Object::Str(n)), Some(&hash)) =
                (ext.name_objs.get(i), ext.name_hashes.get(i))
            {
                let limit = cls.shared_keys.get().map_or(0, |k| k.names_before(n, hash));
                cache.inst_ver.set(cls.attr_version.get());
                cache.inst_func.set(fp as u64);
                cache.inst_limit.set(limit as u64);
            }
        }
        (V::Fn(fp), Object::Type(cls)) if matches!(recv, V::Null) => {
            cache.type_ver.set(cls.attr_version.get());
            cache.type_func.set(fp as u64);
        }
        _ => {}
    }
    0
}

/// `0`/`1` for the comparison's result, `2` to decline.
extern "C" fn h_compare(ta: u64, pa: u64, tb: u64, pb: u64, kind: u32) -> u32 {
    // SAFETY: the translation checked `kind` names a comparison.
    let kind: CompareKind = unsafe { std::mem::transmute(kind as u8) };
    match compare(value(ta, pa), value(tb, pb), kind) {
        Some(b) => u32::from(b),
        None => 2,
    }
}

/// `0`/`1` for the operand's truth, `2` to decline.
extern "C" fn h_truth(t: u64, p: u64) -> u32 {
    match truth(value(t, p)) {
        Some(b) => u32::from(b),
        None => 2,
    }
}

extern "C" fn h_is(ta: u64, pa: u64, tb: u64, pb: u64) -> u32 {
    u32::from(is_same(value(ta, pa), value(tb, pb)))
}

unsafe extern "C" fn h_unary(ctx: *mut Ctx<'static>, t: u64, p: u64, kind: u32) -> u32 {
    // SAFETY: called by native code with its context.
    let c = unsafe { cx(ctx) };
    match unary(value(t, p), kind as u8) {
        Some(v) => {
            c.put(v);
            0
        }
        None => 1,
    }
}

unsafe extern "C" fn h_binary(
    ctx: *mut Ctx<'static>,
    ta: u64,
    pa: u64,
    tb: u64,
    pb: u64,
    kind: u32,
) -> u32 {
    // SAFETY: called by native code with its context.
    let c = unsafe { cx(ctx) };
    match binary(value(ta, pa), value(tb, pb), kind as u8) {
        Some(v) => {
            c.put(v);
            0
        }
        None => 1,
    }
}

unsafe extern "C" fn h_new(ctx: *mut Ctx<'static>, dict: u32) -> u32 {
    // SAFETY: called by native code with its context.
    let c = unsafe { cx(ctx) };
    match new_container(&mut c.owned, dict != 0) {
        Some(v) => {
            c.put(v);
            0
        }
        None => 1,
    }
}

/// `a[b]` (see [`subscr`]).
unsafe extern "C" fn h_subscr(ctx: *mut Ctx<'static>, ta: u64, pa: u64, tb: u64, pb: u64) -> u32 {
    // SAFETY: called by native code with its context.
    let c = unsafe { cx(ctx) };
    match subscr(&mut c.owned, value(ta, pa), value(tb, pb)) {
        Some(v) => {
            c.put(v);
            0
        }
        None => 1,
    }
}

/// The evaluation's own `recv.name = val` (only the top frame stores).
/// A store nothing after it can decline (`last`, decided when the plan
/// compiled), with no store buffered before it, lands at once: the
/// evaluation can no longer abandon it. Any other store is buffered for
/// the return to commit.
unsafe extern "C" fn h_store(
    ctx: *mut Ctx<'static>,
    tr: u64,
    pr: u64,
    tv: u64,
    pv: u64,
    pc_name: u32,
    last: u32,
) -> u32 {
    // SAFETY: called by native code with its context.
    let c = unsafe { cx(ctx) };
    let V::R(rp) = value(tr, pr) else {
        return 1;
    };
    let (pc, name) = (pc_name as u16, (pc_name >> 16) as u16);
    if last != 0 && c.pend.n == 0 {
        // SAFETY: as `norm`; the receiver is rooted by an argument or a
        // field of one, and nothing has run since it was read. A handle
        // of its own keeps it while the store runs (a field's slot may
        // move when the store grows the instance holding it).
        let inst = match unsafe { &*rp } {
            Object::Instance(inst) => inst.clone(),
            _ => return 1,
        };
        let Some(v) = super::to_object(value(tv, pv)) else {
            return 1;
        };
        // SAFETY: the evaluation's own frame (its code is alive).
        let (code, _, _) = unsafe { c.top.parts() };
        // On `true` the value moved into the instance; a declined store
        // touched nothing, and the evaluation declines whole.
        let v = std::mem::ManuallyDrop::new(v);
        if Interpreter::core_store_attr(code, &inst, usize::from(pc), u32::from(name), &v) {
            return 0;
        }
        drop(std::mem::ManuallyDrop::into_inner(v));
        return 1;
    }
    match plan_store(
        c.args,
        &mut c.owned,
        &mut c.pend,
        rp,
        value(tv, pv),
        pc,
        name,
    ) {
        Some(()) => 0,
        None => 1,
    }
}

/// A leaf call at `pc` of frame `fr` (at in-line `depth`) on the registers
/// whose words start at `w`: the callee, its self slot, then `argc`
/// arguments. A pure-leaf Python callee is evaluated one level down
/// (natively, once compiled); a read-only builtin runs on borrowed copies
/// of its arguments.
unsafe extern "C" fn h_call<const EFFECT: bool>(
    ctx: *mut Ctx<'static>,
    fr: *const Frame,
    w: *const u64,
    argc_depth: u32,
    pc: u32,
) -> u32 {
    // SAFETY: called by native code with its context and a live frame.
    let (c, (code, _, _)) = unsafe { (cx(ctx), (*fr).parts()) };
    let (argc, depth) = (argc_depth & 0xff, (argc_depth >> 8) as u8);
    let nest = c.nest + depth;
    // Only while no store is buffered (the callee would not see one).
    if (EFFECT && c.pend.n > 0) || nest >= NEST {
        return 1;
    }
    // SAFETY: native code spilled `argc + 2` registers' words at `w`.
    let reg = |k: usize| unsafe { value(*w.add(2 * k), *w.add(2 * k + 1)) };
    let first = if matches!(reg(1), V::Null) { 2 } else { 1 };
    let n = argc as usize + 2 - first;
    if n > 8 {
        return 1;
    }
    let (fp, native) = match reg(0) {
        V::Fn(fp) => (fp, None),
        V::Bi(b) => (std::ptr::null(), Some((b, None))),
        // SAFETY: as `norm`.
        V::R(p) => match unsafe { &*p } {
            Object::Function(func) => (Rc::as_ptr(func), None),
            Object::Builtin(b) => (std::ptr::null(), Some((Rc::as_ptr(b), Some(b)))),
            _ => return 1,
        },
        _ => return 1,
    };
    // Scalar arguments are staged as objects (no drop glue); heap ones are
    // borrowed in place.
    let mut staged = [const { std::mem::MaybeUninit::<Object>::uninit() }; 8];
    let mut ptrs = [std::ptr::null::<Object>(); 8];
    for k in 0..n {
        let o = match reg(first + k) {
            V::R(p) => {
                ptrs[k] = p;
                continue;
            }
            V::I(i) => Object::Int(i),
            V::F(x) => Object::Float(x),
            V::B(b) => Object::Bool(b),
            V::N => Object::None,
            V::Fn(_) | V::Bi(_) | V::Null => return 1,
        };
        ptrs[k] = staged[k].write(o);
    }
    if let Some((b, rc)) = native {
        // A read-only builtin, on borrowed copies of the arguments: never
        // dropped, so no reference moves.
        let mut copies = [const { std::mem::MaybeUninit::<Object>::uninit() }; 8];
        for k in 0..n {
            // SAFETY: each pointer names a live object; the copy is never
            // dropped.
            copies[k].write(unsafe { std::ptr::read(ptrs[k]) });
        }
        // SAFETY: the first `n` copies were written.
        let args = unsafe { std::slice::from_raw_parts(copies.as_ptr().cast::<Object>(), n) };
        let Some(r) = c.interp.leaf_pure_builtin(code, pc as usize, b, rc, args) else {
            return 1;
        };
        return match c.owned.own(r) {
            Some(v) => {
                c.put(v);
                0
            }
            None => 1,
        };
    }
    // SAFETY: the class or the namespace holds the callee, and nothing
    // here runs code that could release it.
    let callee = unsafe { &*fp };
    // SAFETY: GIL-serialized raw read of the code cell.
    let ccode: &Rc<CodeObject> = unsafe { &*callee.code.as_ptr() };
    if !crate::code_is_pure_leaf(ccode)
        || !Interpreter::leaf_code_ok(ccode)
        || n != crate::leaf_arity(ccode)
        || ccode.has_varkeywords
        || crate::recursion::current_depth() + usize::from(nest) + 1
            >= crate::recursion::recursion_limit()
    {
        return 1;
    }
    let v = match c
        .interp
        .leaf_eval_nested(ccode, callee, &ptrs[..n], nest + 1)
    {
        Some(LeafRet::Borrowed(v)) => v,
        Some(LeafRet::Owned(r)) => match c.owned.own(r) {
            Some(v) => v,
            None => return 1,
        },
        None => return 1,
    };
    c.put(v);
    0
}

// ---- the compiler -------------------------------------------------------

/// The process's plan compiler. Code it emits lives as long as the
/// process: plans are shared between threads, and their code is never
/// freed.
struct Engine {
    module: JITModule,
    ctx: cranelift_codegen::Context,
    fbctx: FunctionBuilderContext,
    ptr: types::Type,
    next: u32,
}

// SAFETY: the engine is only used under its mutex.
unsafe impl Send for Engine {}

static ENGINE: std::sync::Mutex<Option<Option<Engine>>> = std::sync::Mutex::new(None);

impl Engine {
    fn new() -> Option<Engine> {
        if !tags_match() {
            return None;
        }
        let mut flags = settings::builder();
        flags.set("use_colocated_libcalls", "false").ok()?;
        flags.set("is_pic", "false").ok()?;
        // No mid-end optimization: a plan's lowering emits the code it
        // wants, and the pass cost more to run than it saved.
        // `WEAVEPY_JIT_OPT=1` turns it on.
        let opt = if std::env::var_os("WEAVEPY_JIT_OPT").is_some_and(|v| v == "1") {
            "speed"
        } else {
            "none"
        };
        flags.set("opt_level", opt).ok()?;
        if std::env::var_os("WEAVEPY_JIT_QUICK").is_some() {
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
            next: 0,
        })
    }
}

/// Whether to verify the IR of every compiled plan and report failures
/// (`WEAVEPY_PLAN_VERIFY`, a debugging aid).
fn verify() -> bool {
    static VERIFY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *VERIFY.get_or_init(|| std::env::var_os("WEAVEPY_PLAN_VERIFY").is_some())
}

/// Compile `plan` (the translation of `code`) for `effect` mode; `None`
/// leaves it to the runner. A compiler panic turns plan compilation off
/// for the process rather than taking the interpreter down.
fn compile(
    code: &CodeObject,
    ext: &CodeConstObjects,
    plan: &LeafPlan,
    f: &Rc<PyFunction>,
    effect: bool,
) -> Option<Native> {
    crate::tier2::ensure_obj_layout();
    let mut guard = ENGINE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let engine = guard.get_or_insert_with(Engine::new).as_mut()?;
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        compile_with(engine, code, ext, plan, f, effect)
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
    plan: &LeafPlan,
    f: &Rc<PyFunction>,
    effect: bool,
) -> Option<Native> {
    engine.module.clear_context(&mut engine.ctx);
    let ptr = engine.ptr;
    let sig = &mut engine.ctx.func.signature;
    sig.params.push(AbiParam::new(ptr));
    sig.params.push(AbiParam::new(ptr));
    sig.returns.push(AbiParam::new(types::I32));
    let name = format!("wpplan_{}", engine.next);
    engine.next += 1;
    let id = engine
        .module
        .declare_function(&name, Linkage::Local, &engine.ctx.func.signature)
        .ok()?;
    let lowered = {
        let b = FunctionBuilder::new(&mut engine.ctx.func, &mut engine.fbctx);
        let mut lower = Lower {
            b,
            effect,
            ptr,
            ctx: Value::from_u32(0),
            decline: Block::from_u32(0),
            sigs: Sigs::default(),
            depth: 0,
            frames: Vec::new(),
            keep: Vec::new(),
            fields: Vec::new(),
            globals: Vec::new(),
            methods: Vec::new(),
        };
        let done = lower.lower(code, ext, plan, f);
        let Lower {
            b,
            depth,
            frames,
            keep,
            fields,
            globals,
            methods,
            ..
        } = lower;
        done.map(|()| {
            b.finalize();
            (depth, frames, keep, fields, globals, methods)
        })
    };
    let Some((depth, frames, keep, fields, globals, methods)) = lowered else {
        // An unfinished function leaves the builder's state behind.
        engine.fbctx = FunctionBuilderContext::new();
        engine.module.clear_context(&mut engine.ctx);
        return None;
    };
    let defined = engine.module.define_function(id, &mut engine.ctx);
    if let (Err(e), true) = (&defined, verify()) {
        eprintln!(
            "weavepy: plan for {:?} failed to compile: {e:?}",
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
        depth,
        _frames: frames,
        _keep: keep,
        _fields: fields,
        _globals: globals,
        _methods: methods,
    })
}

/// The helper signatures one function imports, by shape.
#[derive(Default)]
struct Sigs {
    map: Vec<(Vec<types::Type>, SigRef)>,
}

/// One plan's registers and blocks while it is lowered (the evaluation's
/// own, or an in-line callee's).
struct FrameLower<'p> {
    plan: &'p LeafPlan,
    code: &'p CodeObject,
    /// The frame descriptor the helpers get.
    fr: Value,
    /// In-line depth (0 for the evaluation's own frame).
    depth: u8,
    tags: Vec<Variable>,
    pays: Vec<Variable>,
    blocks: Vec<Block>,
    /// Where a callee's return goes: the caller's result register (in the
    /// caller's variables) and the block after the call.
    ret: Option<(Variable, Variable, Block)>,
    /// Whether an attribute store may have run before an op (only the
    /// evaluation's own frame stores).
    may_store: Vec<bool>,
    /// The function the frame runs: an in-line callee's (its guard proved
    /// it), or the one the plan was compiled for (the evaluation's own
    /// may be another with the same code).
    func: *const PyFunction,
}

struct Lower<'b> {
    b: FunctionBuilder<'b>,
    effect: bool,
    ptr: types::Type,
    ctx: Value,
    decline: Block,
    sigs: Sigs,
    /// The deepest in-line callee.
    depth: u8,
    // Boxed: the code embeds each descriptor's address.
    #[allow(clippy::vec_box)]
    frames: Vec<Box<Frame>>,
    keep: Vec<(crate::sync::Weak<PyFunction>, Rc<CodeObject>)>,
    #[allow(clippy::vec_box)]
    fields: Vec<Box<FieldCache>>,
    #[allow(clippy::vec_box)]
    globals: Vec<Box<GlobalCache>>,
    #[allow(clippy::vec_box)]
    methods: Vec<Box<MethodCache>>,
}

impl Lower<'_> {
    fn sig(&mut self, params: &[types::Type]) -> SigRef {
        if let Some((_, s)) = self.sigs.map.iter().find(|(p, _)| p == params) {
            return *s;
        }
        let mut sig = Signature::new(self.b.func.signature.call_conv);
        for &t in params {
            sig.params.push(AbiParam::new(t));
        }
        sig.returns.push(AbiParam::new(types::I32));
        let s = self.b.import_signature(sig);
        self.sigs.map.push((params.to_vec(), s));
        s
    }

    /// Call the helper at `addr`: its `u32` result.
    fn call(&mut self, addr: usize, args: &[Value]) -> Value {
        let params: Vec<types::Type> = args
            .iter()
            .map(|&a| self.b.func.dfg.value_type(a))
            .collect();
        let sig = self.sig(&params);
        let callee = self.b.ins().iconst(self.ptr, addr as i64);
        let inst = self.b.ins().call_indirect(sig, callee, args);
        self.b.inst_results(inst)[0]
    }

    /// Continue in a fresh block when `status` is `0`; decline otherwise.
    fn check(&mut self, status: Value) {
        let ok = self.b.create_block();
        self.b.ins().brif(status, self.decline, &[], ok, &[]);
        self.b.switch_to_block(ok);
    }

    fn get(&mut self, fl: &FrameLower<'_>, r: u8) -> (Value, Value) {
        let r = usize::from(r);
        (self.b.use_var(fl.tags[r]), self.b.use_var(fl.pays[r]))
    }

    fn set(&mut self, fl: &FrameLower<'_>, r: u8, t: Value, p: Value) {
        let r = usize::from(r);
        self.b.def_var(fl.tags[r], t);
        self.b.def_var(fl.pays[r], p);
    }

    /// Branch to `miss` when `cond`; continue in a fresh block otherwise.
    fn miss_if(&mut self, cond: Value, miss: Block) {
        let ok = self.b.create_block();
        self.b.ins().brif(cond, miss, &[], ok, &[]);
        self.b.switch_to_block(ok);
    }

    /// The member slots of the instance at `inst` (its payload pointer),
    /// as [`crate::types::PyInstance::laid_out_slot`] reads them: their
    /// values' pointer, and whether they're unreadable here (borrowed for
    /// a write, or not laid out over the layout whose word is `layout`).
    /// The caller checks that cells are unshared.
    fn laid_out_slots(
        &mut self,
        l: &weavepy_jit::ObjLayout,
        inst: Value,
        layout: Value,
    ) -> (Value, Value) {
        let f = MemFlags::trusted();
        let borrow = self.b.ins().sload32(f, inst, l.inst_slots_borrow);
        let form = self.b.ins().uload8(types::I32, f, inst, l.inst_slots_tag);
        let names = self.b.ins().load(self.ptr, f, inst, l.inst_slots_layout);
        let vals = self.b.ins().load(self.ptr, f, inst, l.inst_slots_values);
        let busy = self.b.ins().icmp_imm(IntCC::SignedLessThan, borrow, 0);
        let other = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, form, i64::from(l.slots_laid_out));
        let foreign = self.b.ins().icmp(IntCC::NotEqual, names, layout);
        let bad = self.b.ins().bor(busy, other);
        let bad = self.b.ins().bor(bad, foreign);
        (vals, bad)
    }

    /// [`h_field`]'s first cache entry in line: the receiver register
    /// `(t, p)` is a plain instance whose class has the entry's version
    /// and whose values are split over its class's names, holding the
    /// entry's position (or, for a slot entry, whose member slots are laid
    /// out over the entry's layout, the one there set, at a `slot_site`);
    /// the field's value as register words. Anything else (and, with
    /// buffered stores, everything) branches to `miss`.
    #[allow(clippy::too_many_arguments)]
    fn inline_field(
        &mut self,
        l: &weavepy_jit::ObjLayout,
        t: Value,
        p: Value,
        cache: i64,
        effect: bool,
        slot_site: bool,
        miss: Block,
    ) -> (Value, Value) {
        let f = MemFlags::trusted();
        let ptr = self.ptr;
        // (The checks are grouped by what each group's loads need proven:
        // fewer blocks compile faster.)
        let not_ref = self.b.ins().icmp_imm(IntCC::NotEqual, t, T_R as i64);
        self.miss_if(not_ref, miss);
        let otag = self.b.ins().uload8(types::I32, f, p, 0);
        let mut bad = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, otag, i64::from(l.tag_instance));
        if effect {
            let n = self.b.ins().load(types::I64, f, self.ctx, PEND_N_OFFSET);
            let pending = self.b.ins().icmp_imm(IntCC::NotEqual, n, 0);
            bad = self.b.ins().bor(bad, pending);
        }
        self.miss_if(bad, miss);
        // An instance: its class, its split values, and the site's entry.
        let inst = self.b.ins().load(ptr, f, p, 8);
        let cls = self.b.ins().load(ptr, f, inst, l.inst_class);
        let ver = self.b.ins().load(types::I64, f, cls, l.type_attr_version);
        let at = self.b.ins().iconst(ptr, cache);
        let want = self.b.ins().load(types::I64, f, at, FIELD_VER_OFFSET);
        let idx = self.b.ins().uload32(f, at, FIELD_IDX_OFFSET);
        let lazy = self.b.ins().load(ptr, f, inst, l.inst_dict_lazy);
        let flag = self.b.ins().iconst(ptr, l.cells_unguarded as i64);
        let shared = self.b.ins().uload8(types::I64, f, flag, 0);
        let borrow = self.b.ins().sload32(f, inst, l.inst_split_borrow);
        let block = self.b.ins().load(ptr, f, inst, l.inst_split_block);
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
        // A slot entry fails the split checks (its position is out of any
        // split range), so the slot read waits behind them; it's compiled
        // only for a site that has read member slots (the helper reads one
        // otherwise).
        let slots = l.slots_ok && slot_site;
        let slot = if slots { self.b.create_block() } else { miss };
        self.miss_if(bad, slot);
        // A block over the class's names that holds the position.
        let keys = self.b.ins().load(ptr, f, block, l.split_keys);
        let ckeys = self.b.ins().load(ptr, f, cls, l.type_shared_keys);
        let len = self.b.ins().uload32(f, block, l.split_len);
        let foreign = self.b.ins().icmp(IntCC::NotEqual, keys, ckeys);
        let absent = self
            .b
            .ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, idx, len);
        let bad = self.b.ins().bor(foreign, absent);
        self.miss_if(bad, slot);
        let off = self.b.ins().ishl_imm(idx, 4);
        let v = self.b.ins().iadd(block, off);
        let v = self.b.ins().iadd_imm(v, i64::from(l.split_values));
        let v = if slots {
            let read = self.b.create_block();
            self.b.append_block_param(read, ptr);
            self.b.ins().jump(read, &[v.into()]);
            // A member slot laid out over the entry's layout, and set, with
            // the class checked and cells unshared. (Past the entry's kind,
            // everything is read again here: values the split path reads
            // stay unneeded past it.)
            self.b.switch_to_block(slot);
            let split_miss = self.b.ins().band_imm(idx, i64::from(SLOT_FIELD));
            let split_miss = self.b.ins().icmp_imm(IntCC::Equal, split_miss, 0);
            self.miss_if(split_miss, miss);
            let inst = self.b.ins().load(ptr, f, p, 8);
            let cls = self.b.ins().load(ptr, f, inst, l.inst_class);
            let ver = self.b.ins().load(types::I64, f, cls, l.type_attr_version);
            let at = self.b.ins().iconst(ptr, cache);
            let want = self.b.ins().load(types::I64, f, at, FIELD_VER_OFFSET);
            let flag = self.b.ins().iconst(ptr, l.cells_unguarded as i64);
            let shared = self.b.ins().uload8(types::I64, f, flag, 0);
            let unset = self.b.ins().icmp_imm(IntCC::Equal, want, 0);
            let stale = self.b.ins().icmp(IntCC::NotEqual, ver, want);
            let shared = self.b.ins().icmp_imm(IntCC::NotEqual, shared, 0);
            let bad = self.b.ins().bor(unset, stale);
            let bad = self.b.ins().bor(bad, shared);
            self.miss_if(bad, miss);
            let layout = self.b.ins().load(ptr, f, at, FIELD_LAYOUT_OFFSET);
            let (vals, bad) = self.laid_out_slots(l, inst, layout);
            self.miss_if(bad, miss);
            let i = self.b.ins().band_imm(idx, i64::from(!SLOT_FIELD));
            let off = self.b.ins().ishl_imm(i, 4);
            let v = self.b.ins().iadd(vals, off);
            let vt = self.b.ins().uload8(types::I32, f, v, 0);
            let none = self
                .b
                .ins()
                .icmp_imm(IntCC::Equal, vt, i64::from(l.tag_unbound));
            self.miss_if(none, miss);
            self.b.ins().jump(read, &[v.into()]);
            self.b.switch_to_block(read);
            self.b.block_params(read)[0]
        } else {
            v
        };
        // `norm`: the scalars by value, anything else by reference.
        let vt = self.b.ins().uload8(types::I64, f, v, 0);
        let word = self.b.ins().load(types::I64, f, v, 8);
        let byte = self.b.ins().uload8(types::I64, f, v, 1);
        let zero = self.b.ins().iconst(types::I64, 0);
        let mut rt = self.b.ins().iconst(types::I64, T_R as i64);
        let mut rp = v;
        for (tag, vtag, pay) in [
            (l.tag_int, T_I, word),
            (l.tag_float, T_F, word),
            (l.tag_bool, T_B, byte),
            (l.tag_none, T_N, zero),
        ] {
            let hit = self.b.ins().icmp_imm(IntCC::Equal, vt, i64::from(tag));
            let tv = self.b.ins().iconst(types::I64, vtag as i64);
            rt = self.b.ins().select(hit, tv, rt);
            rp = self.b.ins().select(hit, pay, rp);
        }
        (rt, rp)
    }

    /// [`h_method`]'s cache in line, for the receiver register `(t, p)`: an
    /// instance whose class has the cached version and that holds none of
    /// the class's names from the method's on (in its split values over
    /// the class's names, with no dictionary of its own), or a class with
    /// the cached version. Jumps to `done` with the function's and the
    /// self slot's words, or to `slow`.
    fn inline_method(
        &mut self,
        l: &weavepy_jit::ObjLayout,
        t: Value,
        p: Value,
        cache: i64,
        done: Block,
        slow: Block,
    ) {
        let f = MemFlags::trusted();
        let ptr = self.ptr;
        let not_ref = self.b.ins().icmp_imm(IntCC::NotEqual, t, T_R as i64);
        self.miss_if(not_ref, slow);
        let otag = self.b.ins().uload8(types::I32, f, p, 0);
        let at = self.b.ins().iconst(ptr, cache);
        let is_inst = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, otag, i64::from(l.tag_instance));
        let inst_b = self.b.create_block();
        let other = self.b.create_block();
        self.b.ins().brif(is_inst, inst_b, &[], other, &[]);
        // A class: its version.
        self.b.switch_to_block(other);
        let is_type = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, otag, i64::from(l.tag_type));
        let type_b = self.b.create_block();
        self.b.ins().brif(is_type, type_b, &[], slow, &[]);
        self.b.switch_to_block(type_b);
        let cls = self.b.ins().load(ptr, f, p, 8);
        let ver = self.b.ins().load(types::I64, f, cls, l.type_attr_version);
        let want = self.b.ins().load(types::I64, f, at, MC_TYPE_VER);
        let func = self.b.ins().load(types::I64, f, at, MC_TYPE_FUNC);
        let unset = self.b.ins().icmp_imm(IntCC::Equal, want, 0);
        let stale = self.b.ins().icmp(IntCC::NotEqual, ver, want);
        let bad = self.b.ins().bor(unset, stale);
        self.miss_if(bad, slow);
        let tf = self.b.ins().iconst(types::I64, T_FN as i64);
        let tn = self.b.ins().iconst(types::I64, T_NULL as i64);
        let zero = self.b.ins().iconst(types::I64, 0);
        self.b
            .ins()
            .jump(done, &[tf.into(), func.into(), tn.into(), zero.into()]);
        // An instance: its class's version, and its fields.
        self.b.switch_to_block(inst_b);
        let inst = self.b.ins().load(ptr, f, p, 8);
        let cls = self.b.ins().load(ptr, f, inst, l.inst_class);
        let ver = self.b.ins().load(types::I64, f, cls, l.type_attr_version);
        let want = self.b.ins().load(types::I64, f, at, MC_INST_VER);
        let func = self.b.ins().load(types::I64, f, at, MC_INST_FUNC);
        let limit = self.b.ins().load(types::I64, f, at, MC_INST_LIMIT);
        let lazy = self.b.ins().load(ptr, f, inst, l.inst_dict_lazy);
        let flag = self.b.ins().iconst(ptr, l.cells_unguarded as i64);
        let shared = self.b.ins().uload8(types::I64, f, flag, 0);
        let borrow = self.b.ins().sload32(f, inst, l.inst_split_borrow);
        let block = self.b.ins().load(ptr, f, inst, l.inst_split_block);
        let unset = self.b.ins().icmp_imm(IntCC::Equal, want, 0);
        let stale = self.b.ins().icmp(IntCC::NotEqual, ver, want);
        let busy = self.b.ins().icmp_imm(IntCC::SignedLessThan, borrow, 0);
        let other = self.b.ins().bor(lazy, shared);
        let other = self.b.ins().icmp_imm(IntCC::NotEqual, other, 0);
        let bad = self.b.ins().bor(unset, stale);
        let bad = self.b.ins().bor(bad, busy);
        let bad = self.b.ins().bor(bad, other);
        self.miss_if(bad, slow);
        let tf = self.b.ins().iconst(types::I64, T_FN as i64);
        let tr = self.b.ins().iconst(types::I64, T_R as i64);
        let hit_args = [tf.into(), func.into(), tr.into(), p.into()];
        // No fields at all, or none from the method's name on.
        let fields = self.b.create_block();
        let empty = self.b.ins().icmp_imm(IntCC::Equal, block, 0);
        self.b.ins().brif(empty, done, &hit_args, fields, &[]);
        self.b.switch_to_block(fields);
        let keys = self.b.ins().load(ptr, f, block, l.split_keys);
        let ckeys = self.b.ins().load(ptr, f, cls, l.type_shared_keys);
        // (`uload32` zero-extends to `i64` itself.)
        let len = self.b.ins().uload32(f, block, l.split_len);
        let foreign = self.b.ins().icmp(IntCC::NotEqual, keys, ckeys);
        let shadows = self.b.ins().icmp(IntCC::UnsignedGreaterThan, len, limit);
        let bad = self.b.ins().bor(foreign, shadows);
        self.miss_if(bad, slow);
        self.b.ins().jump(done, &hit_args);
    }

    /// [`h_global`]'s cache in line: the frame runs its compile-time
    /// function, whose globals still have the cached stamp, under the
    /// cached value epoch. Jumps to `done` with the value's words, or to
    /// `slow`.
    fn inline_global(&mut self, fl: &FrameLower<'_>, cache: i64, done: Block, slow: Block) {
        let f = MemFlags::trusted();
        let ptr = self.ptr;
        if fl.depth == 0 {
            // The evaluation's own function may be another with this code.
            let top = self.b.ins().load(ptr, f, self.ctx, TOP_F_OFFSET);
            let other = self.b.ins().icmp_imm(IntCC::NotEqual, top, fl.func as i64);
            self.miss_if(other, slow);
        }
        // SAFETY: the frame's function is alive while its code runs (see
        // `FrameLower::func`), and so its globals.
        let stamp_at =
            unsafe { (*fl.func).globals.as_ptr() } as usize + crate::object::DictData::STAMP_OFFSET;
        let stamp_at = self.b.ins().iconst(ptr, stamp_at as i64);
        let epoch_at = self
            .b
            .ins()
            .iconst(ptr, crate::object::global_value_epoch_ptr() as i64);
        let at = self.b.ins().iconst(ptr, cache);
        let stamp = self.b.ins().load(types::I64, f, stamp_at, 0);
        let epoch = self.b.ins().load(types::I64, f, epoch_at, 0);
        let want = self.b.ins().load(types::I64, f, at, GC_STAMP);
        let want_epoch = self.b.ins().load(types::I64, f, at, GC_EPOCH);
        let t = self.b.ins().load(types::I64, f, at, GC_TAG);
        let p = self.b.ins().load(types::I64, f, at, GC_PAY);
        let unset = self.b.ins().icmp_imm(IntCC::Equal, want, 0);
        let stale = self.b.ins().icmp(IntCC::NotEqual, stamp, want);
        let moved = self.b.ins().icmp(IntCC::NotEqual, epoch, want_epoch);
        let bad = self.b.ins().bor(unset, stale);
        let bad = self.b.ins().bor(bad, moved);
        self.miss_if(bad, slow);
        self.b.ins().jump(done, &[t.into(), p.into()]);
    }

    /// The interpreter's indexed store (`core_store_attr` through the
    /// site's [`crate::FieldSlot`] at `slot`) in line, for a scalar value
    /// over a scalar field: the receiver register is a plain instance whose
    /// class has the slot's version and whose values, split over its
    /// class's names and borrowed by nobody, hold the slot's position. Its
    /// old value owns nothing, and neither does the new one (no write
    /// barrier, no release). Jumps to `done` stored, or to `slow` untouched.
    fn inline_store(
        &mut self,
        l: &weavepy_jit::ObjLayout,
        (tr, pr): (Value, Value),
        (tv, pv): (Value, Value),
        slot: i64,
        done: Block,
        slow: Block,
    ) {
        let f = MemFlags::trusted();
        let ptr = self.ptr;
        // The value: a scalar register.
        let is_i = self.b.ins().icmp_imm(IntCC::Equal, tv, T_I as i64);
        let is_f = self.b.ins().icmp_imm(IntCC::Equal, tv, T_F as i64);
        let is_b = self.b.ins().icmp_imm(IntCC::Equal, tv, T_B as i64);
        let is_n = self.b.ins().icmp_imm(IntCC::Equal, tv, T_N as i64);
        let any = self.b.ins().bor(is_i, is_f);
        let any = self.b.ins().bor(any, is_b);
        let any = self.b.ins().bor(any, is_n);
        let not_scalar = self.b.ins().icmp_imm(IntCC::Equal, any, 0);
        let not_ref = self.b.ins().icmp_imm(IntCC::NotEqual, tr, T_R as i64);
        let bad = self.b.ins().bor(not_scalar, not_ref);
        self.miss_if(bad, slow);
        let otag = self.b.ins().uload8(types::I32, f, pr, 0);
        let not_inst = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, otag, i64::from(l.tag_instance));
        self.miss_if(not_inst, slow);
        let inst = self.b.ins().load(ptr, f, pr, 8);
        let cls = self.b.ins().load(ptr, f, inst, l.inst_class);
        let ver = self.b.ins().load(types::I64, f, cls, l.type_attr_version);
        let at = self.b.ins().iconst(ptr, slot);
        let want = self
            .b
            .ins()
            .load(types::I64, f, at, crate::FIELD_SLOT_VER as i32);
        let idx = self.b.ins().uload32(f, at, crate::FIELD_SLOT_IDX as i32);
        let lazy = self.b.ins().load(ptr, f, inst, l.inst_dict_lazy);
        let flag = self.b.ins().iconst(ptr, l.cells_unguarded as i64);
        let shared = self.b.ins().uload8(types::I64, f, flag, 0);
        let watched = self.b.ins().iconst(ptr, l.dict_watchers as i64);
        let watched = self.b.ins().uload8(types::I64, f, watched, 0);
        let borrow = self.b.ins().sload32(f, inst, l.inst_split_borrow);
        let block = self.b.ins().load(ptr, f, inst, l.inst_split_block);
        let unset = self.b.ins().icmp_imm(IntCC::Equal, want, 0);
        let stale = self.b.ins().icmp(IntCC::NotEqual, ver, want);
        let busy = self.b.ins().icmp_imm(IntCC::NotEqual, borrow, 0);
        let empty = self.b.ins().icmp_imm(IntCC::Equal, block, 0);
        let other = self.b.ins().bor(lazy, shared);
        let other = self.b.ins().bor(other, watched);
        let other = self.b.ins().icmp_imm(IntCC::NotEqual, other, 0);
        let bad = self.b.ins().bor(unset, stale);
        let bad = self.b.ins().bor(bad, busy);
        let bad = self.b.ins().bor(bad, empty);
        let bad = self.b.ins().bor(bad, other);
        self.miss_if(bad, slow);
        let keys = self.b.ins().load(ptr, f, block, l.split_keys);
        let ckeys = self.b.ins().load(ptr, f, cls, l.type_shared_keys);
        let len = self.b.ins().uload32(f, block, l.split_len);
        let foreign = self.b.ins().icmp(IntCC::NotEqual, keys, ckeys);
        let absent = self
            .b
            .ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, idx, len);
        let bad = self.b.ins().bor(foreign, absent);
        self.miss_if(bad, slow);
        let off = self.b.ins().ishl_imm(idx, 4);
        let v = self.b.ins().iadd(block, off);
        let v = self.b.ins().iadd_imm(v, i64::from(l.split_values));
        // The old value: a scalar, which owns nothing.
        let old = self.b.ins().uload8(types::I32, f, v, 0);
        let mut owns = self.b.ins().iconst(types::I8, 1);
        for tag in [l.tag_int, l.tag_float, l.tag_bool, l.tag_none] {
            let hit = self.b.ins().icmp_imm(IntCC::Equal, old, i64::from(tag));
            let clear = self.b.ins().bxor_imm(hit, 1);
            owns = self.b.ins().band(owns, clear);
        }
        self.miss_if(owns, slow);
        // The new value's tag byte and payload (a bool's byte at offset 1).
        let t_int = self.b.ins().iconst(types::I32, i64::from(l.tag_int));
        let t_float = self.b.ins().iconst(types::I32, i64::from(l.tag_float));
        let t_bool = self.b.ins().iconst(types::I32, i64::from(l.tag_bool));
        let t_none = self.b.ins().iconst(types::I32, i64::from(l.tag_none));
        let tag = self.b.ins().select(is_i, t_int, t_none);
        let tag = self.b.ins().select(is_f, t_float, tag);
        let tag = self.b.ins().select(is_b, t_bool, tag);
        self.b.ins().istore8(f, tag, v, 0);
        let byte = self.b.ins().band_imm(pv, 1);
        self.b.ins().istore8(f, byte, v, 1);
        self.b.ins().store(f, pv, v, 8);
        self.b.ins().jump(done, &[]);
    }

    /// A class attribute the site's stamp remembers, in line: the receiver
    /// register `(t, p)` is a class at the stamp's version, which resolved
    /// the name to a scalar (see `class_attr_fill`). Jumps to `done` with
    /// its words, or to `slow`.
    fn inline_class_attr(
        &mut self,
        l: &weavepy_jit::ObjLayout,
        t: Value,
        p: Value,
        slot: i64,
        done: Block,
        slow: Block,
    ) {
        let f = MemFlags::trusted();
        let ptr = self.ptr;
        let not_ref = self.b.ins().icmp_imm(IntCC::NotEqual, t, T_R as i64);
        self.miss_if(not_ref, slow);
        let otag = self.b.ins().uload8(types::I32, f, p, 0);
        let not_type = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, otag, i64::from(l.tag_type));
        self.miss_if(not_type, slow);
        let cls = self.b.ins().load(ptr, f, p, 8);
        let ver = self.b.ins().load(types::I64, f, cls, l.type_attr_version);
        let at = self.b.ins().iconst(ptr, slot);
        let want = self.b.ins().load(types::I64, f, at, 0);
        let tag = self.b.ins().load(types::I64, f, at, 8);
        let bits = self.b.ins().load(types::I64, f, at, 16);
        let unset = self.b.ins().icmp_imm(IntCC::Equal, want, 0);
        let stale = self.b.ins().icmp(IntCC::NotEqual, ver, want);
        let bad = self.b.ins().bor(unset, stale);
        self.miss_if(bad, slow);
        for (stamp, vtag) in CLASS_ATTR_SCALARS {
            let next = self.b.create_block();
            let hit = self.b.ins().icmp_imm(IntCC::Equal, tag, stamp as i64);
            let tv = self.b.ins().iconst(types::I64, vtag as i64);
            let pay = if vtag == T_N {
                self.b.ins().iconst(types::I64, 0)
            } else {
                bits
            };
            self.b
                .ins()
                .brif(hit, done, &[tv.into(), pay.into()], next, &[]);
            self.b.switch_to_block(next);
        }
        self.b.ins().jump(slow, &[]);
    }

    /// Load the result words a helper left at `ctx.out[k..k + 2]` into `r`.
    fn set_out(&mut self, fl: &FrameLower<'_>, r: u8, k: i32) {
        let flags = MemFlags::trusted();
        let t = self.b.ins().load(types::I64, flags, self.ctx, 8 * k);
        let p = self.b.ins().load(types::I64, flags, self.ctx, 8 * k + 8);
        self.set(fl, r, t, p);
    }

    /// A boolean (`i8`) into register `r` as `V::B`.
    fn set_bool(&mut self, fl: &FrameLower<'_>, r: u8, bit: Value) {
        let t = self.b.ins().iconst(types::I64, T_B as i64);
        let p = self.b.ins().uextend(types::I64, bit);
        self.set(fl, r, t, p);
    }

    /// A payload word as the float it holds.
    fn as_f64(&mut self, p: Value) -> Value {
        self.b.ins().bitcast(types::F64, MemFlags::new(), p)
    }

    fn u32c(&mut self, x: u32) -> Value {
        self.b.ins().iconst(types::I32, i64::from(x))
    }

    /// Decline unless the `0`/`1` helper result `r` is not `2`; the `i8`
    /// bit.
    fn bit_or_decline(&mut self, r: Value) -> Value {
        let declines = self.b.ins().icmp_imm(IntCC::Equal, r, 2);
        let ok = self.b.create_block();
        self.b.ins().brif(declines, self.decline, &[], ok, &[]);
        self.b.switch_to_block(ok);
        self.b.ins().ireduce(types::I8, r)
    }

    /// A register's truth as an `i8`, declining where the interpreter's
    /// `truth` would: in line for the scalar tags, through the helper for
    /// a heap value.
    fn truth(&mut self, fl: &FrameLower<'_>, src: u8) -> Value {
        let (t, p) = self.get(fl, src);
        let merge = self.b.create_block();
        self.b.append_block_param(merge, types::I8);
        let not_b = self.b.create_block();
        let not_i = self.b.create_block();
        let not_n = self.b.create_block();
        let is_b = self.b.ins().icmp_imm(IntCC::Equal, t, T_B as i64);
        let low = self.b.ins().band_imm(p, 0xff);
        let b_truth = self.b.ins().icmp_imm(IntCC::NotEqual, low, 0);
        self.b
            .ins()
            .brif(is_b, merge, &[b_truth.into()], not_b, &[]);
        self.b.switch_to_block(not_b);
        let is_i = self.b.ins().icmp_imm(IntCC::Equal, t, T_I as i64);
        let i_truth = self.b.ins().icmp_imm(IntCC::NotEqual, p, 0);
        self.b
            .ins()
            .brif(is_i, merge, &[i_truth.into()], not_i, &[]);
        self.b.switch_to_block(not_i);
        let is_n = self.b.ins().icmp_imm(IntCC::Equal, t, T_N as i64);
        let falsy = self.b.ins().iconst(types::I8, 0);
        self.b.ins().brif(is_n, merge, &[falsy.into()], not_n, &[]);
        self.b.switch_to_block(not_n);
        let r = self.call(h_truth as *const () as usize, &[t, p]);
        let bit = self.bit_or_decline(r);
        self.b.ins().jump(merge, &[bit.into()]);
        self.b.switch_to_block(merge);
        self.b.block_params(merge)[0]
    }

    fn lower(
        &mut self,
        code: &CodeObject,
        ext: &CodeConstObjects,
        plan: &LeafPlan,
        f: &Rc<PyFunction>,
    ) -> Option<()> {
        let entry = self.b.create_block();
        self.b.append_block_params_for_function_params(entry);
        self.b.switch_to_block(entry);
        self.ctx = self.b.block_params(entry)[0];
        let argw = self.b.block_params(entry)[1];
        self.decline = self.b.create_block();
        let fr = self.b.ins().iadd_imm(self.ctx, i64::from(TOP_OFFSET));
        let fl = self.frame(plan, code, ext, fr, 0, None, Rc::as_ptr(f));
        self.keep.push((Rc::downgrade(f), f.code()));
        let flags = MemFlags::trusted();
        for k in 0..usize::from(plan.nargs) {
            let t = self.b.ins().load(types::I64, flags, argw, (16 * k) as i32);
            let p = self
                .b
                .ins()
                .load(types::I64, flags, argw, (16 * k + 8) as i32);
            self.set(&fl, k as u8, t, p);
        }
        self.b.ins().jump(fl.blocks[0], &[]);
        self.body(&fl)?;
        self.b.switch_to_block(self.decline);
        let one = self.b.ins().iconst(types::I32, 1);
        self.b.ins().return_(&[one]);
        self.b.seal_all_blocks();
        Some(())
    }

    /// Declare a frame's registers (zeroed in the current block) and its
    /// op blocks.
    fn frame<'p>(
        &mut self,
        plan: &'p LeafPlan,
        code: &'p CodeObject,
        ext: &'p CodeConstObjects,
        fr: Value,
        depth: u8,
        ret: Option<(Variable, Variable, Block)>,
        func: *const PyFunction,
    ) -> FrameLower<'p> {
        let nregs = usize::from(plan.nregs);
        let mut tags = Vec::with_capacity(nregs);
        let mut pays = Vec::with_capacity(nregs);
        let zero = self.b.ins().iconst(types::I64, 0);
        for _ in 0..nregs {
            let t = self.b.declare_var(types::I64);
            let p = self.b.declare_var(types::I64);
            self.b.def_var(t, zero);
            self.b.def_var(p, zero);
            tags.push(t);
            pays.push(p);
        }
        let blocks = (0..plan.ops.len()).map(|_| self.b.create_block()).collect();
        // Conservatively: a store anywhere earlier in the op order.
        let mut seen = false;
        let may_store = plan
            .ops
            .iter()
            .map(|op| {
                let before = seen;
                seen |= matches!(op, Op::StoreAttr { .. });
                before
            })
            .collect();
        let _ = ext;
        FrameLower {
            plan,
            code,
            fr,
            depth,
            tags,
            pays,
            blocks,
            ret,
            may_store,
            func,
        }
    }

    /// Lower every op of a frame into its blocks.
    fn body(&mut self, fl: &FrameLower<'_>) -> Option<()> {
        for (i, op) in fl.plan.ops.iter().enumerate() {
            self.b.switch_to_block(fl.blocks[i]);
            if self.op(fl, i, op)? {
                // Falls through (the translation ends every path in a
                // return or a jump, so a next op exists).
                let next = *fl.blocks.get(i + 1)?;
                self.b.ins().jump(next, &[]);
            }
        }
        Some(())
    }

    /// The pure-leaf callee to compile in line for the `CALL` at op `i`
    /// whose callee register `at` a method load filled: what that load's
    /// site last resolved, with its plan and arity (self included).
    fn inline_target(
        &self,
        fl: &FrameLower<'_>,
        i: usize,
        at: u8,
        argc: u8,
    ) -> Option<(
        Rc<PyFunction>,
        Rc<CodeObject>,
        &'static CodeConstObjects,
        &'static LeafPlan,
    )> {
        if fl.depth >= INLINE_DEPTH || (self.effect && fl.depth == 0 && fl.may_store[i]) {
            return None;
        }
        let (pc, _) = fl.plan.ops[..i].iter().rev().find_map(|op| match *op {
            Op::Method { dst, pc, name, .. } if dst == at => Some((pc, name)),
            _ => None,
        })?;
        let f = crate::code_method_slot(fl.code, u32::from(pc))?.last_fn()?;
        let ccode = f.code();
        if !crate::code_is_pure_leaf(&ccode)
            || !Interpreter::leaf_code_ok(&ccode)
            || crate::leaf_arity(&ccode) != usize::from(argc) + 1
            || ccode.has_varkeywords
        {
            return None;
        }
        let cext = crate::code_vm_ext(&ccode)?;
        let cplan = cext
            .leaf_plan
            .get_or_init(|| super::build(&ccode, cext).map(Box::new))
            .as_deref()?;
        // A pure callee stores nothing.
        if cplan
            .ops
            .iter()
            .any(|op| matches!(op, Op::StoreAttr { .. }))
        {
            return None;
        }
        // SAFETY: `ccode` (kept alive with the compiled code) owns its
        // extension and plan, which never move.
        let (cext, cplan) = unsafe { (&*std::ptr::from_ref(cext), &*std::ptr::from_ref(cplan)) };
        Some((f, ccode, cext, cplan))
    }

    /// Lower one op; whether it falls through to the next.
    #[allow(clippy::too_many_lines)]
    fn op(&mut self, fl: &FrameLower<'_>, i: usize, op: &Op) -> Option<bool> {
        let effect = self.effect && fl.depth == 0;
        match *op {
            Op::Move { dst, src } => {
                let (t, p) = self.get(fl, src);
                self.set(fl, dst, t, p);
            }
            Op::Const { dst, k } => {
                let [t, p] = words(*fl.plan.consts.get(usize::from(k))?);
                let t = self.b.ins().iconst(types::I64, t as i64);
                let p = self.b.ins().iconst(types::I64, p as i64);
                self.set(fl, dst, t, p);
            }
            Op::Global { dst, pc } => {
                let Some(slot) = crate::code_stamp_slot(fl.code, u32::from(pc)) else {
                    self.b.ins().jump(self.decline, &[]);
                    return Some(false);
                };
                let slot = self
                    .b
                    .ins()
                    .iconst(self.ptr, std::ptr::from_ref(slot) as i64);
                let cache = Box::<GlobalCache>::default();
                let cache_at = std::ptr::from_ref(&*cache) as i64;
                self.globals.push(cache);
                let done = self.b.create_block();
                self.b.append_block_param(done, types::I64);
                self.b.append_block_param(done, types::I64);
                let slow = self.b.create_block();
                self.inline_global(fl, cache_at, done, slow);
                self.b.switch_to_block(slow);
                let cache_v = self.b.ins().iconst(self.ptr, cache_at);
                let pc = self.u32c(u32::from(pc));
                let st = self.call(
                    h_global as *const () as usize,
                    &[self.ctx, fl.fr, slot, cache_v, pc],
                );
                self.check(st);
                let flags = MemFlags::trusted();
                let t = self.b.ins().load(types::I64, flags, self.ctx, 0);
                let p = self.b.ins().load(types::I64, flags, self.ctx, 8);
                self.b.ins().jump(done, &[t.into(), p.into()]);
                self.b.switch_to_block(done);
                let (t, p) = (self.b.block_params(done)[0], self.b.block_params(done)[1]);
                self.set(fl, dst, t, p);
            }
            Op::Deref { dst, idx } => {
                let idx = self.u32c(u32::from(idx));
                let st = self.call(h_deref as *const () as usize, &[self.ctx, fl.fr, idx]);
                self.check(st);
                let flags = MemFlags::trusted();
                let t = self.b.ins().load(types::I64, flags, self.ctx, 0);
                let p = self.b.ins().load(types::I64, flags, self.ctx, 8);
                self.set(fl, dst, t, p);
            }
            Op::Attr { dst, src, pc, name } => {
                let (t, p) = self.get(fl, src);
                let pn = self.u32c(u32::from(pc) | u32::from(name) << 16);
                let cache = Box::<FieldCache>::default();
                let cache_at = std::ptr::from_ref(&*cache) as i64;
                let cache_v = self.b.ins().iconst(self.ptr, cache_at);
                self.fields.push(cache);
                // The site's most recent field, read in line.
                let done = self.b.create_block();
                self.b.append_block_param(done, types::I64);
                self.b.append_block_param(done, types::I64);
                if let Some(l) = crate::tier2::published_obj_layout() {
                    let slow = self.b.create_block();
                    // A site the interpreter has seen read a member slot
                    // reads one in line too.
                    let slot_site = matches!(
                        fl.code.caches.get(u32::from(pc)),
                        weavepy_compiler::InlineCache::LoadAttrSlot { .. }
                    );
                    let (vt, vp) = self.inline_field(l, t, p, cache_at, effect, slot_site, slow);
                    self.b.ins().jump(done, &[vt.into(), vp.into()]);
                    self.b.switch_to_block(slow);
                    if let Some(slot) = crate::code_stamp_slot(fl.code, u32::from(pc)) {
                        let slow = self.b.create_block();
                        let slot = std::ptr::from_ref(slot) as i64;
                        self.inline_class_attr(l, t, p, slot, done, slow);
                        self.b.switch_to_block(slow);
                    }
                }
                let addr = if effect {
                    h_field::<true> as *const () as usize
                } else {
                    h_field::<false> as *const () as usize
                };
                let st = self.call(addr, &[self.ctx, fl.fr, cache_v, t, p, pn]);
                self.check(st);
                let flags = MemFlags::trusted();
                let rt = self.b.ins().load(types::I64, flags, self.ctx, 0);
                let rp = self.b.ins().load(types::I64, flags, self.ctx, 8);
                self.b.ins().jump(done, &[rt.into(), rp.into()]);
                self.b.switch_to_block(done);
                let (rt, rp) = (self.b.block_params(done)[0], self.b.block_params(done)[1]);
                self.set(fl, dst, rt, rp);
            }
            Op::Method { dst, src, pc, name } => {
                let Some(ms) = crate::code_method_slot(fl.code, u32::from(pc)) else {
                    self.b.ins().jump(self.decline, &[]);
                    return Some(false);
                };
                let (t, p) = self.get(fl, src);
                let cache = Box::<MethodCache>::default();
                let cache_at = std::ptr::from_ref(&*cache) as i64;
                self.methods.push(cache);
                let done = self.b.create_block();
                for _ in 0..4 {
                    self.b.append_block_param(done, types::I64);
                }
                let slow = self.b.create_block();
                if let Some(l) = crate::tier2::published_obj_layout() {
                    self.inline_method(l, t, p, cache_at, done, slow);
                } else {
                    self.b.ins().jump(slow, &[]);
                }
                self.b.switch_to_block(slow);
                let ms = self.b.ins().iconst(self.ptr, std::ptr::from_ref(ms) as i64);
                let cache_v = self.b.ins().iconst(self.ptr, cache_at);
                let nm = self.u32c(u32::from(name));
                let st = self.call(
                    h_method as *const () as usize,
                    &[self.ctx, fl.fr, ms, cache_v, t, p, nm],
                );
                self.check(st);
                let flags = MemFlags::trusted();
                let w: Vec<_> = (0..4)
                    .map(|k| self.b.ins().load(types::I64, flags, self.ctx, 8 * k).into())
                    .collect();
                self.b.ins().jump(done, &w);
                self.b.switch_to_block(done);
                let ps = self.b.block_params(done).to_vec();
                self.set(fl, dst, ps[0], ps[1]);
                self.set(fl, dst + 1, ps[2], ps[3]);
            }
            Op::Compare { dst, a, b, kind } => {
                let (ta, pa) = self.get(fl, a);
                let (tb, pb) = self.get(fl, b);
                let cc = match kind {
                    k if k == CompareKind::Lt as u8 => IntCC::SignedLessThan,
                    k if k == CompareKind::LtE as u8 => IntCC::SignedLessThanOrEqual,
                    k if k == CompareKind::Eq as u8 => IntCC::Equal,
                    k if k == CompareKind::NotEq as u8 => IntCC::NotEqual,
                    k if k == CompareKind::Gt as u8 => IntCC::SignedGreaterThan,
                    k if k == CompareKind::GtE as u8 => IntCC::SignedGreaterThanOrEqual,
                    _ => return None,
                };
                let fcc = match kind {
                    k if k == CompareKind::Lt as u8 => FloatCC::LessThan,
                    k if k == CompareKind::LtE as u8 => FloatCC::LessThanOrEqual,
                    k if k == CompareKind::Eq as u8 => FloatCC::Equal,
                    k if k == CompareKind::NotEq as u8 => FloatCC::NotEqual,
                    k if k == CompareKind::Gt as u8 => FloatCC::GreaterThan,
                    _ => FloatCC::GreaterThanOrEqual,
                };
                // Two ints, or two floats, compare in line.
                let ia = self.b.ins().icmp_imm(IntCC::Equal, ta, T_I as i64);
                let ib = self.b.ins().icmp_imm(IntCC::Equal, tb, T_I as i64);
                let both = self.b.ins().band(ia, ib);
                let fast = self.b.create_block();
                let not_int = self.b.create_block();
                let float = self.b.create_block();
                let slow = self.b.create_block();
                let merge = self.b.create_block();
                self.b.append_block_param(merge, types::I8);
                self.b.ins().brif(both, fast, &[], not_int, &[]);
                self.b.switch_to_block(fast);
                let bit = self.b.ins().icmp(cc, pa, pb);
                self.b.ins().jump(merge, &[bit.into()]);
                self.b.switch_to_block(not_int);
                let fa = self.b.ins().icmp_imm(IntCC::Equal, ta, T_F as i64);
                let fb = self.b.ins().icmp_imm(IntCC::Equal, tb, T_F as i64);
                let both = self.b.ins().band(fa, fb);
                self.b.ins().brif(both, float, &[], slow, &[]);
                self.b.switch_to_block(float);
                let (xa, xb) = (self.as_f64(pa), self.as_f64(pb));
                // A NaN declines, as the interpreter's `compare` does.
                let nan = self.b.ins().fcmp(FloatCC::Unordered, xa, xb);
                let ordered = self.b.create_block();
                self.b.ins().brif(nan, self.decline, &[], ordered, &[]);
                self.b.switch_to_block(ordered);
                let bit = self.b.ins().fcmp(fcc, xa, xb);
                self.b.ins().jump(merge, &[bit.into()]);
                self.b.switch_to_block(slow);
                let k = self.u32c(u32::from(kind));
                let r = self.call(h_compare as *const () as usize, &[ta, pa, tb, pb, k]);
                let bit = self.bit_or_decline(r);
                self.b.ins().jump(merge, &[bit.into()]);
                self.b.switch_to_block(merge);
                let bit = self.b.block_params(merge)[0];
                self.set_bool(fl, dst, bit);
            }
            Op::Is { dst, a, b, invert } => {
                let (ta, pa) = self.get(fl, a);
                let (tb, pb) = self.get(fl, b);
                let r = self.call(h_is as *const () as usize, &[ta, pa, tb, pb]);
                let r = if invert {
                    self.b.ins().bxor_imm(r, 1)
                } else {
                    r
                };
                let bit = self.b.ins().ireduce(types::I8, r);
                self.set_bool(fl, dst, bit);
            }
            Op::Truth { dst, src } => {
                let bit = self.truth(fl, src);
                self.set_bool(fl, dst, bit);
            }
            Op::Unary { dst, src, kind } => {
                let (t, p) = self.get(fl, src);
                let k = self.u32c(u32::from(kind));
                let st = self.call(h_unary as *const () as usize, &[self.ctx, t, p, k]);
                self.check(st);
                self.set_out(fl, dst, 0);
            }
            Op::Binary { dst, a, b, kind } => {
                let (ta, pa) = self.get(fl, a);
                let (tb, pb) = self.get(fl, b);
                let int_op = [BinOpKind::Add, BinOpKind::Sub, BinOpKind::Mult]
                    .into_iter()
                    .find(|k| *k as u8 == kind);
                let float_div = kind == BinOpKind::Div as u8;
                let merge = self.b.create_block();
                self.b.append_block_param(merge, types::I64);
                self.b.append_block_param(merge, types::I64);
                let slow = self.b.create_block();
                if let Some(k) = int_op {
                    // Two ints add, subtract and multiply in line; an
                    // overflow declines (the interpreter's `binary` does).
                    let ia = self.b.ins().icmp_imm(IntCC::Equal, ta, T_I as i64);
                    let ib = self.b.ins().icmp_imm(IntCC::Equal, tb, T_I as i64);
                    let both = self.b.ins().band(ia, ib);
                    let fast = self.b.create_block();
                    let not_int = self.b.create_block();
                    self.b.ins().brif(both, fast, &[], not_int, &[]);
                    self.b.switch_to_block(fast);
                    let (r, ovf) = match k {
                        BinOpKind::Add => {
                            let r = self.b.ins().iadd(pa, pb);
                            let axr = self.b.ins().bxor(pa, r);
                            let bxr = self.b.ins().bxor(pb, r);
                            let and = self.b.ins().band(axr, bxr);
                            (r, self.b.ins().icmp_imm(IntCC::SignedLessThan, and, 0))
                        }
                        BinOpKind::Sub => {
                            let r = self.b.ins().isub(pa, pb);
                            let axb = self.b.ins().bxor(pa, pb);
                            let axr = self.b.ins().bxor(pa, r);
                            let and = self.b.ins().band(axb, axr);
                            (r, self.b.ins().icmp_imm(IntCC::SignedLessThan, and, 0))
                        }
                        _ => {
                            let lo = self.b.ins().imul(pa, pb);
                            let hi = self.b.ins().smulhi(pa, pb);
                            let sign = self.b.ins().sshr_imm(lo, 63);
                            (lo, self.b.ins().icmp(IntCC::NotEqual, hi, sign))
                        }
                    };
                    let ok = self.b.create_block();
                    self.b.ins().brif(ovf, self.decline, &[], ok, &[]);
                    self.b.switch_to_block(ok);
                    let ti = self.b.ins().iconst(types::I64, T_I as i64);
                    self.b.ins().jump(merge, &[ti.into(), r.into()]);
                    // Two floats in line too; a NaN result takes the helper
                    // (which gives it its identity).
                    self.b.switch_to_block(not_int);
                    let fa = self.b.ins().icmp_imm(IntCC::Equal, ta, T_F as i64);
                    let fb = self.b.ins().icmp_imm(IntCC::Equal, tb, T_F as i64);
                    let both = self.b.ins().band(fa, fb);
                    let float = self.b.create_block();
                    self.b.ins().brif(both, float, &[], slow, &[]);
                    self.b.switch_to_block(float);
                    let (xa, xb) = (self.as_f64(pa), self.as_f64(pb));
                    let r = match k {
                        BinOpKind::Add => self.b.ins().fadd(xa, xb),
                        BinOpKind::Sub => self.b.ins().fsub(xa, xb),
                        _ => self.b.ins().fmul(xa, xb),
                    };
                    let nan = self.b.ins().fcmp(FloatCC::Unordered, r, r);
                    let fine = self.b.create_block();
                    self.b.ins().brif(nan, slow, &[], fine, &[]);
                    self.b.switch_to_block(fine);
                    let bits = self.b.ins().bitcast(types::I64, MemFlags::new(), r);
                    let tf = self.b.ins().iconst(types::I64, T_F as i64);
                    self.b.ins().jump(merge, &[tf.into(), bits.into()]);
                } else if float_div {
                    // Two floats divide in line by a nonzero divisor; a NaN
                    // result takes the helper.
                    let fa = self.b.ins().icmp_imm(IntCC::Equal, ta, T_F as i64);
                    let fb = self.b.ins().icmp_imm(IntCC::Equal, tb, T_F as i64);
                    let both = self.b.ins().band(fa, fb);
                    let float = self.b.create_block();
                    self.b.ins().brif(both, float, &[], slow, &[]);
                    self.b.switch_to_block(float);
                    let (xa, xb) = (self.as_f64(pa), self.as_f64(pb));
                    let zero = self.b.ins().f64const(0.0);
                    let by_zero = self.b.ins().fcmp(FloatCC::Equal, xb, zero);
                    let nonzero = self.b.create_block();
                    self.b.ins().brif(by_zero, slow, &[], nonzero, &[]);
                    self.b.switch_to_block(nonzero);
                    let r = self.b.ins().fdiv(xa, xb);
                    let nan = self.b.ins().fcmp(FloatCC::Unordered, r, r);
                    let fine = self.b.create_block();
                    self.b.ins().brif(nan, slow, &[], fine, &[]);
                    self.b.switch_to_block(fine);
                    let bits = self.b.ins().bitcast(types::I64, MemFlags::new(), r);
                    let tf = self.b.ins().iconst(types::I64, T_F as i64);
                    self.b.ins().jump(merge, &[tf.into(), bits.into()]);
                } else {
                    self.b.ins().jump(slow, &[]);
                }
                self.b.switch_to_block(slow);
                let kv = self.u32c(u32::from(kind));
                let st = self.call(
                    h_binary as *const () as usize,
                    &[self.ctx, ta, pa, tb, pb, kv],
                );
                self.check(st);
                let flags = MemFlags::trusted();
                let t = self.b.ins().load(types::I64, flags, self.ctx, 0);
                let p = self.b.ins().load(types::I64, flags, self.ctx, 8);
                self.b.ins().jump(merge, &[t.into(), p.into()]);
                self.b.switch_to_block(merge);
                let (t, p) = (self.b.block_params(merge)[0], self.b.block_params(merge)[1]);
                self.set(fl, dst, t, p);
            }
            Op::Swap { a, b } => {
                let (ta, pa) = self.get(fl, a);
                let (tb, pb) = self.get(fl, b);
                self.set(fl, a, tb, pb);
                self.set(fl, b, ta, pa);
            }
            Op::BranchIf { src, when, target } => {
                let bit = self.truth(fl, src);
                let to = *fl.blocks.get(usize::from(target))?;
                let next = self.b.create_block();
                if when {
                    self.b.ins().brif(bit, to, &[], next, &[]);
                } else {
                    self.b.ins().brif(bit, next, &[], to, &[]);
                }
                self.b.switch_to_block(next);
            }
            Op::BranchNone { src, when, target } => {
                let (t, _) = self.get(fl, src);
                let is_none = self.b.ins().icmp_imm(IntCC::Equal, t, T_N as i64);
                let to = *fl.blocks.get(usize::from(target))?;
                let next = self.b.create_block();
                if when {
                    self.b.ins().brif(is_none, to, &[], next, &[]);
                } else {
                    self.b.ins().brif(is_none, next, &[], to, &[]);
                }
                self.b.switch_to_block(next);
            }
            Op::Jump { target } => {
                let to = *fl.blocks.get(usize::from(target))?;
                self.b.ins().jump(to, &[]);
                return Some(false);
            }
            Op::Call { at, argc, pc } => self.call_op(fl, i, at, argc, pc)?,
            Op::Return { src } => {
                let (t, p) = self.get(fl, src);
                if let Some((rt, rp, cont)) = fl.ret {
                    // An in-line callee's return: the caller's result
                    // register, and on past its call.
                    self.b.def_var(rt, t);
                    self.b.def_var(rp, p);
                    self.b.ins().jump(cont, &[]);
                } else {
                    let flags = MemFlags::trusted();
                    self.b.ins().store(flags, t, self.ctx, 0);
                    self.b.ins().store(flags, p, self.ctx, 8);
                    let zero = self.b.ins().iconst(types::I32, 0);
                    self.b.ins().return_(&[zero]);
                }
                return Some(false);
            }
            Op::New { dst, dict } => {
                let d = self.u32c(u32::from(dict));
                let st = self.call(h_new as *const () as usize, &[self.ctx, d]);
                self.check(st);
                self.set_out(fl, dst, 0);
            }
            Op::Subscr { dst, a, b } => {
                let (ta, pa) = self.get(fl, a);
                let (tb, pb) = self.get(fl, b);
                let st = self.call(h_subscr as *const () as usize, &[self.ctx, ta, pa, tb, pb]);
                self.check(st);
                self.set_out(fl, dst, 0);
            }
            Op::Decline => {
                self.b.ins().jump(self.decline, &[]);
                return Some(false);
            }
            Op::StoreAttr {
                recv,
                val,
                pc,
                name,
            } => {
                if !effect {
                    self.b.ins().jump(self.decline, &[]);
                    return Some(false);
                }
                let (tr, pr) = self.get(fl, recv);
                let (tv, pv) = self.get(fl, val);
                let last = Self::infallible_after(fl.plan, i);
                // The plan's only store, with nothing after it that can
                // decline: a scalar over a scalar field lands in line.
                let done = self.b.create_block();
                if last && !fl.may_store[i] {
                    if let (Some(l), Some(slot)) = (
                        crate::tier2::published_obj_layout(),
                        crate::code_field_slot(fl.code, usize::from(pc)),
                    ) {
                        let slow = self.b.create_block();
                        self.inline_store(l, (tr, pr), (tv, pv), slot as i64, done, slow);
                        self.b.switch_to_block(slow);
                    }
                }
                let pn = self.u32c(u32::from(pc) | u32::from(name) << 16);
                let last = self.u32c(u32::from(last));
                let st = self.call(
                    h_store as *const () as usize,
                    &[self.ctx, tr, pr, tv, pv, pn, last],
                );
                self.check(st);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
            }
        }
        Some(true)
    }

    /// Whether nothing after op `i` can decline the evaluation: every
    /// later op is a move, a constant, a return or a forward jump.
    fn infallible_after(plan: &LeafPlan, i: usize) -> bool {
        plan.ops
            .iter()
            .enumerate()
            .skip(i + 1)
            .all(|(k, op)| match *op {
                Op::Move { .. } | Op::Const { .. } | Op::Return { .. } => true,
                Op::Jump { target } => usize::from(target) > k,
                _ => false,
            })
    }

    /// A `CALL` (op `i`): the callee its site last resolved in line behind
    /// a guard, falling back to the helper.
    fn call_op(&mut self, fl: &FrameLower<'_>, i: usize, at: u8, argc: u8, pc: u16) -> Option<()> {
        let cont = self.b.create_block();
        if let Some((f, ccode, cext, cplan)) = self.inline_target(fl, i, at, argc) {
            // The guard: the register holds this function, called with an
            // instance receiver, and the function still runs this code.
            let (tf, pf) = self.get(fl, at);
            let (ts, _) = self.get(fl, at + 1);
            let is_fn = self.b.ins().icmp_imm(IntCC::Equal, tf, T_FN as i64);
            let same = self
                .b
                .ins()
                .icmp_imm(IntCC::Equal, pf, Rc::as_ptr(&f) as i64);
            let has_self = self.b.ins().icmp_imm(IntCC::Equal, ts, T_R as i64);
            let ok = self.b.ins().band(is_fn, same);
            let ok = self.b.ins().band(ok, has_self);
            let code_check = self.b.create_block();
            let inline = self.b.create_block();
            let generic = self.b.create_block();
            self.b.ins().brif(ok, code_check, &[], generic, &[]);
            self.b.switch_to_block(code_check);
            let cell = f.code.as_ptr();
            // SAFETY: the cell holds an `Rc` (one pointer word); the guard
            // above proved the function alive wherever this load runs.
            let bits = unsafe { *cell.cast::<u64>() };
            let addr = self.b.ins().iconst(self.ptr, cell as i64);
            let now = self.b.ins().load(types::I64, MemFlags::trusted(), addr, 0);
            let same_code = self.b.ins().icmp_imm(IntCC::Equal, now, bits as i64);
            self.b.ins().brif(same_code, inline, &[], generic, &[]);
            self.b.switch_to_block(inline);
            let frame = Box::new(Frame {
                code: Rc::as_ptr(&ccode),
                ext: cext,
                f: Rc::as_ptr(&f),
            });
            let fr = self
                .b
                .ins()
                .iconst(self.ptr, std::ptr::from_ref(&*frame) as i64);
            let depth = fl.depth + 1;
            self.depth = self.depth.max(depth);
            let ret = Some((fl.tags[usize::from(at)], fl.pays[usize::from(at)], cont));
            let cfl = self.frame(cplan, &ccode, cext, fr, depth, ret, Rc::as_ptr(&f));
            // The receiver and the arguments become the callee's first
            // registers.
            for k in 0..=usize::from(argc) {
                let (t, p) = self.get(fl, at + 1 + k as u8);
                self.set(&cfl, k as u8, t, p);
            }
            self.b.ins().jump(cfl.blocks[0], &[]);
            self.frames.push(frame);
            self.keep.push((Rc::downgrade(&f), ccode.clone()));
            self.body(&cfl)?;
            self.b.switch_to_block(generic);
        }
        let n = usize::from(argc) + 2;
        let slot = self.b.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            (16 * n) as u32,
            3,
        ));
        for k in 0..n {
            let (t, p) = self.get(fl, at + k as u8);
            self.b.ins().stack_store(t, slot, (16 * k) as i32);
            self.b.ins().stack_store(p, slot, (16 * k + 8) as i32);
        }
        let w = self.b.ins().stack_addr(self.ptr, slot, 0);
        let ad = self.u32c(u32::from(argc) | u32::from(fl.depth) << 8);
        let pc = self.u32c(u32::from(pc));
        let addr = if self.effect && fl.depth == 0 {
            h_call::<true> as *const () as usize
        } else {
            h_call::<false> as *const () as usize
        };
        let st = self.call(addr, &[self.ctx, fl.fr, w, ad, pc]);
        self.check(st);
        self.set_out(fl, at, 0);
        self.b.ins().jump(cont, &[]);
        self.b.switch_to_block(cont);
        Some(())
    }
}
