//! Bytecode instruction set for the WeavePy VM.
//!
//! Opcode names track CPython 3.13's `Lib/opcode.py` where applicable
//! so `dis` listings look familiar. Each instruction is a fixed
//! `{ op, arg }` pair — the slice favours simplicity of emission and
//! dispatch over CPython's 16-bit-packed encoding, which is RFC 0007
//! territory.
//!
//! # Compatibility level
//!
//! - **Tracks CPython** for opcode names and rough semantics.
//! - **Experimental** for the binary encoding; we explicitly do not
//!   promise wire compatibility with CPython's `.pyc` format.

/// Sub-operation tag for [`OpCode::BinaryOp`]. Mirrors CPython 3.11+'s
/// `_NB_*` enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum BinOpKind {
    Add = 0,
    Sub = 1,
    Mult = 2,
    Div = 3,
    FloorDiv = 4,
    Mod = 5,
    Pow = 6,
    LShift = 7,
    RShift = 8,
    BitOr = 9,
    BitXor = 10,
    BitAnd = 11,
    MatMult = 12,
}

/// Comparison operator tag for [`OpCode::CompareOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CompareKind {
    Lt = 0,
    LtE = 1,
    Eq = 2,
    NotEq = 3,
    Gt = 4,
    GtE = 5,
}

impl CompareKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lt => "<",
            Self::LtE => "<=",
            Self::Eq => "==",
            Self::NotEq => "!=",
            Self::Gt => ">",
            Self::GtE => ">=",
        }
    }

    /// The opcode argument that encodes this comparison.
    pub fn as_arg(self) -> u32 {
        self as u32
    }

    /// Recover a [`CompareKind`] from its opcode argument.
    pub fn from_arg(arg: u32) -> Option<Self> {
        Some(match arg {
            0 => Self::Lt,
            1 => Self::LtE,
            2 => Self::Eq,
            3 => Self::NotEq,
            4 => Self::Gt,
            5 => Self::GtE,
            _ => return None,
        })
    }
}

impl BinOpKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mult => "*",
            Self::Div => "/",
            Self::FloorDiv => "//",
            Self::Mod => "%",
            Self::Pow => "**",
            Self::LShift => "<<",
            Self::RShift => ">>",
            Self::BitOr => "|",
            Self::BitXor => "^",
            Self::BitAnd => "&",
            Self::MatMult => "@",
        }
    }

    /// The opcode argument that encodes this binary operator.
    pub fn as_arg(self) -> u32 {
        self as u32
    }

    /// Recover a [`BinOpKind`] from its opcode argument.
    pub fn from_arg(arg: u32) -> Option<Self> {
        Some(match arg {
            0 => Self::Add,
            1 => Self::Sub,
            2 => Self::Mult,
            3 => Self::Div,
            4 => Self::FloorDiv,
            5 => Self::Mod,
            6 => Self::Pow,
            7 => Self::LShift,
            8 => Self::RShift,
            9 => Self::BitOr,
            10 => Self::BitXor,
            11 => Self::BitAnd,
            12 => Self::MatMult,
            _ => return None,
        })
    }
}

/// Unary op tag for [`OpCode::UnaryOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum UnaryKind {
    Pos = 0,
    Neg = 1,
    Not = 2,
    Invert = 3,
}

impl UnaryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pos => "+",
            Self::Neg => "-",
            Self::Not => "not",
            Self::Invert => "~",
        }
    }

    /// The opcode argument that encodes this unary operator.
    pub fn as_arg(self) -> u32 {
        self as u32
    }

    /// Recover a [`UnaryKind`] from its opcode argument.
    pub fn from_arg(arg: u32) -> Option<Self> {
        Some(match arg {
            0 => Self::Pos,
            1 => Self::Neg,
            2 => Self::Not,
            3 => Self::Invert,
            _ => return None,
        })
    }
}

