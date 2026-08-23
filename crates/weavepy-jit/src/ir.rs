//! The typed mid-IR the analyzer emits and the lowerer consumes.
//!
//! It is a *stack machine* mirroring the bytecode, but with every
//! operation resolved to a concrete [`JitType`] lane and every local
//! resolved to a slot index. Keeping a tiny IR between bytecode and
//! Cranelift means [`crate::analyze`] can be unit-tested without a
//! codegen backend and [`crate::lower`] stays a straight syntax-directed
//! translation.
//!
//! Cross-block operand-stack values are carried as Cranelift *block
//! parameters* in lowering; [`TBlock::entry_stack`] records their static
//! types so the lowerer can declare the right params. Locals become
//! Cranelift *variables*, so merges are handled by the SSA builder
//! without explicit phis.

use crate::value::JitType;

/// Index of a [`TBlock`] within a [`TFunc`].
pub type BlockId = usize;

/// Arithmetic operations the JIT lowers. `TrueDiv` (`/`) always yields a
/// `float`; `FloorDiv`/`Mod` carry Python's round-toward-negative-
/// infinity semantics on integers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArithKind {
    Add,
    Sub,
    Mul,
    FloorDiv,
    Mod,
    TrueDiv,
    And,
    Or,
    Xor,
}

/// Comparison operators (six-way), matching `CompareKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CmpKind {
    Lt,
    Le,
    Eq,
    Ne,
    Gt,
    Ge,
}

/// RFC 0069 WS2 — the math-module intrinsics the JIT burns in as
/// native instructions. `Sqrt` lowers to Cranelift's `sqrt`; `Sin`/
/// `Cos` call the registered libm-backed helpers (bit-identical to
/// what the interpreter's `math` module computes); `Fabs` lowers to
/// `fabs`. Domain surprises (negative `sqrt` operand, non-finite
/// `sin`/`cos` input) deopt *before* the operation so the interpreter
/// re-executes the call and raises the exact `ValueError`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MathFunc {
    Sqrt,
    Sin,
    Cos,
    Fabs,
}

impl MathFunc {
    /// Map a `math` attribute name to its intrinsic, or `None` for
    /// anything outside the burned-in set.
    #[must_use]
    pub fn from_attr(name: &str) -> Option<MathFunc> {
        match name {
            "sqrt" => Some(MathFunc::Sqrt),
            "sin" => Some(MathFunc::Sin),
            "cos" => Some(MathFunc::Cos),
            "fabs" => Some(MathFunc::Fabs),
            _ => None,
        }
    }
}

/// RFC 0069 WS1 — how a burned-in method call's result is typed. A
/// provably-`None` return (every return site is the `None` constant)
/// exists only on the interpreter stack — the analyzer requires the
/// following instruction to consume it (`POP_TOP`) — while a scalar
/// return rides the native stack like a `CallPy` result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MethodRet {
    /// The callee provably returns `None` (procedure shape).
    None,
    /// The callee returns one stable scalar lane.
    Scalar(JitType),
}

