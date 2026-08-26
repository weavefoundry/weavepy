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
    /// RFC 0073 WS5 — a Python-to-Python *keyword* call (`CALL_KW`)
    /// through the same `wpjit_call_py` helper. Pops `argc + kwc`
    /// values (positionals below, keyword values above, interpreter
    /// stack order); the analyzer resolved each keyword to its
    /// parameter slot at compile time, packed 4 bits per keyword in
    /// `perm` (keyword value `j` → slot `(perm >> 4j) & 0xF`, tier-1's
    /// `CallPyKwNames` encoding). The filled slots are validated to be
    /// exactly `0..argc+kwc`, so lowering marshals a plain positional
    /// prefix and the call helper needs no keyword awareness (the
    /// trailing-defaults window binds any remaining tail). The names
    /// tuple's `LOAD_CONST` is erased from the trace; it never exists
    /// on the native stack. Exits mirror [`TOp::CallPy`].
    CallPyKw {
        token: u32,
        argc: u8,
        kwc: u8,
        perm: u32,
        ret: JitType,
    },
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
    /// RFC 0073 WS1 — `LIST_APPEND` inside an inlined comprehension
    /// loop: pops the value (staged through `ret_bits` like
    /// [`Self::ListAppend`]) but *keeps* the accumulator pin on the
    /// stack — the comprehension's accumulator stays live across the
    /// whole loop. The analyzer guarantees the value's lane matches
    /// the accumulator's element lane; a non-zero status (defensive)
    /// deopts at this pc, where the interpreter re-executes the
    /// `LIST_APPEND`.
    ListAppendKeep,
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
    /// RFC 0073 WS2 — `d[k]` on a pinned exact `dict`: pops the key
    /// (`key` lane: `Int` bits, or a `Str` pin) and the dict pin, and
    /// calls the registered `wpjit_dict_get` helper. A hit whose value
    /// matches the trained `val` lane pushes it (a fresh pin for
    /// `Obj`); a *missing key* takes the `Raised` exit with the exact
    /// `KeyError(key)` parked — missing keys are control flow in real
    /// code, so they must not charge the deopt budget; a key/value
    /// lane surprise deopts at this pc and the interpreter re-executes
    /// the subscript generically.
    DictGet { key: JitType, val: JitType },
    /// RFC 0073 WS2 — `d[k] = v` on a pinned exact `dict`: pops the
    /// key, the dict pin, and the value (staged through `ret_bits`,
    /// interpreted per the trained `val` lane), and calls the
    /// registered `wpjit_dict_set` helper — the interpreter's own
    /// `dict_insert` chokepoint, so PEP 509 / watcher discipline is
    /// identical. A displaced value that would run the prompt-reap
    /// cascade deopts *before* the store (the `wpjit_attr_set`
    /// discipline), as do active C-API dict watchers and any key-lane
    /// surprise.
    DictSet { key: JitType, val: JitType },
    /// RFC 0073 WS2 — `k in d` / `k not in d` on a pinned exact
    /// `dict`: pops the dict pin and the key, calls the registered
    /// `wpjit_dict_contains` helper, and pushes the `bool` (inverted
    /// when `negate`). A key-lane surprise deopts at this pc.
    DictContains { negate: bool, key: JitType },
    /// RFC 0073 WS2 — `len(d)` on a pinned exact `dict`: like
    /// [`Self::ListLen`] but for the dict pin.
    DictLen,
    /// RFC 0073 WS3 — a burned-in native `str`-method call on a
    /// pinned exact-`str` receiver: pops `argc` lane-typed arguments
    /// (staged through the marshal buffer with per-slot tags) and the
    /// receiver pin, and calls the registered `wpjit_str_method`
    /// helper, which dispatches on the burned [`StrMethod`] to the
    /// interpreter's own builtin body (identical validation, arity
    /// wording, and raise behavior). `site` indexes
    /// [`TFunc::str_method_sites`]. The result must wear the method's
    /// static return lane (`Str`/`Int`/`Bool`/`ListObj`); a lane
    /// surprise (e.g. a `WStr`-producing `join`) deopts at this pc —
    /// `str` methods are pure, so the interpreter's re-execution is
    /// exact. A raise takes the `Raised` exit.
    CallStrMethod { site: u32, argc: u8, ret: JitType },
    /// RFC 0073 WS3 — guarded exact-`str` `+` (the
    /// `BINARY_OP_ADD_UNICODE` shape): pops two `str` pins, calls the
    /// registered `wpjit_str_concat` helper (which allocates the
    /// joined `Rc<str>`), and pushes the fresh pin. Cap pressure or a
    /// pin surprise deopts at this pc and the interpreter re-executes
    /// the add.
    StrConcat,
    /// RFC 0073 WS3 — `s[i]` on a pinned exact `str` with an `int`
    /// index (the `SubscrStrInt` shape): O(1) byte indexing on an
    /// ASCII payload, single-codepoint result pinned on the `Str`
    /// lane. A non-ASCII receiver, an out-of-range index (the exact
    /// `IndexError` comes from the interpreter's re-execution), or
    /// cap pressure deopts at this pc.
    StrGetItem,
    /// RFC 0073 WS3 — `BUILD_STRING n` (the f-string join): pops `n`
    /// `str` pins (staged through the marshal buffer), concatenates
    /// them in order through the registered `wpjit_build_string`
    /// helper, and pushes the fresh pin. Cap pressure deopts at this
    /// pc.
    BuildString { n: u32 },
    /// RFC 0073 WS2 — an exact-`str` constant (`LOAD_CONST`): pushes a
    /// pin of the materialized `str` through the registered
    /// `wpjit_const_str` helper, which memoizes per `(activation,
    /// constant index)` — a loop re-executing the load reuses one pin,
    /// so the pin table stays bounded. Cap pressure deopts at this pc.
    PushConstStr { idx: u32 },
    /// RFC 0073 WS2 — `BUILD_MAP n` (`n` *pairs*): pops `2n`
    /// interleaved key/value entries (staged through the marshal
    /// buffer with per-slot tags), builds a fresh GC-tracked dict
    /// through the registered `wpjit_build_map` helper, and pushes the
    /// pinned dict. Keys must box to exact `str`/`int` (the analyzer
    /// enforces the lanes; the helper re-validates). Cap pressure
    /// deopts at this pc and the interpreter re-executes the
    /// `BUILD_MAP`.
    BuildMap { n: u32 },
    /// RFC 0071 WS4 — the opaque-iterator capture behind an erased
    /// `GET_ITER` whose operand rides the object lane: pops the pin
    /// and calls the registered `wpjit_get_iter` helper, which admits
    /// only objects where `iter(x) is x` (generators, builtin
    /// iterators) and stores the pin into `iter_slot`. Anything else
    /// (an instance with `__iter__`, a non-iterable) deopts at this
    /// pc with the operand spilled, and the interpreter executes the
    /// `GET_ITER` — and the whole loop — generically.
    /// RFC 0074 WS3 — `materialize` switches the helper to the
    /// *generic* capture: `iter(x)` is built through the interpreter
    /// core for any pinned iterable (dict views, `enumerate`/`zip`
    /// objects, lists — the tuple-target loop's shape) and the fresh
    /// iterator's pin lands in `iter_slot`; a non-iterable raises the
    /// exact `TypeError` at this pc. A user `__iter__` may run
    /// arbitrary Python (the dirtiness discipline applies) —
    /// invalidated guards deopt at the *next* pc with the built
    /// iterator spilled on top (the interpreter's `FOR_ITER` accepts
    /// an iterator it didn't build).
    IterCapture { iter_slot: u32, materialize: bool },
    /// RFC 0073 WS2 — the dict-loop capture behind an erased
    /// `GET_ITER` whose operand is a pinned exact `dict`: pops the
    /// pin and calls the registered `wpjit_dict_iter_new` helper,
    /// which materializes the *real* `DictKeys` iterator (the same
    /// object the interpreter's `GET_ITER` builds — carrying the
    /// creation-time length snapshot for the mutation guard) and
    /// stores its fresh pin into `iter_slot`. The loop then rides
    /// [`TTerm::ForIter`]: each step goes through the interpreter's
    /// own checked iterator step, so a structural mutation raises the
    /// exact CPython `RuntimeError`, and a deopt mid-loop re-inserts
    /// the live iterator object itself. Cap pressure deopts at this
    /// pc with the dict spilled.
    DictIterNew { iter_slot: u32 },
    /// RFC 0071 WS4 — `BUILD_LIST k`: pops `n` same-lane elements
    /// (staged through the frame's marshal buffer) and pushes a fresh
    /// pinned list of the `elem` lane. `none_fill` covers the
    /// `[None, ...]` literal shape: nothing is popped (the `None`
    /// constants never reached the native stack) and the helper
    /// writes `n` `None` elements. RFC 0073 WS1 — `mixed` marks a
    /// literal whose elements wear different lanes: per-element
    /// [`SlotTag`](crate::SlotTag)s ride the marshal tag buffer, the
    /// helper boxes each element by its own tag, and the result rides
    /// the object-element lane (`elem` is `Obj`). Pin-cap pressure
    /// deopts at this pc and the interpreter re-executes the
    /// `BUILD_LIST`.
    BuildList {
        n: u32,
        elem: JitType,
        none_fill: bool,
        mixed: bool,
    },
    /// RFC 0073 WS1 — `BUILD_TUPLE k`: pops `n` elements (staged
    /// through the marshal buffer with per-element tags, exactly like
    /// a mixed `BuildList`) and pushes the fresh tuple as an
    /// object-lane pin. Tuples are immutable, so no element-lane
    /// tracking survives construction — and unlike lists, the
    /// interpreter does not GC-track fresh tuples (refcount-only;
    /// a tuple cannot close a cycle it wasn't born into). Pin-cap
    /// pressure deopts at this pc and the interpreter re-executes
    /// the `BUILD_TUPLE`.
    BuildTuple { n: u32 },
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
    /// RFC 0074 WS1 — a `LOAD_GLOBAL` resolving to an arbitrary
    /// object (a builtin, a class, a `*args`/`**kwargs` function, a
    /// module-level container or string — anything the specialized
    /// resolutions don't cover): pushes a pin of the snapshotted
    /// object through the registered `wpjit_global_obj` helper, which
    /// memoizes per `(activation, token)` so a loop re-executing the
    /// load reuses one pin. The identity guard rides the ordinary
    /// global-guard snapshot (`token` indexes the embedder's
    /// obj-global table, parallel to resolution order); `lane` is the
    /// snapshot value's graded lane (`Str`/`Bytes`/`Dict`/a list
    /// lane/`Obj`), re-validated by the helper — a lane surprise
    /// (impossible while the identity guard holds) or cap pressure
    /// deopts at this pc.
    PushGlobalObj { token: u32, lane: JitType },
    /// RFC 0074 WS2 — the opaque-call lane: `CALL argc` on a callee
    /// that is a plain native value (any lane) instead of a burned-in
    /// mark. Pops `argc` arguments (staged through the marshal buffer
    /// with per-slot tags) and the callee value, and calls the
    /// registered `wpjit_call_dyn` helper, which boxes everything and
    /// runs the call through the interpreter core — arbitrary Python
    /// may run (the dirtiness discipline applies). The result rides
    /// the object lane. Statuses mirror `CallPy`: `Ok` pushes the
    /// pinned result and native execution *continues*; `Raised` exits
    /// at this pc; `Boxed` (guards invalidated by callee side effects,
    /// or pin-cap pressure on the result) deopts *after* the call
    /// with the parked result — the call is never re-executed.
    /// The keyword form (`kwc > 0`) stages `kwc` keyword values above
    /// the positionals; `names` is the constant-pool index of the
    /// interned kwnames tuple (the erased `LOAD_CONST` of the
    /// `CALL_KW` shape).
    CallDyn { argc: u8, kwc: u8, names: u32 },
    /// RFC 0074 WS2/WS4 — an eager generic `LOAD_ATTR` (either form)
    /// on a receiver the burned-fingerprint lanes don't cover: pops
    /// the receiver pin and performs the interpreter's exact
    /// attribute load *at this pc* (bound methods materialize,
    /// descriptors run, `AttributeError` raises exactly here),
    /// pushing the loaded value as a fresh object-lane pin. No
    /// fingerprint guard — the lookup is generic per execution. For
    /// the method form, the implicit self-or-null marker above the
    /// result is interpreter-only (a null span re-inserts `Unbound`
    /// on deopt), so the following `CALL` consumes the loaded value
    /// through the opaque-call lane. `name` is the `names` index.
    /// Cap pressure deopts at this pc.
    DynAttrGet { name: u32 },
    /// RFC 0074 WS4 — the matching `STORE_ATTR` fallback: pops the
    /// receiver pin (stack top) and the value below it (staged
    /// through the marshal buffer with its tag), and performs the
    /// interpreter's exact attribute store at this pc (`__setattr__`
    /// dispatch included — arbitrary Python may run, so the
    /// dirtiness discipline applies; invalidated guards deopt at the
    /// *next* pc with the store already performed).
    DynAttrSet { name: u32 },
    /// RFC 0074 WS5 — `str % x` (`BINARY_OP %` with a pinned exact-
    /// `str` lhs): pops the rhs (any plain lane, staged with its tag)
    /// and the lhs pin, and runs the interpreter's `%`-formatting.
    /// An exact-`str` result pushes a fresh pin; a raise exits at
    /// this pc; a non-`str` result (a `str` subclass lhs is already
    /// excluded by the pin lane) or cap pressure parks the computed
    /// result and deopts *after* the op — formatting side effects
    /// (`__str__`/`__repr__` of the operands) never re-run.
    StrMod,
    /// RFC 0074 WS5 — `s[a:b]` on a pinned exact `str` (the erased
    /// `BUILD_SLICE` marker shape, unit step): pops the present
    /// bounds (`stop` above `start`) and the pin, applies CPython's
    /// slice clamping through the interpreter's subscript core, and
    /// pushes the fresh `str` pin. Cap pressure deopts at this pc.
    StrSlice { start: bool, stop: bool },
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
    /// RFC 0074 WS3 — a recognized tuple-target `FOR_ITER`
    /// (`for a, b in it:`, the `UNPACK_SEQUENCE 2` + two-store
    /// prologue), stepped through the registered
    /// `wpjit_iter_next_pair` helper over the pinned iterator in
    /// `iter_slot` (captured at the erased `GET_ITER`). Each step
    /// advances the iterator through the interpreter core and unpacks
    /// the yielded 2-sequence: elements in the compiled lanes →
    /// stores into `var1_slot`/`var2_slot`, branch to `body`;
    /// exhaustion → branch to `exit`; a non-2-sequence element or a
    /// lane surprise → the element was already *consumed*, so the
    /// deopt resumes at `store_pc` (the `UNPACK_SEQUENCE`) with the
    /// raw element spilled on top of the rebuilt stack (the
    /// interpreter re-executes the unpack, raising the exact
    /// `ValueError`/`TypeError` when malformed); a raise inside the
    /// iterator propagates through the `Raised` exit at the header pc.
    ForIterPair {
        iter_slot: u32,
        var1_slot: u32,
        var2_slot: u32,
        /// The first element's lane (`var1_slot`'s lane).
        elem1: JitType,
        /// The second element's lane (`var2_slot`'s lane).
        elem2: JitType,
        /// The `FOR_ITER` pc (raise site; also the deopt point when
        /// the pin is not steppable — nothing consumed yet).
        pc: u32,
        /// The `UNPACK_SEQUENCE` pc — the resume point for a
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
    /// RFC 0074 WS3 — the canonical builtin `enumerate`. Burns like any
    /// obj-global (the plan gate routes it through the obj-global
    /// probe, so its `LOAD_GLOBAL` pushes an identity-guarded pin and
    /// its `CALL` rides the opaque-call lane); the classification only
    /// *certifies* canonicality, so the tuple-target loop recognizer
    /// may train the pair lanes (`i` is `Int`, the element wears the
    /// iterable's probed lane). The per-step lane tags re-validate at
    /// runtime, so the certification is a prediction like every burn.
    EnumerateBuiltin,
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
        /// RFC 0071 WS2 / RFC 0073 WS1 — the callee is a burned-in
        /// *class constructor* (the call returns a fresh instance on
        /// the object lane). Attribute sites on a local bound from
        /// such a call may resolve their fingerprint against the
        /// class's post-construction canonical shape when the local
        /// has no live value to probe (RFC 0073 WS1 — the receiver
        /// residue).
        ctor: bool,
    },
    /// RFC 0069 WS2 — the canonical `math` module. Only consumable by
    /// an immediately following method-form attribute load of a
    /// burned-in intrinsic name; any other use disqualifies the frame.
    MathModule,
    /// RFC 0074 WS1 — any other *resolvable* object: burned in as an
    /// identity-guarded object-lane pin ([`TOp::PushGlobalObj`]).
    /// `token` indexes the embedder's obj-global table (parallel to
    /// resolution order); `lane` is the snapshot value's graded lane.
    /// Also the fallback for specialized resolutions used outside
    /// their recognized shapes (`range` as a value, `len` passed
    /// around) — the embedder's resolver reports the specialized
    /// variant and the analyzer downgrades through the
    /// `obj_global` probe.
    ObjGlobal { token: u32, lane: JitType },
    /// A name that did not resolve at analysis time (a genuine
    /// `NameError` at runtime) — the load disqualifies the frame.
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
    /// RFC 0073 WS1 — the site resolved against a class's
    /// *post-construction canonical shape* instead of a live probe
    /// (the receiver local had no value at compile time — it is
    /// (re)bound from a burned-in constructor call). `(class global
    /// name, field index in construction order)`: the embedder's
    /// guard snapshot resolves the class by name and burns
    /// `Indexed(field index)` with the class's `(rc_id,
    /// attr_version)` — exactly the fingerprint the site would have
    /// learned from a live instance one call later.
    pub ctor: Option<(String, u32)>,
    /// RFC 0073 WS1 — the *self-body* residue: the receiver's live
    /// value is a fresh instance mid-construction (an `__init__`
    /// compiled at entry), and this load follows same-body *new-key*
    /// stores of the name. `Some(k)` = the name is the `k`-th
    /// first-store in this body's store order on the same receiver
    /// local; the guard snapshot burns `Indexed(k)` from the live
    /// receiver's class, with new-key *store* eligibility standing in
    /// for the (necessarily failing) load probe. The runtime helper
    /// re-validates `(type_id, ver, key-at-index, lane)` per access,
    /// so a body entered with a non-empty instance dict deopts.
    pub self_ctor: Option<u32>,
}