/// Opcodes emitted by the WeavePy compiler. The argument's meaning
/// depends on the opcode — see comments per variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum OpCode {
    /// No-op. Used to reserve a slot during compilation.
    Nop,
    /// Marker for the start of a frame. Matches CPython's `RESUME 0`.
    Resume,

    // Constants / names / variables
    /// Push `co_consts[arg]`.
    LoadConst,
    /// Push the value bound to `co_names[arg]` from globals/builtins.
    LoadName,
    /// Push the value bound to `co_names[arg]` from globals.
    LoadGlobal,
    /// Push `co_varnames[arg]` from the current frame's locals.
    LoadFast,
    /// Pop and store TOS into `co_varnames[arg]`.
    StoreFast,
    /// Pop and store TOS into globals[co_names[arg]].
    StoreGlobal,
    /// Pop and store TOS into locals/globals[co_names[arg]].
    StoreName,
    /// Delete locals[co_varnames[arg]].
    DeleteFast,
    /// Delete globals[co_names[arg]].
    DeleteGlobal,
    /// Delete locals/globals[co_names[arg]].
    DeleteName,
    /// Push the cell at `co_freevars[arg]` contents (closure value).
    LoadDeref,
    /// Store TOS into the cell at `co_freevars[arg]`.
    StoreDeref,
    /// Push a new empty cell into the cells array for this frame
    /// (cellvar at offset `arg`).
    MakeCell,
    /// Push the cell object itself (not its content) — for building closures.
    LoadClosure,

    // Attributes / subscripts
    /// `value = stack[-1]; push value.<co_names[arg]>`
    LoadAttr,
    /// `value = stack[-1]; tos = stack[-2]; tos.<co_names[arg]> = value`
    StoreAttr,
    /// Delete attribute.
    DeleteAttr,
    /// `i = pop; obj = pop; push obj[i]`
    BinarySubscr,
    /// `i = pop; obj = pop; v = pop; obj[i] = v`
    StoreSubscr,
    /// Delete subscript.
    DeleteSubscr,

    // Arithmetic / logical
    /// Pop two values, apply [`BinOpKind`] from `arg`, push result.
    BinaryOp,
    /// Pop one value, apply [`UnaryKind`] from `arg`, push result.
    UnaryOp,
    /// Compare top two values using [`CompareKind`].
    CompareOp,
    /// `is` (or `is not` if arg == 1) — pops two, pushes bool.
    IsOp,
    /// `in` (or `not in` if arg == 1) — pops two, pushes bool.
    ContainsOp,

    // Stack management
    /// Pop and discard the top of stack.
    PopTop,
    /// Push a copy of the top of stack.
    CopyTop,
    /// Swap two top stack values.
    Swap,

    // Calls
    /// Call a callable with `arg` positional arguments.
    /// Stack layout (top-down): `arg_n, arg_(n-1), ..., arg_1, callable`.
    Call,
    /// Call with keyword arguments. `arg` = positional arg count;
    /// stack also carries kw arg names (tuple) and values.
    CallKw,
    /// Call with packed args. `arg = 0` means the stack carries only
    /// `(callable, args_tuple)`; `arg = 1` means `(callable,
    /// args_tuple, kwargs_dict)`. Used for `*args` and `**kwargs`
    /// splats that can't be lowered to a static arg count.
    CallEx,
    /// Return TOS from the current frame.
    ReturnValue,

    // Control flow
    /// Pop TOS; if falsy, jump by `arg` instructions (signed).
    PopJumpIfFalse,
    /// Pop TOS; if truthy, jump by `arg` instructions (signed).
    PopJumpIfTrue,
    /// Unconditional forward jump.
    JumpForward,
    /// Unconditional backward jump (`arg` is a positive distance to subtract).
    JumpBackward,

    // Iterators
    /// Pop iterable, push its iterator.
    GetIter,
    /// Advance the iterator at TOS; on StopIteration, jump by `arg`.
    ForIter,
    /// Pop iterator (matches CPython's `END_FOR`).
    EndFor,

    // Containers
    BuildList,
    BuildTuple,
    BuildSet,
    BuildMap,
    BuildString,
    ListAppend,
    SetAdd,
    MapAdd,
    /// Unpack iterable at TOS into `arg` values, push them in
    /// reverse order (so the first element ends up at the bottom).
    UnpackSequence,
    /// Unpack iterable at TOS into `before + 1 + after` values, with
    /// a starred middle that captures the remainder as a `list`.
    /// `arg` encodes `(before << 8) | after`. The stack on exit is
    /// `[..., after_n-1, ..., after_0, list_of_middle, before_n-1, ..., before_0]`
    /// — i.e. all extracted values pushed top-down so a sequence of
    /// `STORE_FAST` emitted in source order pops them in the right
    /// order. Mirrors CPython's `UNPACK_EX`.
    UnpackEx,
    /// `dict.update(other)` as a pure stack op. Stack on entry:
    /// `[..., dict, other]`. Pops `other`, applies it to `dict`
    /// (which is left at TOS for further updates). Used for `{**d}`
    /// dict-literal spreads.
    DictUpdate,

    // Functions / closures
    /// Build a function object from the code object on TOS.
    /// `arg` is a bitmask: 0x01 = defaults tuple, 0x02 = kw_defaults
    /// dict, 0x04 = annotations dict, 0x08 = closure tuple.
    MakeFunction,
    /// Slice — build a Slice(low, high, step) and push it.
    BuildSlice,

    // Classes
    /// Push the magic `__build_class__` builtin onto the stack.
    LoadBuildClass,
    /// Class-scope deref: like `LOAD_DEREF` but first tries the active
    /// class namespace (for forward references inside a class body).
    LoadClassderef,

    // Exceptions
    /// Pop TOS as the exception to raise. `arg` is the raise form:
    /// 0 = re-raise current; 1 = `raise X`; 2 = `raise X from Y` (Y at TOS, X below).
    RaiseVarargs,
    /// Match the exception at `stack[-2]` against the type at TOS. Pops
    /// the type, peeks the exception, pushes a bool.
    CheckExcMatch,
    /// PEP 654 / RFC 0018: split an exception group. Stack on entry:
    /// `[..., exc, type]`. Pops both; pushes `[..., rest, matched]`
    /// where `matched` is either `None` (nothing in the group matches
    /// `type`) or a new group of the same class containing the
    /// matches, and `rest` is `None` (every member matched) or a
    /// new group with the unmatched members. If `exc` is not a
    /// group, it is treated as a singleton — `matched` is `exc` and
    /// `rest` is `None`, or vice versa.
    CheckEGMatch,
    /// Push the exception currently being handled onto the
    /// `exception_handlers` stack — TOS is the exception value.
    PushExcInfo,
    /// Pop the top of the `exception_handlers` stack.
    PopExcept,
    /// Pop and re-raise the top of the `exception_handlers` stack.
    Reraise,

    // Context managers
    /// `with` enter: pop cm, push `__exit__` bound to cm, then push
    /// the result of `__enter__()`.
    BeforeWith,
    /// `with` exception path. Stack on entry: `[__exit__, exc]`.
    /// Calls `__exit__(type(exc), exc, None)` and leaves `[exc, result]`
    /// on the stack so the compiler can branch on the result.
    WithExceptStart,

    // Imports (RFC 0012)
    /// Pop `fromlist` and `level` (top-down), look up the dotted
    /// module name `co_names[arg]`, and push the resolved module:
    /// the top-level package when `fromlist` is empty, the leaf
    /// otherwise. Mirrors CPython's `IMPORT_NAME`.
    ImportName,
    /// Peek the module on TOS and push its attribute
    /// `co_names[arg]`. Raises `ImportError` if the attribute is
    /// missing. Mirrors CPython's `IMPORT_FROM`.
    ImportFrom,
    /// Pop the module on TOS and bind every public name into the
    /// current namespace (locals for function scope, globals for
    /// module scope). Honours `__all__` if defined. Mirrors
    /// CPython's `IMPORT_STAR`.
    ImportStar,

    // f-strings (RFC 0005)
    /// Format the value at TOS, optionally with a format spec also
    /// on the stack. `arg & 0x03` is the conversion (0 = none,
    /// 1 = `!s`, 2 = `!r`, 3 = `!a`); `arg & 0x04` indicates a
    /// spec is on top (popped before the value).
    FormatValue,

    // Generators (RFC 0006)
    /// Pop the value at TOS; suspend this frame, returning the
    /// value to the caller's `send()` / `__next__()`. On resume,
    /// the sent value (or `None` for `next`) is pushed at TOS.
    YieldValue,
    /// Pop an iterable, push its iterator. Unlike `GET_ITER`, this
    /// returns the value unchanged when it's already a generator.
    GetYieldFromIter,
    /// At the top of a generator code object, suspend the frame
    /// and push a `Generator` object to the caller. Subsequent
    /// `__next__`/`send` calls resume the frame from here.
    ReturnGenerator,
    /// `SEND` runs sub-iter delegation for `yield from`. Stack on
    /// entry: `[..., iter, value]`. The opcode calls
    /// `iter.send(value)` (or `iter.__next__()` for `value is None`).
    /// On `StopIteration(v)` it pops the iterator, pushes `v`, and
    /// jumps by `arg`. Otherwise it leaves `[iter, yielded]` and
    /// falls through.
    Send,
    /// `END_SEND` (RFC 0016). Stack on entry: `[..., iter, value]`.
    /// Pops the iterator (`stack[-2]`) and leaves the value on TOS —
    /// the result of `yield from` / `await` once the sub-iterator
    /// completes.
    EndSend,

    // Async (RFC 0016)
    /// Replace TOS with `TOS.__await__()` (an iterator). The `arg`
    /// indicates the surrounding context: 0 = ordinary `await`,
    /// 1 = `async for` (used by the runtime for error messages),
    /// 2 = `async with`.
    GetAwaitable,
    /// `aiter = TOS; TOS = aiter.__aiter__()` — the async-iter
    /// equivalent of `GET_ITER`.
    GetAiter,
    /// Peek the async iterator at TOS (don't pop), push
    /// `aiter.__anext__()` (an awaitable). Used in the `async for`
    /// loop preamble before awaiting.
    GetAnext,
    /// `async for` cleanup: caught `StopAsyncIteration`; pop the
    /// exception and the underlying iterator. Stack on entry:
    /// `[..., aiter, exc]`. Stack on exit: `[...]`.
    EndAsyncFor,
    /// `async with` enter: pop the context manager, push its
    /// `__aexit__` method (saved for the exit path), then push
    /// `cm.__aenter__()` (the awaitable that yields the bound value).
    BeforeAsyncWith,

    // Pattern matching (RFC 0009)
    /// Peek TOS, push True if it's a sequence (list/tuple/range).
    MatchSequence,
    /// Peek TOS, push True if it's a mapping (dict).
    MatchMapping,
    /// `arg` = positional count. Stack on entry (top-down):
    /// names_tuple, cls, subject. Pops all three; pushes a tuple
    /// of extracted values on success, or `None` on failure.
    MatchClass,
    /// Stack on entry: keys_tuple (TOS), subject (below). Pops
    /// keys_tuple, peeks subject; pushes a tuple of looked-up
    /// values, or `None` if any key is missing.
    MatchKeys,
    /// Peek TOS, push `len(TOS)` as an int.
    GetLen,

    /// Echo TOS through `sys.displayhook` (CPython `PRINT_EXPR`).
    /// Emitted only for top-level expression statements compiled in
    /// interactive ("single") mode — the REPL (`code`/`codeop`) and
    /// `doctest`. In "exec" mode an expression statement uses
    /// `PopTop` instead.
    PrintExpr,
}