/// A single stack-machine operation. Operands are implicit (the top of
/// the abstract value stack); results are pushed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TOp {
    /// Push an `int` constant.
    PushConstInt(i64),
    /// Push a `float` constant (stored as `f64::to_bits` so `TOp` stays
    /// `Copy` + `PartialEq`).
    PushConstFloat(u64),
    /// Push a `bool` constant.
    PushConstBool(bool),
    /// Push `locals[slot]`.
    LoadLocal(u32),
    /// Pop into `locals[slot]`.
    StoreLocal(u32),
    /// `int (op) int → int`. `Add`/`Sub`/`Mul` deopt on i64 overflow;
    /// `FloorDiv`/`Mod` deopt on zero divisor or `MIN / -1`. Never
    /// carries `TrueDiv` (see [`TOp::IntTrueDiv`]).
    IntArith(ArithKind),
    /// `float (op) float → float`. `Add`/`Sub`/`Mul`/`TrueDiv`, plus
    /// (RFC 0069 WS2) `FloorDiv`/`Mod` with Python's sign-follows-
    /// divisor semantics; `TrueDiv`/`FloorDiv`/`Mod` deopt on a zero
    /// divisor (the interpreter raises `ZeroDivisionError`).
    FloatArith(ArithKind),
    /// `int / int → float` (Python true division). Deopts on a zero
    /// divisor (the interpreter raises `ZeroDivisionError`).
    IntTrueDiv,
    /// `int (cmp) int → bool`.
    IntCmp(CmpKind),
    /// `float (cmp) float → bool`.
    FloatCmp(CmpKind),
    /// `-int`. Deopts on `MIN` negation overflow.
    IntNeg,
    /// `-float`.
    FloatNeg,
    /// `~int`.
    IntInvert,
    /// `not x` for an integral (`int`/`bool`) operand → `bool`.
    IntNot,
    /// `not x` for a `float` operand → `bool`.
    FloatNot,
    /// Discard TOS.
    Pop,
    /// Duplicate TOS (`COPY`).
    Dup,
    /// Swap the top two stack entries (`SWAP 2`).
    Swap2,
    /// Convert the integral value at TOS to `float` (RFC 0058 WS4 mixed
    /// arithmetic promotion, matching the interpreter's `as f64` cast).
    /// When `guarded`, deopt unless `|v| <= 2^53` — the range where the
    /// conversion is exact — because mixed-lane *comparisons* are
    /// mathematically exact in the interpreter.
    IntToFloatTos { guarded: bool },
    /// Same conversion applied to the entry *below* TOS. A dedicated op
    /// (rather than `Swap2` + `IntToFloatTos` + `Swap2`) so a guarded
    /// deopt spills the operand stack in its original order.
    IntToFloatSecond { guarded: bool },
    /// RFC 0059 WS3 — a Python-to-Python call through the embedder's
    /// `wpjit_call_py` helper. Pops `argc` scalar arguments (the burned
    /// callee never reached the JIT stack — see
    /// [`CalleeSpanMeta`]), calls `token` (an index into the compiled
    /// frame's callee table), and pushes a `ret` result. A raised
    /// callee takes the `Raised` exit at this pc; a result outside the
    /// `ret` lane (or a caller guard invalidated by the callee's side
    /// effects) deopts *after* the call with the result spilled.
    CallPy { token: u32, argc: u8, ret: JitType },
    /// RFC 0061 WS5 — `BINARY_SUBSCR` on a pinned list: pops the `int`
    /// index and the pin reference, calls the registered
    /// `wpjit_list_get` helper (bounds + element-lane checked against
    /// the real `Object::List`), and pushes the `elem`-lane result.
    /// Any surprise (out of range, aliased shape change) deopts at
    /// this pc with both operands spilled, so the interpreter
    /// re-executes the subscript generically.
    ListGet { elem: JitType },
    /// RFC 0061 WS5 — `STORE_SUBSCR` on a pinned list: pops the index,
    /// the pin reference, and the value (staged through the frame's
    /// `ret_bits`), and calls `wpjit_list_set`. Out-of-range deopts at
    /// this pc; the interpreter re-executes the store and raises.
    ListSet,
    /// RFC 0065 WS5 — `len(x)` on a pinned list: pops the pin
    /// reference, calls the registered `wpjit_list_len` helper, and
    /// pushes the `int` length. The helper returns a negative value
    /// only on a pin-table miss (defensive — cannot happen by
    /// construction), which deopts at this pc.
    ListLen,
    /// RFC 0065 WS5 — `x.append(v)` on a pinned list: pops the value
    /// (staged through the frame's `ret_bits`, interpreted per the
    /// pin's element lane) and the pin reference, and calls
    /// `wpjit_list_append`. The analyzer guarantees the value's lane
    /// matches the pinned element lane, so the append preserves the
    /// pinned shape; a non-zero status (defensive) deopts at this pc,
    /// where the interpreter re-executes the `CALL`.
    ListAppend,
    /// RFC 0065 WS5 — `LOAD_ATTR` on a pinned instance: pops the pin
    /// reference, calls the registered `wpjit_attr_get` helper with
    /// `site` (an index into [`TFunc::attr_sites`]), and pushes the
    /// `out`-lane result. The helper re-validates the burned-in shape
    /// (class identity + attr-version, indexed instance-dict hit with
    /// name match, value lane) and deopts at this pc on any surprise,
    /// so the interpreter re-executes the attribute load generically —
    /// descriptors, `__getattr__`, and `AttributeError` all behave
    /// exactly as tier 1.
    AttrGet { site: u32, out: JitType },
    /// RFC 0065 WS5 — `STORE_ATTR` on a pinned instance: pops the pin
    /// reference and the value (staged through `ret_bits`), and calls
    /// `wpjit_attr_set` with `site`. The helper additionally requires
    /// the *displaced* dict value to be a scalar (dropping a heap
    /// object belongs to the interpreter's store path); any surprise
    /// deopts at this pc and the interpreter re-executes the store.
    AttrSet { site: u32 },
    /// RFC 0069 WS2 — a burned-in `math` intrinsic: pops one `float`
    /// operand (already promoted) and pushes the `float` result.
    /// Domain surprises deopt at this pc *before* the operation with
    /// the operand spilled, so the interpreter re-executes the call
    /// (the enclosing math span rebuilds the `[func, null]` pair the
    /// interpreter holds below the argument) and raises exactly.
    MathIntrinsic(MathFunc),
    /// RFC 0070 WS1 — `x is None` / `x is not None` on a nullable
    /// object lane: pops the `Obj` value (machine value: pin index, or
    /// `-1` for `None`) and pushes the `bool` result. Purely native —
    /// no helper call, no deopt.
    IsNone { negate: bool },
    /// RFC 0070 WS1 — push the `None` singleton in the nullable object
    /// lane (machine value `-1`). Emitted where a `None` constant must
    /// occupy a *native* stack slot: a `StoreFast` into an `Obj` local,
    /// or the value operand of an object-lane `AttrSet`.
    PushNone,
    /// RFC 0070 WS1 — deopt at this pc when TOS (an `Obj` lane) is
    /// `None` (machine value `-1`), *without* popping it. Emitted at an
    /// erased method-form `LOAD_ATTR` whose burned-in resolution
    /// assumed an instance receiver: the deopt spills the receiver as
    /// the real `None` and the interpreter re-executes the attribute
    /// load, raising the exact `AttributeError`.
    GuardNotNone,
    /// RFC 0069 WS1 — a guarded method call on a pinned receiver: pops
    /// `argc` scalar arguments and the receiver pin, and calls the
    /// registered `wpjit_call_method` helper with `token` (an index
    /// into the compiled frame's method table). The helper
    /// re-validates the burned-in class fingerprint + `__code__`
    /// identity per call; a mismatch deopts *at* this pc with the
    /// receiver + arguments spilled (the call never ran — the
    /// interpreter re-executes it generically). A raised callee takes
    /// the `Raised` exit; an unrepresentable result (or invalidated
    /// caller guard) deopts *after* the call, exactly the `CallPy`
    /// protocol.
    CallMethod {
        token: u32,
        argc: u8,
        ret: MethodRet,
    },
    /// RFC 0071 WS6 — `==`/`!=` on two pinned `str` values: pops both
    /// pins, calls the registered `wpjit_str_eq` helper (identical-pin
    /// and pointer equality answer before a content compare), and
    /// pushes the `bool` result (inverted when `negate`). A pin-table
    /// surprise (defensive) deopts at this pc with both operands
    /// spilled.
    StrEq { negate: bool },
    /// RFC 0071 WS6 — `len(s)` on a pinned `str`: pops the pin, calls
    /// the registered `wpjit_str_len` helper, and pushes the `int`
    /// *character* count (the helper counts scalar values, matching
    /// `str.__len__`). Negative status (pin miss) deopts at this pc.
    StrLen,
    /// RFC 0071 WS6 — `len(b)` on a pinned `bytes`: like [`Self::StrLen`]
    /// but the byte count.
    BytesLen,
    /// RFC 0071 WS6 — `BINARY_SUBSCR` on a pinned `bytes` with an `int`
    /// index: pops the index and the pin, calls the registered
    /// `wpjit_bytes_get` helper (bounds-checked, negative index
    /// normalized), and pushes the byte as an `Int`. Out of range
    /// deopts at this pc and the interpreter re-executes the subscript
    /// to raise exactly.
    BytesGetItem,
    /// RFC 0071 WS4 — the opaque-iterator capture behind an erased
    /// `GET_ITER` whose operand rides the object lane: pops the pin
    /// and calls the registered `wpjit_get_iter` helper, which admits
    /// only objects where `iter(x) is x` (generators, builtin
    /// iterators) and stores the pin into `iter_slot`. Anything else
    /// (an instance with `__iter__`, a non-iterable) deopts at this
    /// pc with the operand spilled, and the interpreter executes the
    /// `GET_ITER` — and the whole loop — generically.
    IterCapture { iter_slot: u32 },
    /// RFC 0071 WS4 — `BUILD_LIST k`: pops `n` same-lane elements
    /// (staged through the frame's marshal buffer) and pushes a fresh
    /// pinned list of the `elem` lane. `none_fill` covers the
    /// `[None, ...]` literal shape: nothing is popped (the `None`
    /// constants never reached the native stack) and the helper
    /// writes `n` `None` elements. Pin-cap pressure deopts at this pc
    /// and the interpreter re-executes the `BUILD_LIST`.
    BuildList {
        n: u32,
        elem: JitType,
        none_fill: bool,
    },
    /// RFC 0071 WS4 — `list * int` (`BINARY_OP *` with a pinned-list
    /// lhs): pops the count and the pin, calls the registered
    /// `wpjit_list_repeat` helper (element `Arc`s are shared, exactly
    /// CPython's aliasing), and pushes the fresh pinned list on the
    /// same lane. Cap pressure deopts at this pc.
    ListRepeat,
    /// RFC 0071 WS4 — `xs[a:b]` on a pinned list (an erased
    /// `BUILD_SLICE 3` whose step is the `None` constant followed by
    /// `BINARY_SUBSCR`): pops the present bounds (`stop` above
    /// `start`, each only when its flag is set; an absent bound is the
    /// `None` marker, never a native value) and the pin, calls the
    /// registered `wpjit_list_slice` helper (CPython index clamping),
    /// and pushes the fresh pinned list on the same lane. Cap
    /// pressure deopts at this pc.
    ListSlice { start: bool, stop: bool },
}

