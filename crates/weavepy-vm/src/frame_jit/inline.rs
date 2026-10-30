//! Python callees lowered into their callers' native code.
//!
//! A direct call between compiled functions costs about 455 instructions,
//! spread over its binding, its `State`, moves and reference counts. At a
//! `CALL` whose site has settled on a small callee, the compiler lowers the
//! callee's body into the caller instead, guarded by the callee's code
//! ([`InlineSite::key`]): no activation, no `State`, no call helper.
//!
//! The callee's frame lives on the caller's operand stack, as CPython's
//! does: its locals start at its first argument, and its operand stack
//! follows them. Slots are numbered from the caller's stack base throughout,
//! so a helper that reads operands through `st.stack` and an absolute depth
//! works unchanged in an inlined body. An argument that is a caller's local
//! or a constant stays where it lies when the callee never rebinds it (no
//! reference taken or released), and a scalar stays unboxed.
//!
//! Only frameless work runs in an inlined body: in-line fast paths and
//! helpers that run no Python code and don't read the running frame. Anything
//! else leaves through [`h_inline_exit`], which builds the real activations
//! (each inlined level's, as a direct call binds them) and hands the callee
//! to the core loop at the instruction it stopped at, exactly as if it had
//! run there from its start. So nothing can see a frame that isn't there:
//! a raise, a Python dunder, a finalizer, a trace function coming on (the
//! entry checks the loop generation) all happen with the frames in place. A
//! site whose exits come too often turns itself off and takes the ordinary
//! call from then on.
//!
//! Between two operations that could change anything, a field read needs no
//! copy: the value stays where it lies, read through a temporary "local" until
//! something takes it (see [`Lower::inline_field`]), and a second read of the
//! same receiver checks only the field's own position. Nothing runs in between
//! that could move it, and the only way out materializes it.
//!
//! A `BINARY_OP` whose site ran a Python operator method lowers the method the
//! same way, guarded by the left operand's class version and the right
//! operand's class or tag. Its frame has no callable below it, and its
//! `NotImplemented` leaves for the core loop with the operator's protocol.
//!
//! A method call whose arguments are plain loads fuses with its method load
//! ([`Lower::inline_method`]): the receiver's class version guards the body,
//! the receiver stays where it lies, and neither the method nor the receiver
//! takes a reference.
//!
//! A `CALL` of a compiled Python function in an inlined body runs as a direct
//! call ([`h_inline_call`]), which is lazy already: its callee registers only
//! if something needs the core loop's view, and registering builds the
//! inlined bodies' activations first ([`spill`]). Otherwise the frames never
//! exist, and the body goes on when the callee returns.

use super::{
    block_starts, call_kernel_in_place, call_leaf_in_place, ctor_store_init, direct_call,
    h_call_kw, h_call_site, h_for_iter, h_stack_store_attr, kernel_for, maybe_unbound_locals,
    never_ran_blocks, settle, stats, type_of_call, type_slot_layout, BinOpKind, Block, BuiltinSite,
    Cell, CodeConstObjects, CodeObject, DirectSite, FieldSlot, GlobalCache, InstBuilder, IntCC,
    Interpreter, Item, Lower, Object, OpCode, PyIterator, Rc, State, Value, CALL_DECLINED,
    DICT_STAMP, EXIT_LAST_SHIFT, EXIT_PC_SHIFT, FLAGS, GC_BDATA, GC_BSTAMP, GC_BUILTINS, GC_GDATA,
    GC_GLOBALS, GC_GSTAMP, GC_VALUE, INTERP, I_MISSING, MARKED, RAISED, RELOAD, S_INTERP, S_SNAP,
    S_SW,
};
use super::{fill_method_cache, MethodCache, MC_BEFORE, MC_KEYS, MC_VER};
use cranelift_codegen::ir::types;

/// How many callees deep a site's inlining goes.
const MAX_LEVELS: usize = 3;
/// The longest callee inlined, in instructions.
const MAX_BODY: usize = 64;
/// How many instructions of callees one caller's lowering takes at first
/// (see [`plan`]; `WEAVEPY_FRAME_JIT_INLINE_BUDGET` overrides it, a tuning
/// aid).
pub(super) fn max_total() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("WEAVEPY_FRAME_JIT_INLINE_BUDGET")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(160)
    })
}
/// How many callees one site guards for, one after another, and how many of
/// the shortest (a table of small lambdas, a tree's node classes).
const MAX_POLY: usize = 3;
const MAX_POLY_TINY: usize = 6;
const TINY_BODY: usize = 8;
/// How many exits a site takes before its ratio to the entries counts.
const MIN_EXITS: u32 = 64;
/// A site turns off once more than one entry in this many exits.
const EXIT_RATIO: u64 = 8;

/// Where the packed exit constant (see [`Lower::inline_exit`]) keeps the
/// status; below it, as `Lower::exit`'s constant has them, the depth, pc
/// and last instruction.
const EXIT_STATUS_SHIFT: i64 = 56;

/// Whether callees are inlined (`WEAVEPY_FRAME_JIT_INLINE=0` turns it off,
/// a debugging aid).
pub(super) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WEAVEPY_FRAME_JIT_INLINE").is_none_or(|v| v != "0"))
}

/// A running inlined body's call site: what its guard compares, and what an
/// exit needs to build its activation.
#[repr(C)]
pub(crate) struct InlineSite {
    /// The callee's code handle, `0` once the site turned itself off.
    key: Cell<u64>,
    /// The body's entries (the outermost site's only).
    entries: Cell<u64>,
    exits: Cell<u32>,
    /// The site this one's caller was inlined at, or null.
    parent: *const InlineSite,
    /// The callee's code (which the guard's handle bits and the compiled
    /// body's addresses depend on).
    code: Rc<CodeObject>,
    call_pc: usize,
    /// The callable's slot, the first local's, the operand stack's bottom,
    /// and the locals' count.
    start: usize,
    first: usize,
    base: usize,
    nlocals: usize,
    /// The arguments the body borrows: each local's slot and where its
    /// value lies, for the activation to take a reference.
    borrowed: Vec<(usize, Place)>,
    /// A function no stack slot holds (an operator's or a fused method
    /// call's), and for an operator, its `BINARY_OP`'s argument. No count
    /// taken, so the code keeps no class alive: while the receiver's class
    /// stands at the version its guard read, the class holds the function,
    /// and a body changes no class (see [`h_inline_store_attr`]) but through
    /// a Python call, around which [`h_inline_call`] holds it.
    method: Option<(std::mem::ManuallyDrop<Object>, Option<u32>)>,
}

/// What a fused method call's slow path remembers for its guards: for each
/// callee, its function and the caches its guards read (see
/// [`h_inline_method_note`]).
pub(crate) struct MethodNotes {
    entries: Vec<(*const crate::object::PyFunction, Vec<*mut MethodCache>)>,
    ext: *const CodeConstObjects,
    name: u32,
}

const IS_KEY: i32 = std::mem::offset_of!(InlineSite, key) as i32;
const IS_ENTRIES: i32 = std::mem::offset_of!(InlineSite, entries) as i32;
const S_DEPTH: i32 = std::mem::offset_of!(State, depth_cell) as i32;
const SW_CALLS_OK: i32 = std::mem::offset_of!(crate::CoreSwitch, calls_ok) as i32;

/// A `CALL` in an inlined body that runs as a direct call.
pub(crate) struct InlineCall {
    /// The body's call site.
    site: *const InlineSite,
    /// The callable's slot, the `CALL`'s pc, and the inlined levels around
    /// it (each a frame the callee's recursion depth counts).
    start: usize,
    pc: usize,
    levels: usize,
}

/// Where a function's fields sit from an `Object::Function`'s payload word
/// (measured on a live function: `Rc`'s allocation layout isn't fixed).
#[derive(Clone, Copy)]
pub(super) struct FnLayout {
    code: i32,
    globals: i32,
    builtins: i32,
}

impl FnLayout {
    fn of(f: &Rc<crate::object::PyFunction>) -> Self {
        let obj = Object::Function(f.clone());
        // SAFETY: an `Object` is 16 bytes, its second word the payload.
        let word = unsafe { std::ptr::from_ref(&obj).cast::<[u64; 2]>().read() }[1] as usize;
        let at = |p: usize| (p - word) as i32;
        FnLayout {
            code: at(f.code.as_ptr() as usize),
            globals: at(std::ptr::from_ref(&f.globals) as usize),
            builtins: at(std::ptr::from_ref(&f.builtins) as usize),
        }
    }
}

/// A callee a `CALL` site lowers in line (see [`plan`]).
pub(super) struct Plan {
    code: Rc<CodeObject>,
    depths: Vec<i64>,
    starts: Vec<bool>,
    never_ran: Vec<bool>,
    has_self: bool,
    missing: usize,
    nlocals: usize,
    /// The locals its body rebinds.
    stored: Vec<bool>,
    /// The callees of its own `CALL`s lowered in line, by pc.
    inner: Vec<(usize, Vec<Plan>)>,
    /// The stack slots it takes from its first local up.
    need: usize,
    layout: FnLayout,
    /// The function, and how a `BINARY_OP` site calls it, for an operator
    /// method.
    func: Rc<crate::object::PyFunction>,
    dunder: Option<Dunder>,
    /// A `LOAD_ATTR` site's `property` getter: the class version it was
    /// resolved under, and where the property holds it.
    getter: Option<(u64, i64)>,
    /// How many receiver classes the site recorded for it.
    classes: usize,
}

/// A `BINARY_OP` site's guard on the operands (see `dunder_note`).
#[derive(Clone, Copy)]
struct Dunder {
    ver: u64,
    right: Option<u8>,
    arg: u32,
}

/// The `property` getter of the `LOAD_ATTR` at `pc`, to lower in line.
fn getter_plan(
    ext: &CodeConstObjects,
    pc: usize,
    path: &mut Vec<usize>,
    budget: &mut usize,
) -> Result<Plan, &'static str> {
    let Some((f, ver, cell)) = crate::property_site_getter(ext, pc) else {
        return Err("no getter known");
    };
    let callee = f.code();
    let mut p = admit(&f, &callee, false, 1, None, false, path, budget)?;
    if p.missing != 0 {
        return Err("defaults");
    }
    p.getter = Some((ver, cell as i64));
    Ok(p)
}

/// The method of the `BINARY_OP` at `pc`, to lower in line.
fn dunder_plan(
    code: &CodeObject,
    ext: &CodeConstObjects,
    pc: usize,
    path: &mut Vec<usize>,
    budget: &mut usize,
) -> Result<Plan, &'static str> {
    let Some((f, ver, right)) = crate::dunder_seen(ext, pc) else {
        return Err("no method known");
    };
    let callee = f.code();
    let mut p = admit(&f, &callee, false, 2, None, false, path, budget)?;
    if p.missing != 0 {
        return Err("defaults");
    }
    p.dunder = Some(Dunder {
        ver,
        right,
        arg: code.instructions[pc].arg,
    });
    Ok(p)
}

impl Plan {
    fn inner_at(&self, pc: usize) -> Option<&[Plan]> {
        self.inner
            .iter()
            .find(|(k, _)| *k == pc)
            .map(|(_, p)| p.as_slice())
    }
}

/// The callees of a code's `CALL` sites lowered in line.
#[derive(Default)]
pub(super) struct Plans {
    calls: Vec<(usize, Vec<Plan>)>,
    /// The stack capacity the code needs for them.
    pub(super) need: usize,
    /// How many instructions of callees they lower.
    pub(super) total: usize,
}

impl Plans {
    fn at(&self, pc: usize) -> Option<&[Plan]> {
        self.calls
            .iter()
            .find(|(k, _)| *k == pc)
            .map(|(_, p)| p.as_slice())
    }
}

/// The instructions that run in an inlined body (anything else leaves
/// through [`h_inline_exit`]).
fn inline_op(op: OpCode) -> bool {
    matches!(
        op,
        OpCode::Nop
            | OpCode::NotTaken
            | OpCode::Resume
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
            | OpCode::LoadAttr
            | OpCode::IsOp
            | OpCode::CopyTop
            | OpCode::Swap
            | OpCode::BinarySubscr
            | OpCode::StoreSubscr
            | OpCode::UnpackSequence
            | OpCode::ContainsOp
            | OpCode::LoadGlobal
            | OpCode::PushNull
            | OpCode::LoadMethodAttr
            | OpCode::StoreAttr
            | OpCode::BuildTuple
            | OpCode::BuildList
            | OpCode::UnaryOp
            | OpCode::ReturnValue
            | OpCode::Call
            | OpCode::CallKw
            | OpCode::CopyFreeVars
            | OpCode::LoadDeref
            | OpCode::GetIter
            | OpCode::ForIter
            | OpCode::JumpBackward
            | OpCode::PopIter
            | OpCode::BinarySlice
            | OpCode::StoreSlice
            | OpCode::DeleteSubscr
            | OpCode::ListAppend
            | OpCode::ListExtend
            | OpCode::SetAdd
            | OpCode::MapAdd
            | OpCode::UnpackEx
            | OpCode::ListToTuple
            | OpCode::BuildMap
            | OpCode::DeleteFast
            | OpCode::LoadFastAndClear
            | OpCode::LoadSpecial
            | OpCode::LoadSuperAttr
    )
}

/// The slot of the callable of the `CALL` or `CALL_KW` at `pc`, before which
/// the stack is `depth` deep (`-1` for a `CALL_KW` whose names aren't a
/// constant).
fn call_start(code: &CodeObject, ext: &CodeConstObjects, pc: usize, depth: i64) -> i64 {
    let i = code.instructions[pc];
    if i.op != OpCode::CallKw {
        return depth - i64::from(i.arg) - 2;
    }
    match kw_names(code, ext, pc) {
        Some(names) => depth - i64::from(i.arg) - names.len() as i64 - 3,
        None => -1,
    }
}