/// RFC 0073 WS1 — where one canonical constructor field's value comes
/// from, in a class whose `__init__` is the pure store prologue
/// (`self.a = <param or const>` repeated, then `return None`). Lets a
/// caller type `inst.a` from its *own* constructor-call argument
/// lanes without a live instance to probe.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CtorFieldSrc {
    /// The field holds the caller's positional argument `i`
    /// (0-based, `self` excluded).
    Param(u32),
    /// The field holds a constant of this lane (`Obj` for `None`).
    Lane(JitType),
}

/// RFC 0073 WS3 — the burnable native `str` method set. Exact `str`'s
/// method table is immutable (builtin types reject attribute stores),
/// so a site whose receiver wears the pinned `Str` lane can burn the
/// method *statically* — no fingerprint, no per-call revalidation
/// beyond the receiver pin itself. The VM helper dispatches on this
/// discriminant to the same builtin bodies tier-1's
/// `CallNativeMethod` invokes, so argument validation, arity wording,
/// and raises are identical to the interpreter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum StrMethod {
    Upper,
    Lower,
    Casefold,
    Strip,
    Lstrip,
    Rstrip,
    Replace,
    Startswith,
    Endswith,
    Find,
    Rfind,
    Index,
    Rindex,
    Count,
    Split,
    Rsplit,
    Join,
    Title,
    Capitalize,
    Swapcase,
    Zfill,
    Removeprefix,
    Removesuffix,
    Isdigit,
    Isnumeric,
    Isdecimal,
    Isalpha,
    Isalnum,
    Isspace,
    Isupper,
    Islower,
    Istitle,
    Isascii,
    Isidentifier,
    Isprintable,
}