/// One IR statement: a [`TOp`] tagged with its originating bytecode pc
/// so a side exit can name the exact resume point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TStmt {
    pub pc: u32,
    pub op: TOp,
}

/// How a basic block transfers control.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TTerm {
    /// Pop TOS and return it from the frame.
    Return,
    /// RFC 0069 WS1 — `return None` (including the implicit
    /// function-tail return): no native value is popped; the frame
    /// exits `Returned` with the `None` slot tag.
    ReturnNone,
    /// Unconditional branch; the current abstract stack is passed as
    /// block args.
    Jump(BlockId),
    /// `POP_JUMP_IF_FALSE`: pop the condition; branch to `target` if
    /// falsy, else `fallthrough`.
    BranchFalse {
        target: BlockId,
        fallthrough: BlockId,
    },
    /// `POP_JUMP_IF_TRUE`: pop the condition; branch to `target` if
    /// truthy, else `fallthrough`.
    BranchTrue {
        target: BlockId,
        fallthrough: BlockId,
    },
    /// RFC 0058 WS4 — a recognized `FOR_ITER` over a unit-step `range`,
    /// rewritten to an i64 counted loop over two synthetic local slots.
    /// If `cur < stop`: store `cur` into `var_slot`, bump `cur`, and
    /// branch to `body`; else branch to `exit`. `cur < stop <= i64::MAX`
    /// makes the unit-step increment provably overflow-free.
    ForRange {
        cur_slot: u32,
        stop_slot: u32,
        var_slot: u32,
        body: BlockId,
        exit: BlockId,
    },
    /// RFC 0071 WS4 — a recognized `FOR_ITER` over a pinned list,
    /// rewritten to an index-stepped loop over two synthetic slots:
    /// `seq_slot` holds the pinned list (captured at the erased
    /// `GET_ITER`, so rebinding the source local mid-loop cannot
    /// retarget the iteration) and `idx_slot` the next element index.
    /// Each step calls the registered `wpjit_list_next` helper, which
    /// re-checks the index against the *live* length (mutation during
    /// iteration is defined behavior — CPython's `FOR_ITER_LIST` does
    /// the same) and re-validates the element lane: a yielded element
    /// stores into `var_slot` and branches to `body`; exhaustion
    /// branches to `exit`; an element-shape surprise deopts at the
    /// header pc, where the interpreter resumes on a freshly rebuilt
    /// list iterator.
    ForList {
        seq_slot: u32,
        idx_slot: u32,
        var_slot: u32,
        /// The element lane (`var_slot`'s lane).
        elem: JitType,
        /// The `FOR_ITER` pc — the deopt resume point on an
        /// element-shape surprise (the interpreter re-executes the
        /// step on the rebuilt iterator).
        pc: u32,
        body: BlockId,
        exit: BlockId,
    },
    /// RFC 0071 WS4 — a recognized `FOR_ITER` over an *opaque*
    /// iterator (a generator or builtin iterator object riding the
    /// object lane), stepped through the registered `wpjit_iter_next`
    /// helper. The iterator object is pinned in `iter_slot` (captured
    /// at the erased `GET_ITER`, which verified `iter(x) is x`); each
    /// step advances it through the interpreter core. Statuses:
    /// element in the compiled `elem` lane → store into `var_slot`,
    /// branch to `body`; exhaustion → branch to `exit` (the helper
    /// promptly reaps the iterator — RFC 0068's finalization
    /// discipline); a lane surprise → the element was already
    /// *consumed*, so the deopt resumes at `store_pc` (the fused
    /// `STORE_FAST`) with the raw element spilled on top of the
    /// rebuilt stack; a raise inside the iterator propagates through
    /// the ordinary `Raised` exit at the header pc.
    ForIter {
        iter_slot: u32,
        var_slot: u32,
        /// The element lane (`var_slot`'s lane).
        elem: JitType,
        /// The `FOR_ITER` pc (raise site; also the deopt point when
        /// the pin is not steppable — nothing consumed yet).
        pc: u32,
        /// The fused `STORE_FAST` pc — the resume point for a
        /// consumed-element deopt.
        store_pc: u32,
        body: BlockId,
        exit: BlockId,
    },
    /// RFC 0070 WS2 — `YIELD_VALUE`: an unconditional deopt-shaped
    /// side exit with [`crate::runtime::JitStatus::Yielded`]. Locals
    /// are written back and the abstract stack is spilled with the
    /// yielded value on top; `pc` names the `YIELD_VALUE` instruction
    /// itself, which the embedder *re-executes* in the interpreter to
    /// perform the actual suspension. The block has no native
    /// successors — post-yield code runs interpreted until the next
    /// loop back edge OSR-enters the compiled body again.
    Yield { pc: u32 },
}