impl OpCode {
    pub fn name(self) -> &'static str {
        match self {
            OpCode::Nop => "NOP",
            OpCode::Resume => "RESUME",
            OpCode::LoadConst => "LOAD_CONST",
            OpCode::LoadName => "LOAD_NAME",
            OpCode::LoadGlobal => "LOAD_GLOBAL",
            OpCode::LoadFast => "LOAD_FAST",
            OpCode::StoreFast => "STORE_FAST",
            OpCode::StoreGlobal => "STORE_GLOBAL",
            OpCode::StoreName => "STORE_NAME",
            OpCode::DeleteFast => "DELETE_FAST",
            OpCode::DeleteGlobal => "DELETE_GLOBAL",
            OpCode::DeleteName => "DELETE_NAME",
            OpCode::LoadDeref => "LOAD_DEREF",
            OpCode::StoreDeref => "STORE_DEREF",
            OpCode::MakeCell => "MAKE_CELL",
            OpCode::LoadClosure => "LOAD_CLOSURE",
            OpCode::LoadAttr => "LOAD_ATTR",
            OpCode::StoreAttr => "STORE_ATTR",
            OpCode::DeleteAttr => "DELETE_ATTR",
            OpCode::BinarySubscr => "BINARY_SUBSCR",
            OpCode::StoreSubscr => "STORE_SUBSCR",
            OpCode::DeleteSubscr => "DELETE_SUBSCR",
            OpCode::BinaryOp => "BINARY_OP",
            OpCode::UnaryOp => "UNARY_OP",
            OpCode::CompareOp => "COMPARE_OP",
            OpCode::IsOp => "IS_OP",
            OpCode::ContainsOp => "CONTAINS_OP",
            OpCode::PopTop => "POP_TOP",
            OpCode::CopyTop => "COPY",
            OpCode::Swap => "SWAP",
            OpCode::Call => "CALL",
            OpCode::CallKw => "CALL_KW",
            OpCode::CallEx => "CALL_FUNCTION_EX",
            OpCode::ReturnValue => "RETURN_VALUE",
            OpCode::PopJumpIfFalse => "POP_JUMP_IF_FALSE",
            OpCode::PopJumpIfTrue => "POP_JUMP_IF_TRUE",
            OpCode::JumpForward => "JUMP_FORWARD",
            OpCode::JumpBackward => "JUMP_BACKWARD",
            OpCode::GetIter => "GET_ITER",
            OpCode::ForIter => "FOR_ITER",
            OpCode::EndFor => "END_FOR",
            OpCode::BuildList => "BUILD_LIST",
            OpCode::BuildTuple => "BUILD_TUPLE",
            OpCode::BuildSet => "BUILD_SET",
            OpCode::BuildMap => "BUILD_MAP",
            OpCode::BuildString => "BUILD_STRING",
            OpCode::ListAppend => "LIST_APPEND",
            OpCode::SetAdd => "SET_ADD",
            OpCode::MapAdd => "MAP_ADD",
            OpCode::UnpackSequence => "UNPACK_SEQUENCE",
            OpCode::UnpackEx => "UNPACK_EX",
            OpCode::DictUpdate => "DICT_UPDATE",
            OpCode::MakeFunction => "MAKE_FUNCTION",
            OpCode::BuildSlice => "BUILD_SLICE",
            OpCode::LoadBuildClass => "LOAD_BUILD_CLASS",
            OpCode::LoadClassderef => "LOAD_CLASSDEREF",
            OpCode::RaiseVarargs => "RAISE_VARARGS",
            OpCode::CheckExcMatch => "CHECK_EXC_MATCH",
            OpCode::CheckEGMatch => "CHECK_EG_MATCH",
            OpCode::PushExcInfo => "PUSH_EXC_INFO",
            OpCode::PopExcept => "POP_EXCEPT",
            OpCode::Reraise => "RERAISE",
            OpCode::BeforeWith => "BEFORE_WITH",
            OpCode::WithExceptStart => "WITH_EXCEPT_START",
            OpCode::ImportName => "IMPORT_NAME",
            OpCode::ImportFrom => "IMPORT_FROM",
            OpCode::ImportStar => "IMPORT_STAR",
            OpCode::FormatValue => "FORMAT_VALUE",
            OpCode::YieldValue => "YIELD_VALUE",
            OpCode::GetYieldFromIter => "GET_YIELD_FROM_ITER",
            OpCode::ReturnGenerator => "RETURN_GENERATOR",
            OpCode::Send => "SEND",
            OpCode::EndSend => "END_SEND",
            OpCode::GetAwaitable => "GET_AWAITABLE",
            OpCode::GetAiter => "GET_AITER",
            OpCode::GetAnext => "GET_ANEXT",
            OpCode::EndAsyncFor => "END_ASYNC_FOR",
            OpCode::BeforeAsyncWith => "BEFORE_ASYNC_WITH",
            OpCode::MatchSequence => "MATCH_SEQUENCE",
            OpCode::MatchMapping => "MATCH_MAPPING",
            OpCode::MatchClass => "MATCH_CLASS",
            OpCode::MatchKeys => "MATCH_KEYS",
            OpCode::GetLen => "GET_LEN",
            OpCode::PrintExpr => "PRINT_EXPR",
        }
    }
}

