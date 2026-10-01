//! Native code for leaf plans.
//!
//! A plan that keeps running is compiled, once per effect mode, into a
//! function over the same registers: each register is a pair of SSA
//! variables holding the [`V`] tag and payload words; moves, constants,
//! jumps, and scalar compares, truth tests and arithmetic run in line; and
//! every other operation calls a helper that runs the interpreter's own
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
    binary, compare, is_same, new_container, norm, plan_store, truth, unary, LeafPlan, LeafRet, Op,
    Owned, Pending, NEST, V,
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

/// Evaluations of a plan (per effect mode) before it is compiled.
const WARM_RUNS: u32 = 64;

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
}

/// An attribute site's split-field positions by receiver class version:
/// a site several classes reach (methods a base class shares) keeps one
/// for each. A version names one class in one state, and a class's shared
/// names never move, so a hit needs only the instance to hold the field.
#[derive(Default)]
pub(super) struct FieldCache {
    entries: [std::cell::Cell<(u64, u32)>; 4],
    next: std::cell::Cell<u8>,
}

/// A global site's resolution: the globals and builtins stamps it was
/// found under (process-unique, so they name the dicts and their key
/// layouts), which of the two holds it, and at what index.
#[derive(Default)]
pub(super) struct GlobalCache {
    stamps: std::cell::Cell<(u64, u64)>,
    at: std::cell::Cell<(bool, u32)>,
}

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
            slot.get_or_init(|| compile(code, ext, plan, EFFECT))
                .as_ref()?
        }
    };
    // In-line callees run where the interpreter would nest frames: near
    // the recursion limit, it decides.
    if crate::recursion::current_depth() + usize::from(nest) + usize::from(native.depth) + 1
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
    // SAFETY (raw dict reads): nothing runs code here.
    let (gs, bs) = unsafe {
        (
            (*f.globals.as_ptr()).mutation_stamp(),
            (*f.builtins.as_ptr()).mutation_stamp(),
        )
    };
    if cache.stamps.get() == (gs, bs) {
        let (builtin, idx) = cache.at.get();
        let dict = if builtin { &f.builtins } else { &f.globals };
        // SAFETY: as above.
        if let Some((_, v)) = unsafe { (*dict.as_ptr()).get_index(idx as usize) } {
            c.put(norm(v));
            return 0;
        }
    }
    let Some(v) = c.interp.plan_global_at(code, slot, f, pc as u16) else {
        return 1;
    };
    // Remember where the value lives (the site's inline cache says which
    // namespace), under the stamps it was read with.
    use weavepy_compiler::InlineCache as IC;
    match code.caches.get(pc) {
        IC::LoadGlobalModule { key_idx, .. } => {
            cache.stamps.set((gs, bs));
            cache.at.set((false, key_idx));
        }
        IC::LoadGlobalBuiltin { key_idx, .. } => {
            cache.stamps.set((gs, bs));
            cache.at.set((true, key_idx));
        }
        _ => {}
    }
    c.put(v);
    0
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
                if let Some(v) = unsafe { inst.split_field(idx as usize) } {
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
        if let Some((_, Some(idx))) =
            Interpreter::leaf_resolve_instance_attr_ix(code, inst, pc_name >> 16)
        {
            if ver != 0 && inst.dict.published().is_none() {
                let k = usize::from(cache.next.get()) % cache.entries.len();
                cache.entries[k].set((ver, idx));
                cache.next.set(cache.next.get().wrapping_add(1));
            }
        }
    }
    status
}