/// How many arguments, keywords included, the `CALL` or `CALL_KW` at `pc`
/// passes.
pub(super) fn call_argc(code: &CodeObject, ext: &CodeConstObjects, pc: usize) -> usize {
    let i = code.instructions[pc];
    i.arg as usize
        + if i.op == OpCode::CallKw {
            kw_names(code, ext, pc).map_or(0, <[Object]>::len)
        } else {
            0
        }
}

/// A `CALL_KW`'s (at `pc`) keywords' names, from its `LOAD_CONST`.
fn kw_names<'e>(code: &CodeObject, ext: &'e CodeConstObjects, pc: usize) -> Option<&'e [Object]> {
    let load = code.instructions.get(pc.checked_sub(1)?)?;
    if load.op != OpCode::LoadConst {
        return None;
    }
    match ext.objects.get(load.arg as usize)? {
        Object::Tuple(t) => Some(t),
        _ => None,
    }
}

/// Whether keywords `names`, after the `given` positional arguments, are the
/// next parameters of `code` in order, so they bind as positional arguments.
fn keywords_in_place(code: &CodeObject, given: usize, names: &[Object]) -> bool {
    given >= code.posonly_count as usize
        && given + names.len() <= code.arg_count as usize
        && names.iter().enumerate().all(
            |(k, name)| matches!(name, Object::Str(n) if code.varnames[given + k].as_str() == &**n),
        )
}

/// The `CALL` a method load at `attr_pc` feeds: the first later `CALL`
/// whose callable's slot is the receiver's.
fn method_call_pc(
    code: &CodeObject,
    ext: &CodeConstObjects,
    depths: &[i64],
    attr_pc: usize,
) -> Option<usize> {
    let recv = depths.get(attr_pc)? - 1;
    let ins = &code.instructions;
    (attr_pc + 1..ins.len().min(attr_pc + 32)).find(|&k| {
        matches!(ins[k].op, OpCode::Call | OpCode::CallKw)
            && call_start(code, ext, k, depths[k]) == recv
    })
}

/// The method load whose call the `CALL` at `pc` (its callable's slot
/// `start`) is, if any.
fn call_method_pc(code: &CodeObject, depths: &[i64], pc: usize, start: i64) -> Option<usize> {
    let ins = &code.instructions;
    // (Back to where the stack was at the callable's slot, the callable's
    // expression starting there.)
    let from = (0..pc)
        .rev()
        .find(|&k| depths[k] >= 0 && depths[k] <= start)?;
    (from + 1..pc)
        .rev()
        .find(|&k| ins[k].op == OpCode::LoadMethodAttr && depths[k] - 1 == start)
}

/// The Python functions the `CALL` at `pc` has settled on, from what the
/// core loop recorded, latest first: each function, and whether its self
/// slot was filled.
fn callees_at(
    code: &CodeObject,
    ext: &CodeConstObjects,
    depths: &[i64],
    pc: usize,
) -> Vec<(Rc<crate::object::PyFunction>, bool, usize)> {
    let mut found = crate::call_site_callees(ext, pc);
    let start = call_start(code, ext, pc, depths[pc]);
    if let Some(attr) = call_method_pc(code, depths, pc, start) {
        found.extend(
            crate::method_site_callees(ext, attr)
                .into_iter()
                .map(|f| (f, true)),
        );
    }
    let mut seen: Vec<(Rc<crate::object::PyFunction>, bool, usize)> = Vec::new();
    for (f, has_self) in found {
        let c = f.code();
        match seen
            .iter_mut()
            .find(|(g, s, _)| *s == has_self && Rc::ptr_eq(&g.code(), &c))
        {
            Some(entry) => entry.2 += 1,
            None => seen.push((f, has_self, 1)),
        }
    }
    seen
}

/// The callees of `code`'s `CALL` sites to lower in line, those of its most
/// deeply nested loops first, at most `budget` instructions of them.
pub(super) fn plan(
    code: &CodeObject,
    ext: &CodeConstObjects,
    depths: &[i64],
    budget: usize,
) -> Plans {
    let mut plans = Plans::default();
    if !enabled()
        || code.is_generator
        || code.is_coroutine
        || code.is_async_generator
        || depths.len() != code.instructions.len()
    {
        return plans;
    }
    let ins = &code.instructions;
    let mut sites: Vec<(usize, usize)> = ins
        .iter()
        .enumerate()
        .filter(|&(pc, i)| {
            (i.op == OpCode::BinaryOp && depths[pc] >= 2 && crate::dunder_known(ext, pc))
                || (i.op == OpCode::LoadAttr && depths[pc] >= 1 && crate::property_site(ext, pc))
                || (matches!(i.op, OpCode::Call | OpCode::CallKw)
                    && call_start(code, ext, pc, depths[pc]) >= 0
                    && crate::await_send_after(code, pc).is_none())
        })
        .map(|(pc, _)| {
            let loops = ins
                .iter()
                .enumerate()
                .filter(|&(j, i)| {
                    i.op == OpCode::JumpBackward && j > pc && j + 1 - i.arg as usize <= pc
                })
                .count();
            (loops, pc)
        })
        .collect();
    sites.sort_by_key(|&(loops, pc)| (std::cmp::Reverse(loops), pc));
    let mut left = budget;
    let mut path = vec![std::ptr::from_ref(code) as usize];
    for (_, pc) in sites {
        let i = ins[pc];
        if i.op == OpCode::LoadAttr {
            match getter_plan(ext, pc, &mut path, &mut left) {
                Ok(p) => {
                    note(code, pc, "inlined", &p.code.qualname);
                    plans.need = plans.need.max(depths[pc] as usize - 1 + p.need);
                    plans.calls.push((pc, vec![p]));
                }
                Err(why) => note(code, pc, why, "getter"),
            }
            continue;
        }
        if i.op == OpCode::BinaryOp {
            match dunder_plan(code, ext, pc, &mut path, &mut left) {
                Ok(p) => {
                    note(code, pc, "inlined", &p.code.qualname);
                    plans.need = plans.need.max(depths[pc] as usize - 2 + p.need);
                    plans.calls.push((pc, vec![p]));
                }
                Err(why) => note(code, pc, why, "operator"),
            }
            continue;
        }
        let callees = callees_at(code, ext, depths, pc);
        if callees.is_empty() {
            note(code, pc, "no callee known", "");
        }
        let names = kw_names(code, ext, pc).filter(|_| i.op == OpCode::CallKw);
        let poly = callees.len() > 1;
        let mut chosen = Vec::new();
        for (f, has_self, classes) in callees.into_iter().take(MAX_POLY_TINY) {
            let callee = f.code();
            if chosen.len() >= MAX_POLY && callee.instructions.len() > TINY_BODY {
                continue;
            }
            match admit(
                &f,
                &callee,
                has_self,
                call_argc(code, ext, pc),
                names,
                poly,
                &mut path,
                &mut left,
            ) {
                Ok(mut p) => {
                    p.classes = classes;
                    note(code, pc, "inlined", &callee.qualname);
                    let start = call_start(code, ext, pc, depths[pc]) as usize;
                    plans.need = plans.need.max(start + 2 + p.need);
                    chosen.push(p);
                }
                Err(why) => note(code, pc, why, &callee.qualname),
            }
        }
        if !chosen.is_empty() {
            plans.calls.push((pc, chosen));
        }
    }
    plans.total = budget - left;
    plans
}

fn note(code: &CodeObject, pc: usize, what: &str, callee: &str) {
    if stats::enabled() {
        eprintln!("frame jit inline: {}@{pc} {callee}: {what}", code.qualname);
    }
}

/// The blocks of a callee (of `starts`) worth lowering: those it ran. As
/// [`never_ran_blocks`], and besides those a block with attribute or global
/// sites none of which ever resolved.
fn callee_never_ran(code: &CodeObject, ext: &CodeConstObjects, starts: &[bool]) -> Vec<bool> {
    use weavepy_compiler::InlineCache as IC;
    let mut cold = never_ran_blocks(code, starts);
    let n = starts.len();
    // A handler (and what only it leads to) runs in the activation an exit
    // builds, never in line.
    let ins = &code.instructions;
    let mut reached = vec![false; n];
    let mut work = vec![0];
    while let Some(from) = work.pop() {
        if std::mem::replace(&mut reached[from], true) {
            continue;
        }
        let end = (from + 1..n).find(|&k| starts[k]).unwrap_or(n);
        let last = ins[end - 1];
        let next = end;
        let target = match last.op {
            OpCode::JumpForward
            | OpCode::PopJumpIfFalse
            | OpCode::PopJumpIfTrue
            | OpCode::PopJumpIfNone
            | OpCode::PopJumpIfNotNone
            | OpCode::ForIter => Some(next + last.arg as usize),
            OpCode::JumpBackward => Some(next.saturating_sub(last.arg as usize)),
            _ => None,
        };
        let falls = !matches!(
            last.op,
            OpCode::JumpForward
                | OpCode::JumpBackward
                | OpCode::ReturnValue
                | OpCode::RaiseVarargs
                | OpCode::Reraise
        );
        // (A loop's exit lies past its `END_FOR` and `POP_ITER`.)
        let mut targets = vec![target, falls.then_some(next)];
        if last.op == OpCode::ForIter {
            for k in 1..=2 {
                targets.push(target.map(|t| t + k));
            }
        }
        for t in targets.into_iter().flatten() {
            if t < n && starts[t] {
                work.push(t);
            }
        }
    }
    for k in 0..n {
        if starts[k] && !reached[k] {
            cold[k] = true;
        }
    }
    let resolved = |pc: usize| {
        !matches!(code.caches.get(pc as u32), IC::Empty) || crate::site_resolved(ext, pc)
    };
    if !(0..n).any(|pc| {
        matches!(
            code.instructions[pc].op,
            OpCode::LoadGlobal | OpCode::LoadAttr | OpCode::LoadMethodAttr | OpCode::StoreAttr
        ) && resolved(pc)
    }) {
        return cold;
    }
    for start in (1..n).filter(|&s| starts[s]) {
        let end = (start + 1..n).find(|&k| starts[k]).unwrap_or(n);
        let mut sites = (start..end).filter(|&k| {
            matches!(
                code.instructions[k].op,
                OpCode::LoadGlobal | OpCode::LoadAttr | OpCode::LoadMethodAttr | OpCode::StoreAttr
            )
        });
        if let Some(first) = sites.next() {
            if !resolved(first) && sites.all(|k| !resolved(k)) {
                cold[start] = true;
            }
        }
    }
    cold
}