/// A basic block: a static entry-stack shape, a straight-line body, and
/// a terminator.
#[derive(Clone, Debug, PartialEq)]
pub struct TBlock {
    /// Types of the operand-stack values live on entry (lowered to
    /// Cranelift block parameters), bottom-to-top.
    pub entry_stack: Vec<JitType>,
    pub stmts: Vec<TStmt>,
    pub term: TTerm,
}

/// What a `LOAD_GLOBAL` name resolved to at analysis time. Provided by
/// the embedder's resolver closure; anything other than `Opaque` is
/// burned into the compiled code, and the embedder must re-validate the
/// resolution (an identity guard) on every native entry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ResolvedGlobal {
    /// The canonical builtin `range` — eligible as a counted-loop callee.
    RangeBuiltin,
    /// An `int` global, burned in as a constant.
    ConstInt(i64),
    /// A `float` global, burned in as a constant (stored as bits so the
    /// enum stays `Copy` + `PartialEq`).
    ConstFloat(u64),
    /// A `bool` global, burned in as a constant.
    ConstBool(bool),
    /// RFC 0065 WS5 — the canonical builtin `len`, recognized as a
    /// pinned-list length callee (lowered to [`TOp::ListLen`], never a
    /// real call).
    LenBuiltin,
    /// RFC 0059 WS3 — a plain Python function, callable natively through
    /// the `wpjit_call_py` helper. `token` indexes the embedder's callee
    /// table (parallel to resolution order); `arg_count` is the callee's
    /// positional arity; `min_args` is the arity minus its trailing
    /// defaults (RFC 0069 WS3 — a call site passing `min_args..=
    /// arg_count` positionals is admitted, and the embedder binds the
    /// snapshotted defaults for the tail); `ret` is the callee's
    /// inferred scalar return lane (`None` when the callee is the
    /// function being compiled — the analyzer resolves self-recursion
    /// through its own return-lane fixpoint).
    PyFunc {
        token: u32,
        arg_count: u32,
        min_args: u32,
        is_self: bool,
        ret: Option<JitType>,
    },
    /// RFC 0069 WS2 — the canonical `math` module. Only consumable by
    /// an immediately following method-form attribute load of a
    /// burned-in intrinsic name; any other use disqualifies the frame.
    MathModule,
    /// Anything else — not representable; the load disqualifies the frame.
    Opaque,
}