impl StrMethod {
    /// Every burnable method, in discriminant order (the helper's
    /// decode table).
    pub const ALL: [StrMethod; 35] = [
        StrMethod::Upper,
        StrMethod::Lower,
        StrMethod::Casefold,
        StrMethod::Strip,
        StrMethod::Lstrip,
        StrMethod::Rstrip,
        StrMethod::Replace,
        StrMethod::Startswith,
        StrMethod::Endswith,
        StrMethod::Find,
        StrMethod::Rfind,
        StrMethod::Index,
        StrMethod::Rindex,
        StrMethod::Count,
        StrMethod::Split,
        StrMethod::Rsplit,
        StrMethod::Join,
        StrMethod::Title,
        StrMethod::Capitalize,
        StrMethod::Swapcase,
        StrMethod::Zfill,
        StrMethod::Removeprefix,
        StrMethod::Removesuffix,
        StrMethod::Isdigit,
        StrMethod::Isnumeric,
        StrMethod::Isdecimal,
        StrMethod::Isalpha,
        StrMethod::Isalnum,
        StrMethod::Isspace,
        StrMethod::Isupper,
        StrMethod::Islower,
        StrMethod::Istitle,
        StrMethod::Isascii,
        StrMethod::Isidentifier,
        StrMethod::Isprintable,
    ];