/// Whether `callee` (the function `f`'s code) called with `argc` arguments
/// (the last of them the keywords `names`, if any) and its self slot as
/// `has_self` says can run in line, and how: why not otherwise. `path` holds
/// the codes it would be inlined inside.
fn admit(
    f: &Rc<crate::object::PyFunction>,
    callee: &Rc<CodeObject>,
    has_self: bool,
    argc: usize,
    names: Option<&[Object]>,
    poly: bool,
    path: &mut Vec<usize>,
    budget: &mut usize,
) -> Result<Plan, &'static str> {
    let code: &CodeObject = callee;
    let n = code.instructions.len();
    if n > MAX_BODY {
        return Err("too long");
    }
    if n > *budget {
        return Err("over budget");
    }
    if path.len() > MAX_LEVELS {
        return Err("too deep");
    }
    // (A recursive function takes itself in line once, at a site that calls
    // nothing else: most frames of a chain's recursion then never exist. A
    // tree's, whose sites call several classes, lose more to the guards and
    // the compile budget than they save.)
    let at = std::ptr::from_ref(code) as usize;
    if path.iter().filter(|&&c| c == at).count() >= 2 || (poly && path.contains(&at)) {
        return Err("recursive");
    }
    if code.is_generator || code.is_coroutine || code.is_async_generator {
        return Err("generator");
    }
    if Interpreter::has_extended_params(code) {
        return Err("extended parameters");
    }
    if !code.cellvars.is_empty() || f.lean_cells_ref(code).is_none() {
        return Err("cells");
    }
    if code.wire.as_ref().is_some_and(|w| w.exec_error.is_some()) {
        return Err("wire");
    }
    let given = argc + usize::from(has_self);
    let Some(missing) = (code.arg_count as usize).checked_sub(given) else {
        return Err("surplus arguments");
    };
    if let Some(names) = names {
        if !keywords_in_place(code, given - names.len(), names) {
            return Err("keywords");
        }
    }
    if missing > 0 && !Interpreter::defaults_cover(f, missing) {
        return Err("defaults");
    }
    let Some(ext) = crate::code_vm_ext(code) else {
        return Err("no extension");
    };
    if ext.dispatch_len == 0 {
        return Err("no dispatch");
    }
    // A callee that has barely run doesn't pay for its compile time here, and
    // shows little of what it does.
    if !crate::callee_warm(code, ext) {
        return Err("not warm");
    }
    let depths = weavepy_compiler::cpython_code::compute_startdepths(code);
    if depths.len() != n {
        return Err("depths");
    }
    let starts = block_starts(code, &depths);
    let never_ran = callee_never_ran(code, ext, &starts);
    let nlocals = code.varnames.len();
    let mut stored = vec![false; nlocals];
    let mut inner = Vec::new();
    let mut inner_need = 0;
    path.push(std::ptr::from_ref(code) as usize);
    *budget -= n;
    let mut pc = 0;
    let mut result = Ok(());
    while pc < n {
        if starts[pc] && never_ran[pc] {
            pc += 1;
            while pc < n && !starts[pc] {
                pc += 1;
            }
            continue;
        }
        let ins = code.instructions[pc];
        let mut leaves = !inline_op(ins.op);
        match ins.op {
            OpCode::StoreFast | OpCode::DeleteFast | OpCode::LoadFastAndClear => {
                if let Some(s) = stored.get_mut(ins.arg as usize) {
                    *s = true;
                }
            }
            OpCode::LoadAttr if crate::property_site(ext, pc) => {
                match getter_plan(ext, pc, path, budget) {
                    Ok(p) => {
                        let start = nlocals + depths[pc] as usize - 1;
                        inner_need = inner_need.max(start + p.need);
                        inner.push((pc, vec![p]));
                    }
                    Err(_) => leaves = true,
                }
            }
            OpCode::BinaryOp if crate::instance_site(ext, pc) => {
                match dunder_plan(code, ext, pc, path, budget) {
                    Ok(p) => {
                        let start = nlocals + depths[pc] as usize - 2;
                        inner_need = inner_need.max(start + p.need);
                        inner.push((pc, vec![p]));
                    }
                    Err(_) => leaves = true,
                }
            }
            OpCode::CompareOp | OpCode::BinarySubscr if crate::instance_site(ext, pc) => {
                leaves = true;
            }
            // (`FOR_ITER` takes the loop's exit past it.)
            OpCode::EndFor => leaves = false,
            OpCode::CallKw if call_start(code, ext, pc, depths[pc]) >= 0 => {
                // (Lowered in line, or it leaves.)
                let names = kw_names(code, ext, pc);
                let mut chosen = Vec::new();
                if names.is_some() {
                    let callees = callees_at(code, ext, &depths, pc);
                    let poly = callees.len() > 1;
                    for (g, self_slot, classes) in callees.into_iter().take(MAX_POLY_TINY) {
                        let c = g.code();
                        if chosen.len() >= MAX_POLY && c.instructions.len() > TINY_BODY {
                            continue;
                        }
                        let n = call_argc(code, ext, pc);
                        if let Ok(mut p) = admit(&g, &c, self_slot, n, names, poly, path, budget) {
                            p.classes = classes;
                            let start = nlocals + call_start(code, ext, pc, depths[pc]) as usize;
                            inner_need = inner_need.max(start + 2 + p.need);
                            chosen.push(p);
                        }
                    }
                }
                leaves = chosen.is_empty();
                if !chosen.is_empty() {
                    inner.push((pc, chosen));
                }
            }
            OpCode::Call if depths[pc] >= i64::from(ins.arg) + 2 => {
                if !builtin_in_line(ext, pc, ins.arg as usize) {
                    let callees = callees_at(code, ext, &depths, pc);
                    // (Anything not lowered in line runs as a direct call,
                    // see `h_inline_call`, and a site with an inline cache
                    // and no Python function may be a class's.)
                    let poly = callees.len() > 1;
                    leaves = callees.is_empty()
                        && matches!(
                            code.caches.get(pc as u32),
                            weavepy_compiler::InlineCache::Empty
                        );
                    let mut chosen = Vec::new();
                    for (g, self_slot, classes) in callees.into_iter().take(MAX_POLY_TINY) {
                        let c = g.code();
                        if chosen.len() >= MAX_POLY && c.instructions.len() > TINY_BODY {
                            continue;
                        }
                        if c.is_generator || c.is_coroutine || c.is_async_generator {
                            leaves = true;
                        }
                        if let Ok(mut p) = admit(
                            &g,
                            &c,
                            self_slot,
                            ins.arg as usize,
                            None,
                            poly,
                            path,
                            budget,
                        ) {
                            p.classes = classes;
                            let start = nlocals + (depths[pc] - i64::from(ins.arg) - 2) as usize;
                            inner_need = inner_need.max(start + 2 + p.need);
                            chosen.push(p);
                        }
                    }
                    if !chosen.is_empty() {
                        inner.push((pc, chosen));
                    }
                }
            }
            _ => {}
        }
        // What a body has run that can't run in line would leave every time
        // it runs again. (Error paths are worth leaving by.)
        if leaves && !matches!(ins.op, OpCode::RaiseVarargs | OpCode::Reraise) {
            result = Err(match ins.op {
                OpCode::Call if crate::call_site_builtin(ext, pc).is_some() => "calls a builtin",
                op => op.name(),
            });
            break;
        }
        pc += 1;
    }
    path.pop();
    if let Err(why) = result {
        *budget += n;
        return Err(why);
    }
    let top = depths.iter().copied().max().unwrap_or(0).max(0) as usize;
    Ok(Plan {
        code: callee.clone(),
        need: (nlocals + top + 4).max(inner_need),
        depths,
        starts,
        never_ran,
        has_self,
        missing,
        nlocals,
        stored,
        inner,
        layout: FnLayout::of(f),
        func: f.clone(),
        dunder: None,
        getter: None,
        classes: 1,
    })
}

/// Whether the `CALL` of `argc` arguments at `pc` settled on a builtin an
/// inlined body runs itself (see [`h_inline_builtin`]).
fn builtin_in_line(ext: &CodeConstObjects, pc: usize, argc: usize) -> bool {
    match crate::call_site_builtin(ext, pc) {
        Some((_, Some(kind), _)) => {
            kind.runs_in_core()
                || (kind != crate::LeafKind::Opaque
                    && (1..=4).any(|t| kernel_for(t, kind, argc).is_some()))
        }
        _ => false,
    }
}

/// Where a local of an inlined body lives.
#[derive(Clone, Copy)]
enum Place {
    /// At this slot of the operand stack.
    Stack(usize),
    /// The outermost code's local (a borrowed argument).
    Root(u32),
    /// A constant at this address (a borrowed argument).
    At(i64),
    /// At this address, valid until the next operation that isn't
    /// [`pure_op`] (a field read's value, see [`Lower::inline_field`]).
    Temp(Value),
}

/// What a field read proved about its receiver, for the next read of the
/// same receiver up to the next operation that isn't [`pure_op`].
#[derive(Clone, Copy)]
enum FieldMemo {
    /// Its split values block and their count.
    Split { block: Value, len: Value },
    /// Its laid-out `__slots__` values.
    Slots { values: Value },
}

/// The operations between which a field read's value can't move: they change
/// nothing a field read looked at, and run no Python code. (A `BINARY_OP`'s or
/// `COMPARE_OP`'s helpers take their operands' copies and touch nothing else.)
fn pure_op(op: OpCode) -> bool {
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
            | OpCode::LoadAttr
            | OpCode::BinaryOp
            | OpCode::CompareOp
            | OpCode::UnaryOp
            | OpCode::IsOp
            | OpCode::ToBool
            | OpCode::CopyTop
            | OpCode::Swap
            | OpCode::BinarySubscr
            | OpCode::PopJumpIfFalse
            | OpCode::PopJumpIfTrue
            | OpCode::PopJumpIfNone
            | OpCode::PopJumpIfNotNone
            | OpCode::JumpForward
            | OpCode::ReturnValue
    )
}

/// An inlined body being lowered: what its instructions need beyond the
/// caller's fields, and those fields, to be put back (see
/// [`Lower::inline_call`]).
pub(super) struct InlineFrame<'a> {
    plan: &'a Plan,
    site: i64,
    start: usize,
    base: usize,
    /// Where the function's `Object` is, when no stack slot holds it.
    method_at: Option<i64>,
    places: Vec<Place>,
    /// A parameter's value, where it's known unboxed (it never changes).
    values: Vec<Option<Item>>,
    /// What this block's field reads proved, by receiver (see
    /// [`Lower::inline_field`]).
    fields: Vec<(u32, FieldMemo)>,
    exit_block: Block,
    ret: Block,
    saved: Saved<'a>,
}

/// A caller's lowering state, while a callee's body lowers.
struct Saved<'a> {
    code: &'a CodeObject,
    ext: &'a CodeConstObjects,
    nlocals: usize,
    depths: &'a [i64],
    field_slots: crate::Sites<'a, FieldSlot>,
    maybe_unbound: Vec<bool>,
    bound: Vec<bool>,
    blocks: Vec<Option<Block>>,
    never_ran: Vec<bool>,
    starts: Vec<bool>,
    range: std::ops::Range<usize>,
    cold: bool,
    vs: Vec<Item>,
    depth: usize,
    last_pc: Option<usize>,
}

