//! Bytecode instruction set for the WeavePy VM.
//!
//! Opcode names track CPython 3.14's `Lib/opcode.py` where applicable
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

/// Flag bit OR-ed into the [`OpCode::BinaryOp`] argument to mark an
/// *augmented* assignment (`a += b`). The low byte still encodes the
/// [`BinOpKind`]; the VM strips this bit to recover the operator and,
/// when set, first tries the in-place dunder (`__iadd__`, …) before the
/// regular binary fallback. Kept above `0xFF` so `arg as u8` recovers
/// the operator kind unchanged.
pub const BINARY_OP_INPLACE_FLAG: u32 = 0x100;

/// [`OpCode::CompareOp`] argument bit: the result is converted to a
/// `bool` (CPython's `COMPARE_OP` oparg bit 4, set when the flowgraph
/// fuses a following `TO_BOOL` into the compare). The low nibble is
/// the [`CompareKind`].
pub const COMPARE_OP_TO_BOOL_FLAG: u32 = 0x10;

/// [`OpCode::LoadSpecial`] operands, in CPython 3.14's
/// `_Py_SpecialMethods` order (`opcode._special_method_names`).
pub const SPECIAL_ENTER: u32 = 0;
pub const SPECIAL_EXIT: u32 = 1;
pub const SPECIAL_AENTER: u32 = 2;
pub const SPECIAL_AEXIT: u32 = 3;

/// [`OpCode::LoadCommonConstant`] operands, in CPython 3.14's
/// `_Py_CommonConstants` order (`opcode._common_constants`).
pub const COMMON_CONSTANT_ASSERTION_ERROR: u32 = 0;
pub const COMMON_CONSTANT_NOT_IMPLEMENTED_ERROR: u32 = 1;
pub const COMMON_CONSTANT_TUPLE: u32 = 2;
pub const COMMON_CONSTANT_ALL: u32 = 3;
pub const COMMON_CONSTANT_ANY: u32 = 4;

/// The PEP 695/696 intrinsic ids [`OpCode::CallIntrinsic1`] and
/// [`OpCode::CallIntrinsic2`] carry as their `arg` (CPython
/// `Include/internal/pycore_intrinsics.h`; the same numbers travel on
/// the wire as the `CALL_INTRINSIC_1` / `CALL_INTRINSIC_2` oparg).
pub mod intrinsic {
    /// `INTRINSIC_PRINT(value)`: interactive-mode expression echo through
    /// `sys.displayhook`; returns `None` (the codegen follows it with a
    /// `POP_TOP`).
    pub const PRINT: u32 = 1;
    /// `INTRINSIC_TYPEVAR(name)`: a `TypeVar` with no bound.
    pub const TYPEVAR: u32 = 7;
    /// `INTRINSIC_PARAMSPEC(name)`.
    pub const PARAMSPEC: u32 = 8;
    /// `INTRINSIC_TYPEVARTUPLE(name)`.
    pub const TYPEVARTUPLE: u32 = 9;
    /// `INTRINSIC_SUBSCRIPT_GENERIC(params)`: `Generic[*params]`, the
    /// implicit trailing base of a generic class.
    pub const SUBSCRIPT_GENERIC: u32 = 10;
    /// `INTRINSIC_TYPEALIAS((name, type_params | None, evaluate_value))`.
    pub const TYPEALIAS: u32 = 11;

