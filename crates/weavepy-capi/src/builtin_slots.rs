//! C-level protocol slots for the built-in static types.
//!
//! Macro-heavy Cython reads a type's protocol suites **directly** off the
//! `PyTypeObject` struct rather than going through the abstract C-API. For
//! example `__Pyx_PyObject_GetItem` is
//!
//! ```c
//! PyMappingMethods *mm = Py_TYPE(obj)->tp_as_mapping;
//! if (mm && mm->mp_subscript) return mm->mp_subscript(obj, key);
//! PySequenceMethods *sm = Py_TYPE(obj)->tp_as_sequence;
//! if (sm && sm->sq_item) return __Pyx_PyObject_GetIndex(obj, key);
//! /* else: "'%.200s' object is not subscriptable" */
//! ```
//!
//! WeavePy's exported `PyList_Type`/`PyTuple_Type`/… historically left
//! `tp_as_sequence`/`tp_as_mapping` NULL (native dispatch happens in the VM,
//! which never reads these slots). The consequence: a Cython extension that
//! indexes / iterates / concatenates one of *our* built-in containers saw it
//! as "not subscriptable" even though the VM handles the operation. This is
//! exactly the gap `frozenlist` (`cdef list _items; … self._items[index]`)
//! tripped over.
//!
//! Each slot here is a thin `extern "C"` bridge that forwards to the
//! corresponding abstract entry point (`PyObject_GetItem`,
//! `PySequence_Concat`, …), which already dispatches through the VM on the
//! runtime `Object`. The bridges are therefore recursion-safe: the abstract
//! functions match on the Rust `Object` enum and never re-read these C
//! slots. The protocol-method suites are leaked once at init (immortal,
//! like the static types themselves).

use std::ffi::c_void;
use std::os::raw::c_int;

use crate::layout::{PyMappingMethods, PyNumberMethods, PySequenceMethods};
use crate::object::{PyObject, PySsizeT};
use crate::types::StaticType;

// ---------------------------------------------------------------------------
// Sequence-protocol bridges (`tp_as_sequence`).
// ---------------------------------------------------------------------------

unsafe extern "C" fn sq_length(o: *mut PyObject) -> PySsizeT {
    unsafe { crate::abstract_::PyObject_Length(o) }
}

unsafe extern "C" fn sq_concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abstract_::PySequence_Concat(a, b) }
}

unsafe extern "C" fn sq_repeat(o: *mut PyObject, n: PySsizeT) -> *mut PyObject {
    unsafe { crate::abstract_::PySequence_Repeat(o, n) }
}

unsafe extern "C" fn sq_item(o: *mut PyObject, i: PySsizeT) -> *mut PyObject {
    unsafe { crate::abstract_::PySequence_GetItem(o, i) }
}

/// `sq_ass_item` carries both assignment (`v != NULL`) and deletion
/// (`v == NULL`), matching CPython's `ssizeobjargproc` contract.
unsafe extern "C" fn sq_ass_item(o: *mut PyObject, i: PySsizeT, v: *mut PyObject) -> c_int {
    if v.is_null() {
        unsafe { crate::abstract_::PySequence_DelItem(o, i) }
    } else {
        unsafe { crate::abstract_::PySequence_SetItem(o, i, v) }
    }
}

unsafe extern "C" fn sq_contains(o: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe { crate::abstract_::PySequence_Contains(o, v) }
}

unsafe extern "C" fn sq_inplace_concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abstract_::PySequence_InPlaceConcat(a, b) }
}

unsafe extern "C" fn sq_inplace_repeat(o: *mut PyObject, n: PySsizeT) -> *mut PyObject {
    unsafe { crate::abstract_::PySequence_InPlaceRepeat(o, n) }
}

// ---------------------------------------------------------------------------
// Mapping-protocol bridges (`tp_as_mapping`).
// ---------------------------------------------------------------------------

unsafe extern "C" fn mp_length(o: *mut PyObject) -> PySsizeT {
    unsafe { crate::abstract_::PyObject_Length(o) }
}

unsafe extern "C" fn mp_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abstract_::PyObject_GetItem(o, k) }
}

/// `mp_ass_subscript` carries both assignment and deletion;
/// `PyObject_SetItem` already routes a NULL value to `PyObject_DelItem`.
unsafe extern "C" fn mp_ass_subscript(
    o: *mut PyObject,
    k: *mut PyObject,
    v: *mut PyObject,
) -> c_int {
    unsafe { crate::abstract_::PyObject_SetItem(o, k, v) }
}

/// `dict`'s `in` checks *keys*, not values; `PySequence_Contains` only knows
/// the linear sequence types, so dict gets a dedicated `sq_contains`.
unsafe extern "C" fn dict_sq_contains(o: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe { crate::containers::PyDict_Contains(o, v) }
}