impl<'a> Lower<'a> {
    /// The callee lowered in line at the `CALL` at `pc`, in the code being
    /// lowered.
    pub(super) fn plan_at(&self, pc: usize) -> Option<&'a [Plan]> {
        match self.inl.last() {
            Some(f) => f.plan.inner_at(pc),
            None => self.plans.at(pc),
        }
    }

    /// Whether the method load at `attr_pc` feeds a `CALL` lowered in line
    /// (which takes the plain method load, not a leaf call's).
    pub(super) fn method_inlined(&self, attr_pc: usize) -> bool {
        method_call_pc(self.code, self.ext, self.depths, attr_pc)
            .is_some_and(|k| self.plan_at(k).is_some())
    }

    /// Whether an inlined body is being lowered.
    #[inline]
    pub(super) fn inlining(&self) -> bool {
        !self.inl.is_empty()
    }

    /// The stack depth before the instruction at `pc`, in this lowering's
    /// numbering: an inlined body's operand stack starts past its locals.
    pub(super) fn depth_at(&self, pc: usize) -> i64 {
        let d = self.depths.get(pc).copied().unwrap_or(-1);
        match self.inl.last() {
            Some(f) if d >= 0 => d + f.base as i64,
            _ => d,
        }
    }

    /// Where local `i` of an inlined body lives, if one is being lowered.
    pub(super) fn inline_local_addr(&mut self, i: u32) -> Option<Value> {
        let place = self.inl.last()?.places[i as usize];
        Some(match place {
            Place::Stack(s) => self.slot_addr(s),
            Place::Root(j) => self.b.ins().iadd_imm(self.locals, 16 * i64::from(j)),
            Place::At(a) => self.b.ins().iconst(self.ptr, a),
            Place::Temp(v) => v,
        })
    }

    /// Whether a field read of the local `i` has proved its receiver this
    /// block.
    fn field_memo(&self, i: u32) -> Option<FieldMemo> {
        let f = self.inl.last()?;
        f.fields.iter().find(|(k, _)| *k == i).map(|&(_, m)| m)
    }

    /// Forget what field reads proved, and write the values they left on the
    /// virtual stack to the stack (with their references taken): something
    /// that could move them is next.
    pub(super) fn inline_settle_fields(&mut self) {
        let Some(f) = self.inl.last_mut() else {
            return;
        };
        f.fields.clear();
        let nlocals = self.nlocals as u32;
        let base = self.depth - self.vs.len();
        for k in 0..self.vs.len() {
            if let Item::Local(i) = self.vs[k] {
                if i >= nlocals {
                    self.materialize(Item::Local(i), base + k);
                    self.vs[k] = Item::Mem(base + k);
                }
            }
        }
    }

    /// `LOAD_ATTR` in an inlined body of the local atop the stack, at a site
    /// settled on one instance field of one class: the guards (leaving for
    /// the core loop, with nothing done, when one fails), or only the
    /// position's when an earlier read proved the receiver, and the value
    /// left where it lies. `None`, emitting nothing, for another site.
    pub(super) fn inline_field(&mut self, pc: usize) -> Option<bool> {
        let l = *self.layout?;
        let &Item::Local(i) = self.vs.last()? else {
            return None;
        };
        if crate::property_site(self.ext, pc) || self.ext.attr_poly_slot(pc).is_some() {
            return None;
        }
        let (ver, idx) = self.field_slots.get(pc)?.get();
        let slots = idx & crate::SLOT_FIELD != 0;
        if ver == 0 || (slots && !l.slots_ok) {
            return None;
        }
        let item = self.pop();
        let out = self.exit_with(pc, &[item], INTERP);
        let ptr = self.ptr;
        let memo = match self.field_memo(i) {
            Some(m) => m,
            None => {
                let at = self.local_addr(i);
                let otag = self.tag_at(at);
                let not_inst =
                    self.b
                        .ins()
                        .icmp_imm(IntCC::NotEqual, otag, i64::from(self.tags.instance));
                self.branch_out(not_inst, out);
                let inst = self.b.ins().load(ptr, FLAGS, at, 8);
                let cls = self.b.ins().load(ptr, FLAGS, inst, l.inst_class);
                let cver = self
                    .b
                    .ins()
                    .load(types::I64, FLAGS, cls, l.type_attr_version);
                let want = self.b.ins().iconst(types::I64, ver as i64);
                let stale = self.b.ins().icmp(IntCC::NotEqual, cver, want);
                let flag = self.b.ins().iconst(ptr, l.cells_unguarded as i64);
                let shared = self.b.ins().uload8(types::I64, FLAGS, flag, 0);
                let m = if slots {
                    let clayout = self.b.ins().load(ptr, FLAGS, cls, type_slot_layout(&l));
                    let borrow = self.b.ins().sload32(FLAGS, inst, l.inst_slots_borrow);
                    let form = self
                        .b
                        .ins()
                        .uload8(types::I32, FLAGS, inst, l.inst_slots_tag);
                    let names = self.b.ins().load(ptr, FLAGS, inst, l.inst_slots_layout);
                    let values = self.b.ins().load(ptr, FLAGS, inst, l.inst_slots_values);
                    let busy = self.b.ins().icmp_imm(IntCC::SignedLessThan, borrow, 0);
                    let other =
                        self.b
                            .ins()
                            .icmp_imm(IntCC::NotEqual, form, i64::from(l.slots_laid_out));
                    let foreign = self.b.ins().icmp(IntCC::NotEqual, names, clayout);
                    let unsafe_now = self.b.ins().icmp_imm(IntCC::NotEqual, shared, 0);
                    let bad = self.b.ins().bor(stale, busy);
                    let bad = self.b.ins().bor(bad, other);
                    let bad = self.b.ins().bor(bad, foreign);
                    let bad = self.b.ins().bor(bad, unsafe_now);
                    self.branch_out(bad, out);
                    FieldMemo::Slots { values }
                } else {
                    let lazy = self.b.ins().load(ptr, FLAGS, inst, l.inst_dict_lazy);
                    let borrow = self.b.ins().sload32(FLAGS, inst, l.inst_split_borrow);
                    let block = self.b.ins().load(ptr, FLAGS, inst, l.inst_split_block);
                    let busy = self.b.ins().icmp_imm(IntCC::SignedLessThan, borrow, 0);
                    let empty = self.b.ins().icmp_imm(IntCC::Equal, block, 0);
                    let other = self.b.ins().bor(lazy, shared);
                    let other = self.b.ins().icmp_imm(IntCC::NotEqual, other, 0);
                    let bad = self.b.ins().bor(stale, busy);
                    let bad = self.b.ins().bor(bad, empty);
                    let bad = self.b.ins().bor(bad, other);
                    self.branch_out(bad, out);
                    let keys = self.b.ins().load(ptr, FLAGS, block, l.split_keys);
                    let ckeys = self.b.ins().load(ptr, FLAGS, cls, l.type_shared_keys);
                    let foreign = self.b.ins().icmp(IntCC::NotEqual, keys, ckeys);
                    self.branch_out(foreign, out);
                    let len = self.b.ins().uload32(FLAGS, block, l.split_len);
                    FieldMemo::Split { block, len }
                };
                self.inl
                    .last_mut()
                    .expect("in an inlined body")
                    .fields
                    .push((i, m));
                m
            }
        };
        let v = match memo {
            FieldMemo::Slots { values } => {
                let off = i64::from(idx & !crate::SLOT_FIELD) * 16;
                let v = self.b.ins().iadd_imm(values, off);
                let vt = self.tag_at(v);
                let none = self
                    .b
                    .ins()
                    .icmp_imm(IntCC::Equal, vt, i64::from(self.tags.unbound));
                self.branch_out(none, out);
                v
            }
            FieldMemo::Split { block, len } => {
                let absent =
                    self.b
                        .ins()
                        .icmp_imm(IntCC::UnsignedLessThanOrEqual, len, i64::from(idx));
                self.branch_out(absent, out);
                let off = i64::from(idx) * 16 + i64::from(l.split_values);
                self.b.ins().iadd_imm(block, off)
            }
        };
        let f = self.inl.last_mut().expect("in an inlined body");
        let temp = f.places.len() as u32;
        f.places.push(Place::Temp(v));
        self.push(Item::Local(temp));
        self.set_last(pc);
        Some(true)
    }

    /// An inlined body's parameter `i`, where its value is known unboxed.
    pub(super) fn inline_value(&self, i: u32) -> Option<Item> {
        self.inl.last()?.values.get(i as usize).copied().flatten()
    }

    /// The instruction at `pc` as an inlined body runs it: `Some` when it
    /// doesn't (it leaves for the core loop there).
    pub(super) fn inline_gate(&mut self, pc: usize, op: OpCode) -> Option<bool> {
        if !self.inlining() {
            return None;
        }
        if !inline_op(op) {
            self.exit(INTERP, pc);
            return Some(false);
        }
        // (A method lowered in line may do anything; so may the fused
        // shapes a `LOAD_FAST` starts.)
        let next = self.code.instructions.get(pc + 1).map(|i| i.op);
        let fused = matches!(op, OpCode::LoadFast | OpCode::LoadFastBorrow)
            && matches!(next, Some(OpCode::StoreAttr | OpCode::LoadMethodAttr));
        let in_line =
            matches!(op, OpCode::BinaryOp | OpCode::LoadAttr) && self.plan_at(pc).is_some();
        if !pure_op(op) || fused || in_line {
            self.inline_settle_fields();
        }
        None
    }

    /// `CALL` at `pc` of the callees `plans` lower in line: each one's guards
    /// and body in turn, then the ordinary call for anything their guards
    /// don't take. The result on the stack where the callable was.
    pub(super) fn inline_call(&mut self, pc: usize, argc: usize, plans: &'a [Plan]) -> bool {
        let before = self.b.func.dfg.num_insts();
        let nested = self.inlining();
        // (A `CALL_KW`'s names, a constant, were checked against each callee's
        // parameters: its arguments bind as positional ones.)
        let is_kw = self.code.instructions[pc].op == OpCode::CallKw;
        let names = is_kw.then(|| self.pop());
        if let Some(n) = names {
            if !matches!(n, Item::Const(_)) {
                self.push(n);
                self.exit(INTERP, pc);
                return false;
            }
        }
        let start = self.depth - argc - 2;
        let mut args: Vec<Item> = (0..argc).map(|_| self.pop()).collect();
        args.reverse();
        // The callable, its self slot and all below on the stack; the
        // arguments as they are.
        self.flush();
        let call_last = self.last_pc;
        let join = self.b.create_block();
        let mut slow = None;
        for plan in plans {
            let next = self.b.create_block();
            self.vs.clear();
            self.depth = start + 2;
            self.last_pc = call_last;
            self.inline_body(pc, plan, &args, start, next, join, None);
            self.b.switch_to_block(next);
            slow = Some(next);
        }
        // Anything the guards didn't take.
        if let Some(slow) = slow {
            self.b.set_cold_block(slow);
        }
        self.vs = args;
        self.depth = start + 2 + argc;
        self.last_pc = call_last;
        if let Some(n) = names {
            self.push(n);
            if nested {
                self.exit(INTERP, pc);
            } else {
                self.call_with(pc, h_call_kw as *const () as usize, false);
                self.b.ins().jump(join, &[]);
            }
        } else {
            self.plain_call(pc, argc);
            self.b.ins().jump(join, &[]);
        }
        self.b.switch_to_block(join);
        self.vs.clear();
        self.depth = start + 1;
        self.set_last(pc);
        if !nested {
            self.inline_ir += self.b.func.dfg.num_insts() - before;
        }
        true
    }

    /// The ordinary `CALL` of `argc` arguments at `pc`, what its site's
    /// bodies' guards didn't take: the block goes on with the result in place.
    fn plain_call(&mut self, pc: usize, argc: usize) {
        if self.inlining() {
            self.inline_python_call(pc, argc);
            return;
        }
        self.flush();
        self.store_last();
        let code = self.code_ptr();
        let ext = self.ext_ptr();
        let len = self.b.ins().iconst(types::I64, self.depth as i64);
        let pc_argc = self
            .b
            .ins()
            .iconst(types::I64, (pc as i64) | ((argc as i64) << 32));
        let dsite: Box<DirectSite> = Box::default();
        let dsite_at = self
            .b
            .ins()
            .iconst(self.ptr, std::ptr::from_ref::<DirectSite>(&dsite) as i64);
        self.direct_sites.push(dsite);
        let r = self
            .call(
                h_call_site as *const () as usize,
                &[self.st, code, ext, pc_argc, len, dsite_at],
                true,
            )
            .expect("returns");
        self.called(pc, argc, r);
    }

    /// `LOAD_ATTR` of a method at `attr_pc` whose `CALL`, past plain loads of
    /// its arguments, runs callees lowered in line: the whole call here, each
    /// body guarded by the receiver's class version, with the ordinary method
    /// load and call for anything their guards don't take (a load that
    /// declines leaves for the core loop at `attr_pc`, nothing done: the
    /// argument loads have no effects). The block goes on past the `CALL`.
    /// `None`, emitting nothing, for any other shape.
    pub(super) fn inline_method(&mut self, attr_pc: usize) -> Option<bool> {
        self.layout?;
        let before = self.b.func.dfg.num_insts();
        let code = self.code;
        let ins = &code.instructions;
        let call_pc = method_call_pc(code, self.ext, self.depths, attr_pc)?;
        // (A `CALL_KW`'s names, the load before it, aren't an argument.)
        let is_kw = ins[call_pc].op == OpCode::CallKw;
        let last_arg = call_pc - usize::from(is_kw);
        if call_argc(code, self.ext, call_pc) != last_arg - attr_pc - 1
            || self.depth == 0
            || self.starts[attr_pc + 1..=call_pc].iter().any(|&s| s)
        {
            return None;
        }
        let plans = self.plan_at(call_pc)?;
        if !plans.iter().all(|p| p.has_self && p.dunder.is_none()) {
            return None;
        }
        for i in &ins[attr_pc + 1..last_arg] {
            match i.op {
                OpCode::LoadFast | OpCode::LoadFastBorrow => {
                    let k = i.arg as usize;
                    if k >= self.nlocals || (self.maybe_unbound[k] && !self.bound[k]) {
                        return None;
                    }
                }
                OpCode::LoadConst if (i.arg as usize) < self.ext.objects.len() => {}
                OpCode::LoadSmallInt => {}
                _ => return None,
            }
        }
        if !matches!(self.vs.last(), None | Some(Item::Local(_) | Item::Mem(_))) {
            return None;
        }
        let recv = self.pop();
        let mut items = vec![recv];
        for i in &ins[attr_pc + 1..last_arg] {
            items.push(match i.op {
                OpCode::LoadConst => Item::Const(i.arg),
                OpCode::LoadSmallInt => {
                    Item::Int(self.b.ins().iconst(types::I64, i64::from(i.arg)))
                }
                _ => self.inline_value(i.arg).unwrap_or(Item::Local(i.arg)),
            });
        }
        // All below the receiver on the stack; it and the arguments as they
        // are.
        self.flush();
        let start = self.depth;
        let call_last = self.last_pc;
        let nested = self.inlining();
        let join = self.b.create_block();
        let mut notes = Box::new(MethodNotes {
            entries: Vec::new(),
            ext: std::ptr::from_ref(self.ext),
            name: ins[attr_pc].arg,
        });
        let mut slow = None;
        for plan in plans {
            let next = self.b.create_block();
            let mut caches = Vec::new();
            for _ in 0..plan.classes.min(2) {
                let mut cache: Box<MethodCache> = Box::default();
                caches.push(std::ptr::from_mut::<MethodCache>(&mut cache));
                self.method_caches.push(cache);
            }
            let addrs: Vec<i64> = caches.iter().map(|&c| c as i64).collect();
            notes.entries.push((Rc::as_ptr(&plan.func), caches));
            self.vs.clear();
            self.depth = start;
            self.last_pc = call_last;
            self.inline_body(call_pc, plan, &items, start, next, join, Some(&addrs));
            self.b.switch_to_block(next);
            slow = Some(next);
        }
        if let Some(slow) = slow {
            self.b.set_cold_block(slow);
        }
        // Anything else: the method's load, what the guards need for next
        // time, and the call.
        self.vs = vec![recv];
        self.depth = start + 1;
        self.last_pc = call_last;
        self.flush();
        let cache: Box<MethodCache> = Box::default();
        let cache_at = std::ptr::from_ref::<MethodCache>(&cache) as i64;
        self.method_caches.push(cache);
        let done = self.b.create_block();
        self.load_method_helper(attr_pc, cache_at, done);
        let callee = self.slot_addr(start);
        let notes_at = self
            .b
            .ins()
            .iconst(self.ptr, std::ptr::from_ref::<MethodNotes>(&notes) as i64);
        self.method_notes.push(notes);
        self.call(
            h_inline_method_note as *const () as usize,
            &[callee, notes_at],
            false,
        );
        self.vs = items[1..].to_vec();
        self.depth = start + 2 + items.len() - 1;
        if is_kw {
            self.push(Item::Const(ins[last_arg].arg));
            if nested {
                self.exit(INTERP, call_pc);
            } else {
                self.call_with(call_pc, h_call_kw as *const () as usize, false);
                self.b.ins().jump(join, &[]);
            }
        } else {
            self.plain_call(call_pc, items.len() - 1);
            self.b.ins().jump(join, &[]);
        }
        self.b.switch_to_block(join);
        self.vs.clear();
        self.depth = start + 1;
        self.set_last(call_pc);
        if !nested {
            self.inline_ir += self.b.func.dfg.num_insts() - before;
        }
        self.skip_to = Some(call_pc);
        Some(true)
    }

    /// The guards of a fused method call: the receiver an instance of a class
    /// one of the caches read, which holds no dictionary and no value that
    /// could shadow the method (a mismatch of either kind goes to `slow`).
    /// The values to test: the site's being off, and a value false.
    fn method_guards(
        &mut self,
        recv: Item,
        caches: &[i64],
        sv: Value,
        slow: Block,
    ) -> (Value, Value) {
        let l = *self.layout.expect("checked by the caller");
        let ptr = self.ptr;
        let at = self
            .operand_addr(recv)
            .expect("checked by the caller: a local or on the stack");
        let tag = self.tag_at(at);
        let not_inst = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, tag, i64::from(self.tags.instance));
        self.branch_out(not_inst, slow);
        let inst = self.b.ins().load(ptr, FLAGS, at, 8);
        let cls = self.b.ins().load(ptr, FLAGS, inst, l.inst_class);
        let ver = self
            .b
            .ins()
            .load(types::I64, FLAGS, cls, l.type_attr_version);
        let c = self.b.ins().iconst(ptr, caches[0]);
        let want = self.b.ins().load(types::I64, FLAGS, c, MC_VER);
        let lazy = self.b.ins().load(ptr, FLAGS, inst, l.inst_dict_lazy);
        let flag = self.b.ins().iconst(ptr, l.cells_unguarded as i64);
        let unguarded = self.b.ins().uload8(types::I64, FLAGS, flag, 0);
        let borrow = self.b.ins().sload32(FLAGS, inst, l.inst_split_borrow);
        let busy = self.b.ins().icmp_imm(IntCC::SignedLessThan, borrow, 0);
        let other = self.b.ins().bor(lazy, unguarded);
        let other = self.b.ins().icmp_imm(IntCC::NotEqual, other, 0);
        let ckeys = self.b.ins().load(types::I64, FLAGS, c, MC_KEYS);
        let before = self.b.ins().uload32(FLAGS, c, MC_BEFORE);
        let (stale, ckeys, before) = if let Some(&second) = caches.get(1) {
            let c2 = self.b.ins().iconst(ptr, second);
            let want2 = self.b.ins().load(types::I64, FLAGS, c2, MC_VER);
            let keys2 = self.b.ins().load(types::I64, FLAGS, c2, MC_KEYS);
            let before2 = self.b.ins().uload32(FLAGS, c2, MC_BEFORE);
            let first = self.b.ins().icmp(IntCC::Equal, ver, want);
            let second = self.b.ins().icmp(IntCC::Equal, ver, want2);
            let hit = self.b.ins().bor(first, second);
            let stale = self.b.ins().bxor_imm(hit, 1);
            let ckeys = self.b.ins().select(first, ckeys, keys2);
            let before = self.b.ins().select(first, before, before2);
            (stale, ckeys, before)
        } else {
            (self.b.ins().icmp(IntCC::NotEqual, ver, want), ckeys, before)
        };
        let bad = self.b.ins().bor(stale, busy);
        let bad = self.b.ins().bor(bad, other);
        self.branch_out(bad, slow);
        let block = self.b.ins().load(ptr, FLAGS, inst, l.inst_split_block);
        let check = self.b.create_block();
        let hit = self.b.create_block();
        self.b.ins().brif(block, check, &[], hit, &[]);
        self.b.switch_to_block(check);
        let n = self.b.ins().uload32(FLAGS, block, l.split_len);
        let keys = self.b.ins().load(types::I64, FLAGS, block, l.split_keys);
        let empty = self.b.ins().icmp_imm(IntCC::Equal, n, 0);
        let same = self.b.ins().icmp(IntCC::Equal, keys, ckeys);
        let short = self.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, n, before);
        let held = self.b.ins().band(same, short);
        let ok = self.b.ins().bor(empty, held);
        self.b.ins().brif(ok, hit, &[], slow, &[]);
        self.b.switch_to_block(hit);
        let key = self.b.ins().load(types::I64, FLAGS, sv, IS_KEY);
        let off = self.b.ins().icmp_imm(IntCC::Equal, key, 0);
        let no = self.b.ins().iconst(types::I8, 0);
        (off, no)
    }

    /// One callee of [`Self::inline_call`], the callable and all below it on
    /// the stack and `args` as they are: its guards, going to `slow` with
    /// nothing done, then its body, going on to `join` with its result in
    /// the callable's slot.
    fn inline_body(
        &mut self,
        pc: usize,
        plan: &'a Plan,
        args: &[Item],
        start: usize,
        slow: Block,
        join: Block,
        method: Option<&[i64]>,
    ) {
        let nested = self.inlining();
        let argc = args.len();
        let code: &'a CodeObject = &plan.code;
        // (A fused method call's arguments start with its receiver.)
        let self_slot = usize::from(plan.has_self && method.is_none());
        let no_callable = plan.dunder.is_some() || plan.getter.is_some() || method.is_some();
        let first = match (no_callable, plan.has_self) {
            (true, _) => start,
            (false, true) => start + 1,
            (false, false) => start + 2,
        };
        let nlocals = plan.nlocals;
        let base = first + nlocals;
        let handle = {
            // SAFETY: an `Rc` is one word (see `GlobalCache`).
            unsafe { std::ptr::from_ref(&plan.code).cast::<u64>().read() }
        };
        let mut site = Box::new(InlineSite {
            key: Cell::new(
                plan.dunder
                    .map(|d| d.ver)
                    .or(plan.getter.map(|g| g.0))
                    .unwrap_or(handle),
            ),
            entries: Cell::new(0),
            exits: Cell::new(0),
            parent: self
                .inl
                .last()
                .map_or(std::ptr::null(), |f| f.site as *const InlineSite),
            code: plan.code.clone(),
            call_pc: pc,
            start,
            first,
            base,
            nlocals,
            borrowed: Vec::new(),
            method: no_callable
                .then(|| (borrowed_function(&plan.func), plan.dunder.map(|d| d.arg))),
        });
        let site_at = std::ptr::from_ref::<InlineSite>(&site) as i64;
        let method_at = site
            .method
            .as_ref()
            .map(|(f, _)| std::ptr::from_ref::<Object>(f) as i64);

        // The guards: nothing has happened yet when one fails.
        let ptr = self.ptr;
        let sv = self.b.ins().iconst(ptr, site_at);
        let (other, wrong) = match (plan.dunder, method) {
            _ if plan.getter.is_some() => {
                let (other, wrong) = self.getter_guards(plan, args[0], sv, slow);
                let stale = self.b.ins().bor(other, wrong);
                self.branch_out(stale, slow);
                let swapped = self.code_swapped(plan, handle);
                let no = self.b.ins().iconst(types::I8, 0);
                (no, swapped)
            }
            (Some(d), _) => {
                // (The function's fields only under the class's version.)
                let (other, wrong) = self.dunder_guards(d, args, sv, slow);
                let stale = self.b.ins().bor(other, wrong);
                self.branch_out(stale, slow);
                let swapped = self.code_swapped(plan, handle);
                let no = self.b.ins().iconst(types::I8, 0);
                (no, swapped)
            }
            (None, Some(caches)) => {
                let (other, wrong) = self.method_guards(args[0], caches, sv, slow);
                let swapped = self.code_swapped(plan, handle);
                (other, self.b.ins().bor(wrong, swapped))
            }
            (None, None) => {
                let at = self.slot_addr(start);
                let tag = self.tag_at(at);
                let not_fn =
                    self.b
                        .ins()
                        .icmp_imm(IntCC::NotEqual, tag, i64::from(self.tags.function));
                self.branch_out(not_fn, slow);
                let word = self.b.ins().load(ptr, FLAGS, at, 8);
                let current = self.b.ins().load(types::I64, FLAGS, word, plan.layout.code);
                let key = self.b.ins().load(types::I64, FLAGS, sv, IS_KEY);
                let other = self.b.ins().icmp(IntCC::NotEqual, current, key);
                let self_at = self.slot_addr(start + 1);
                let self_tag = self.tag_at(self_at);
                let no_self =
                    self.b
                        .ins()
                        .icmp_imm(IntCC::Equal, self_tag, i64::from(self.tags.unbound));
                let wrong = if plan.has_self {
                    no_self
                } else {
                    self.b.ins().bxor_imm(no_self, 1)
                };
                (other, wrong)
            }
        };
        let gen_addr = self
            .b
            .ins()
            .iconst(ptr, crate::hot_gates::loop_gen_ptr() as i64);
        let g = self.b.ins().load(types::I64, FLAGS, gen_addr, 0);
        let snap = self.b.ins().load(types::I64, FLAGS, self.st, S_SNAP);
        let moved = self.b.ins().icmp(IntCC::NotEqual, g, snap);
        let sw = self.b.ins().load(ptr, FLAGS, self.st, S_SW);
        let calls_ok = self.b.ins().uload8(types::I32, FLAGS, sw, SW_CALLS_OK);
        let no_calls = self.b.ins().icmp_imm(IntCC::Equal, calls_ok, 0);
        // The ordinary call's `RecursionError` check, for every level.
        let dc = self.b.ins().load(ptr, FLAGS, self.st, S_DEPTH);
        let d = self.b.ins().load(types::I64, FLAGS, dc, 0);
        let d = self.b.ins().iadd_imm(d, self.inl.len() as i64 + 1);
        let limit_at = self
            .b
            .ins()
            .iconst(ptr, crate::recursion::recursion_limit_ptr() as i64);
        let limit = self.b.ins().load(types::I64, FLAGS, limit_at, 0);
        let deep = self.b.ins().icmp(IntCC::SignedGreaterThan, d, limit);
        let bad = self.b.ins().bor(other, wrong);
        let bad = self.b.ins().bor(bad, moved);
        let bad = self.b.ins().bor(bad, no_calls);
        let bad = self.b.ins().bor(bad, deep);
        self.branch_out(bad, slow);
        // The defaults (`__defaults__` may have been rebound).
        if plan.missing > 0 {
            let given = argc + self_slot;
            let dst = self.slot_addr(first + given);
            let n = self.b.ins().iconst(types::I64, plan.missing as i64);
            let at = match method_at {
                Some(a) => self.b.ins().iconst(ptr, a),
                None => self.slot_addr(start),
            };
            let r = self
                .call(h_inline_defaults as *const () as usize, &[at, dst, n], true)
                .expect("returns");
            self.branch_out(r, slow);
        }
        // Committed.
        if !nested {
            let e = self.b.ins().load(types::I64, FLAGS, sv, IS_ENTRIES);
            let e = self.b.ins().iadd_imm(e, 1);
            self.b.ins().store(FLAGS, e, sv, IS_ENTRIES);
        }
        for i in code.arg_count as usize..nlocals {
            let at = self.slot_addr(first + i);
            self.write_tag(at, self.tags.unbound);
        }
        // The arguments: each at its local's slot, or left where it lies
        // for a local the body never rebinds.
        let mut places: Vec<Place> = (0..nlocals).map(|i| Place::Stack(first + i)).collect();
        let mut values: Vec<Option<Item>> = vec![None; nlocals];
        let mut owned = vec![true; nlocals];
        let mut borrowed: Vec<(usize, Place)> = Vec::new();
        for (k, &item) in args.iter().enumerate() {
            let li = k + self_slot;
            let home = first + li;
            let keep = !plan.stored[li];
            match item {
                Item::Mem(_) => {}
                Item::Local(j) if keep => {
                    places[li] = match self.inl.last() {
                        Some(f) => f.places[j as usize],
                        None => Place::Root(j),
                    };
                    // (A parent's unboxed parameter stays unboxed.)
                    values[li] = self.inline_value(j);
                    owned[li] = false;
                    borrowed.push((home, places[li]));
                }
                Item::Const(c) if keep => {
                    let v = &self.ext.objects[c as usize];
                    places[li] = Place::At(std::ptr::from_ref(v) as i64);
                    owned[li] = false;
                    borrowed.push((home, places[li]));
                    match *v {
                        Object::Int(x) => {
                            values[li] = Some(Item::Int(self.b.ins().iconst(types::I64, x)));
                        }
                        Object::Float(x) => {
                            values[li] = Some(Item::Float(self.b.ins().f64const(x)));
                        }
                        Object::Bool(x) => {
                            values[li] =
                                Some(Item::Bool(self.b.ins().iconst(types::I8, i64::from(x))));
                        }
                        _ => {}
                    }
                }
                Item::Int(_) | Item::Float(_) | Item::Bool(_) | Item::Dyn(..) if keep => {
                    self.materialize(item, home);
                    values[li] = Some(item);
                    owned[li] = false;
                }
                _ => self.materialize(item, home),
            }
        }
        site.borrowed = borrowed;
        self.inline_sites.push(site);

        // The body, in the callee's terms.
        let ext = crate::code_vm_ext(code).expect("checked by the plan");
        self.held_codes.push(plan.code.clone());
        // (All of a site's bodies leave by the outermost's exit.)
        let exit_block = match self.inl.last() {
            Some(f) => f.exit_block,
            None => {
                let block = self.b.create_block();
                self.b.set_cold_block(block);
                self.b.append_block_param(block, types::I64);
                self.b.append_block_param(block, ptr);
                block
            }
        };
        let ret = self.b.create_block();
        let n = code.instructions.len();
        let blocks: Vec<Option<Block>> = plan
            .starts
            .iter()
            .map(|&s| s.then(|| self.b.create_block()))
            .collect();
        let entry = blocks[0].expect("a body starts at its start");
        let saved = Saved {
            code: std::mem::replace(&mut self.code, code),
            ext: std::mem::replace(&mut self.ext, ext),
            nlocals: std::mem::replace(&mut self.nlocals, nlocals),
            depths: std::mem::replace(&mut self.depths, &plan.depths),
            field_slots: std::mem::replace(&mut self.field_slots, ext.alloc_field_sites(code)),
            maybe_unbound: std::mem::replace(
                &mut self.maybe_unbound,
                maybe_unbound_locals(code, nlocals),
            ),
            bound: std::mem::replace(&mut self.bound, vec![false; nlocals]),
            blocks: std::mem::replace(&mut self.blocks, blocks),
            never_ran: std::mem::replace(&mut self.never_ran, plan.never_ran.clone()),
            starts: std::mem::replace(&mut self.starts, plan.starts.clone()),
            range: std::mem::replace(&mut self.range, 0..n),
            cold: std::mem::replace(&mut self.cold, false),
            vs: std::mem::take(&mut self.vs),
            depth: std::mem::replace(&mut self.depth, base),
            last_pc: self.last_pc.take(),
        };
        let mark = self.side_exits.len();
        self.inl.push(InlineFrame {
            plan,
            site: site_at,
            start,
            base,
            method_at,
            places,
            values,
            fields: Vec::new(),
            exit_block,
            ret,
            saved,
        });
        self.b.ins().jump(entry, &[]);
        for s in 0..n {
            if let Some(blk) = self.blocks[s] {
                self.lower_block(s, blk);
            }
        }
        while self.side_exits.len() > mark {
            let x = self.side_exits.pop().expect("checked");
            self.fill_side_exit(x);
        }
        // Back in the caller's terms.
        let frame = self.inl.pop().expect("pushed above");
        let saved = frame.saved;
        self.code = saved.code;
        self.ext = saved.ext;
        self.nlocals = saved.nlocals;
        self.depths = saved.depths;
        self.field_slots = saved.field_slots;
        self.maybe_unbound = saved.maybe_unbound;
        self.bound = saved.bound;
        self.blocks = saved.blocks;
        self.never_ran = saved.never_ran;
        self.starts = saved.starts;
        self.range = saved.range;
        self.cold = saved.cold;
        self.vs = saved.vs;
        self.depth = saved.depth;
        self.last_pc = saved.last_pc;

        // An exit.
        if !nested {
            self.b.switch_to_block(exit_block);
            let packed = self.b.block_params(exit_block)[0];
            let from = self.b.block_params(exit_block)[1];
            self.store_last();
            let r = self
                .call(
                    h_inline_exit as *const () as usize,
                    &[self.st, from, packed],
                    true,
                )
                .expect("returns");
            self.b.ins().return_(&[r]);
        }

        // The return: the locals released in order and the callable, as an
        // activation's return releases them, the result in the callable's
        // slot.
        self.b.switch_to_block(ret);
        for (i, &own) in owned.iter().enumerate() {
            if own {
                let at = self.slot_addr(first + i);
                self.release_slot(at);
            }
        }
        let at = self.slot_addr(start);
        if method_at.is_none() {
            self.release_slot(at);
        }
        let src = self.slot_addr(base);
        self.copy16(at, src);
        self.vs.clear();
        self.depth = start + 1;
        self.set_last(pc);
        self.check_released(pc + 1);
        self.b.ins().jump(join, &[]);
    }

    /// Whether a function a body's guards found by its class no longer runs
    /// the body's code (`__code__` was reassigned, which moves no version).
    fn code_swapped(&mut self, plan: &Plan, handle: u64) -> Value {
        let obj = borrowed_function(&plan.func);
        // SAFETY: an `Object` is 16 bytes, its second word the payload.
        let word = unsafe { std::ptr::from_ref::<Object>(&obj).cast::<[u64; 2]>().read() }[1];
        let at = self.b.ins().iconst(self.ptr, word as i64);
        let current = self.b.ins().load(types::I64, FLAGS, at, plan.layout.code);
        let want = self.b.ins().iconst(types::I64, handle as i64);
        self.b.ins().icmp(IntCC::NotEqual, current, want)
    }

    /// The guards of a `property` getter: the receiver an instance of the
    /// class at the site's version, whose property holds the getter still.
    fn getter_guards(&mut self, plan: &Plan, recv: Item, sv: Value, slow: Block) -> (Value, Value) {
        let l = *self.layout.expect("checked by the caller");
        let ptr = self.ptr;
        let at = self
            .operand_addr(recv)
            .expect("checked by the caller: a local or on the stack");
        let tag = self.tag_at(at);
        let not_inst = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, tag, i64::from(self.tags.instance));
        self.branch_out(not_inst, slow);
        let inst = self.b.ins().load(ptr, FLAGS, at, 8);
        let cls = self.b.ins().load(ptr, FLAGS, inst, l.inst_class);
        let ver = self
            .b
            .ins()
            .load(types::I64, FLAGS, cls, l.type_attr_version);
        let key = self.b.ins().load(types::I64, FLAGS, sv, IS_KEY);
        let other = self.b.ins().icmp(IntCC::NotEqual, ver, key);
        self.branch_out(other, slow);
        // (The class holds the property under this version.)
        let (_, cell) = plan.getter.expect("checked by the caller");
        let obj = borrowed_function(&plan.func);
        // SAFETY: an `Object` is 16 bytes, its second word the payload.
        let word = unsafe { std::ptr::from_ref::<Object>(&obj).cast::<[u64; 2]>().read() }[1];
        let at = self.b.ins().iconst(ptr, cell);
        let current = self.b.ins().load(types::I64, FLAGS, at, 8);
        let wrong = self.b.ins().icmp_imm(IntCC::NotEqual, current, word as i64);
        let no = self.b.ins().iconst(types::I8, 0);
        (no, wrong)
    }

    /// `LOAD_ATTR` at `pc` of the `property` getter `plans` lowers in line:
    /// its guards and body, with the instruction's other shapes for anything
    /// the guards don't take. The result on the stack where the receiver was.
    pub(super) fn inline_getter(&mut self, pc: usize, arg: u32, plans: &'a [Plan]) -> bool {
        let top = self.vs.last().copied();
        if self.layout.is_none() || !matches!(top, None | Some(Item::Local(_) | Item::Mem(_))) {
            if self.inlining() {
                self.exit(INTERP, pc);
                return false;
            }
            return self.load_attr_plain(pc, arg);
        }
        let before = self.b.func.dfg.num_insts();
        let nested = self.inlining();
        let recv = self.pop();
        self.flush();
        let start = self.depth;
        let call_last = self.last_pc;
        let join = self.b.create_block();
        let slow = self.b.create_block();
        self.inline_body(pc, &plans[0], &[recv], start, slow, join, None);
        self.b.switch_to_block(slow);
        self.b.set_cold_block(slow);
        self.vs = vec![recv];
        self.depth = start + 1;
        self.last_pc = call_last;
        if nested {
            self.exit(INTERP, pc);
        } else if self.load_attr_plain(pc, arg) {
            self.flush();
            self.b.ins().jump(join, &[]);
        }
        self.b.switch_to_block(join);
        self.vs.clear();
        self.depth = start + 1;
        self.set_last(pc);
        if !nested {
            self.inline_ir += self.b.func.dfg.num_insts() - before;
        }
        true
    }

    /// The guards of a `BINARY_OP`'s method: the left operand an instance of
    /// a class at the site's version, the right as the site saw it (a
    /// mismatch of either kind goes to `slow`). The values to test: the
    /// version compared, and a value false.
    fn dunder_guards(
        &mut self,
        d: Dunder,
        args: &[Item],
        sv: Value,
        slow: Block,
    ) -> (Value, Value) {
        let l = *self.layout.expect("checked by the caller");
        let ptr = self.ptr;
        let Some(a_at) = self.operand_addr(args[0]) else {
            self.b.ins().jump(slow, &[]);
            let dead = self.b.create_block();
            self.b.switch_to_block(dead);
            let f = self.b.ins().iconst(types::I8, 0);
            return (f, f);
        };
        let tag = self.tag_at(a_at);
        let not_inst = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, tag, i64::from(self.tags.instance));
        self.branch_out(not_inst, slow);
        let inst = self.b.ins().load(ptr, FLAGS, a_at, 8);
        let cls = self.b.ins().load(ptr, FLAGS, inst, l.inst_class);
        let ver = self
            .b
            .ins()
            .load(types::I64, FLAGS, cls, l.type_attr_version);
        let key = self.b.ins().load(types::I64, FLAGS, sv, IS_KEY);
        let other = self.b.ins().icmp(IntCC::NotEqual, ver, key);
        let wrong = match (d.right, args[1]) {
            (Some(t), Item::Int(_)) => self
                .b
                .ins()
                .iconst(types::I8, i64::from(t != self.tags.int)),
            (Some(t), Item::Float(_)) => self
                .b
                .ins()
                .iconst(types::I8, i64::from(t != self.tags.float)),
            (Some(t), Item::Const(k)) => {
                let same = self.ext.objects[k as usize].tag() == t;
                self.b.ins().iconst(types::I8, i64::from(!same))
            }
            (right, item) => {
                let Some(b_at) = self.operand_addr(item) else {
                    self.b.ins().jump(slow, &[]);
                    let dead = self.b.create_block();
                    self.b.switch_to_block(dead);
                    let f = self.b.ins().iconst(types::I8, 0);
                    return (f, f);
                };
                let btag = self.tag_at(b_at);
                match right {
                    Some(t) => self.b.ins().icmp_imm(IntCC::NotEqual, btag, i64::from(t)),
                    None => {
                        let not_inst = self.b.ins().icmp_imm(
                            IntCC::NotEqual,
                            btag,
                            i64::from(self.tags.instance),
                        );
                        self.branch_out(not_inst, slow);
                        let binst = self.b.ins().load(ptr, FLAGS, b_at, 8);
                        let bcls = self.b.ins().load(ptr, FLAGS, binst, l.inst_class);
                        self.b.ins().icmp(IntCC::NotEqual, bcls, cls)
                    }
                }
            }
        };
        (other, wrong)
    }

    /// The address of an operand that's a local or on the stack.
    fn operand_addr(&mut self, item: Item) -> Option<Value> {
        match item {
            Item::Local(i) => Some(self.local_addr(i)),
            Item::Mem(s) => Some(self.slot_addr(s)),
            _ => None,
        }
    }

    /// `BINARY_OP` at `pc` of the Python method `plans` lowers in line: its
    /// guards and body, with the instruction's other shapes for anything
    /// the guards don't take. The result on the stack where the left
    /// operand was.
    pub(super) fn inline_binop(&mut self, pc: usize, arg: u32, plans: &'a [Plan]) -> bool {
        let before = self.b.func.dfg.num_insts();
        let nested = self.inlining();
        let bi = self.pop();
        let ai = self.pop();
        let args = [ai, bi];
        self.flush();
        let start = self.depth;
        let call_last = self.last_pc;
        let join = self.b.create_block();
        let slow = self.b.create_block();
        self.inline_body(pc, &plans[0], &args, start, slow, join, None);
        self.b.switch_to_block(slow);
        self.b.set_cold_block(slow);
        self.vs = args.to_vec();
        self.depth = start + 2;
        self.last_pc = call_last;
        if nested {
            self.exit(INTERP, pc);
        } else if self.binary_plain(pc, arg) {
            self.flush();
            self.b.ins().jump(join, &[]);
        }
        self.b.switch_to_block(join);
        self.vs.clear();
        self.depth = start + 1;
        self.set_last(pc);
        if !nested {
            self.inline_ir += self.b.func.dfg.num_insts() - before;
        }
        true
    }

    /// The address of the running inlined body's function's `Object`.
    fn inline_callable(&mut self) -> Value {
        let f = self.inl.last().expect("in an inlined body");
        match (f.method_at, f.start) {
            (Some(at), _) => self.b.ins().iconst(self.ptr, at),
            (None, start) => self.slot_addr(start),
        }
    }

    /// Release the value at `at`: a plain decrement while another owner
    /// holds it, otherwise through [`h_release`], whose finalizers wait for
    /// the next check.
    fn release_slot(&mut self, at: Value) {
        let tag = self.tag_at(at);
        let scalar = self.is_scalar(tag);
        let unbound = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, tag, i64::from(self.tags.unbound));
        let plain = self.b.ins().bor(scalar, unbound);
        let heap = self.b.create_block();
        let done = self.b.create_block();
        self.release_or(plain, at, tag, done, heap);
        self.b.switch_to_block(heap);
        self.call(h_release as *const () as usize, &[at], false);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
    }

    /// `Lower::exit` in an inlined body: the virtual stack written out, and
    /// on to the body's exit (see [`h_inline_exit`]).
    pub(super) fn inline_exit(&mut self, status: u32, pc: usize) {
        let cold = std::mem::replace(&mut self.cold, true);
        self.flush();
        self.cold = cold;
        let f = self.inl.last().expect("in an inlined body");
        let (depth, site, block) = (self.depth - f.base, f.site, f.exit_block);
        debug_assert!(depth < 1 << EXIT_PC_SHIFT);
        // (Where a block started, the last instruction isn't known here,
        // and only a tracer's line events, which aren't on, would ask.)
        let last = self.last_pc.unwrap_or(pc.saturating_sub(1));
        let packed = depth as i64
            | (pc as i64) << EXIT_PC_SHIFT
            | (last as i64) << EXIT_LAST_SHIFT
            | i64::from(status) << EXIT_STATUS_SHIFT;
        let v = self.b.ins().iconst(types::I64, packed);
        let s = self.b.ins().iconst(self.ptr, site);
        self.b.ins().jump(block, &[v.into(), s.into()]);
    }

    /// `RETURN_VALUE` in an inlined body: the value on the callee's stack,
    /// on to its return.
    pub(super) fn inline_return(&mut self, pc: usize) -> bool {
        let f = self.inl.last().expect("in an inlined body");
        let (base, ret, dunder) = (f.base, f.ret, f.plan.dunder.is_some());
        if self.depth != base + 1 {
            self.exit(INTERP, pc);
            return false;
        }
        self.flush();
        // An operator method's `NotImplemented` is the core loop's: the
        // activation's return runs the operator's protocol.
        if dunder {
            let ni = crate::vm_singletons::not_implemented();
            // SAFETY: an `Object` is 16 bytes, its second word the payload.
            let word = unsafe { std::ptr::from_ref(&ni).cast::<[u64; 2]>().read() }[1];
            let at = self.slot_addr(base);
            let tag = self.tag_at(at);
            let w = self.b.ins().load(types::I64, FLAGS, at, 8);
            let same_tag = self
                .b
                .ins()
                .icmp_imm(IntCC::Equal, tag, i64::from(ni.tag()));
            let same_word = self.b.ins().icmp_imm(IntCC::Equal, w, word as i64);
            let is_ni = self.b.ins().band(same_tag, same_word);
            let out = self.exit_with(pc, &[], INTERP);
            self.branch_out(is_ni, out);
        }
        self.b.ins().jump(ret, &[]);
        false
    }

    /// `LOAD_GLOBAL` in an inlined body: as `Lower::load_global`, with the
    /// namespaces the callee's function holds (no frame carries them), and
    /// no fused shapes.
    pub(super) fn inline_load_global(&mut self, pc: usize) -> bool {
        let f = self.inl.last().expect("in an inlined body");
        let layout = f.plan.layout;
        let s = self.depth;
        let dst = self.slot_addr(s);
        let cache: Box<GlobalCache> = Box::default();
        let cache_at = std::ptr::from_ref::<GlobalCache>(&cache) as i64;
        self.global_caches.push(cache);
        let helper = self.b.create_block();
        let done = self.b.create_block();
        let ptr = self.ptr;
        let callee = self.inline_callable();
        let word = self.b.ins().load(ptr, FLAGS, callee, 8);
        let c = self.b.ins().iconst(ptr, cache_at);
        let g = self.b.ins().load(types::I64, FLAGS, word, layout.globals);
        let cg = self.b.ins().load(types::I64, FLAGS, c, GC_GLOBALS);
        let other = self.b.ins().icmp(IntCC::NotEqual, g, cg);
        self.branch_out(other, helper);
        let gd = self.b.ins().load(ptr, FLAGS, c, GC_GDATA);
        let gs = self.b.ins().load(types::I64, FLAGS, gd, DICT_STAMP);
        let cgs = self.b.ins().load(types::I64, FLAGS, c, GC_GSTAMP);
        let moved = self.b.ins().icmp(IntCC::NotEqual, gs, cgs);
        self.branch_out(moved, helper);
        let cb = self.b.ins().load(types::I64, FLAGS, c, GC_BUILTINS);
        let builtin = self.b.create_block();
        let read = self.b.create_block();
        self.b.ins().brif(cb, builtin, &[], read, &[]);
        self.b.switch_to_block(builtin);
        let b = self.b.ins().load(types::I64, FLAGS, word, layout.builtins);
        let other = self.b.ins().icmp(IntCC::NotEqual, b, cb);
        self.branch_out(other, helper);
        let bd = self.b.ins().load(ptr, FLAGS, c, GC_BDATA);
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
        self.b.switch_to_block(helper);
        let code = self.code_ptr();
        let pcv = self.b.ins().iconst(types::I64, pc as i64);
        let c = self.b.ins().iconst(ptr, cache_at);
        let r = self
            .call(
                h_inline_global as *const () as usize,
                &[self.st, callee, code, pcv, dst, c],
                true,
            )
            .expect("returns");
        let out = self.exit_with(pc, &[], INTERP);
        self.b.ins().brif(r, out, &[], done, &[]);
        self.b.switch_to_block(done);
        self.push(Item::Mem(s));
        self.set_last(pc);
        true
    }

    /// `LOAD_DEREF` in an inlined body: the cell its function's closure
    /// holds, through [`h_inline_deref`].
    pub(super) fn inline_load_deref(&mut self, pc: usize, i: u32) -> bool {
        let s = self.depth;
        let dst = self.slot_addr(s);
        let callee = self.inline_callable();
        let iv = self.b.ins().iconst(types::I64, i64::from(i));
        let r = self
            .call(
                h_inline_deref as *const () as usize,
                &[callee, iv, dst],
                true,
            )
            .expect("returns");
        let out = self.exit_with(pc, &[], INTERP);
        self.branch_out(r, out);
        self.push(Item::Mem(s));
        true
    }

    /// `CALL` in an inlined body of what isn't lowered in line: a builtin
    /// the site settled on that runs no Python code through
    /// [`h_inline_builtin`], the result in place; anything else leaves
    /// for the core loop here.
    pub(super) fn inline_builtin_call(&mut self, pc: usize, argc: usize) -> bool {
        use crate::LeafKind;
        let Some((f, Some(kind), via_call)) = crate::call_site_builtin(self.ext, pc) else {
            return self.inline_python_call(pc, argc);
        };
        let (kernel, recv_tag) = if kind == LeafKind::Opaque {
            (None, 0)
        } else {
            (1..=4)
                .find_map(|t| kernel_for(t, kind, argc).map(|kn| (Some(kn), t)))
                .map_or((None, 0), |(kn, t)| {
                    let tag = match t {
                        1 => self.tags.list,
                        2 => self.tags.dict,
                        3 => self.tags.set,
                        _ => self.tags.str,
                    };
                    (kn, tag)
                })
        };
        if kernel.is_none() && !kind.runs_in_core() {
            return self.inline_python_call(pc, argc);
        }
        self.flush();
        let site = Box::new(BuiltinSite {
            f: Rc::as_ptr(&f),
            argc,
            kind: Some(kind),
            kernel,
            recv_tag,
            via_call,
        });
        self.held.push(f);
        let at = self
            .b
            .ins()
            .iconst(self.ptr, std::ptr::from_ref::<BuiltinSite>(&site) as i64);
        self.builtin_sites.push(site);
        let pcv = self.b.ins().iconst(types::I64, pc as i64);
        let len = self.b.ins().iconst(types::I64, self.depth as i64);
        let r = self
            .call(
                h_inline_builtin as *const () as usize,
                &[self.st, pcv, len, at],
                true,
            )
            .expect("returns");
        let ran = self.b.create_block();
        let other = self.b.create_block();
        self.b.ins().brif(r, other, &[], ran, &[]);
        self.b.switch_to_block(other);
        let raised = self.b.ins().icmp_imm(IntCC::Equal, r, i64::from(RAISED));
        let raise_b = self.b.create_block();
        let declined = self.b.create_block();
        self.b.ins().brif(raised, raise_b, &[], declined, &[]);
        // Raised: the operands gone, the error with the state.
        self.b.switch_to_block(raise_b);
        let (vs, depth, last) = (std::mem::take(&mut self.vs), self.depth, self.last_pc);
        self.depth = depth - argc - 2;
        self.set_last(pc);
        self.exit(RAISED, pc + 1);
        self.vs = vs;
        self.depth = depth;
        self.last_pc = last;
        self.b.switch_to_block(declined);
        self.exit(INTERP, pc);
        self.b.switch_to_block(ran);
        self.depth -= argc + 1;
        self.set_last(pc);
        self.check_released(pc + 1);
        true
    }
}