/// One emitted instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instruction {
    pub op: OpCode,
    pub arg: u32,
}

impl Instruction {
    #[inline]
    pub const fn new(op: OpCode, arg: u32) -> Self {
        Self { op, arg }
    }
}

// ---------- inline caches (RFC 0021) ----------

/// Per-instruction inline cache slot. The dispatcher consults this
/// before entering the generic handler for a hot opcode and, on
/// recognised states, takes a type-specific fast path that skips the
/// dunder-method search and the dict-keyed lookups.
///
/// The state machine is:
///
/// - `Empty` — the next dispatch will try to specialize.
/// - one of the type-specific variants below — the next dispatch
///   guards on the cached fingerprint and either fast-paths or
///   transitions to `Cooldown`.
/// - `Cooldown(n)` — the previous specialization attempt deopted;
///   run the generic handler `n` more times before retrying.
///
/// Variants are 24 bytes or smaller; the enum is `Copy` so it fits
/// in a `Cell<…>`.
///
/// `type_id` / `module_id` / `globals_id` / `builtins_id` are all
/// `Rc::as_ptr(&value) as u64` — a cheap monotonic identity that
/// changes when the underlying allocation does. Address reuse after
/// drop is handled by the deopt path on the next guard miss.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum InlineCache {
    /// Initial / fully cold state. Generic handler will attempt to
    /// install a specialized cache after running.
    #[default]
    Empty,
    /// Specialization attempt declined or deopted. Skip the
    /// fast-path machinery for `n` more dispatches.
    Cooldown(u8),

    // BINARY_OP family — both operands int / float / str.
    BinOpAddInt,
    BinOpSubInt,
    BinOpMulInt,
    BinOpAddFloat,
    BinOpSubFloat,
    BinOpMulFloat,
    BinOpAddStr,

    // COMPARE_OP family — both operands int / float / str.
    CompareOpInt,
    CompareOpFloat,
    CompareOpStr,

    // LOAD_ATTR family — fingerprint + dict slot index.
    LoadAttrInstance {
        type_id: u64,
        key_idx: u32,
    },
    LoadAttrModule {
        module_id: u64,
        key_idx: u32,
    },
    LoadAttrSlot {
        type_id: u64,
        slot_idx: u32,
    },
    LoadAttrType {
        type_id: u64,
        key_idx: u32,
    },

    // LOAD_GLOBAL family — globals/builtins dict version + key idx.
    LoadGlobalModule {
        globals_id: u64,
        key_idx: u32,
    },
    LoadGlobalBuiltin {
        builtins_id: u64,
        key_idx: u32,
    },

    // STORE_ATTR family — fingerprint + dict slot index.
    StoreAttrInstance {
        type_id: u64,
        key_idx: u32,
    },
    StoreAttrSlot {
        type_id: u64,
        slot_idx: u32,
    },

    // FOR_ITER family.
    ForIterList,
    ForIterTuple,
    ForIterRange,

    // UNPACK_SEQUENCE family.
    UnpackSequenceTuple,
    UnpackSequenceList,
    UnpackSequenceTwoTuple,

    // CALL family (RFC 0032). `func_id` is the `Rc::as_ptr` fingerprint
    // of the called `PyFunction`; `argc` is the (fixed) call-site arity.
    /// Plain Python function: exact positional arity, no keywords, no
    /// `*args`/`**kwargs`/kw-only/defaults needed, and no cells or
    /// closure — so the frame's locals are just the arguments padded
    /// with `None`, skipping the whole argument-binding dance.
    CallPyExactNoFree {
        func_id: u64,
        argc: u32,
    },
    /// Plain Python function with the same exact-arity guarantee but a
    /// non-trivial cell/closure layout — still skips argument binding,
    /// but builds the frame (and its cells) through `make_frame`.
    CallPyExact {
        func_id: u64,
        argc: u32,
    },
}