    /// Decode a `u32` discriminant the compiled code burned in.
    #[must_use]
    pub fn from_raw(v: u32) -> Option<StrMethod> {
        Self::ALL.get(v as usize).copied()
    }

    /// Resolve a method name to its discriminant (the analyzer's
    /// admission check).
    #[must_use]
    pub fn from_name(name: &str) -> Option<StrMethod> {
        Some(match name {
            "upper" => StrMethod::Upper,
            "lower" => StrMethod::Lower,
            "casefold" => StrMethod::Casefold,
            "strip" => StrMethod::Strip,
            "lstrip" => StrMethod::Lstrip,
            "rstrip" => StrMethod::Rstrip,
            "replace" => StrMethod::Replace,
            "startswith" => StrMethod::Startswith,
            "endswith" => StrMethod::Endswith,
            "find" => StrMethod::Find,
            "rfind" => StrMethod::Rfind,
            "index" => StrMethod::Index,
            "rindex" => StrMethod::Rindex,
            "count" => StrMethod::Count,
            "split" => StrMethod::Split,
            "rsplit" => StrMethod::Rsplit,
            "join" => StrMethod::Join,
            "title" => StrMethod::Title,
            "capitalize" => StrMethod::Capitalize,
            "swapcase" => StrMethod::Swapcase,
            "zfill" => StrMethod::Zfill,
            "removeprefix" => StrMethod::Removeprefix,
            "removesuffix" => StrMethod::Removesuffix,
            "isdigit" => StrMethod::Isdigit,
            "isnumeric" => StrMethod::Isnumeric,
            "isdecimal" => StrMethod::Isdecimal,
            "isalpha" => StrMethod::Isalpha,
            "isalnum" => StrMethod::Isalnum,
            "isspace" => StrMethod::Isspace,
            "isupper" => StrMethod::Isupper,
            "islower" => StrMethod::Islower,
            "istitle" => StrMethod::Istitle,
            "isascii" => StrMethod::Isascii,
            "isidentifier" => StrMethod::Isidentifier,
            "isprintable" => StrMethod::Isprintable,
            _ => return None,
        })
    }