/// `f`'s `Object`, as a copy of its bits that holds no count.
fn borrowed_function(f: &Rc<crate::object::PyFunction>) -> std::mem::ManuallyDrop<Object> {
    let held = Object::Function(f.clone());
    // SAFETY: a bitwise copy, which is never dropped, of a live value.
    let bits = std::mem::ManuallyDrop::new(unsafe { std::ptr::read(&raw const held) });
    drop(held);
    bits
}

/// The rest of `Lower`'s inlined calls.
impl Lower<'_> {
    /// `CALL` in an inlined body of what may be a compiled Python function:
    /// a direct call through [`h_inline_call`], the result in place, and
    /// anything else leaves for the core loop here.
    fn inline_python_call(&mut self, pc: usize, argc: usize) -> bool {
        self.flush();
        let site = self.inl.last().expect("in an inlined body").site;
        let call = Box::new(InlineCall {
            site: site as *const InlineSite,
            start: self.depth - argc - 2,
            pc,
            levels: self.inl.len(),
        });
        let call_at = self
            .b
            .ins()
            .iconst(self.ptr, std::ptr::from_ref::<InlineCall>(&call) as i64);
        self.inline_calls.push(call);
        let dsite: Box<DirectSite> = Box::default();
        let dsite_at = self
            .b
            .ins()
            .iconst(self.ptr, std::ptr::from_ref::<DirectSite>(&dsite) as i64);
        self.direct_sites.push(dsite);
        let pc_argc = self
            .b
            .ins()
            .iconst(types::I64, (pc as i64) | ((argc as i64) << 32));
        let len = self.b.ins().iconst(types::I64, self.depth as i64);
        let code = self.code_ptr();
        let r = self
            .call(
                h_inline_call as *const () as usize,
                &[self.st, code, pc_argc, len, dsite_at, call_at],
                true,
            )
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
        // The frames were built, and the core loop goes on with them.
        self.b.switch_to_block(leave);
        self.b.ins().return_(&[r]);
        self.b.switch_to_block(ran);
        self.depth -= argc + 1;
        self.set_last(pc);
        self.check_released(pc + 1);
        true
    }
}