    /// `INTRINSIC_TYPEVAR_WITH_BOUND(name, evaluate_bound)`.
    pub const TYPEVAR_WITH_BOUND: u32 = 2;
    /// `INTRINSIC_TYPEVAR_WITH_CONSTRAINTS(name, evaluate_constraints)`.
    pub const TYPEVAR_WITH_CONSTRAINTS: u32 = 3;
    /// `INTRINSIC_SET_FUNCTION_TYPE_PARAMS(func, type_params)`: stamps
    /// `func.__type_params__` and returns `func`.
    pub const SET_FUNCTION_TYPE_PARAMS: u32 = 4;
    /// `INTRINSIC_SET_TYPEPARAM_DEFAULT(type_param, evaluate_default)`:
    /// attaches the PEP 696 default and returns the parameter.
    pub const SET_TYPEPARAM_DEFAULT: u32 = 5;
}

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

    /// Recover a [`CompareKind`] from its opcode argument (ignoring
    /// [`COMPARE_OP_TO_BOOL_FLAG`]).
    pub fn from_arg(arg: u32) -> Option<Self> {
        Some(match arg & !COMPARE_OP_TO_BOOL_FLAG {
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

    /// The in-place dunder name for this operator (`a += b` → `__iadd__`).
    /// Used by the VM's augmented-assignment path.
    pub fn inplace_dunder(self) -> &'static str {
        match self {
            Self::Add => "__iadd__",
            Self::Sub => "__isub__",
            Self::Mult => "__imul__",
            Self::Div => "__itruediv__",
            Self::FloorDiv => "__ifloordiv__",
            Self::Mod => "__imod__",
            Self::Pow => "__ipow__",
            Self::LShift => "__ilshift__",
            Self::RShift => "__irshift__",
            Self::BitOr => "__ior__",
            Self::BitXor => "__ixor__",
            Self::BitAnd => "__iand__",
            Self::MatMult => "__imatmul__",
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

/// How the codec presents an instruction on the CPython wire
/// ([`crate::CodeObject::wire_marks`], one byte per instruction).
/// CPython 3.14's flowgraph refines `LOAD_FAST` into borrowing and
/// checked forms and fuses adjacent fast-local moves (and `LOAD_GLOBAL;
/// PUSH_NULL`) into superinstructions; WeavePy executes the plain
/// forms and records the refinement here, so the wire view is
/// byte-identical to CPython's while the VM, its inline caches, and the
/// JIT keep seeing one simple opcode per stack effect.
pub mod wire {
    /// Plain presentation: the instruction's own opcode.
    pub const PLAIN: u8 = 0;
    /// `LOAD_FAST` presented as `LOAD_FAST_BORROW` (on a fusion head,
    /// the pair as `LOAD_FAST_BORROW_LOAD_FAST_BORROW`).
    pub const BORROW: u8 = 1;
    /// `LOAD_FAST` presented as `LOAD_FAST_CHECK`.
    pub const CHECK: u8 = 2;
    /// Head of a two-instruction fusion: this instruction and the next
    /// (marked [`FUSE_TAIL`]) form one wire instruction —
    /// `LOAD_FAST_LOAD_FAST`, `STORE_FAST_LOAD_FAST`,
    /// `STORE_FAST_STORE_FAST`, or the callable-flagged `LOAD_GLOBAL`.
    pub const FUSE_HEAD: u8 = 4;
    /// Tail of a fusion: zero-width on the wire (shares the head's
    /// offset, location, and exception coverage).
    pub const FUSE_TAIL: u8 = 8;
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
    /// PEP 709 (CPython `LOAD_FAST_AND_CLEAR`): push the slot's current
    /// value — `Unbound` included, without raising — and clear the
    /// slot. Pairs with a later `StoreFast` to save/restore a hidden
    /// local around an inlined comprehension.
    LoadFastAndClear,
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
    /// Runtime no-op in the WeavePy VM (frame setup already copies the
    /// closure cells); encodes as CPython's `COPY_FREE_VARS n` prologue
    /// unit so `co_code`/`dis` match (RFC 0068 WS1). Appears only in
    /// code decoded from the CPython wire form — WeavePy's own encoder
    /// synthesizes the prologue.
    CopyFreeVars,

    /// Push the NULL marker (`Object::Unbound` in the VM) that fills
    /// CPython's self-or-null call slot (RFC 0068 WS1) and, since 3.14,
    /// the kwargs slot of a `CALL_FUNCTION_EX` without `**`. The
    /// compiler emits it after every callable load that CPython pairs
    /// with a NULL push.
    PushNull,

    // Attributes / subscripts
    /// `value = stack[-1]; push value.<co_names[arg]>`
    LoadAttr,
    /// Same runtime behaviour as [`OpCode::LoadAttr`] (one bound value
    /// is pushed), but marks CPython's method-call optimization site:
    /// encodes as `LOAD_ATTR` with oparg bit 0 set (the wire view
    /// pushes `attr, self_or_null` there, so no `PUSH_NULL` precedes
    /// the call).
    LoadMethodAttr,
    /// CPython 3.13 `LOAD_SUPER_ATTR`: pops `self, class, global_super`
    /// (top-down), calls `global_super` — with `(class, self)` when arg
    /// bit 1 is set (explicit two-argument `super(a, b)`), zero-arg
    /// otherwise — then loads `co_names[arg >> 2]` from the resulting
    /// super object. Pushes the attribute, plus a null self slot when
    /// arg bit 0 is set (method-call form, mirrors [`OpCode::LoadMethodAttr`]).
    LoadSuperAttr,
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
    /// Call a callable with `arg` positional arguments. Stack layout
    /// (top-down): `arg_n, ..., arg_1, self_or_null, callable`; a
    /// non-NULL self slot rides as the first positional argument
    /// (CPython's `CALL` convention, RFC 0068 WS1).
    Call,
    /// Same runtime behaviour as [`OpCode::Call`], but encodes as
    /// CPython's self-slot shape: `CALL arg-1` with the first argument
    /// sitting in the wire view's self-or-null slot (no `PUSH_NULL`).
    /// CPython uses this for decorator application and comprehension
    /// invocation.
    CallSelf,
    /// Call with keyword arguments. `arg` = positional arg count;
    /// stack also carries kw arg names (tuple) and values.
    CallKw,
    /// Call with packed args (CPython 3.14 `CALL_FUNCTION_EX`). The
    /// stack carries `(callable, self_or_null, args_tuple,
    /// kwargs_or_null)`; a call without `**` pushes NULL in the kwargs
    /// slot. Used for `*args` and `**kwargs` splats that can't be
    /// lowered to a static arg count. `arg` is unused (always 0).
    CallEx,
    /// Return TOS from the current frame.
    ReturnValue,

    // Control flow
    /// Pop TOS; if falsy, jump by `arg` instructions (signed).
    PopJumpIfFalse,
    /// Pop TOS; if truthy, jump by `arg` instructions (signed).
    PopJumpIfTrue,
    /// Pop TOS; if it is `None`, jump by `arg` instructions (CPython
    /// 3.13 `POP_JUMP_IF_NONE`, the `except*` match-check branch).
    PopJumpIfNone,
    /// Pop TOS; if it is not `None`, jump by `arg` instructions
    /// (CPython 3.13 `POP_JUMP_IF_NOT_NONE`, the `except*` reraise
    /// branch).
    PopJumpIfNotNone,
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
    /// Pop an iterable and extend the list `arg` entries below TOS with
    /// it (CPython `LIST_EXTEND`). A non-iterable operand raises
    /// CPython's splat wording: "Value after * must be an iterable,
    /// not X".
    ListExtend,
    /// Pop a list, push a tuple of its elements (CPython 3.13's
    /// `CALL_INTRINSIC_1` / `INTRINSIC_LIST_TO_TUPLE`).
    ListToTuple,
    SetAdd,
    /// Pop an iterable and update the set `arg` entries below TOS with
    /// it (CPython `SET_UPDATE`).
    SetUpdate,
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
    /// `[..., dict, unused[depth - 1], other]`. Pops `other`, applies
    /// it to the `dict` sitting `depth` slots below it (CPython's
    /// `DICT_UPDATE oparg`). Arg layout: bit 0 selects the call-site
    /// `DICT_MERGE` semantics (mapping required, duplicate keyword is a
    /// `TypeError`), `arg >> 1` is `depth - 1`. `0` is therefore the
    /// `{**d}` dict-display spread, `1` the `f(**kw)` splat, `2` the
    /// mapping-pattern `**rest` copy (`DICT_UPDATE 2`).
    DictUpdate,
    /// CPython `SETUP_ANNOTATIONS`: bind `__annotations__` to a fresh
    /// empty dict in the current scope *unless it is already bound*.
    /// Emitted once at the top of any module/class body that contains
    /// an annotated statement, so code preceding the first annotation
    /// can already read `__annotations__` (ann_module.py does
    /// `__annotations__[1] = 2` at module top).
    SetupAnnotations,

    // Functions / closures
    /// Build a function object from the code object on TOS.
    /// `arg` is a bitmask: 0x01 = defaults tuple, 0x02 = kw_defaults
    /// dict, 0x04 = annotations dict, 0x08 = closure tuple. The
    /// compiler emits `arg == 0` and attaches attributes with
    /// [`OpCode::SetFunctionAttribute`] (CPython 3.13's shape); the
    /// flag form remains for decoded legacy streams.
    MakeFunction,
    /// `func = pop; value = pop; set func attribute per the flag bits
    /// of `arg` (same 0x01/0x02/0x04/0x08 meanings as
    /// [`OpCode::MakeFunction`]); push func` — CPython 3.13's
    /// `SET_FUNCTION_ATTRIBUTE`.
    SetFunctionAttribute,
    /// Slice — build a Slice(low, high, step) and push it.
    BuildSlice,

    // Classes
    /// Push the magic `__build_class__` builtin onto the stack.
    LoadBuildClass,
    /// Push the frame's locals mapping (CPython `LOAD_LOCALS`): the
    /// class namespace inside a class body. Always followed by
    /// `LoadClassdictOrDeref` / `LoadClassdictOrGlobal`, which is how
    /// 3.14 spells a class body's class-namespace-first deref load.
    LoadLocals,
    /// `LOAD_FROM_DICT_OR_DEREF`: pops a mapping (the class namespace
    /// from `LoadLocals`, or the `__classdict__` cell's contents inside
    /// a PEP 695 annotation scope) and pushes `mapping[name]` if
    /// present, else the cell at `arg`.
    LoadClassdictOrDeref,
    /// PEP 695 lazy-scope global (CPython `LOAD_FROM_DICT_OR_GLOBALS`):
    /// pops a mapping and pushes `mapping[name]` if present, else
    /// resolves `names[arg]` through globals → builtins.
    LoadClassdictOrGlobal,

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
    /// PEP 654: CPython's `CALL_INTRINSIC_2(PREP_RERAISE_STAR)`. Stack
    /// on entry: `[..., excs_list, orig]`; pops both and pushes the
    /// exception to re-raise after the `except*` clauses ran (or `None`
    /// when everything was handled). `excs_list` holds the exceptions
    /// raised/re-raised by the handler bodies plus the unmatched
    /// remainder (possibly `None`) as its last element.
    PrepReraiseStar,

    /// CPython 3.14 `LOAD_COMMON_CONSTANT`: push one of the interpreter's
    /// well-known objects regardless of any shadowing binding. `arg`
    /// indexes `_Py_CommonConstants`: 0 `AssertionError` (the `assert`
    /// statement, bpo-34880), 1 `NotImplementedError` (PEP 649
    /// `__annotate__` bodies), 2 `tuple`, 3 `all`, 4 `any`.
    LoadCommonConstant,

    // Context managers
    /// CPython 3.14 `LOAD_SPECIAL`: pop `owner`, look `arg` up in the
    /// special-method table (0 `__enter__`, 1 `__exit__`, 2 `__aenter__`,
    /// 3 `__aexit__`) on `type(owner)`, and push `[method, owner]` in
    /// the method-call shape (`CALL 0` then binds `owner` as `self`).
    /// A missing method raises the 3.14 `TypeError` ("'X' object does
    /// not support the context manager protocol (missed __exit__
    /// method)"). Replaces 3.13's `BEFORE_WITH`/`BEFORE_ASYNC_WITH` in
    /// the `with`/`async with` prologue, which is now `COPY 1;
    /// LOAD_SPECIAL exit; SWAP 2; SWAP 3; LOAD_SPECIAL enter; CALL 0`.
    LoadSpecial,
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
    /// CPython 3.13's CONVERT_VALUE: apply an f-string conversion to
    /// TOS in place (`arg`: 1 = `!s`, 2 = `!r`, 3 = `!a`). Emitted as a
    /// separate instruction before FORMAT_SIMPLE/FORMAT_WITH_SPEC, as
    /// CPython's codegen does (test_dis grades the shape).
    ConvertValue,
    /// CPython 3.13's TO_BOOL: replace TOS with its truthiness as an
    /// exact `bool` (calls `__bool__`/`__len__`, may raise). Emitted
    /// before `POP_JUMP_IF_*` whenever the condition isn't statically
    /// boolean (comparisons, `not`, `is`, `in` fuse instead), and in
    /// the `with`-cleanup after `WITH_EXCEPT_START` — test_dis grades
    /// the shape.
    ToBool,

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
    /// `[..., aiter, exc]`. Stack on exit: `[...]`. CPython 3.14 gives
    /// it an oparg (the backward distance to the loop's `SEND`, an
    /// instrumentation anchor); WeavePy's `arg` is the same
    /// instruction delta so the codec can present it.
    EndAsyncFor,

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

    /// Clear the cell at `co_freevars[arg]` (CPython `DELETE_DEREF`).
    /// Empties the cell's contents without touching the value stack;
    /// raises `NameError` if the cell is already empty. Used for
    /// `del NAME` where NAME is a cell or free variable.
    ///
    /// Appended at the end of the enum so existing `#[repr(u8)]`
    /// discriminants stay stable for any cached bytecode.
    DeleteDeref,

    /// CPython 3.13 `CLEANUP_THROW`: handler for an exception delivered
    /// by `throw()`/`close()` while suspended at a send-dance
    /// `YIELD_VALUE`. Stack on entry: `[sub_iter, last_sent, exc]`. If
    /// `exc` is a `StopIteration`, pops all three and pushes
    /// `[None, exc.value]` (the following `END_SEND` keeps the value);
    /// otherwise re-raises `exc` without recording a new traceback
    /// entry.
    CleanupThrow,
    /// CPython's `CALL_INTRINSIC_1(STOPITERATION_ERROR)`: TOS is an
    /// exception instance escaping a generator-family frame. Replace a
    /// `StopIteration` (or an async generator's `StopAsyncIteration`)
    /// with the PEP 479 `RuntimeError`, chaining the original as cause
    /// and context; any other exception passes through unchanged.
    StopIterationError,
    /// CPython's `CALL_INTRINSIC_1(ASYNC_GEN_WRAP)`: mark TOS as an
    /// async generator's *own* yielded value (CPython wraps it in
    /// `PyAsyncGenWrappedValue`) so `__anext__` can tell it apart from
    /// a value passed through by an inner await's send dance.
    AsyncGenWrap,
    /// PEP 695/696 type-parameter intrinsics (CPython
    /// `CALL_INTRINSIC_1`, `Python/intrinsics.c`): `arg` is the
    /// intrinsic id, one of [`intrinsic::TYPEVAR`], [`intrinsic::PARAMSPEC`],
    /// [`intrinsic::TYPEVARTUPLE`], [`intrinsic::SUBSCRIPT_GENERIC`], or
    /// [`intrinsic::TYPEALIAS`]. Pops one operand, pushes the result.
    /// The other one-operand intrinsics have their own opcodes above
    /// ([`OpCode::ImportStar`], [`OpCode::StopIterationError`], ...).
    CallIntrinsic1,
    /// PEP 695/696 type-parameter intrinsics taking two operands
    /// (CPython `CALL_INTRINSIC_2`): [`intrinsic::TYPEVAR_WITH_BOUND`],
    /// [`intrinsic::TYPEVAR_WITH_CONSTRAINTS`],
    /// [`intrinsic::SET_FUNCTION_TYPE_PARAMS`], or
    /// [`intrinsic::SET_TYPEPARAM_DEFAULT`]. Stack on entry:
    /// `[value1, value2]`; pushes `intrinsic(value1, value2)`.
    CallIntrinsic2,

    /// PEP 750 t-strings (`-X lang=next`, RFC 0076 WS15; CPython 3.14
    /// `BUILD_INTERPOLATION`). Builds one `string.templatelib.
    /// Interpolation`. Stack on entry: `[value, expr_text_str,
    /// (spec_str)]` — CPython's oparg layout: the spec is present when
    /// `arg & 1` is set, bit 1 is the always-set base (`2`), and
    /// `arg >> 2` is the FVC_* conversion (0 none, 1 `s`, 2 `r`, 3 `a`).
    BuildInterpolation,
    /// PEP 750 t-strings (CPython 3.14 `BUILD_TEMPLATE`). Stack on
    /// entry: `[strings_tuple, interpolations_tuple]`; pushes the
    /// `string.templatelib.Template`.
    BuildTemplate,

    // CPython 3.14 additions (RFC 0077 WS9)
    /// Push the `int` `arg` (`0..=255`) directly, without a constant-pool
    /// slot (CPython 3.14 `LOAD_SMALL_INT`). The compiler's late
    /// `convert_small_int_consts` pass rewrites `LOAD_CONST` of such an
    /// int to this form and prunes the pool entry (except slot 0, which
    /// CPython keeps for the docstring rule).
    LoadSmallInt,
    /// Instrumentation anchor placed on the not-taken edge of a
    /// conditional branch (CPython 3.14 `NOT_TAKEN`); a runtime no-op.
    /// The compiler emits it only where CPython's *codegen* does (the
    /// `async for` success path); the codec synthesizes the copies
    /// CPython's flowgraph appends after every `POP_JUMP_IF_*`.
    NotTaken,
    /// Pop the exhausted iterator closing a `for` loop (CPython 3.14
    /// `POP_ITER`). Same runtime effect as [`OpCode::PopTop`]; a
    /// distinct opcode so `sys.monitoring` can tell loop exits from
    /// ordinary pops.
    PopIter,
    /// `stop = pop; start = pop; container = pop; push
    /// container[start:stop]` (CPython 3.12+ `BINARY_SLICE`) — the
    /// two-element slice shape the compiler uses for non-constant
    /// `a[x:y]`.
    BinarySlice,
    /// `stop = pop; start = pop; container = pop; value = pop;
    /// container[start:stop] = value` (CPython 3.12+ `STORE_SLICE`).
    StoreSlice,

    // Flowgraph-only refinements of the fast-local and global loads.
    // CPython's optimizer produces them as real opcodes; WeavePy's
    // runtime keeps the plain forms (its ICs and the JIT key on
    // `LoadFast`/`StoreFast`/`LoadGlobal`/`PushNull`), so
    // `flowgraph::flatten` lowers each of them back to the plain
    // instruction(s) plus a [`wire`] mark, and the codec re-derives the
    // CPython opcode from the mark. They never reach the VM.
    /// `LOAD_FAST` whose reference the flowgraph's `optimize_load_fast`
    /// proved is consumed before the slot can be rebound (CPython
    /// `LOAD_FAST_BORROW`). Lowers to `LoadFast` + [`wire::BORROW`].
    LoadFastBorrow,
    /// `LOAD_FAST` of a slot the flowgraph's uninitialized-locals
    /// analysis could not prove bound on every path (CPython
    /// `LOAD_FAST_CHECK`); never fuses into a superinstruction. Lowers
    /// to `LoadFast` + [`wire::CHECK`].
    LoadFastCheck,
    /// `LOAD_GLOBAL` followed by `PUSH_NULL` (CPython's `LOAD_GLOBAL`
    /// with the low oparg bit set). `arg` is the name index. Lowers to
    /// `LoadGlobal` [`wire::FUSE_HEAD`] + `PushNull` [`wire::FUSE_TAIL`].
    LoadGlobalPushNull,
    /// `LOAD_FAST a; LOAD_FAST b` (CPython `LOAD_FAST_LOAD_FAST`):
    /// `arg = (a << 4) | b`, both slots `< 16`. Lowers to two
    /// `LoadFast`s marked head/tail.
    LoadFastLoadFast,
    /// The borrowing pair (CPython `LOAD_FAST_BORROW_LOAD_FAST_BORROW`):
    /// head marked `FUSE_HEAD | BORROW`.
    LoadFastBorrowLoadFastBorrow,
    /// `LOAD_CLOSURE` (a `LOAD_FAST` of a cell slot on the CPython wire)
    /// whose reference `optimize_load_fast` proved consumed (CPython
    /// `LOAD_FAST_BORROW`). Lowers to `LoadClosure` + [`wire::BORROW`].
    LoadClosureBorrow,
    /// `STORE_FAST a; LOAD_FAST b` (CPython `STORE_FAST_LOAD_FAST`).
    StoreFastLoadFast,
    /// `STORE_FAST a; STORE_FAST b` (CPython `STORE_FAST_STORE_FAST`).
    StoreFastStoreFast,

    // Flowgraph pseudo-ops (CPython `pycore_opcode_metadata.h`
    // `IS_PSEUDO_INSTR`). They exist only inside the flowgraph
    // optimizer between construction and flattening; the VM never
    // sees them.
    /// Direction-agnostic unconditional jump that polls the eval
    /// breaker (`JUMP`); flattens to `JumpForward` / `JumpBackward`.
    Jump,
    /// `JUMP_NO_INTERRUPT`: flattens to `JumpForward` or a
    /// `JumpBackward` recorded in [`crate::CodeObject::no_interrupt_jumps`].
    JumpNoInterrupt,
    /// `JUMP_IF_FALSE`: peek-and-branch; `convert_pseudo_conditional_jumps`
    /// expands it to `COPY 1; TO_BOOL; POP_JUMP_IF_FALSE`.
    JumpIfFalse,
    /// `JUMP_IF_TRUE`, likewise.
    JumpIfTrue,
    /// `SETUP_FINALLY`: pushes an exception handler (the target).
    SetupFinally,
    /// `SETUP_CLEANUP`: pushes a `lasti` handler.
    SetupCleanup,
    /// `SETUP_WITH`: pushes a `lasti` handler at depth +1.
    SetupWith,
    /// `POP_BLOCK`: pops the innermost handler.
    PopBlock,
    /// `STORE_FAST_MAYBE_NULL`: a store that may write `NULL` (the
    /// inlined-comprehension restore); becomes `STORE_FAST`.
    StoreFastMaybeNull,
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
            OpCode::LoadFastAndClear => "LOAD_FAST_AND_CLEAR",
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
            OpCode::CopyFreeVars => "COPY_FREE_VARS",
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
            OpCode::CallSelf => "CALL",
            OpCode::PushNull => "PUSH_NULL",
            OpCode::LoadMethodAttr => "LOAD_ATTR",
            OpCode::LoadSuperAttr => "LOAD_SUPER_ATTR",
            OpCode::CallKw => "CALL_KW",
            OpCode::CallEx => "CALL_FUNCTION_EX",
            OpCode::ReturnValue => "RETURN_VALUE",
            OpCode::PopJumpIfFalse => "POP_JUMP_IF_FALSE",
            OpCode::PopJumpIfTrue => "POP_JUMP_IF_TRUE",
            OpCode::PopJumpIfNone => "POP_JUMP_IF_NONE",
            OpCode::PopJumpIfNotNone => "POP_JUMP_IF_NOT_NONE",
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
            OpCode::ListExtend => "LIST_EXTEND",
            OpCode::ListToTuple => "LIST_TO_TUPLE",
            OpCode::SetAdd => "SET_ADD",
            OpCode::SetUpdate => "SET_UPDATE",
            OpCode::MapAdd => "MAP_ADD",
            OpCode::UnpackSequence => "UNPACK_SEQUENCE",
            OpCode::UnpackEx => "UNPACK_EX",
            OpCode::DictUpdate => "DICT_UPDATE",
            OpCode::SetupAnnotations => "SETUP_ANNOTATIONS",
            OpCode::MakeFunction => "MAKE_FUNCTION",
            OpCode::SetFunctionAttribute => "SET_FUNCTION_ATTRIBUTE",
            OpCode::BuildSlice => "BUILD_SLICE",
            OpCode::LoadBuildClass => "LOAD_BUILD_CLASS",
            OpCode::LoadLocals => "LOAD_LOCALS",
            OpCode::LoadClassdictOrDeref => "LOAD_FROM_DICT_OR_DEREF",
            OpCode::LoadClassdictOrGlobal => "LOAD_FROM_DICT_OR_GLOBALS",
            OpCode::RaiseVarargs => "RAISE_VARARGS",
            OpCode::CheckExcMatch => "CHECK_EXC_MATCH",
            OpCode::CheckEGMatch => "CHECK_EG_MATCH",
            OpCode::PrepReraiseStar => "PREP_RERAISE_STAR",
            OpCode::LoadCommonConstant => "LOAD_COMMON_CONSTANT",
            OpCode::PushExcInfo => "PUSH_EXC_INFO",
            OpCode::PopExcept => "POP_EXCEPT",
            OpCode::Reraise => "RERAISE",
            OpCode::LoadSpecial => "LOAD_SPECIAL",
            OpCode::WithExceptStart => "WITH_EXCEPT_START",
            OpCode::ImportName => "IMPORT_NAME",
            OpCode::ImportFrom => "IMPORT_FROM",
            OpCode::ImportStar => "IMPORT_STAR",
            OpCode::FormatValue => "FORMAT_VALUE",
            OpCode::ConvertValue => "CONVERT_VALUE",
            OpCode::ToBool => "TO_BOOL",
            OpCode::YieldValue => "YIELD_VALUE",
            OpCode::GetYieldFromIter => "GET_YIELD_FROM_ITER",
            OpCode::ReturnGenerator => "RETURN_GENERATOR",
            OpCode::Send => "SEND",
            OpCode::EndSend => "END_SEND",
            OpCode::GetAwaitable => "GET_AWAITABLE",
            OpCode::GetAiter => "GET_AITER",
            OpCode::GetAnext => "GET_ANEXT",
            OpCode::EndAsyncFor => "END_ASYNC_FOR",
            OpCode::MatchSequence => "MATCH_SEQUENCE",
            OpCode::MatchMapping => "MATCH_MAPPING",
            OpCode::MatchClass => "MATCH_CLASS",
            OpCode::MatchKeys => "MATCH_KEYS",
            OpCode::GetLen => "GET_LEN",
            OpCode::PrintExpr => "PRINT_EXPR",
            OpCode::DeleteDeref => "DELETE_DEREF",
            OpCode::CleanupThrow => "CLEANUP_THROW",
            OpCode::StopIterationError => "CALL_INTRINSIC_1",
            OpCode::AsyncGenWrap => "CALL_INTRINSIC_1",
            OpCode::CallIntrinsic1 => "CALL_INTRINSIC_1",
            OpCode::CallIntrinsic2 => "CALL_INTRINSIC_2",
            OpCode::BuildInterpolation => "BUILD_INTERPOLATION",
            OpCode::BuildTemplate => "BUILD_TEMPLATE",
            OpCode::LoadSmallInt => "LOAD_SMALL_INT",
            OpCode::LoadFastCheck => "LOAD_FAST_CHECK",
            OpCode::LoadGlobalPushNull => "LOAD_GLOBAL",
            OpCode::LoadFastBorrow => "LOAD_FAST_BORROW",
            OpCode::LoadClosureBorrow => "LOAD_FAST_BORROW",
            OpCode::LoadFastLoadFast => "LOAD_FAST_LOAD_FAST",
            OpCode::LoadFastBorrowLoadFastBorrow => "LOAD_FAST_BORROW_LOAD_FAST_BORROW",
            OpCode::StoreFastLoadFast => "STORE_FAST_LOAD_FAST",
            OpCode::StoreFastStoreFast => "STORE_FAST_STORE_FAST",
            OpCode::Jump => "JUMP",
            OpCode::JumpNoInterrupt => "JUMP_NO_INTERRUPT",
            OpCode::JumpIfFalse => "JUMP_IF_FALSE",
            OpCode::JumpIfTrue => "JUMP_IF_TRUE",
            OpCode::SetupFinally => "SETUP_FINALLY",
            OpCode::SetupCleanup => "SETUP_CLEANUP",
            OpCode::SetupWith => "SETUP_WITH",
            OpCode::PopBlock => "POP_BLOCK",
            OpCode::StoreFastMaybeNull => "STORE_FAST_MAYBE_NULL",
            OpCode::NotTaken => "NOT_TAKEN",
            OpCode::PopIter => "POP_ITER",
            OpCode::BinarySlice => "BINARY_SLICE",
            OpCode::StoreSlice => "STORE_SLICE",
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
/// Variants are 16 bytes or smaller. Storage encodes their initialized
/// fields explicitly, without reading enum padding.
///
/// Attribute versions are process-unique class-resolution tokens.
/// `module_id` / `globals_id` / `builtins_id` are all
/// allocation-address fingerprints; indexed loads also validate the
/// cached key. Narrow fields precede u64 fields so each tagged payload
/// fits within sixteen bytes.
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
    // RFC 0058 WS3 — division/modulo/power completion. The int shapes
    // deopt to the generic (bignum) path when the i64 primitive can't
    // represent the result; error semantics (ZeroDivisionError…) are
    // shared with the generic path via the same helper.
    BinOpDivInt,
    BinOpFloorDivInt,
    BinOpModInt,
    BinOpPowInt,
    BinOpDivFloat,
    BinOpFloorDivFloat,
    BinOpModFloat,
    BinOpPowFloat,

    // COMPARE_OP family — both operands int / float / str.
    CompareOpInt,
    CompareOpFloat,
    CompareOpStr,

    // LOAD_ATTR family — dict slot index + the class's
    // attribute-resolution version (`TypeObject::attr_version`) observed
    // at specialisation time. A class-dict or MRO mutation bumps the
    // version, deopting stale sites (CPython's `tp_version_tag` role).
    LoadAttrInstance {
        key_idx: u32,
        ver: u64,
    },
    LoadAttrModule {
        key_idx: u32,
        module_id: u64,
    },
    /// Attribute backed by a `__slots__` member descriptor: the value
    /// lives in the instance's ordered slot storage. The cached index is
    /// checked against the name because population order can differ.
    LoadAttrSlot {
        key_idx: u32,
        ver: u64,
    },
    LoadAttrType {
        key_idx: u32,
        ver: u64,
    },
    /// Method load (`obj.m` resolving to a plain Python function on the
    /// class MRO, about to be bound). `mro_idx` locates the defining
    /// class inside the receiver-type's MRO and `key_idx` the slot in
    /// that class's dict — so a hit skips the MRO walk and the name
    /// allocation, going straight to bind. Guarded by `ver`
    /// (`TypeObject::attr_version`) plus a name check on the slot.
    LoadAttrMethod {
        mro_idx: u16,
        key_idx: u32,
        ver: u64,
    },

    // LOAD_GLOBAL family — globals/builtins dict version + key idx.
    LoadGlobalModule {
        key_idx: u32,
        globals_id: u64,
    },
    LoadGlobalBuiltin {
        key_idx: u32,
        builtins_id: u64,
    },

    // STORE_ATTR family — fingerprint + dict slot index + resolution
    // version (see the LOAD_ATTR family note).
    StoreAttrInstance {
        key_idx: u32,
        ver: u64,
    },
    /// Store to a `__slots__` member: writes the instance's slot side
    /// table directly (the class was validated at specialisation time to
    /// have the stock `__setattr__` and a genuine slot descriptor). The
    /// index is guarded by its name, with named insertion on a miss.
    StoreAttrSlot {
        key_idx: u32,
        ver: u64,
    },
    /// First store of a not-yet-present attribute on a plain instance —
    /// the constructor pattern (`self.x = …` on a fresh object). One
    /// hash probe decides insert vs overwrite; the class was validated
    /// at specialisation time (stock `__setattr__`, no data descriptor
    /// for the name, instances carry a `__dict__`) and `ver` guards
    /// against class mutations since.
    StoreAttrNewKey {
        ver: u64,
    },

    // FOR_ITER family.
    ForIterList,
    ForIterTuple,
    ForIterRange,
    /// `for c in s:` over a `str` iterator (RFC 0058 WS3).
    ForIterStr,
    /// `for k in d:` / dict-view loops — one shape for keys, values
    /// and items cursors; the step runs the same checked `__next__`
    /// (size/keys-changed guards) as the generic path.
    ForIterDict,

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
        argc: u32,
        func_id: u64,
    },
    /// Plain Python function with the same exact-arity guarantee but a
    /// non-trivial cell/closure layout — still skips argument binding,
    /// but builds the frame (and its cells) through `make_frame`.
    CallPyExact {
        argc: u32,
        func_id: u64,
    },
    /// Bound method (`obj.m(...)`) whose target is a plain Python
    /// function with exact arity `argc + 1` (receiver prepended) —
    /// the binder skip applied to method calls. WeavePy has no
    /// `LOAD_METHOD` opcode; the `LoadAttrMethod` + this pair is the
    /// CPython fusion equivalent (RFC 0058 WS3).
    CallBoundMethodExact {
        argc: u32,
        func_id: u64,
    },
    /// Plain Python function called with fewer positionals than it
    /// declares, the missing tail covered verbatim by `__defaults__`
    /// — skips the binder and splices the defaults suffix directly
    /// (RFC 0058 WS3).
    CallPyDefaults {
        argc: u32,
        func_id: u64,
    },
    /// Keyword call (`CALL_KW`) on a plain Python function whose name
    /// tuple resolves to a fixed name→slot permutation: bind is the
    /// positional moves, the permuted keyword writes, and a defaults
    /// fill — no per-call name *search* (RFC 0069 WS3). The hit guard
    /// re-verifies each keyword against `varnames[slot]` directly
    /// (one short string compare per keyword), so a rebound
    /// `__code__` with renamed parameters can never mis-bind — the
    /// permutation is self-validating against the live code. `perm`
    /// packs the keyword→slot map 4 bits per keyword; sites with more
    /// than 8 keywords, or callees with more than 16 parameter slots,
    /// stay generic.
    CallPyKwNames {
        argc: u8,
        kwc: u8,
        perm: u32,
        func_id: u64,
    },
    /// Module-level native callable (`math.sqrt`, `ord`, …) with no
    /// interpreter-aware dispatch chain: straight to the Rust `fn`
    /// (RFC 0058 WS3). Deopts whenever observers (profile/trace
    /// hooks) are active so `c_call` events still fire.
    CallNative {
        argc: u32,
        func_id: u64,
    },
    /// Bound native method (`xs.append`, `s.startswith`, …): receiver
    /// prepended, straight to the Rust `fn` (RFC 0058 WS3). Same
    /// observer deopt as [`InlineCache::CallNative`].
    CallNativeMethod {
        argc: u32,
        func_id: u64,
    },

    // BINARY_SUBSCR family (RFC 0058 WS3). The container's enum
    // variant is the fingerprint, checked at the start of each hit.
    /// `list[int]` — in-range index (negative handled inline).
    SubscrListInt,
    /// `tuple[int]`.
    SubscrTupleInt,
    /// Pure-ASCII `str[int]` (code-point count == byte count, both
    /// cached) — O(1) byte indexing; re-verified on every hit.
    SubscrStrInt,
    /// `dict[key]` — skips the instance/type/foreign dispatch chain;
    /// the hash lookup itself is unavoidable (CPython's
    /// `BINARY_SUBSCR_DICT` does the same).
    SubscrDict,

    // STORE_SUBSCR family (RFC 0058 WS3).
    /// `list[int] = v` — in-range element overwrite.
    StoreSubscrListInt,
    /// `dict[key] = v`.
    StoreSubscrDict,

    // ----- RFC 0061 (WS2b): fused dispatch -----
    //
    // A fusion marker lives in the *first* instruction's cache slot and
    // means "the fall-through pair starting here may execute as one
    // dispatch". The instruction stream is untouched (`co_code`, `dis`,
    // line tables and jump targets cannot tell), a jump landing on the
    // second instruction executes it normally, and the dispatcher only
    // honours markers while no observer (trace/profile/monitoring) is
    // active — under observation every instruction single-steps through
    // the generic arms, so PEP 669 / `sys.settrace` event streams are
    // bit-identical.
    /// `LOAD_FAST a; LOAD_FAST b` — push two locals in one dispatch.
    FuseLoadFastLoadFast,
    /// `LOAD_FAST a; LOAD_CONST c` — local + materialized constant.
    FuseLoadFastLoadConst,
    /// `LOAD_FAST a; LOAD_ATTR n` — attribute of a local. The second
    /// slot's own LOAD_ATTR cache supplies the specialization; the
    /// fused arm reads the receiver *in place* (no clone onto the
    /// operand stack, no Arc round-trip).
    FuseLoadFastLoadAttr,
    /// `COMPARE_OP (int, int); POP_JUMP_IF_{TRUE,FALSE}` — compare and
    /// branch without materializing the intermediate `Bool`. Replaces
    /// `CompareOpInt` on the compare's slot (its guards subsume it).
    FuseCompareIntPopJump,
    /// The dispatcher inspected this site once and found no fusable
    /// pair; permanent (the fall-through successor never changes).
    FuseBlocked,
}

/// Number of generic dispatches a deopted cache must serve before it
/// re-attempts specialization. Damps thrashing on polymorphic call
/// sites.
pub const COOLDOWN: u8 = 64;

impl InlineCache {
    // Explicit initialized bits, independent of Rust enum padding, target
    // endianness, and field alignment. Tags are private cache storage only.
    #[inline(always)]
    const fn encode(self) -> u128 {
        match self {
            Self::Empty => 0,
            Self::Cooldown(value) => 1 | ((value as u128) << 8),
            Self::BinOpAddInt => 2,
            Self::BinOpSubInt => 3,
            Self::BinOpMulInt => 4,
            Self::BinOpAddFloat => 5,
            Self::BinOpSubFloat => 6,
            Self::BinOpMulFloat => 7,
            Self::BinOpAddStr => 8,
            Self::BinOpDivInt => 9,
            Self::BinOpFloorDivInt => 10,
            Self::BinOpModInt => 11,
            Self::BinOpPowInt => 12,
            Self::BinOpDivFloat => 13,
            Self::BinOpFloorDivFloat => 14,
            Self::BinOpModFloat => 15,
            Self::BinOpPowFloat => 16,
            Self::CompareOpInt => 17,
            Self::CompareOpFloat => 18,
            Self::CompareOpStr => 19,
            Self::LoadAttrInstance { key_idx, ver } => {
                20 | ((key_idx as u128) << 8) | ((ver as u128) << 40)
            }
            Self::LoadAttrModule { key_idx, module_id } => {
                21 | ((key_idx as u128) << 8) | ((module_id as u128) << 40)
            }
            Self::LoadAttrSlot { key_idx, ver } => {
                22 | ((key_idx as u128) << 8) | ((ver as u128) << 40)
            }
            Self::LoadAttrType { key_idx, ver } => {
                23 | ((key_idx as u128) << 8) | ((ver as u128) << 40)
            }
            Self::LoadAttrMethod {
                mro_idx,
                key_idx,
                ver,
            } => 24 | ((mro_idx as u128) << 8) | ((key_idx as u128) << 24) | ((ver as u128) << 56),
            Self::LoadGlobalModule {
                key_idx,
                globals_id,
            } => 25 | ((key_idx as u128) << 8) | ((globals_id as u128) << 40),
            Self::LoadGlobalBuiltin {
                key_idx,
                builtins_id,
            } => 26 | ((key_idx as u128) << 8) | ((builtins_id as u128) << 40),
            Self::StoreAttrInstance { key_idx, ver } => {
                27 | ((key_idx as u128) << 8) | ((ver as u128) << 40)
            }
            Self::StoreAttrSlot { key_idx, ver } => {
                28 | ((key_idx as u128) << 8) | ((ver as u128) << 40)
            }
            Self::StoreAttrNewKey { ver } => 29 | ((ver as u128) << 8),
            Self::ForIterList => 30,
            Self::ForIterTuple => 31,
            Self::ForIterRange => 32,
            Self::ForIterStr => 33,
            Self::ForIterDict => 34,
            Self::UnpackSequenceTuple => 35,
            Self::UnpackSequenceList => 36,
            Self::UnpackSequenceTwoTuple => 37,
            Self::CallPyExactNoFree { argc, func_id } => {
                38 | ((argc as u128) << 8) | ((func_id as u128) << 40)
            }
            Self::CallPyExact { argc, func_id } => {
                39 | ((argc as u128) << 8) | ((func_id as u128) << 40)
            }
            Self::CallBoundMethodExact { argc, func_id } => {
                40 | ((argc as u128) << 8) | ((func_id as u128) << 40)
            }
            Self::CallPyDefaults { argc, func_id } => {
                41 | ((argc as u128) << 8) | ((func_id as u128) << 40)
            }
            Self::CallPyKwNames {
                argc,
                kwc,
                perm,
                func_id,
            } => {
                42 | ((argc as u128) << 8)
                    | ((kwc as u128) << 16)
                    | ((perm as u128) << 24)
                    | ((func_id as u128) << 56)
            }
            Self::CallNative { argc, func_id } => {
                43 | ((argc as u128) << 8) | ((func_id as u128) << 40)
            }
            Self::CallNativeMethod { argc, func_id } => {
                44 | ((argc as u128) << 8) | ((func_id as u128) << 40)
            }
            Self::SubscrListInt => 45,
            Self::SubscrTupleInt => 46,
            Self::SubscrStrInt => 47,
            Self::SubscrDict => 48,
            Self::StoreSubscrListInt => 49,
            Self::StoreSubscrDict => 50,
            Self::FuseLoadFastLoadFast => 51,
            Self::FuseLoadFastLoadConst => 52,
            Self::FuseLoadFastLoadAttr => 53,
            Self::FuseCompareIntPopJump => 54,
            Self::FuseBlocked => 55,
        }
    }

    #[inline(always)]
    const fn decode(bits: u128) -> Self {
        match bits as u8 {
            0 => Self::Empty,
            1 => Self::Cooldown((bits >> 8) as u8),
            2 => Self::BinOpAddInt,
            3 => Self::BinOpSubInt,
            4 => Self::BinOpMulInt,
            5 => Self::BinOpAddFloat,
            6 => Self::BinOpSubFloat,
            7 => Self::BinOpMulFloat,
            8 => Self::BinOpAddStr,
            9 => Self::BinOpDivInt,
            10 => Self::BinOpFloorDivInt,
            11 => Self::BinOpModInt,
            12 => Self::BinOpPowInt,
            13 => Self::BinOpDivFloat,
            14 => Self::BinOpFloorDivFloat,
            15 => Self::BinOpModFloat,
            16 => Self::BinOpPowFloat,
            17 => Self::CompareOpInt,
            18 => Self::CompareOpFloat,
            19 => Self::CompareOpStr,
            20 => Self::LoadAttrInstance {
                key_idx: (bits >> 8) as u32,
                ver: (bits >> 40) as u64,
            },
            21 => Self::LoadAttrModule {
                key_idx: (bits >> 8) as u32,
                module_id: (bits >> 40) as u64,
            },
            22 => Self::LoadAttrSlot {
                key_idx: (bits >> 8) as u32,
                ver: (bits >> 40) as u64,
            },
            23 => Self::LoadAttrType {
                key_idx: (bits >> 8) as u32,
                ver: (bits >> 40) as u64,
            },
            24 => Self::LoadAttrMethod {
                mro_idx: (bits >> 8) as u16,
                key_idx: (bits >> 24) as u32,
                ver: (bits >> 56) as u64,
            },
            25 => Self::LoadGlobalModule {
                key_idx: (bits >> 8) as u32,
                globals_id: (bits >> 40) as u64,
            },
            26 => Self::LoadGlobalBuiltin {
                key_idx: (bits >> 8) as u32,
                builtins_id: (bits >> 40) as u64,
            },
            27 => Self::StoreAttrInstance {
                key_idx: (bits >> 8) as u32,
                ver: (bits >> 40) as u64,
            },
            28 => Self::StoreAttrSlot {
                key_idx: (bits >> 8) as u32,
                ver: (bits >> 40) as u64,
            },
            29 => Self::StoreAttrNewKey {
                ver: (bits >> 8) as u64,
            },
            30 => Self::ForIterList,
            31 => Self::ForIterTuple,
            32 => Self::ForIterRange,
            33 => Self::ForIterStr,
            34 => Self::ForIterDict,
            35 => Self::UnpackSequenceTuple,
            36 => Self::UnpackSequenceList,
            37 => Self::UnpackSequenceTwoTuple,
            38 => Self::CallPyExactNoFree {
                argc: (bits >> 8) as u32,
                func_id: (bits >> 40) as u64,
            },
            39 => Self::CallPyExact {
                argc: (bits >> 8) as u32,
                func_id: (bits >> 40) as u64,
            },
            40 => Self::CallBoundMethodExact {
                argc: (bits >> 8) as u32,
                func_id: (bits >> 40) as u64,
            },
            41 => Self::CallPyDefaults {
                argc: (bits >> 8) as u32,
                func_id: (bits >> 40) as u64,
            },
            42 => Self::CallPyKwNames {
                argc: (bits >> 8) as u8,
                kwc: (bits >> 16) as u8,
                perm: (bits >> 24) as u32,
                func_id: (bits >> 56) as u64,
            },
            43 => Self::CallNative {
                argc: (bits >> 8) as u32,
                func_id: (bits >> 40) as u64,
            },
            44 => Self::CallNativeMethod {
                argc: (bits >> 8) as u32,
                func_id: (bits >> 40) as u64,
            },
            45 => Self::SubscrListInt,
            46 => Self::SubscrTupleInt,
            47 => Self::SubscrStrInt,
            48 => Self::SubscrDict,
            49 => Self::StoreSubscrListInt,
            50 => Self::StoreSubscrDict,
            51 => Self::FuseLoadFastLoadFast,
            52 => Self::FuseLoadFastLoadConst,
            53 => Self::FuseLoadFastLoadAttr,
            54 => Self::FuseCompareIntPopJump,
            55 => Self::FuseBlocked,
            _ => Self::Empty,
        }
    }
}

/// One coherent instruction-cache value, including concurrent shared-code
/// reads and writes. No enum padding is read and no pointer is dereferenced
/// from the cached integer fingerprints.
#[derive(Default)]
pub struct CacheSlot {
    inner: crate::cache_snapshot::CacheSnapshot,
}

impl CacheSlot {
    pub const fn new(value: InlineCache) -> Self {
        Self {
            inner: crate::cache_snapshot::CacheSnapshot::new(value.encode()),
        }
    }

    #[inline(always)]
    pub fn get(&self) -> InlineCache {
        self.inner
            .load()
            .map(InlineCache::decode)
            .unwrap_or(InlineCache::Empty)
    }

    #[inline(always)]
    pub fn set(&self, value: InlineCache) {
        self.inner.store(value.encode());
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

/// Per-instruction cache storage allocated on the first cache write.
///
/// The atomic pointer owns either no allocation or exactly `len` slots. An
/// acquire load both checks initialization and obtains the published storage.
/// Only exclusive access can resize or release it. Publication synchronizes
/// initialization; individual cache values also use coherent atomic loads and stores.
#[derive(Default)]
pub struct CacheTable {
    slots: std::sync::atomic::AtomicPtr<CacheSlot>,
    len: usize,
}

impl CacheTable {
    /// Reserve a logical instruction count without allocating cache storage.
    pub fn with_len(n: usize) -> Self {
        Self {
            slots: std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()),
            len: n,
        }
    }

    #[inline]
    fn initialized(&self) -> Option<&[CacheSlot]> {
        let slots = self.slots.load(std::sync::atomic::Ordering::Acquire);
        if slots.is_null() {
            None
        } else {
            // SAFETY: a nonnull pointer owns `len` initialized slots. Acquire
            // observes their publication, and this shared borrow prevents
            // resize/drop. CacheSlot provides the slots' interior mutability.
            Some(unsafe { std::slice::from_raw_parts(slots, self.len) })
        }
    }

    /// Read a cache, returning `Empty` for cold tables or out-of-range indices.
    #[inline(always)]
    pub fn get(&self, pc: u32) -> InlineCache {
        self.initialized()
            .and_then(|slots| slots.get(pc as usize))
            .map(CacheSlot::get)
            .unwrap_or(InlineCache::Empty)
    }

    #[cold]
    #[inline(never)]
    fn allocate(&self) -> &[CacheSlot] {
        let storage: Box<[CacheSlot]> = (0..self.len)
            .map(|_| CacheSlot::new(InlineCache::Empty))
            .collect();
        let allocation = Box::into_raw(storage).cast::<CacheSlot>();
        let slots = match self.slots.compare_exchange(
            std::ptr::null_mut(),
            allocation,
            std::sync::atomic::Ordering::Release,
            std::sync::atomic::Ordering::Acquire,
        ) {
            Ok(_) => allocation,
            Err(published) => {
                // SAFETY: this thread's losing allocation was never published.
                // Restore its exact slice metadata and release it once.
                unsafe {
                    drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                        allocation, self.len,
                    )));
                }
                published
            }
        };
        // SAFETY: the winning allocation has `len` initialized slots. This
        // thread either initialized it or acquired its publication. Shared
        // access prevents exclusive resize/drop for the returned lifetime.
        unsafe { std::slice::from_raw_parts(slots, self.len) }
    }

    /// Set a cache, allocating on its first valid write. Invalid writes are no-ops.
    #[inline(always)]
    pub fn set(&self, pc: u32, value: InlineCache) {
        if pc as usize >= self.len {
            return;
        }
        let slots = match self.initialized() {
            Some(slots) => slots,
            None => self.allocate(),
        };
        if let Some(slot) = slots.get(pc as usize) {
            slot.set(value);
        }
    }

    /// Clear existing caches without allocating storage for a cold table.
    pub fn clear(&self) {
        if let Some(slots) = self.initialized() {
            for slot in slots {
                slot.set(InlineCache::Empty);
            }
        }
    }

    fn take_slots(&mut self) -> Option<Box<[CacheSlot]>> {
        let slots = std::mem::replace(self.slots.get_mut(), std::ptr::null_mut());
        if slots.is_null() {
            None
        } else {
            // SAFETY: exclusive access prevents outstanding storage borrows
            // or concurrent publication. Take ownership with the original
            // length, clearing the pointer so it cannot be released twice.
            Some(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(slots, self.len)) })
        }
    }

    /// Update the instruction count, preserving initialized slots in range.
    pub fn resize(&mut self, n: usize) {
        if n == self.len {
            return;
        }
        let previous = self.take_slots();
        self.len = n;
        if let Some(slots) = previous {
            if n != 0 {
                let mut slots = slots.into_vec();
                slots.resize_with(n, || CacheSlot::new(InlineCache::Empty));
                *self.slots.get_mut() = Box::into_raw(slots.into_boxed_slice()).cast::<CacheSlot>();
            }
        }
    }
}