    /// The method name (for the deopt-span bound-method rebuild).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            StrMethod::Upper => "upper",
            StrMethod::Lower => "lower",
            StrMethod::Casefold => "casefold",
            StrMethod::Strip => "strip",
            StrMethod::Lstrip => "lstrip",
            StrMethod::Rstrip => "rstrip",
            StrMethod::Replace => "replace",
            StrMethod::Startswith => "startswith",
            StrMethod::Endswith => "endswith",
            StrMethod::Find => "find",
            StrMethod::Rfind => "rfind",
            StrMethod::Index => "index",
            StrMethod::Rindex => "rindex",
            StrMethod::Count => "count",
            StrMethod::Split => "split",
            StrMethod::Rsplit => "rsplit",
            StrMethod::Join => "join",
            StrMethod::Title => "title",
            StrMethod::Capitalize => "capitalize",
            StrMethod::Swapcase => "swapcase",
            StrMethod::Zfill => "zfill",
            StrMethod::Removeprefix => "removeprefix",
            StrMethod::Removesuffix => "removesuffix",
            StrMethod::Isdigit => "isdigit",
            StrMethod::Isnumeric => "isnumeric",
            StrMethod::Isdecimal => "isdecimal",
            StrMethod::Isalpha => "isalpha",
            StrMethod::Isalnum => "isalnum",
            StrMethod::Isspace => "isspace",
            StrMethod::Isupper => "isupper",
            StrMethod::Islower => "islower",
            StrMethod::Istitle => "istitle",
            StrMethod::Isascii => "isascii",
            StrMethod::Isidentifier => "isidentifier",
            StrMethod::Isprintable => "isprintable",
        }
    }

    /// The method's static return lane. `Str` results are validated
    /// per call (a `WStr`-producing `join` deopts); `split`/`rsplit`
    /// produce fresh lists of exact strings on the `ListObj` lane.
    #[must_use]
    pub fn ret(self) -> JitType {
        match self {
            StrMethod::Upper
            | StrMethod::Lower
            | StrMethod::Casefold
            | StrMethod::Strip
            | StrMethod::Lstrip
            | StrMethod::Rstrip
            | StrMethod::Replace
            | StrMethod::Join
            | StrMethod::Title
            | StrMethod::Capitalize
            | StrMethod::Swapcase
            | StrMethod::Zfill
            | StrMethod::Removeprefix
            | StrMethod::Removesuffix => JitType::Str,
            StrMethod::Find
            | StrMethod::Rfind
            | StrMethod::Index
            | StrMethod::Rindex
            | StrMethod::Count => JitType::Int,
            StrMethod::Split | StrMethod::Rsplit => JitType::ListObj,
            StrMethod::Startswith
            | StrMethod::Endswith
            | StrMethod::Isdigit
            | StrMethod::Isnumeric
            | StrMethod::Isdecimal
            | StrMethod::Isalpha
            | StrMethod::Isalnum
            | StrMethod::Isspace
            | StrMethod::Isupper
            | StrMethod::Islower
            | StrMethod::Istitle
            | StrMethod::Isascii
            | StrMethod::Isidentifier
            | StrMethod::Isprintable => JitType::Bool,
        }
    }
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
#[derive(Clone, Debug, PartialEq)]
pub struct OsrEntry {
    /// The bytecode pc of the block leader (the backward jump's target).
    pub pc: u32,
    /// The [`TBlock`] to enter.
    pub block: BlockId,
    /// RFC 0073 WS1 — local slots that MAY be read before being
    /// written on some native path from this entry (the complement of
    /// per-entry definite assignment). An *object-lane* local that is
    /// unbound in the entering activation is admissible iff its slot
    /// is **not** listed here: the embedder seeds it as a pinned
    /// `Unbound` (so a deopt writes back exactly the unbound state)
    /// and the definite-assignment guarantee means native code writes
    /// the slot before any read.
    pub unassigned_reads: Vec<u32>,
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
    /// RFC 0073 WS1 — the *interpreter-stack depth* at which the
    /// rebuilt iterator sits. Statement-level loops keep the historic
    /// bottom-of-stack position (their boundary stack is empty, so
    /// this is just the enclosing-loop nesting depth); a loop inside
    /// an inlined comprehension sits above the saved locals, the
    /// accumulator, and whatever expression stack surrounds the
    /// comprehension. Filled during emission.
    pub interp_depth: u32,
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
    /// RFC 0073 WS1 — interpreter-stack depth of the rebuilt
    /// iterator (see [`RangeLoopMeta::interp_depth`]).
    pub interp_depth: u32,
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
    /// RFC 0073 WS1 — interpreter-stack depth of the rebuilt
    /// iterator (see [`RangeLoopMeta::interp_depth`]).
    pub interp_depth: u32,
}