// ---------------------------------------------------------------------------
// Number-protocol bridges (`tp_as_number`).
// ---------------------------------------------------------------------------
//
// Macro-heavy Cython reads a built-in number's conversion slots *directly*
// off `Py_TYPE(x)->tp_as_number` rather than through the abstract C-API. For
// example `<int64_t>x` compiles to `__Pyx_PyNumber_IntOrLong(x)`, which reads
// `Py_TYPE(x)->tp_as_number->nb_int`; a NULL suite makes it raise
// "an integer is required". pandas' `Timedelta("1 day")` parser casts a
// Python `float` to `<int64_t>` this way (`cast_from_unit` in
// `conversion.pyx`), so a WeavePy `float` with no `nb_int` broke every
// "<N> <unit>" timedelta string (while the `hh:mm:ss` branch, which casts a
// pre-built `int`, worked).
//
// Only conversion/inquiry/unary slots are wired. Their contract is "return a
// result or raise" — none of the "return `NotImplemented`" dance the *binary*
// arithmetic slots require. Each bridge forwards to an abstract entry point
// that resolves a *native* operand entirely in Rust and never re-reads these
// C slots, so the forward is recursion-safe: `abstract_::binop` only consults
// C `nb_*` slots when an operand is *foreign*, which a WeavePy int / float /
// complex / bool never is.

unsafe extern "C" fn nb_int(o: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abstract_::PyNumber_Long(o) }
}

unsafe extern "C" fn nb_index(o: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abstract_::PyNumber_Index(o) }
}

unsafe extern "C" fn nb_float(o: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abstract_::PyNumber_Float(o) }
}

unsafe extern "C" fn nb_bool(o: *mut PyObject) -> c_int {
    unsafe { crate::abstract_::PyObject_IsTrue(o) }
}

unsafe extern "C" fn nb_negative(o: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abstract_::PyNumber_Negative(o) }
}

unsafe extern "C" fn nb_positive(o: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abstract_::PyNumber_Positive(o) }
}

unsafe extern "C" fn nb_absolute(o: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abstract_::PyNumber_Absolute(o) }
}

/// Which number slots a given built-in advertises. `nb_bool` is always
/// installed; the rest mirror CPython's per-type `PyNumberMethods` (e.g.
/// `float` has no `nb_index`, `complex` has neither `nb_int`/`nb_float`
/// /`nb_index` nor `nb_negative`/`nb_absolute` — the latter aren't yet
/// resolvable natively by `PyNumber_Negative`/`PyNumber_Absolute`).
#[derive(Clone, Copy, Default)]
struct NumSpec {
    int: bool,
    index: bool,
    float_: bool,
    unary: bool,
    /// The numeric-tower binary ops shared by `int`/`float`/`complex`
    /// (`+ - * % // / **`). Macro-heavy Cython reads these straight off
    /// `Py_TYPE(x)->tp_as_number` and calls them directly — e.g. the
    /// overflow fallback of `__Pyx_PyInt_MultiplyObjC` invokes
    /// `nb_multiply`, so a NULL slot is a hard `blr NULL` crash.
    arith: bool,
    /// Bitwise / shift ops (`<< >> & | ^`), present on `int`/`bool` only.
    bitwise: bool,
}

/// Build, leak, and attach a `PyNumberMethods` suite to `ty`.
unsafe fn install_number(ty: &StaticType, spec: NumSpec) {
    use crate::abstract_ as ab;
    let mut n: PyNumberMethods = unsafe { std::mem::zeroed() };
    n.nb_bool = nb_bool as *mut c_void;
    if spec.int {
        n.nb_int = nb_int as *mut c_void;
    }
    if spec.index {
        n.nb_index = nb_index as *mut c_void;
    }
    if spec.float_ {
        n.nb_float = nb_float as *mut c_void;
    }
    if spec.unary {
        n.nb_negative = nb_negative as *mut c_void;
        n.nb_positive = nb_positive as *mut c_void;
        n.nb_absolute = nb_absolute as *mut c_void;
    }
    if spec.arith {
        n.nb_add = ab::nb_slot_add as *mut c_void;
        n.nb_subtract = ab::nb_slot_subtract as *mut c_void;
        n.nb_multiply = ab::nb_slot_multiply as *mut c_void;
        n.nb_remainder = ab::nb_slot_remainder as *mut c_void;
        n.nb_floor_divide = ab::nb_slot_floor_divide as *mut c_void;
        n.nb_true_divide = ab::nb_slot_true_divide as *mut c_void;
        n.nb_power = ab::nb_slot_power as *mut c_void;
    }
    if spec.bitwise {
        n.nb_lshift = ab::nb_slot_lshift as *mut c_void;
        n.nb_rshift = ab::nb_slot_rshift as *mut c_void;
        n.nb_and = ab::nb_slot_and as *mut c_void;
        n.nb_or = ab::nb_slot_or as *mut c_void;
        n.nb_xor = ab::nb_slot_xor as *mut c_void;
    }
    unsafe {
        (*ty.as_ptr()).tp_as_number = Box::into_raw(Box::new(n)) as *mut c_void;
    }
}

