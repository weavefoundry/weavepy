//! The native-call ABI: the `#[repr(C)]` [`JitFrame`] the VM fills
//! before entering compiled code and reads after it exits, plus the
//! side-exit status protocol.
//!
//! A compiled frame is a single native function with the signature
//!
//! ```text
//! extern "C" fn(frame: *mut JitFrame) -> i64   // an i64 JitStatus
//! ```
//!
//! On a [`JitStatus::Returned`] exit the function has written
//! [`JitFrame::ret_bits`] / [`JitFrame::ret_tag`]. On a
//! [`JitStatus::Deopt`] exit it has written [`JitFrame::deopt_pc`] and
//! spilled the live abstract operand stack into
//! [`JitFrame::stack_spill`] / [`JitFrame::stack_tags`] (bottom-to-top)
//! with [`JitFrame::stack_len`] entries, plus written back every
//! JIT-managed local into [`JitFrame::locals`]. The VM then rebuilds its
//! interpreter state and resumes at `deopt_pc`, bit-for-bit as though
//! the JIT had never run.

/// The status returned (as an `i64`) by a compiled frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum JitStatus {
    /// The frame ran to a `RETURN_VALUE`. The return value is in
    /// [`JitFrame::ret_bits`] / [`JitFrame::ret_tag`].
    Returned = 0,
    /// The frame took a side exit. The VM resumes interpretation at
    /// [`JitFrame::deopt_pc`] with the spilled stack + written-back
    /// locals.
    Deopt = 1,
    /// RFC 0059 WS3 — a native Python-to-Python call raised. The frame
    /// state is written back exactly as for [`JitStatus::Deopt`] (with
    /// the call's operands already consumed), [`JitFrame::deopt_pc`]
    /// names the `CALL` instruction for traceback attribution, and the
    /// exception itself travels through the embedder's side channel
    /// (the `wpjit_call_py` helper parked it before returning its
    /// raised status).
    Raised = 2,
}

impl JitStatus {
    /// Decode the raw `i64` a compiled frame returns.
    #[inline]
    #[must_use]
    pub fn from_raw(v: i64) -> JitStatus {
        match v {
            0 => JitStatus::Returned,
            2 => JitStatus::Raised,
            _ => JitStatus::Deopt,
        }
    }
}

/// Status codes the embedder's `wpjit_call_py` helper returns to native
/// code (RFC 0059 WS3). Distinct from [`JitStatus`]: this is the
/// per-*call* protocol, which the compiled code translates into either
/// a pushed result, a `Deopt` exit (representation/guard trouble), or a
/// `Raised` exit (exception propagation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum CallStatus {
    /// The callee returned a scalar; `out_bits`/`out_tag` hold it.
    Ok = 0,
    /// The callee raised; the embedder parked the exception. The caller
    /// must take its `Raised` exit at the call's pc.
    Raised = 1,
    /// The callee returned a value native code cannot represent (or a
    /// caller guard no longer holds); the embedder parked the *result*
    /// and set `out_tag` to [`SlotTag::Boxed`]. The caller must deopt
    /// *after* the call with the result spilled — the call must never
    /// re-execute.
    Boxed = 2,
}

/// How to interpret a `u64` slot in [`JitFrame::locals`] /
/// [`JitFrame::stack_spill`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum SlotTag {
    /// `i64` bit pattern → `Object::Int`.
    Int = 0,
    /// `f64` bit pattern (via `to_bits`) → `Object::Float`.
    Float = 1,
    /// `0`/`1` → `Object::Bool`.
    Bool = 2,
    /// RFC 0059 WS3 — the value is a full Python object parked in the
    /// embedder's side channel (a native call's unrepresentable
    /// result). Only ever appears in a deopt spill, never in locals.
    Boxed = 3,
    /// RFC 0061 WS5 — the value is an index into the embedder's
    /// per-entry pinned-object table (a pinned `list`). The embedder
    /// rebuilds the real object from the table on deopt/return.
    ListPin = 4,
    /// RFC 0065 WS5 — the value is an index into the embedder's
    /// per-entry pinned-object table (a pinned *instance* receiver).
    /// Same reconstruction contract as [`SlotTag::ListPin`].
    ObjPin = 5,
}

impl SlotTag {
    /// Decode a raw tag written by native code.
    #[inline]
    #[must_use]
    pub fn from_raw(v: u32) -> SlotTag {
        match v {
            1 => SlotTag::Float,
            2 => SlotTag::Bool,
            3 => SlotTag::Boxed,
            4 => SlotTag::ListPin,
            5 => SlotTag::ObjPin,
            _ => SlotTag::Int,
        }
    }
}