/// One entry guard the embedder must re-validate before each native
/// entry: `name` must still resolve (globals-then-builtins) to the same
/// object it resolved to at compile time.
#[derive(Clone, Debug, PartialEq)]
pub struct GlobalGuard {
    pub name: String,
    pub expect: ResolvedGlobal,
}

/// Deopt-reconstruction metadata for one erased callee (RFC 0059 WS3):
/// between the (erased) `LOAD_GLOBAL` of a Python callee and its `CALL`,
/// the *interpreter's* operand stack holds the callee object at absolute
/// depth `interp_depth` (below the argument temporaries, above any
/// enclosing range iterators). A deopt at a pc strictly inside
/// `(live_from, live_to)` must re-insert the callee-table object at that
/// index when rebuilding the stack.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CalleeSpanMeta {
    /// Callee-table index (same space as [`TOp::CallPy`]'s `token`).
    pub token: u32,
    /// The erased `LOAD_GLOBAL` pc.
    pub live_from: u32,
    /// The `CALL` pc (the call itself consumes the callee, so the span
    /// is open only for pcs strictly between the endpoints).
    pub live_to: u32,
    /// Absolute interpreter-stack index of the callee object,
    /// accounting for enclosing erased entities (range iterators and
    /// outer callee spans).
    pub interp_depth: u32,
}