/// Number of generic dispatches a deopted cache must serve before it
/// re-attempts specialization. Damps thrashing on polymorphic call
/// sites.
pub const COOLDOWN: u8 = 64;

/// One [`InlineCache`] slot. RFC 0025: needs to be `Send + Sync`
/// because the parent `CodeObject` is shared across OS threads via
/// `Arc<CodeObject>`. We use raw [`UnsafeCell`] and assert
/// `unsafe impl Send + Sync` with a SAFETY note tied to the GIL.
///
/// Why not `parking_lot::Mutex`? Because the table is read on every
/// single opcode (it's the dispatch hot path); even a 5ns mutex
/// would add measurable per-instruction overhead. CPython does the
/// same thing — its inline caches are bare arrays accessed under
/// the GIL.
///
/// Why not an atomic? Because the largest [`InlineCache`] variant
/// (`LoadAttrInstance { type_id: u64, key_idx: u32 }`) is ~24 bytes
/// and no portable atomic is that wide.
#[derive(Default)]
pub struct CacheSlot {
    inner: std::cell::UnsafeCell<InlineCache>,
}

// SAFETY: `CacheSlot::get` / `set` are only called by the dispatch
// loop while the GIL is held (see `weavepy-vm::gil`). The GIL
// serialises bytecode execution across threads, so concurrent reads
// or writes to the same slot are impossible. Treating the cell as
// `Send + Sync` is therefore sound — the `unsafe impl`s below
// document that invariant at the type level.
unsafe impl Send for CacheSlot {}
unsafe impl Sync for CacheSlot {}