/// Populate `tp_as_number` on the exported built-in numeric static types.
/// Called from [`crate::types::init_static_types`] alongside [`install`].
pub fn install_numbers() {
    use crate::types::{PyBool_Type, PyComplex_Type, PyFloat_Type, PyLong_Type};
    unsafe {
        // int: full conversion surface + unary sign/abs + the complete
        // arithmetic and bitwise binary suites.
        install_number(
            &PyLong_Type,
            NumSpec {
                int: true,
                index: true,
                float_: true,
                unary: true,
                arith: true,
                bitwise: true,
            },
        );
        // float: int/float conversion + unary + numeric-tower arithmetic;
        // no `__index__` (CPython deliberately omits it so a float can't
        // be used as an index) and no bitwise ops.
        install_number(
            &PyFloat_Type,
            NumSpec {
                int: true,
                index: false,
                float_: true,
                unary: true,
                arith: true,
                bitwise: false,
            },
        );
        // bool (an int subclass): conversion surface + full int-style
        // arithmetic/bitwise binary suites (so a direct `nb_*` slot read
        // on `True`/`False` never hits NULL).
        install_number(
            &PyBool_Type,
            NumSpec {
                int: true,
                index: true,
                float_: true,
                unary: false,
                arith: true,
                bitwise: true,
            },
        );
        // complex: only truthiness is resolvable natively today.
        install_number(&PyComplex_Type, NumSpec::default());
    }
}

// ---------------------------------------------------------------------------
// Type-level bridges.
// ---------------------------------------------------------------------------

unsafe extern "C" fn tp_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abstract_::PyObject_GetIter(o) }
}

/// `tp_iter` for an *iterator*: CPython's iterator types return `self`
/// (incref) from `__iter__` so `iter(it) is it`. Cython's `for` loop and
/// `iter()` codegen rely on this identity.
unsafe extern "C" fn iter_self(o: *mut PyObject) -> *mut PyObject {
    if !o.is_null() {
        unsafe { crate::object::Py_IncRef(o) };
    }
    o
}

/// `tp_iternext` bridge: forwards to [`crate::abstract_::PyIter_Next`],
/// which advances the shared `Object::Iter` cursor and returns NULL with
/// **no** exception set on normal exhaustion — exactly the `tp_iternext`
/// contract Cython's `__Pyx_PyIter_Next` / `for`-loop codegen expects.
unsafe extern "C" fn tp_iternext_bridge(o: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abstract_::PyIter_Next(o) }
}

/// Wire a type as a faithful iterator: `tp_iter` returns self and
/// `tp_iternext` drives WeavePy's iteration. Used for `PySeqIter_Type`
/// (the `Object::Iter` umbrella) and the generator type.
pub unsafe fn install_iterator(ty: &StaticType) {
    unsafe {
        (*ty.as_ptr()).tp_iter = iter_self as *mut c_void;
        (*ty.as_ptr()).tp_iternext = tp_iternext_bridge as *mut c_void;
    }
}

// ---------------------------------------------------------------------------
// Suite construction + installation.
// ---------------------------------------------------------------------------

/// Which sequence slots a given built-in advertises. Mirrors CPython's
/// per-type `PySequenceMethods` (e.g. `tuple` has no `sq_ass_item`, `range`
/// has no `sq_concat`/`sq_repeat`).
#[derive(Clone, Copy, Default)]
struct SeqSpec {
    length: bool,
    concat: bool,
    repeat: bool,
    item: bool,
    ass_item: bool,
    contains: bool,
    inplace: bool,
}

#[derive(Clone, Copy, Default)]
struct MapSpec {
    length: bool,
    subscript: bool,
    ass_subscript: bool,
}

/// Build, leak, and attach a `PySequenceMethods` suite to `ty`.
unsafe fn install_sequence(ty: &StaticType, spec: SeqSpec) {
    let mut s: PySequenceMethods = unsafe { std::mem::zeroed() };
    if spec.length {
        s.sq_length = sq_length as *mut c_void;
    }
    if spec.concat {
        s.sq_concat = sq_concat as *mut c_void;
    }
    if spec.repeat {
        s.sq_repeat = sq_repeat as *mut c_void;
    }
    if spec.item {
        s.sq_item = sq_item as *mut c_void;
    }
    if spec.ass_item {
        s.sq_ass_item = sq_ass_item as *mut c_void;
    }
    if spec.contains {
        s.sq_contains = sq_contains as *mut c_void;
    }
    if spec.inplace {
        s.sq_inplace_concat = sq_inplace_concat as *mut c_void;
        s.sq_inplace_repeat = sq_inplace_repeat as *mut c_void;
    }
    unsafe {
        (*ty.as_ptr()).tp_as_sequence = Box::into_raw(Box::new(s)) as *mut c_void;
    }
}