/// Deopt-reconstruction metadata for one erased *method receiver*
/// (RFC 0065 WS5 / RFC 0069 WS1): between a `LOAD_ATTR` method load
/// on a pinned receiver and its `CALL`, the *interpreter's* operand
/// stack holds the bound method (plus the self-or-null `Unbound`
/// marker above it) where the native stack holds the raw pin. A side
/// exit at a pc in `(live_from, live_to)` must rebuild the spilled
/// entry at native-stack index `native_index` as the bound method +
/// marker instead of the bare pin. `live_to` is the pc *after* the
/// `CALL`, so a deopt at the `CALL` itself (a rejected method guard,
/// or `append`'s defensive exit) is still inside the span.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MethodSpanMeta {
    /// Bottom-based index of the receiver in the native operand stack
    /// (equal to its index in the deopt spill).
    pub native_index: u32,
    /// The `LOAD_ATTR` pc (exclusive — the receiver is a plain value
    /// before it executes).
    pub live_from: u32,
    /// The pc after the `CALL` (exclusive).
    pub live_to: u32,
    /// RFC 0069 WS1 — `Some(token)` for a burned-in method site (the
    /// bound method rebuilds from the embedder's method table);
    /// `None` for the RFC 0065 `list.append` shape (rebuilds via a
    /// fresh `append` load on the pinned list).
    pub token: Option<u32>,
}

/// One burned-in attribute-access site (RFC 0065 WS5). The embedder
/// re-probes each site after compilation to snapshot the concrete
/// guard fingerprint (class identity, attr-version, dict index) that
/// its `wpjit_attr_get`/`_set` helpers re-validate per access.
#[derive(Clone, Debug, PartialEq)]
pub struct AttrSiteMeta {
    /// The local slot the receiver chain is rooted at (probe key).
    pub slot: u32,
    /// RFC 0071 WS3 — the attribute names walked from the root local
    /// to reach the receiver (empty for a direct local receiver). The
    /// embedder's probe and guard snapshot walk the same chain; the
    /// runtime helper needs only the receiver's *pin*, so the path is
    /// compile-time metadata.
    pub path: Vec<String>,
    /// The attribute name.
    pub name: String,
    /// The value lane the site was typed with.
    pub lane: JitType,
    /// `true` for a `STORE_ATTR` site.
    pub store: bool,
    /// RFC 0071 WS2 — `true` for the constructor-pattern store shape:
    /// the attribute is not yet present, so the helper performs a
    /// single-probe insert-or-replace (mirroring tier-1's
    /// `StoreAttrNewKey`) instead of an indexed overwrite.
    pub new_key: bool,
}

/// RFC 0069 WS1 — one burned-in method-call site, indexed by
/// [`TOp::CallMethod`]'s `token`. The embedder resolved `(slot, name)`
/// through the receiver's class at analysis time and snapshots the
/// resolved function + class fingerprint per token; the call helper
/// re-validates both per call.
#[derive(Clone, Debug, PartialEq)]
pub struct MethodSiteMeta {
    /// The local slot the receiver was loaded from (probe key).
    pub slot: u32,
    /// The method name.
    pub name: String,
    /// The callee's positional arity, `self` included.
    pub arg_count: u32,
    /// Arity minus trailing defaults (`self` included).
    pub min_args: u32,
    /// The call result's typing.
    pub ret: MethodRet,
}

/// RFC 0069 WS2 — one burned-in `math` intrinsic guard: the global
/// `name` must resolve to the canonical math module *and* its `attr`
/// must still be the function object the embedder snapshotted at
/// compile time (module dicts are mutable, so the entry guard and the
/// per-stride poll both re-validate the pair).
#[derive(Clone, Debug, PartialEq)]
pub struct MathGuardMeta {
    /// The `LOAD_GLOBAL` name (`math`, or an alias bound to the module).
    pub name: String,
    /// The attribute name (`sqrt`, `sin`, `cos`, `fabs`).
    pub attr: String,
    /// The intrinsic burned in for this site.
    pub kind: MathFunc,
}