impl CacheSlot {
    /// Build a slot holding `value`.
    pub const fn new(value: InlineCache) -> Self {
        Self {
            inner: std::cell::UnsafeCell::new(value),
        }
    }

    /// Read the slot. SAFETY relies on the GIL invariant above.
    #[inline]
    pub fn get(&self) -> InlineCache {
        // SAFETY: `&self` plus the GIL invariant guarantees no
        // concurrent write or `&mut InlineCache` exists.
        unsafe { *self.inner.get() }
    }

    /// Overwrite the slot. SAFETY relies on the GIL invariant above.
    #[inline]
    pub fn set(&self, value: InlineCache) {
        // SAFETY: the GIL guarantees no concurrent reader or writer
        // exists. We materialise an exclusive `&mut InlineCache`
        // only for the duration of the assignment.
        unsafe { *self.inner.get() = value };
    }
}

impl std::fmt::Debug for CacheSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("CacheSlot").field(&self.get()).finish()
    }
}

impl Clone for CacheSlot {
    fn clone(&self) -> Self {
        Self::new(self.get())
    }
}

impl PartialEq for CacheSlot {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
}

/// Parallel side-table: one [`InlineCache`] per [`Instruction`].
///
/// Lazily-initialised — the compiler emits an empty `CacheTable` and
/// the VM extends it on first dispatch into a code object. Slots
/// are interior-mutable so the dispatcher can warm them through a
/// shared `&CodeObject`. The slot type is `Send + Sync` (under the
/// GIL invariant) so `Arc<CodeObject>` can cross thread boundaries
/// (RFC 0025).
#[derive(Debug, Default)]
pub struct CacheTable {
    pub slots: Vec<CacheSlot>,
}