/// `CALL` (the instruction of `code` at the low half of `pc_argc`, its
/// argument count the high half, operands up a `len`-deep stack) in an
/// inlined body: a direct call of a compiled Python function, or a class
/// whose construction is a store-only `__init__`'s stores (which run no
/// frame). `0` the result in place, `RELOAD` the core loop goes on with the
/// bodies' frames built, `CALL_DECLINED` untouched.
unsafe extern "C" fn h_inline_call(
    st: *mut State,
    code: *const CodeObject,
    pc_argc: u64,
    len: u64,
    site: *mut DirectSite,
    call: *const InlineCall,
) -> u32 {
    // SAFETY: as `h_call_direct`'s; `call` is this `CALL`'s, and a depth in
    // this lowering's numbering addresses the operands from `st.stack`.
    unsafe {
        let s = &mut *st;
        let pc = (pc_argc & u64::from(u32::MAX)) as usize;
        let argc = (pc_argc >> 32) as usize;
        let len = len as usize;
        let start = len - argc - 2;
        // The bodies' frames count toward the callee's recursion depth.
        let dc = &*s.depth_cell;
        let depth = dc.get();
        let f = match &*s.stack.add(start) {
            Object::Function(f) => std::ptr::from_ref(f),
            Object::Type(_) => {
                if argc == 1 && type_of_call(s, start) {
                    return 0;
                }
                dc.set(depth + (*call).levels);
                let done = ctor_store_init(s, &*code, pc, argc, len);
                dc.set(depth);
                return if done { 0 } else { CALL_DECLINED };
            }
            _ => return CALL_DECLINED,
        };
        // A Python call can change the classes the bodies' functions came
        // from: it holds them.
        let mut at = (*call).site;
        while !at.is_null() {
            if let Some((held, _)) = &(*at).method {
                std::mem::forget(Object::clone(held));
            }
            at = (*at).parent;
        }
        dc.set(depth + (*call).levels);
        s.inl_call = call;
        // What the frame holds while the callee runs: the bodies' values
        // aren't its yet.
        (*s.frame).stack.set_len(root_site((*call).site).start);
        let r = direct_call::<false>(s, pc, start, len, &mut *site, None, None);
        let mut at = (*call).site;
        while !at.is_null() {
            if let Some((held, _)) = &(*at).method {
                match &**held {
                    Object::Function(f) if crate::rc::try_release_shared(Rc::as_ptr(f)) => {}
                    other => drop(std::ptr::read(other)),
                }
            }
            at = (*at).parent;
        }
        if s.inl_gone {
            return r.unwrap_or(RELOAD);
        }
        s.inl_call = std::ptr::null();
        dc.set(depth);
        match r {
            Some(r) => r,
            None => {
                // A callee waiting for its body doesn't count against the
                // site, and gets closer to one.
                let root = root_site((*call).site);
                root.entries.set(root.entries.get() + EXIT_RATIO);
                let code = (*f).code();
                if let Some(ext) = crate::code_vm_ext(&code) {
                    let nlocals = code.varnames.len();
                    ext.frame_jit.warm_switched(&code, ext, nlocals);
                }
                CALL_DECLINED
            }
        }
    }
}