impl Drop for CacheTable {
    fn drop(&mut self) {
        drop(self.take_slots());
    }
}

impl std::fmt::Debug for CacheTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheTable")
            .field("slots", &self.initialized())
            .field("len", &self.len)
            .finish()
    }
}

impl Clone for CacheTable {
    fn clone(&self) -> Self {
        let slots = self.initialized().map_or(std::ptr::null_mut(), |slots| {
            Box::into_raw(slots.to_vec().into_boxed_slice()).cast::<CacheSlot>()
        });
        Self {
            slots: std::sync::atomic::AtomicPtr::new(slots),
            len: self.len,
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

#[cfg(test)]
mod compact_cache_layout_tests {
    #[test]
    fn cache_slots_match_the_selected_atomic_storage() {
        assert_eq!(std::mem::size_of::<super::InlineCache>(), 16);
        assert_eq!(
            std::mem::size_of::<super::CacheSlot>(),
            std::mem::size_of::<crate::cache_snapshot::CacheSnapshot>()
        );
    }
}

#[cfg(test)]
mod lazy_cache_tests {
    use super::*;

    #[test]
    fn cold_code_stays_unallocated_through_reads_clear_clone_and_resize() {
        let mut table = CacheTable::with_len(1000);
        assert!(table.initialized().is_none());
        for pc in [0, 999, 1000, u32::MAX] {
            assert_eq!(table.get(pc), InlineCache::Empty);
        }
        table.set(1000, InlineCache::BinOpAddInt);
        table.clear();
        let copied = table.clone();
        table.resize(2000);
        assert!(table.initialized().is_none());
        assert!(copied.initialized().is_none());
        table.set(1999, InlineCache::CompareOpInt);
        assert_eq!(table.initialized().unwrap().len(), 2000);
        assert_eq!(table.get(1999), InlineCache::CompareOpInt);
        assert_eq!(table.get(1000), InlineCache::Empty);
        assert_eq!(copied.get(1999), InlineCache::Empty);
    }

    #[test]
    fn warm_resize_preserves_slots_and_discards_removed_entries() {
        let mut table = CacheTable::with_len(4);
        table.set(0, InlineCache::BinOpAddInt);
        table.set(3, InlineCache::CompareOpInt);
        table.resize(8);
        assert_eq!(table.get(0), InlineCache::BinOpAddInt);
        assert_eq!(table.get(3), InlineCache::CompareOpInt);
        assert_eq!(table.get(7), InlineCache::Empty);
        table.resize(2);
        table.resize(4);
        assert_eq!(table.get(0), InlineCache::BinOpAddInt);
        assert_eq!(table.get(3), InlineCache::Empty);
        table.clear();
        assert_eq!(table.get(0), InlineCache::Empty);
        table.resize(0);
        assert!(table.initialized().is_none());
        table.resize(5);
        table.set(4, InlineCache::CompareOpInt);
        assert_eq!(table.get(4), InlineCache::CompareOpInt);
    }
}

#[cfg(test)]
mod cache_publication_tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn table_header_is_two_machine_words() {
        assert_eq!(
            std::mem::size_of::<CacheTable>(),
            2 * std::mem::size_of::<usize>()
        );
    }

    #[test]
    fn concurrent_publication_returns_one_initialized_allocation() {
        let table = Arc::new(CacheTable::with_len(64));
        let barrier = Arc::new(Barrier::new(8));
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let table = Arc::clone(&table);
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    barrier.wait();
                    // Exercise even losing allocations without concurrently
                    // mutating any cache value (CacheSlot's separate contract).
                    let slots = table.allocate();
                    assert_eq!(slots.as_ptr(), table.initialized().unwrap().as_ptr());
                    assert_eq!(slots.len(), 64);
                    for slot in slots {
                        assert_eq!(slot.get(), InlineCache::Empty);
                    }
                });
            }
        });
    }

    #[test]
    fn concurrent_first_writes_to_disjoint_slots_survive_resize_and_drop() {
        let table = Arc::new(CacheTable::with_len(8));
        let barrier = Arc::new(Barrier::new(8));
        std::thread::scope(|scope| {
            for pc in 0..8 {
                let table = Arc::clone(&table);
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    barrier.wait();
                    table.set(pc, InlineCache::Cooldown(pc as u8));
                    assert_eq!(table.get(pc), InlineCache::Cooldown(pc as u8));
                });
            }
        });
        let mut table = Arc::try_unwrap(table).unwrap();
        let copied = table.clone();
        table.resize(20);
        for pc in 0..8 {
            assert_eq!(table.get(pc), InlineCache::Cooldown(pc as u8));
            assert_eq!(copied.get(pc), InlineCache::Cooldown(pc as u8));
        }
        table.resize(0);
        assert!(table.initialized().is_none());
        assert_eq!(copied.get(7), InlineCache::Cooldown(7));
    }
}