/// One OSR entry point (RFC 0059 WS3b): a backward-jump target block
/// with an empty boundary stack, enterable mid-frame via
/// [`crate::JitFrame::entry_pc`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OsrEntry {
    /// The bytecode pc of the block leader (the backward jump's target).
    pub pc: u32,
    /// The [`TBlock`] to enter.
    pub block: BlockId,
}

/// Deopt-reconstruction metadata for one rewritten `range` loop: at any
/// deopt pc in `[live_from, live_to)` the *interpreter's* operand stack
/// would hold the live range iterator below the spilled temporaries, so
/// the embedder must rebuild it from the two synthetic slots.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RangeLoopMeta {
    /// Synthetic slot holding the next value to yield.
    pub cur_slot: u32,
    /// Synthetic slot holding the exclusive stop bound.
    pub stop_slot: u32,
    /// First pc (the `FOR_ITER`) at which the iterator is live.
    pub live_from: u32,
    /// The `END_FOR` pc; the iterator is dead from here on.
    pub live_to: u32,
}

/// RFC 0071 WS4 — deopt-reconstruction metadata for one rewritten
/// *list* loop: at any deopt pc in `[live_from, live_to)` the
/// interpreter's operand stack would hold a live list iterator below
/// the spilled temporaries, rebuilt from the pinned list in `seq_slot`
/// and the index in `idx_slot`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ListLoopMeta {
    /// Synthetic slot holding the pinned list (a pin-table index).
    pub seq_slot: u32,
    /// Synthetic slot holding the next element index.
    pub idx_slot: u32,
    /// First pc (the `FOR_ITER`) at which the iterator is live.
    pub live_from: u32,
    /// The `END_FOR` pc; the iterator is dead from here on.
    pub live_to: u32,
}

/// RFC 0071 WS4 — deopt-reconstruction metadata for one rewritten
/// *opaque-iterator* loop: at any deopt pc in `[live_from, live_to)`
/// the interpreter's operand stack would hold the iterator *object
/// itself* (a generator or builtin iterator — `iter(x) is x` was
/// verified at capture) below the spilled temporaries, rebuilt
/// directly from the pin in `iter_slot`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IterLoopMeta {
    /// Synthetic slot holding the pinned iterator (a pin-table index).
    pub iter_slot: u32,
    /// First pc (the `FOR_ITER`) at which the iterator is live.
    pub live_from: u32,
    /// The `END_FOR` pc; the iterator is dead from here on.
    pub live_to: u32,
}