/// Build, leak, and attach a `PyMappingMethods` suite to `ty`.
unsafe fn install_mapping(ty: &StaticType, spec: MapSpec) {
    let mut m: PyMappingMethods = unsafe { std::mem::zeroed() };
    if spec.length {
        m.mp_length = mp_length as *mut c_void;
    }
    if spec.subscript {
        m.mp_subscript = mp_subscript as *mut c_void;
    }
    if spec.ass_subscript {
        m.mp_ass_subscript = mp_ass_subscript as *mut c_void;
    }
    unsafe {
        (*ty.as_ptr()).tp_as_mapping = Box::into_raw(Box::new(m)) as *mut c_void;
    }
}

unsafe fn set_iter(ty: &StaticType) {
    unsafe {
        (*ty.as_ptr()).tp_iter = tp_iter as *mut c_void;
    }
}

/// Populate the protocol slots on the exported built-in static types.
/// Called from [`crate::types::init_static_types`] after the type table and
/// faithful `tp_new`s are in place. Idempotent in practice (init runs once
/// under a lock); each call leaks a fresh suite, so it must not be invoked
/// repeatedly.
pub fn install() {
    use crate::types::{
        PyByteArray_Type, PyBytes_Type, PyDict_Type, PyFrozenSet_Type, PyList_Type, PyRange_Type,
        PySet_Type, PyTuple_Type, PyUnicode_Type,
    };

    // Mutable sequences: the full read/write/in-place surface.
    let mutable_seq = SeqSpec {
        length: true,
        concat: true,
        repeat: true,
        item: true,
        ass_item: true,
        contains: true,
        inplace: true,
    };
    let mutable_map = MapSpec {
        length: true,
        subscript: true,
        ass_subscript: true,
    };

    // Immutable sequences: read-only indexing + concat/repeat.
    let immutable_seq = SeqSpec {
        length: true,
        concat: true,
        repeat: true,
        item: true,
        ass_item: false,
        contains: true,
        inplace: false,
    };
    let immutable_map = MapSpec {
        length: true,
        subscript: true,
        ass_subscript: false,
    };

    unsafe {
        // list / bytearray — mutable.
        install_sequence(&PyList_Type, mutable_seq);
        install_mapping(&PyList_Type, mutable_map);
        set_iter(&PyList_Type);

        install_sequence(&PyByteArray_Type, mutable_seq);
        install_mapping(&PyByteArray_Type, mutable_map);
        set_iter(&PyByteArray_Type);

        // tuple / str / bytes — immutable sequences.
        install_sequence(&PyTuple_Type, immutable_seq);
        install_mapping(&PyTuple_Type, immutable_map);
        set_iter(&PyTuple_Type);

        install_sequence(&PyUnicode_Type, immutable_seq);
        install_mapping(&PyUnicode_Type, immutable_map);
        set_iter(&PyUnicode_Type);

        install_sequence(&PyBytes_Type, immutable_seq);
        install_mapping(&PyBytes_Type, immutable_map);
        set_iter(&PyBytes_Type);

        // range — read-only length + indexing, no concat/repeat.
        install_sequence(
            &PyRange_Type,
            SeqSpec {
                length: true,
                item: true,
                contains: true,
                ..Default::default()
            },
        );
        install_mapping(&PyRange_Type, immutable_map);
        set_iter(&PyRange_Type);

        // dict — mapping protocol; `in` checks keys via a dedicated contains.
        install_mapping(&PyDict_Type, mutable_map);
        let mut dseq: PySequenceMethods = std::mem::zeroed();
        dseq.sq_contains = dict_sq_contains as *mut c_void;
        (*PyDict_Type.as_ptr()).tp_as_sequence = Box::into_raw(Box::new(dseq)) as *mut c_void;
        set_iter(&PyDict_Type);

        // set / frozenset — length + membership + iteration (the numeric
        // set algebra `|`/`&`/`^`/`-` routes through `PyNumber_*` → the VM,
        // which needs no `nb_*` slot for native operands).
        let set_seq = SeqSpec {
            length: true,
            contains: true,
            ..Default::default()
        };
        install_sequence(&PySet_Type, set_seq);
        set_iter(&PySet_Type);
        install_sequence(&PyFrozenSet_Type, set_seq);
        set_iter(&PyFrozenSet_Type);
    }
}