unsafe extern "C" fn h_method(
    ctx: *mut Ctx<'static>,
    fr: *const Frame,
    ms: *const crate::MethodSlot,
    t: u64,
    p: u64,
    name: u32,
) -> u32 {
    // SAFETY: called by native code with its context, a live frame, and
    // the site's method slot (its code keeps the table).
    let (c, (code, _, _), ms) = unsafe { (cx(ctx), (*fr).parts(), &*ms) };
    match c.interp.plan_method_at(code, ms, value(t, p), name as u16) {
        Some((func, recv)) => {
            let [a, b] = words(func);
            let [d, e] = words(recv);
            c.out = [a, b, d, e];
            0
        }
        None => 1,
    }
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

/// The evaluation's own `recv.name = val` (only the top frame stores).
unsafe extern "C" fn h_store(
    ctx: *mut Ctx<'static>,
    tr: u64,
    pr: u64,
    tv: u64,
    pv: u64,
    pc_name: u32,
) -> u32 {
    // SAFETY: called by native code with its context.
    let c = unsafe { cx(ctx) };
    let V::R(rp) = value(tr, pr) else {
        return 1;
    };
    let (pc, name) = (pc_name as u16, (pc_name >> 16) as u16);
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
        flags.set("opt_level", "speed").ok()?;
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
    effect: bool,
) -> Option<Native> {
    let mut guard = ENGINE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let engine = guard.get_or_insert_with(Engine::new).as_mut()?;
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        compile_with(engine, code, ext, plan, effect)
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
        };
        let done = lower.lower(code, ext, plan);
        let Lower {
            b,
            depth,
            frames,
            keep,
            fields,
            globals,
            ..
        } = lower;
        done.map(|()| {
            b.finalize();
            (depth, frames, keep, fields, globals)
        })
    };
    let Some((depth, frames, keep, fields, globals)) = lowered else {
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

    fn lower(&mut self, code: &CodeObject, ext: &CodeConstObjects, plan: &LeafPlan) -> Option<()> {
        let entry = self.b.create_block();
        self.b.append_block_params_for_function_params(entry);
        self.b.switch_to_block(entry);
        self.ctx = self.b.block_params(entry)[0];
        let argw = self.b.block_params(entry)[1];
        self.decline = self.b.create_block();
        let fr = self.b.ins().iadd_imm(self.ctx, i64::from(TOP_OFFSET));
        let fl = self.frame(plan, code, ext, fr, 0, None);
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
                let cache_v = self
                    .b
                    .ins()
                    .iconst(self.ptr, std::ptr::from_ref(&*cache) as i64);
                self.globals.push(cache);
                let pc = self.u32c(u32::from(pc));
                let st = self.call(
                    h_global as *const () as usize,
                    &[self.ctx, fl.fr, slot, cache_v, pc],
                );
                self.check(st);
                self.set_out(fl, dst, 0);
            }
            Op::Attr { dst, src, pc, name } => {
                let (t, p) = self.get(fl, src);
                let pn = self.u32c(u32::from(pc) | u32::from(name) << 16);
                let cache = Box::<FieldCache>::default();
                let cache_v = self
                    .b
                    .ins()
                    .iconst(self.ptr, std::ptr::from_ref(&*cache) as i64);
                self.fields.push(cache);
                let addr = if effect {
                    h_field::<true> as *const () as usize
                } else {
                    h_field::<false> as *const () as usize
                };
                let st = self.call(addr, &[self.ctx, fl.fr, cache_v, t, p, pn]);
                self.check(st);
                self.set_out(fl, dst, 0);
            }
            Op::Method { dst, src, pc, name } => {
                let Some(ms) = crate::code_method_slot(fl.code, u32::from(pc)) else {
                    self.b.ins().jump(self.decline, &[]);
                    return Some(false);
                };
                let (t, p) = self.get(fl, src);
                let ms = self.b.ins().iconst(self.ptr, std::ptr::from_ref(ms) as i64);
                let nm = self.u32c(u32::from(name));
                let st = self.call(
                    h_method as *const () as usize,
                    &[self.ctx, fl.fr, ms, t, p, nm],
                );
                self.check(st);
                self.set_out(fl, dst, 0);
                self.set_out(fl, dst + 1, 2);
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
                let pn = self.u32c(u32::from(pc) | u32::from(name) << 16);
                let st = self.call(
                    h_store as *const () as usize,
                    &[self.ctx, tr, pr, tv, pv, pn],
                );
                self.check(st);
            }
        }
        Some(true)
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
            let cfl = self.frame(cplan, &ccode, cext, fr, depth, ret);
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