/// RFC 0073 WS1 — deopt-reconstruction metadata for one inlined
/// comprehension's *saved target local*: the `LOAD_FAST_AND_CLEAR`
/// prologue parked the local's prior value on the interpreter stack
/// beneath the accumulator, and the analyzer admitted the shape only
/// after proving that value is `Unbound` (the target slot is stored
/// nowhere outside recognized comprehension loops). At any deopt pc in
/// `[live_from, live_to)` the embedder re-inserts `Object::Unbound` at
/// `interp_depth`, and the interpreter's own epilogue (or its
/// exception handler) performs the restore exactly as if the loop had
/// run interpreted from the start.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompSavedMeta {
    /// The `LOAD_FAST_AND_CLEAR` pc (span start).
    pub live_from: u32,
    /// The epilogue's restoring `STORE_FAST` pc (span end, exclusive).
    pub live_to: u32,
    /// Interpreter-stack depth of the parked `Unbound`.
    pub interp_depth: u32,
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
    /// RFC 0073 WS1 — inlined-comprehension saved-local spans: at any
    /// deopt pc inside a span, `Object::Unbound` re-inserts at the
    /// recorded interpreter depth (the parked prior value of the
    /// comprehension target, proven unbound at admission).
    pub comp_saved: Vec<CompSavedMeta>,
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
    /// RFC 0073 WS3 — burned-in native `str`-method sites, indexed by
    /// [`TOp::CallStrMethod`]'s `site` (deduplicated by method, in
    /// first-use order). Static — no embedder table.
    pub str_method_sites: Vec<StrMethod>,
    /// RFC 0073 WS3 — erased `str`-method receivers (the
    /// [`MethodSpanMeta`] discipline; `token` indexes
    /// [`Self::str_method_sites`], and the bound method rebuilds by
    /// name on the pinned `str`).
    pub str_method_spans: Vec<MethodSpanMeta>,
    /// RFC 0069 WS2 — burned-in math-intrinsic guards, one per
    /// distinct `(name, attr)` pair, in first-use order.
    pub math_guards: Vec<MathGuardMeta>,
    /// RFC 0069 WS2 — erased `math` intrinsic callables riding the
    /// interpreter stack between their method load and `CALL`. Same
    /// reconstruction contract as [`Self::len_spans`] (`token` indexes
    /// [`Self::math_guards`], `live_to` is the pc after the `CALL`).
    pub math_spans: Vec<CalleeSpanMeta>,
    /// RFC 0074 WS2 — the self-or-null `Unbound` markers of
    /// opaque-call sites: between a `PUSH_NULL` (the plain-call
    /// shape, marker *above* the native callee value) or a
    /// method-form [`TOp::DynAttrGet`] (marker above the loaded bound
    /// method) and the consuming `CALL`, the interpreter's stack holds
    /// `Object::Unbound` at `interp_depth` while the native stack
    /// holds nothing. A deopt strictly inside `(live_from, live_to)`
    /// re-inserts it (the `token` field is unused).
    pub null_spans: Vec<CalleeSpanMeta>,
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
                | TOp::CallPyKw { .. }
                | TOp::CallMethod { .. }
                | TOp::MathIntrinsic(_)
                | TOp::FloatArith(ArithKind::FloorDiv | ArithKind::Mod)
                | TOp::ListGet { .. }
                | TOp::ListSet
                | TOp::ListLen
                | TOp::ListAppend
                | TOp::ListAppendKeep
                | TOp::AttrGet { .. }
                | TOp::AttrSet { .. }
                | TOp::GuardNotNone
                | TOp::StrEq { .. }
                | TOp::StrLen
                | TOp::BytesLen
                | TOp::BytesGetItem
                | TOp::DictGet { .. }
                | TOp::DictSet { .. }
                | TOp::DictContains { .. }
                | TOp::DictLen
                | TOp::BuildMap { .. }
                | TOp::PushConstStr { .. }
                | TOp::StrConcat
                | TOp::StrGetItem
                | TOp::BuildString { .. }
                | TOp::CallStrMethod { .. }
                | TOp::IterCapture { .. }
                | TOp::DictIterNew { .. }
                | TOp::BuildList { .. }
                | TOp::BuildTuple { .. }
                | TOp::ListRepeat
                | TOp::ListSlice { .. }
                | TOp::PushGlobalObj { .. }
                | TOp::CallDyn { .. }
                | TOp::DynAttrGet { .. }
                | TOp::DynAttrSet { .. }
                | TOp::StrMod
                | TOp::StrSlice { .. }
        )
    }
}