/// The outermost site around `site`.
unsafe fn root_site<'s>(site: *const InlineSite) -> &'s InlineSite {
    // SAFETY: the caller's contract: a live site, whose parents are live
    // (the same code's).
    unsafe {
        let mut at = site;
        while !(*at).parent.is_null() {
            at = (*at).parent;
        }
        &*at
    }
}

/// Copy the `n` trailing defaults of the function at stack slot `callee`
/// to `dst`: `0` done, anything else declined (`__defaults__` was rebound).
unsafe extern "C" fn h_inline_defaults(callee: *const Object, dst: *mut Object, n: u64) -> u32 {
    // SAFETY: the code passes the callable's slot and free stack slots.
    unsafe {
        let Object::Function(f) = &*callee else {
            return 1;
        };
        let n = n as usize;
        if !Interpreter::defaults_cover(f, n) {
            return 1;
        }
        for (k, d) in f.defaults[f.defaults.len() - n..].iter().enumerate() {
            dst.add(k).write(crate::clone_hot(d));
        }
        0
    }
}

/// `LOAD_DEREF i` of the body of the function at stack slot `callee`: `0`
/// the cell's value written to `dst`, anything else declined.
unsafe extern "C" fn h_inline_deref(callee: *const Object, i: u64, dst: *mut Object) -> u32 {
    // SAFETY: the code passes the callable's slot and a free stack slot.
    unsafe {
        let Object::Function(f) = &*callee else {
            return 1;
        };
        let code: &CodeObject = &*f.code.as_ptr();
        let Some(cell) = f
            .lean_cells_ref(code)
            .and_then(|cells| cells.get(i as usize))
        else {
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

/// `FOR_ITER` in an inlined body: [`h_for_iter`] on a range's, list's or
/// tuple's iterator, whose steps run no Python code and don't raise; `3` (a
/// decline) for anything else.
pub(super) unsafe extern "C" fn h_inline_for_iter(
    st: *mut State,
    it: *mut Object,
    out: *mut Object,
    pc: u64,
) -> u32 {
    // SAFETY: as `h_for_iter`'s.
    unsafe {
        if let Object::Iter(rc) = &*it {
            if rc.peek().is_some_and(|i| {
                matches!(
                    i,
                    PyIterator::Range { .. } | PyIterator::List { .. } | PyIterator::Tuple { .. }
                )
            }) {
                return h_for_iter(st, it, out, pc);
            }
        }
        3
    }
}

/// `STORE_ATTR` (the `pc`th instruction of `code`) in an inlined body: as
/// [`h_stack_store_attr`], of an instance only: a body's function must stay
/// its class's (see [`InlineSite::method`]), and storing to a class leaves
/// for the core loop.
pub(super) unsafe extern "C" fn h_inline_store_attr(
    code: *const CodeObject,
    pc: u64,
    at: *mut Object,
) -> u32 {
    // SAFETY: as `h_stack_store_attr`'s.
    unsafe {
        if !matches!(&*at.add(1), Object::Instance(_)) {
            return 1;
        }
        h_stack_store_attr(code, pc, at)
    }
}

/// A fused method call's slow path loaded the method at `callee`, its
/// receiver above it: if it's one of the bodies' functions, called on an
/// instance, the guards take the receiver's class from now on.
unsafe extern "C" fn h_inline_method_note(callee: *const Object, notes: *const MethodNotes) {
    // SAFETY: the code passes the callable's slot, with the self slot above
    // it, and its site's notes; the caches are the same code's.
    unsafe {
        let (Object::Function(f), Object::Instance(inst)) = (&*callee, &*callee.add(1)) else {
            return;
        };
        let notes = &*notes;
        let at = Rc::as_ptr(f);
        let Some((_, caches)) = notes.entries.iter().find(|(p, _)| std::ptr::eq(*p, at)) else {
            return;
        };
        let cls = inst.cls_raw();
        let ver = cls.attr_version.get();
        let target = caches
            .iter()
            .find(|&&c| (*c).ver == 0 || (*c).ver == ver)
            .or(caches.last());
        if let Some(&c) = target {
            fill_method_cache(&mut *c, cls, ver, &*callee, &*notes.ext, notes.name);
        }
    }
}

/// Release the value at `at` (it leaves the stack for good).
unsafe extern "C" fn h_release(at: *const Object) {
    // SAFETY: the code passes an initialized slot, which it doesn't read
    // again.
    unsafe { crate::release_run(at, 1) }
}

/// `LOAD_GLOBAL` (the `pc`th instruction of `code`) of the body of the
/// function at stack slot `callee`, as [`h_load_global`] fills its cache: `0`
/// the value written to `dst`, anything else declined untouched.
unsafe extern "C" fn h_inline_global(
    st: *const State,
    callee: *const Object,
    code: *const CodeObject,
    pc: u64,
    dst: *mut Object,
    cache: *mut GlobalCache,
) -> u32 {
    // SAFETY: as `h_load_global`'s; the function is held by its stack slot.
    // (By name: a body that only ever ran in line or as a frameless leaf
    // never filled the instruction's inline cache.)
    unsafe {
        let Object::Function(f) = &*callee else {
            return 1;
        };
        let interp = &*(*st).interp;
        let (code, pc) = (&*code, pc as usize);
        let Some(name) = code
            .instructions
            .get(pc)
            .and_then(|i| code.names.get(i.arg as usize))
        else {
            return 1;
        };
        let probe = crate::object::StrKey(name.as_str());
        let (gdict, bdict) = (f.globals.as_ptr(), f.builtins.as_ptr());
        let (Some(g), Some(b)) = (f.globals.peek(), f.builtins.peek()) else {
            return 1;
        };
        let g_stamp = (*gdict).mutation_stamp();
        let b_stamp = (*bdict).mutation_stamp();
        let (v, builtin) = match g.get_full(&probe) {
            Some((_, _, v)) => (v, false),
            None if interp.globals_missing_any.get() => return 1,
            None => match b.get_full(&probe) {
                Some((_, _, v)) => (v, true),
                None => return 1,
            },
        };
        let c = &mut *cache;
        c.value = 0;
        c.globals = std::ptr::from_ref(&f.globals).cast::<u64>().read();
        c.gstamp = g_stamp;
        c.gdata = gdict as u64;
        (c.builtins, c.bstamp, c.bdata) = if builtin {
            (
                std::ptr::from_ref(&f.builtins).cast::<u64>().read(),
                b_stamp,
                bdict as u64,
            )
        } else {
            (0, 0, 0)
        };
        c.value = std::ptr::from_ref(v) as u64;
        dst.write(crate::clone_hot(v));
        0
    }
}

/// `CALL` (the `pc`th instruction, operands up a `len`-deep stack) in an
/// inlined body, of the builtin `site` settled on: `0` the result in place,
/// `RAISED` the operands released with the error in the state and its pc
/// past the call, anything else declined untouched.
unsafe extern "C" fn h_inline_builtin(
    st: *mut State,
    pc: u64,
    len: u64,
    site: *const BuiltinSite,
) -> u32 {
    // SAFETY: as `h_call_kernel`'s and `h_call_leaf`'s; a depth in this
    // lowering's numbering addresses the operands from `st.stack`.
    unsafe {
        let s = &mut *st;
        let site = &*site;
        if site.kernel.is_some() && call_kernel_in_place(s, site, len as usize) {
            return 0;
        }
        if site.kind.is_some_and(crate::LeafKind::runs_in_core) {
            if let Some(r) = call_leaf_in_place(s, site, pc, len as usize) {
                return r;
            }
        }
        CALL_DECLINED
    }
}

/// An inlined body leaves for the core loop: `packed` holds its stack depth
/// (from its operand stack's bottom), the pc it stopped at, its last
/// instruction and the status it stopped with, and `site` is its call
/// site. The activations of `site`'s body and of the bodies it's inlined
/// in are bound and pushed, as a direct call's are when it registers (see
/// `direct_enter`, `register_lazy`), each one's locals, operands and
/// callable moved from the stack: what the core loop goes on with is exactly
/// what running the calls would have left. `RELOAD`, always.
pub(super) unsafe extern "C" fn h_inline_exit(
    st: *mut State,
    site: *const InlineSite,
    packed: u64,
) -> u32 {
    // SAFETY: the code passes its live state, with the frame's stack synced
    // up to the outermost site's operands, and the site its body runs at.
    unsafe {
        let st = &mut *st;
        let depth = (packed & ((1 << EXIT_PC_SHIFT) - 1)) as usize;
        let field = (1u64 << (EXIT_LAST_SHIFT - EXIT_PC_SHIFT)) - 1;
        let pc = ((packed >> EXIT_PC_SHIFT) & field) as usize;
        let last = ((packed >> EXIT_LAST_SHIFT)
            & ((1 << (EXIT_STATUS_SHIFT - EXIT_LAST_SHIFT)) - 1)) as usize;
        let status = (packed >> EXIT_STATUS_SHIFT) as u32;
        // A site that keeps leaving takes the ordinary call from now on.
        let root = root_site(site);
        let exits = root.exits.get().saturating_add(1);
        root.exits.set(exits);
        if exits >= MIN_EXITS && u64::from(exits) * EXIT_RATIO > root.entries.get() {
            root.key.set(0);
            if stats::enabled() {
                eprintln!(
                    "frame jit inline: {} off after {exits} exits in {} entries",
                    root.code.qualname,
                    root.entries.get()
                );
            }
        }
        crate::burst_stats::note_call(crate::burst_stats::CALL_INLINE_EXIT);
        bind_levels(st, site, depth, pc, false);
        let sw = &mut *st.sw;
        *sw.last = last;

        if status == MARKED {
            sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Marked));
        } else if status == RAISED {
            if let Some(e) = st.err.take() {
                sw.pending = Some(crate::CoreExit::Stop(crate::LeafStop::Raised(e)));
            }
        }
        RELOAD
    }
}

/// Bind and push the activations of `site`'s body and of the bodies it's
/// inlined in, as a direct call's are when it registers (see `direct_enter`,
/// `register_lazy`), each one's locals, operands and callable moved from the
/// stack: `count` operands and the next instruction `pc` for `site`'s own.
/// `counted` when the recursion depth counts them already. The innermost's
/// frame, the switch's running one now.
unsafe fn bind_levels(
    st: &mut State,
    site: *const InlineSite,
    count: usize,
    pc: usize,
    counted: bool,
) -> *mut crate::Frame {
    // SAFETY: the caller's contract: `st`'s frame synced up to the outermost
    // site's operands.
    unsafe {
        let mut chain: Vec<&InlineSite> = Vec::with_capacity(MAX_LEVELS);
        let mut at = site;
        while !at.is_null() {
            chain.push(&*at);
            at = (*at).parent;
        }
        chain.reverse();
        settle(st);
        let sw = &mut *st.sw;
        let interp = &mut *st.interp.cast_mut();
        let base = st.stack;
        // The borrowed arguments take their slots, as they would have before
        // the call.
        for s in &chain {
            for &(home, place) in &s.borrowed {
                let src: *const Object = match place {
                    Place::Stack(at) => base.add(at),
                    Place::Root(j) => st.locals.add(j as usize),
                    Place::At(a) => a as *const Object,
                    Place::Temp(_) => unreachable!("settled before a call"),
                };
                base.add(home).write(crate::clone_hot(&*src));
            }
        }
        // (The spine's own registration hook stays out of the way: the
        // levels link in order here, as `register_lazy` links its callees.)
        let outer = interp.lazy_top.replace(0);
        let mut caller: *mut crate::Frame = st.frame;
        let mut caller_last = st.last;
        for (k, s) in chain.iter().enumerate() {
            let caller_frame = &mut *caller;
            caller_frame.pc = s.call_pc as u32 + 1;
            let callable: *const Object = match &s.method {
                Some((f, _)) => &raw const **f,
                None => base.add(s.start),
            };
            let Object::Function(f) = &*callable else {
                unreachable!("a site's callable is a function");
            };
            let fp: *const crate::object::PyFunction = Rc::as_ptr(f);
            let code_rc: &Rc<CodeObject> = &*f.code.as_ptr();
            let dc = &*sw.depth_cell;
            if !counted {
                dc.set(dc.get() + 1);
            }
            let mut act = interp.inline_slot();
            let fr: &mut crate::Frame = &mut act.frame;
            // SAFETY: a parked slot's locals storage is its own, and empty.
            let locals = fr.locals.storage();
            if locals.capacity() < s.nlocals {
                locals.reserve(s.nlocals);
            }
            crate::move_objects(base.add(s.first), locals.as_mut_ptr(), s.nlocals);
            locals.set_len(s.nlocals);
            // The slot's handles are stale copies, overwritten without a
            // drop; the rest borrow from the callable, which it holds.
            std::ptr::write(
                &raw mut act.callable,
                match &s.method {
                    Some((f, _)) => Object::clone(f),
                    None => base.add(s.start).read(),
                },
            );
            std::ptr::write(&raw mut fr.code, code_rc.clone());
            std::ptr::write(&raw mut fr.globals, crate::borrowed_rc(&(*fp).globals));
            std::ptr::write(&raw mut fr.builtins, crate::borrowed_rc(&(*fp).builtins));
            std::ptr::write(
                &raw mut fr.cells,
                crate::borrowed_rc(
                    (*fp)
                        .lean_cells_ref(code_rc)
                        .unwrap_or_else(|| crate::object::empty_cells_ref()),
                ),
            );
            act.owns_cells = false;
            // An operator method's `NotImplemented` becomes the operator's
            // `TypeError` (see `InlineAct::binop`).
            if let Some((_, Some(arg))) = &s.method {
                let locals = fr.locals.storage();
                if let Object::Instance(a) = &locals[0] {
                    let cls = a.cls();
                    let right = match &locals[1] {
                        Object::Instance(_) => cls.clone(),
                        b => crate::builtins::class_of(b),
                    };
                    let op: BinOpKind = std::mem::transmute(*arg as u8);
                    let inplace = arg & weavepy_compiler::BINARY_OP_INPLACE_FLAG != 0;
                    act.binop = Some((op, inplace, cls, right));
                }
            }
            // Its operands: up to its callee's callable, or its depth.
            let n = match chain.get(k + 1) {
                Some(next) => next.start - s.base,
                None => count,
            };
            let stack = &mut fr.stack;
            if stack.capacity() < n {
                stack.reserve(n);
            }
            crate::move_objects(base.add(s.base), stack.as_mut_ptr(), n);
            stack.set_len(n);
            fr.pc = match chain.get(k + 1) {
                Some(next) => next.call_pc as u32 + 1,
                None => pc as u32,
            };
            act.parked = false;
            act.direct = false;
            act.call_pc = s.call_pc;
            act.exc_depth = interp.exc_info_len();
            act.guard = Some(crate::recursion::Guard::adopt(dc));
            if k == 0 {
                caller_frame.stack.set_len(s.start);
            }
            *sw.last = caller_last;
            act.caller_pending = interp.core_pending_enter_fast(sw, caller_frame, s.call_pc);
            let callee: *mut crate::Frame = &raw mut *act.frame;
            crate::push_fast(&mut *sw.inl, act);
            sw.cur = callee;
            sw.scratch = usize::MAX;
            sw.last = &raw mut sw.scratch;
            caller = callee;
            caller_last = chain.get(k + 1).map_or(usize::MAX, |next| next.call_pc);
        }
        interp.lazy_top.set(outer);
        caller
    }
}

/// `st`'s `CALL` in an inlined body (see [`h_inline_call`]) has a callee that
/// registers, which needs its caller's frame: the bodies' activations, the
/// innermost suspended at the `CALL`. Its frame.
///
/// # Safety
///
/// `st` is running such a call, the callee's operands gone from the stack.
pub(super) unsafe fn spill(st: &mut State) -> *mut crate::Frame {
    // SAFETY: the caller's contract.
    unsafe {
        let call = &*std::mem::replace(&mut st.inl_call, std::ptr::null());
        st.inl_gone = true;
        let count = call.start - (*call.site).base;
        bind_levels(st, call.site, count, call.pc + 1, true)
    }
}