/// A fully analyzed, JITable function body.
#[derive(Clone, Debug, PartialEq)]
pub struct TFunc {
    /// Number of local slots, *including* the synthetic `range`-loop
    /// slots appended after the code object's real locals.
    pub n_locals: u32,
    /// Stable JIT type of each local slot, or `None` for slots the
    /// region never touches (left untouched by the JIT).
    pub local_types: Vec<Option<JitType>>,
    /// Local slots that are live-in at function entry (read before
    /// written). The VM type-guards and packs exactly these before
    /// entering native code.
    pub livein_locals: Vec<u32>,
    /// Maximum abstract operand-stack depth, for sizing the deopt spill
    /// buffer.
    pub max_stack: u32,
    pub blocks: Vec<TBlock>,
    pub entry_block: BlockId,
    /// Entry guards for every `LOAD_GLOBAL` burned into the code
    /// (RFC 0058 WS4), deduplicated by name.
    pub global_guards: Vec<GlobalGuard>,
    /// Rewritten `range` loops, ordered outermost-first (ascending
    /// `live_from`), for deopt stack reconstruction.
    pub range_loops: Vec<RangeLoopMeta>,
    /// RFC 0071 WS4 — rewritten *list* loops, ascending `live_from`,
    /// for deopt stack reconstruction (interleaved with
    /// [`Self::range_loops`] by `live_from` when rebuilding).
    pub list_loops: Vec<ListLoopMeta>,
    /// RFC 0071 WS4 — rewritten *opaque-iterator* loops, ascending
    /// `live_from`, for deopt stack reconstruction (interleaved with
    /// the other loop kinds by `live_from` when rebuilding).
    pub iter_loops: Vec<IterLoopMeta>,
    /// Erased Python callees (RFC 0059 WS3), ascending `live_from`, for
    /// deopt stack reconstruction during argument computation.
    pub callee_spans: Vec<CalleeSpanMeta>,
    /// RFC 0065 WS5 — erased `len` builtins riding the interpreter
    /// stack between their `LOAD_GLOBAL` and `CALL`. Same
    /// reconstruction contract as [`Self::callee_spans`], except the
    /// re-inserted object is the guard snapshot's `len` (the `token`
    /// field is unused) and `live_to` is the pc *after* the `CALL`.
    pub len_spans: Vec<CalleeSpanMeta>,
    /// RFC 0065 WS5 / RFC 0069 WS1 — erased bound-method receivers
    /// (`list.append` and burned-in method sites), for rewriting the
    /// spilled receiver on a mid-span deopt.
    pub method_spans: Vec<MethodSpanMeta>,
    /// RFC 0065 WS5 — burned-in attribute-access sites, indexed by
    /// [`TOp::AttrGet`]/[`TOp::AttrSet`]'s `site`.
    pub attr_sites: Vec<AttrSiteMeta>,
    /// RFC 0069 WS1 — burned-in method-call sites, indexed by
    /// [`TOp::CallMethod`]'s `token`. The embedder's method table is
    /// parallel to this.
    pub method_sites: Vec<MethodSiteMeta>,
    /// RFC 0069 WS2 — burned-in math-intrinsic guards, one per
    /// distinct `(name, attr)` pair, in first-use order.
    pub math_guards: Vec<MathGuardMeta>,
    /// RFC 0069 WS2 — erased `math` intrinsic callables riding the
    /// interpreter stack between their method load and `CALL`. Same
    /// reconstruction contract as [`Self::len_spans`] (`token` indexes
    /// [`Self::math_guards`], `live_to` is the pc after the `CALL`).
    pub math_spans: Vec<CalleeSpanMeta>,
    /// OSR entry points (RFC 0059 WS3b): backward-jump target blocks
    /// enterable via `entry_pc`.
    pub osr_entries: Vec<OsrEntry>,
    /// RFC 0071 WS5 — generator *resume* entry points: the block after
    /// a `YIELD_VALUE`, enterable via `entry_pc` when the embedder
    /// resumes a suspended generator. Each target block's boundary
    /// stack is exactly `[Obj]` — the sent value, passed in
    /// [`crate::runtime::JitFrame::ret_bits`] on the object lane's
    /// packing (a pin index, `None` as `-1`).
    pub resume_entries: Vec<OsrEntry>,
    /// Widest `CallPy`/`CallMethod` argument count, for sizing the
    /// marshal buffer.
    pub max_call_args: u32,
    /// The function's own scalar return lane, when every `return` site
    /// agrees on one representable lane (RFC 0059 WS3). This is what a
    /// *caller's* analysis burns in as `PyFunc::ret`.
    pub ret_lane: Option<JitType>,
    /// RFC 0069 WS1 — `true` when every return site is provably the
    /// `None` constant (the procedure shape). Mutually exclusive with
    /// a concrete [`Self::ret_lane`]: mixed None/scalar returns poison
    /// the lane to `Unknown`.
    pub ret_none: bool,
}

impl TOp {
    /// `true` for operations that can take a side exit (deopt) and so
    /// need their abstract stack spilled at their pc.
    #[must_use]
    pub fn can_deopt(self) -> bool {
        matches!(
            self,
            TOp::IntArith(
                ArithKind::Add
                    | ArithKind::Sub
                    | ArithKind::Mul
                    | ArithKind::FloorDiv
                    | ArithKind::Mod
            ) | TOp::IntNeg
                | TOp::IntTrueDiv
                | TOp::FloatArith(ArithKind::TrueDiv)
                | TOp::IntToFloatTos { guarded: true }
                | TOp::IntToFloatSecond { guarded: true }
                | TOp::CallPy { .. }
                | TOp::CallMethod { .. }
                | TOp::MathIntrinsic(_)
                | TOp::FloatArith(ArithKind::FloorDiv | ArithKind::Mod)
                | TOp::ListGet { .. }
                | TOp::ListSet
                | TOp::ListLen
                | TOp::ListAppend
                | TOp::AttrGet { .. }
                | TOp::AttrSet { .. }
                | TOp::GuardNotNone
                | TOp::StrEq { .. }
                | TOp::StrLen
                | TOp::BytesLen
                | TOp::BytesGetItem
                | TOp::IterCapture { .. }
                | TOp::BuildList { .. }
                | TOp::ListRepeat
                | TOp::ListSlice { .. }
        )
    }
}