#[cfg(test)]
mod atomic_slot_tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn every_variant_round_trips_at_field_boundaries() {
        for value in [
            InlineCache::Empty,
            InlineCache::Empty,
            InlineCache::Cooldown(0),
            InlineCache::Cooldown(u8::MAX),
            InlineCache::BinOpAddInt,
            InlineCache::BinOpAddInt,
            InlineCache::BinOpSubInt,
            InlineCache::BinOpSubInt,
            InlineCache::BinOpMulInt,
            InlineCache::BinOpMulInt,
            InlineCache::BinOpAddFloat,
            InlineCache::BinOpAddFloat,
            InlineCache::BinOpSubFloat,
            InlineCache::BinOpSubFloat,
            InlineCache::BinOpMulFloat,
            InlineCache::BinOpMulFloat,
            InlineCache::BinOpAddStr,
            InlineCache::BinOpAddStr,
            InlineCache::BinOpDivInt,
            InlineCache::BinOpDivInt,
            InlineCache::BinOpFloorDivInt,
            InlineCache::BinOpFloorDivInt,
            InlineCache::BinOpModInt,
            InlineCache::BinOpModInt,
            InlineCache::BinOpPowInt,
            InlineCache::BinOpPowInt,
            InlineCache::BinOpDivFloat,
            InlineCache::BinOpDivFloat,
            InlineCache::BinOpFloorDivFloat,
            InlineCache::BinOpFloorDivFloat,
            InlineCache::BinOpModFloat,
            InlineCache::BinOpModFloat,
            InlineCache::BinOpPowFloat,
            InlineCache::BinOpPowFloat,
            InlineCache::CompareOpInt,
            InlineCache::CompareOpInt,
            InlineCache::CompareOpFloat,
            InlineCache::CompareOpFloat,
            InlineCache::CompareOpStr,
            InlineCache::CompareOpStr,
            InlineCache::LoadAttrInstance { key_idx: 0, ver: 0 },
            InlineCache::LoadAttrInstance {
                key_idx: u32::MAX,
                ver: u64::MAX,
            },
            InlineCache::LoadAttrModule {
                key_idx: 0,
                module_id: 0,
            },
            InlineCache::LoadAttrModule {
                key_idx: u32::MAX,
                module_id: u64::MAX,
            },
            InlineCache::LoadAttrSlot { key_idx: 0, ver: 0 },
            InlineCache::LoadAttrSlot {
                key_idx: u32::MAX,
                ver: u64::MAX,
            },
            InlineCache::LoadAttrType { key_idx: 0, ver: 0 },
            InlineCache::LoadAttrType {
                key_idx: u32::MAX,
                ver: u64::MAX,
            },
            InlineCache::LoadAttrMethod {
                mro_idx: 0,
                key_idx: 0,
                ver: 0,
            },
            InlineCache::LoadAttrMethod {
                mro_idx: u16::MAX,
                key_idx: u32::MAX,
                ver: u64::MAX,
            },
            InlineCache::LoadGlobalModule {
                key_idx: 0,
                globals_id: 0,
            },
            InlineCache::LoadGlobalModule {
                key_idx: u32::MAX,
                globals_id: u64::MAX,
            },
            InlineCache::LoadGlobalBuiltin {
                key_idx: 0,
                builtins_id: 0,
            },
            InlineCache::LoadGlobalBuiltin {
                key_idx: u32::MAX,
                builtins_id: u64::MAX,
            },
            InlineCache::StoreAttrInstance { key_idx: 0, ver: 0 },
            InlineCache::StoreAttrInstance {
                key_idx: u32::MAX,
                ver: u64::MAX,
            },
            InlineCache::StoreAttrSlot { key_idx: 0, ver: 0 },
            InlineCache::StoreAttrSlot {
                key_idx: u32::MAX,
                ver: u64::MAX,
            },
            InlineCache::StoreAttrNewKey { ver: 0 },
            InlineCache::StoreAttrNewKey { ver: u64::MAX },
            InlineCache::ForIterList,
            InlineCache::ForIterList,
            InlineCache::ForIterTuple,
            InlineCache::ForIterTuple,
            InlineCache::ForIterRange,
            InlineCache::ForIterRange,
            InlineCache::ForIterStr,
            InlineCache::ForIterStr,
            InlineCache::ForIterDict,
            InlineCache::ForIterDict,
            InlineCache::UnpackSequenceTuple,
            InlineCache::UnpackSequenceTuple,
            InlineCache::UnpackSequenceList,
            InlineCache::UnpackSequenceList,
            InlineCache::UnpackSequenceTwoTuple,
            InlineCache::UnpackSequenceTwoTuple,
            InlineCache::CallPyExactNoFree {
                argc: 0,
                func_id: 0,
            },
            InlineCache::CallPyExactNoFree {
                argc: u32::MAX,
                func_id: u64::MAX,
            },
            InlineCache::CallPyExact {
                argc: 0,
                func_id: 0,
            },
            InlineCache::CallPyExact {
                argc: u32::MAX,
                func_id: u64::MAX,
            },
            InlineCache::CallBoundMethodExact {
                argc: 0,
                func_id: 0,
            },
            InlineCache::CallBoundMethodExact {
                argc: u32::MAX,
                func_id: u64::MAX,
            },
            InlineCache::CallPyDefaults {
                argc: 0,
                func_id: 0,
            },
            InlineCache::CallPyDefaults {
                argc: u32::MAX,
                func_id: u64::MAX,
            },
            InlineCache::CallPyKwNames {
                argc: 0,
                kwc: 0,
                perm: 0,
                func_id: 0,
            },
            InlineCache::CallPyKwNames {
                argc: u8::MAX,
                kwc: u8::MAX,
                perm: u32::MAX,
                func_id: u64::MAX,
            },
            InlineCache::CallNative {
                argc: 0,
                func_id: 0,
            },
            InlineCache::CallNative {
                argc: u32::MAX,
                func_id: u64::MAX,
            },
            InlineCache::CallNativeMethod {
                argc: 0,
                func_id: 0,
            },
            InlineCache::CallNativeMethod {
                argc: u32::MAX,
                func_id: u64::MAX,
            },
            InlineCache::SubscrListInt,
            InlineCache::SubscrListInt,
            InlineCache::SubscrTupleInt,
            InlineCache::SubscrTupleInt,
            InlineCache::SubscrStrInt,
            InlineCache::SubscrStrInt,
            InlineCache::SubscrDict,
            InlineCache::SubscrDict,
            InlineCache::StoreSubscrListInt,
            InlineCache::StoreSubscrListInt,
            InlineCache::StoreSubscrDict,
            InlineCache::StoreSubscrDict,
            InlineCache::FuseLoadFastLoadFast,
            InlineCache::FuseLoadFastLoadFast,
            InlineCache::FuseLoadFastLoadConst,
            InlineCache::FuseLoadFastLoadConst,
            InlineCache::FuseLoadFastLoadAttr,
            InlineCache::FuseLoadFastLoadAttr,
            InlineCache::FuseCompareIntPopJump,
            InlineCache::FuseCompareIntPopJump,
            InlineCache::FuseBlocked,
            InlineCache::FuseBlocked,
        ] {
            assert_eq!(InlineCache::decode(value.encode()), value);
            let slot = CacheSlot::new(InlineCache::Empty);
            slot.set(value);
            assert_eq!(slot.get(), value);
        }
    }

    #[test]
    fn shared_slot_updates_publish_whole_values() {
        let a = InlineCache::LoadAttrMethod {
            mro_idx: u16::MAX,
            key_idx: 0,
            ver: u64::MAX,
        };
        let b = InlineCache::CallPyKwNames {
            argc: 3,
            kwc: 5,
            perm: u32::MAX,
            func_id: 1,
        };
        let slot = Arc::new(CacheSlot::new(a));
        let barrier = Arc::new(Barrier::new(4));
        std::thread::scope(|scope| {
            for value in [a, b, a, b] {
                let slot = Arc::clone(&slot);
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    barrier.wait();
                    for _ in 0..100 {
                        slot.set(value);
                        let observed = slot.get();
                        assert!(
                            observed == a || observed == b || observed == InlineCache::Empty,
                            "{observed:?}"
                        );
                    }
                });
            }
        });
    }
}