/// The exchange buffer the VM passes to a compiled frame.
///
/// The VM owns the backing storage (`Vec<u64>` / `Vec<u32>`); this
/// struct holds raw pointers to it for the duration of one native call.
/// All indices the native code touches are bounded by `n_locals` /
/// `stack_cap`, which the VM sizes from the compiled frame's analysis.
#[repr(C)]
#[derive(Debug)]
pub struct JitFrame {
    /// Slot-indexed local storage, one `u64` per code-object local.
    /// Holds `i64` / `f64`-bits / `bool` per the local's stable type.
    pub locals: *mut u64,
    /// Number of valid entries in [`Self::locals`].
    pub n_locals: u32,
    /// OSR entry: the bytecode pc to begin execution at. `0` enters at
    /// the function start; a recognized loop-header pc enters mid-frame
    /// through the entry dispatch (RFC 0059 WS3b).
    pub entry_pc: u32,

    /// `Returned`: the return value's bit pattern. Also serves as the
    /// out-slot the `wpjit_call_py` helper writes a call result into
    /// (it is dead between calls and only meaningful at `Returned`).
    pub ret_bits: u64,
    /// `Returned`: the return value's [`SlotTag`]. Doubles as the call
    /// helper's out-tag, as above.
    pub ret_tag: u32,

    /// `Deopt`: the bytecode pc to resume interpretation at.
    pub deopt_pc: u32,
    /// `Deopt`: spilled abstract operand stack, bottom-to-top.
    pub stack_spill: *mut u64,
    /// `Deopt`: matching [`SlotTag`]s for [`Self::stack_spill`].
    pub stack_tags: *mut u32,
    /// `Deopt`: number of spilled stack entries.
    pub stack_len: u32,
    /// Capacity of [`Self::stack_spill`] / [`Self::stack_tags`].
    pub stack_cap: u32,

    /// RFC 0059 WS3 — opaque embedder context for the `wpjit_call_py`
    /// helper (the VM's per-activation `CallCtx`: interpreter pointer,
    /// callee table, caller guards). Null when the frame makes no calls.
    pub ctx: *mut u8,
    /// Argument marshal buffer for native Python-to-Python calls, at
    /// least `max_call_args` wide.
    pub call_args: *mut u64,
    /// Matching [`SlotTag`]s for [`Self::call_args`].
    pub call_tags: *mut u32,
}

impl JitFrame {
    /// Reinterpret an `f64` as the `u64` stored in a slot.
    #[inline]
    #[must_use]
    pub fn f64_to_bits(v: f64) -> u64 {
        v.to_bits()
    }

    /// Reinterpret a slot's `u64` as the `f64` it encodes.
    #[inline]
    #[must_use]
    pub fn bits_to_f64(bits: u64) -> f64 {
        f64::from_bits(bits)
    }
}

/// The embedder's Python-to-Python call helper (RFC 0059 WS3). Compiled
/// code marshals the arguments into [`JitFrame::call_args`] /
/// [`JitFrame::call_tags`] (bottom-to-top), then calls this with the
/// callee-table `token`, the argument count, and the [`SlotTag`] the
/// caller expects back. The helper performs the full call through the
/// interpreter and returns a [`CallStatus`]; on `Ok` it has written the
/// result into [`JitFrame::ret_bits`] / [`JitFrame::ret_tag`].
///
/// # Safety contract (for implementors)
///
/// `frame` is the same pointer the native function was entered with; it
/// and its buffers stay valid for the whole native activation. The
/// helper may run arbitrary Python (including re-entering compiled
/// code) but must not unwind across the FFI boundary.
pub type CallPyHelper =
    unsafe extern "C" fn(frame: *mut JitFrame, token: u32, argc: u32, expect_tag: u32) -> i64;

/// The registered [`CallPyHelper`], as a `usize` so lowering can burn it
/// into compiled code as an absolute address. `0` = not registered
/// (frames containing calls then refuse to compile).
static CALL_PY_HELPER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Register the process-wide Python-call helper. Must be called before
/// the first frame containing a `CallPy` is compiled; later calls must
/// pass the same function (compiled code holds burned-in addresses).
pub fn register_call_py_helper(helper: CallPyHelper) {
    CALL_PY_HELPER.store(helper as usize, std::sync::atomic::Ordering::Release);
}

/// The registered helper's address, or 0 when absent.
#[must_use]
pub(crate) fn call_py_helper_addr() -> usize {
    CALL_PY_HELPER.load(std::sync::atomic::Ordering::Acquire)
}

/// RFC 0061 WS5 — the embedder's pinned-list *read* helper. `pin`
/// indexes the per-entry pinned-object table on the embedder context;
/// `idx` is the (possibly negative) Python index. Returns `0` (Ok) with
/// the element's bits written into [`JitFrame::ret_bits`], or non-zero
/// when the access must deopt (out of range, or the element no longer
/// matches the pinned lane — aliased mutation through a callee).
///
/// # Safety contract (for implementors)
///
/// Same as [`CallPyHelper`]: `frame`/`ctx` are the live buffers of the
/// current native activation. The helper must not run Python code and
/// must not unwind across the FFI boundary.
pub type ListGetHelper = unsafe extern "C" fn(frame: *mut JitFrame, pin: i64, idx: i64) -> i64;