impl CacheTable {
    /// Allocate `n` empty cache slots.
    pub fn with_len(n: usize) -> Self {
        Self {
            slots: (0..n).map(|_| CacheSlot::new(InlineCache::Empty)).collect(),
        }
    }

    /// Read the cache for instruction `pc`. Out-of-range indices
    /// silently return `Empty` so the dispatcher doesn't have to
    /// branch on the table length on every step.
    #[inline]
    pub fn get(&self, pc: u32) -> InlineCache {
        self.slots
            .get(pc as usize)
            .map(CacheSlot::get)
            .unwrap_or(InlineCache::Empty)
    }

    /// Set the cache for instruction `pc`. No-op when `pc` is out of
    /// range (matches `get`'s defensive shape).
    #[inline]
    pub fn set(&self, pc: u32, value: InlineCache) {
        if let Some(slot) = self.slots.get(pc as usize) {
            slot.set(value);
        }
    }

    /// Clear every slot back to `Empty`. Used after an opcode
    /// rewrite or when the user calls `gc.collect()` and we want to
    /// discard stale type fingerprints.
    pub fn clear(&self) {
        for slot in &self.slots {
            slot.set(InlineCache::Empty);
        }
    }

    /// Resize the table to match a new instruction count. Existing
    /// slots are preserved up to the new length; newly-added slots
    /// start `Empty`.
    pub fn resize(&mut self, n: usize) {
        if self.slots.len() < n {
            self.slots
                .resize_with(n, || CacheSlot::new(InlineCache::Empty));
        } else {
            self.slots.truncate(n);
        }
    }
}