/// RFC 0061 WS5 — the embedder's pinned-list *write* helper. The value
/// to store is pre-staged in [`JitFrame::ret_bits`] (interpreted per
/// the pin's element lane); returns `0` (Ok) or non-zero to deopt
/// (out of range). Same safety contract as [`ListGetHelper`].
pub type ListSetHelper = unsafe extern "C" fn(frame: *mut JitFrame, pin: i64, idx: i64) -> i64;

static LIST_GET_HELPER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static LIST_SET_HELPER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Register the process-wide pinned-list helpers (RFC 0061 WS5). Must
/// precede the first compile of a frame containing list ops; later
/// calls must pass the same functions.
pub fn register_list_helpers(get: ListGetHelper, set: ListSetHelper) {
    LIST_GET_HELPER.store(get as usize, std::sync::atomic::Ordering::Release);
    LIST_SET_HELPER.store(set as usize, std::sync::atomic::Ordering::Release);
}

#[must_use]
pub(crate) fn list_get_helper_addr() -> usize {
    LIST_GET_HELPER.load(std::sync::atomic::Ordering::Acquire)
}

#[must_use]
pub(crate) fn list_set_helper_addr() -> usize {
    LIST_SET_HELPER.load(std::sync::atomic::Ordering::Acquire)
}

/// RFC 0065 WS5 — the embedder's pinned-list *length* helper. Returns
/// the list's length (always `>= 0`), or a negative value on a
/// pin-table miss (defensive — deopts). Never runs Python code and
/// never drops a heap object; same safety contract as
/// [`ListGetHelper`].
pub type ListLenHelper = unsafe extern "C" fn(frame: *mut JitFrame, pin: i64) -> i64;

/// RFC 0065 WS5 — the embedder's pinned-list *append* helper. The
/// value to append is pre-staged in [`JitFrame::ret_bits`],
/// interpreted per the pin's element lane; returns `0` (Ok) or
/// non-zero to deopt (defensive). Same safety contract as
/// [`ListGetHelper`].
pub type ListAppendHelper = unsafe extern "C" fn(frame: *mut JitFrame, pin: i64) -> i64;

static LIST_LEN_HELPER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static LIST_APPEND_HELPER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Register the process-wide pinned-list length/append helpers
/// (RFC 0065 WS5). Must precede the first compile of a frame
/// containing `ListLen`/`ListAppend` ops.
pub fn register_list_extra_helpers(len: ListLenHelper, append: ListAppendHelper) {
    LIST_LEN_HELPER.store(len as usize, std::sync::atomic::Ordering::Release);
    LIST_APPEND_HELPER.store(append as usize, std::sync::atomic::Ordering::Release);
}

#[must_use]
pub(crate) fn list_len_helper_addr() -> usize {
    LIST_LEN_HELPER.load(std::sync::atomic::Ordering::Acquire)
}

#[must_use]
pub(crate) fn list_append_helper_addr() -> usize {
    LIST_APPEND_HELPER.load(std::sync::atomic::Ordering::Acquire)
}

/// RFC 0065 WS5 — the embedder's pinned-instance attribute *read*
/// helper. `pin` indexes the pinned-object table; `site` indexes the
/// compiled frame's attribute-site table (name, class fingerprint,
/// dict index, value lane). Returns `0` (Ok) with the value's bits in
/// [`JitFrame::ret_bits`], or non-zero to deopt (class changed, dict
/// reshaped, value left its lane). Never runs Python code; same
/// safety contract as [`CallPyHelper`].
pub type AttrGetHelper = unsafe extern "C" fn(frame: *mut JitFrame, pin: i64, site: i64) -> i64;

/// RFC 0065 WS5 — the embedder's pinned-instance attribute *write*
/// helper. The value is pre-staged in [`JitFrame::ret_bits`]
/// (interpreted per the site's lane); returns `0` (Ok) or non-zero to
/// deopt — including when the *displaced* value is a heap object,
/// whose drop belongs to the interpreter's store path. Same safety
/// contract as [`AttrGetHelper`].
pub type AttrSetHelper = unsafe extern "C" fn(frame: *mut JitFrame, pin: i64, site: i64) -> i64;

static ATTR_GET_HELPER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static ATTR_SET_HELPER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Register the process-wide pinned-instance attribute helpers
/// (RFC 0065 WS5). Must precede the first compile of a frame
/// containing `AttrGet`/`AttrSet` ops.
pub fn register_attr_helpers(get: AttrGetHelper, set: AttrSetHelper) {
    ATTR_GET_HELPER.store(get as usize, std::sync::atomic::Ordering::Release);
    ATTR_SET_HELPER.store(set as usize, std::sync::atomic::Ordering::Release);
}

#[must_use]
pub(crate) fn attr_get_helper_addr() -> usize {
    ATTR_GET_HELPER.load(std::sync::atomic::Ordering::Acquire)
}

#[must_use]
pub(crate) fn attr_set_helper_addr() -> usize {
    ATTR_SET_HELPER.load(std::sync::atomic::Ordering::Acquire)
}