impl Clone for CacheTable {
    fn clone(&self) -> Self {
        Self {
            slots: self.slots.clone(),
        }
    }
}

impl PartialEq for CacheTable {
    /// Cache state isn't part of code-object identity. Two code
    /// objects with the same bytecode are equal regardless of how
    /// their caches have warmed up. This keeps `CodeObject: PartialEq`
    /// derivable and stops `marshal` round-trips from spuriously
    /// disagreeing on cache state that's intentionally not serialized.
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    #[test]
    fn cache_table_round_trip() {
        let t = CacheTable::with_len(4);
        assert_eq!(t.get(0), InlineCache::Empty);
        t.set(2, InlineCache::BinOpAddInt);
        assert_eq!(t.get(2), InlineCache::BinOpAddInt);
        // Out-of-range reads are defensive.
        assert_eq!(t.get(99), InlineCache::Empty);
    }

    #[test]
    fn cache_table_clone_copies_state() {
        let t = CacheTable::with_len(2);
        t.set(0, InlineCache::CompareOpInt);
        let u = t.clone();
        assert_eq!(u.get(0), InlineCache::CompareOpInt);
        // Subsequent mutations to `t` don't bleed into `u`.
        t.set(0, InlineCache::Empty);
        assert_eq!(u.get(0), InlineCache::CompareOpInt);
    }

    #[test]
    fn cache_table_partial_eq_ignores_state() {
        let a = CacheTable::with_len(3);
        let b = CacheTable::with_len(3);
        a.set(1, InlineCache::BinOpMulFloat);
        // PartialEq is intentionally insensitive to specialization
        // state.
        assert_eq!(a, b);
    }
}
