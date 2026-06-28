//! `PyTypeObject` and the `PyType_FromSpec` family.
//!
//! The C-API bridges WeavePy's `Rc<TypeObject>` (the native type
//! representation) to the C surface CPython exposes. From C the
//! type is just a pointer to a `PyObject` whose `ob_type` is
//! `PyType_Type`; from Rust we hold an `Rc<TypeObject>` so the
//! native type machinery (MRO, `__dict__`, `lookup`) keeps working.
//!
//! Three flavours of types live in this crate:
//!
//! 1. **Static built-ins** (`PyType_Type`, `PyLong_Type`,
//!    `PyUnicode_Type`, …). One static [`PyTypeObject`] per type;
//!    refcount is immortal; `bridge` points at a thread-local
//!    `Rc<TypeObject>` cloned from `BuiltinTypes` at startup.
//! 2. **Heap types from `PyType_FromSpec`**. The spec is interpreted
//!    at the call site, a fresh `Rc<TypeObject>` is built, and a
//!    heap-allocated [`PyTypeObjectBox`] is returned to the
//!    extension.
//! 3. **Capsule / module / NotImplemented types**. Single-instance
//!    types whose only role is to give their (one) instance a
//!    distinct `ob_type`.

use std::cell::UnsafeCell;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;
use std::sync::Mutex;
use weavepy_vm::sync::Rc;

use weavepy_vm::object::{DictData, DictKey, Object};
use weavepy_vm::types::TypeObject;

use crate::object::{PyObject, PySsizeT, IMMORTAL_REFCNT};
use crate::slottable::SlotTable;

/// Layout of a type object as the C side sees it.
///
/// As of RFC 0043 (wave 1) this is **byte-faithful to CPython 3.13's
/// full `PyTypeObject`** for the entire documented prefix (offsets `0`
/// through `416`, i.e. `ob_base` … `tp_vectorcall` + the
/// `tp_watched`/`tp_versions_used` tail). The exact field offsets are
/// pinned, and machine-checked against the host's stock headers, in
/// [`crate::layout::PyTypeObjectFull`]; `debug_assert_type_layout`
/// cross-checks this live struct against that spec.
///
/// WeavePy's *private* bookkeeping (`tp_slots`, the native `bridge`)
/// lives **after** the 416-byte faithful region. Because CPython types
/// are variable-size (`tp_itemsize`) and extensions only ever read the
/// documented slots, those trailing fields are invisible to stock code
/// while letting native dispatch keep its `Rc<TypeObject>`.
#[repr(C)]
pub struct PyTypeObject {
    // --- byte-faithful CPython 3.13 prefix (offsets 0..416) ---
    pub head: PyObject, // 0
    /// `PyVarObject.ob_size`. Types are var-objects; this is 0 for the
    /// built-ins (CPython keeps it 0 too).
    pub ob_size: PySsizeT, // 16
    /// Type's qualified name (`module.Name` for heap types).
    pub tp_name: *const c_char, // 24
    pub tp_basicsize: PySsizeT, // 32
    pub tp_itemsize: PySsizeT, // 40
    /// Instance destructor. Stock inlined `Py_DECREF` → `_Py_Dealloc`
    /// reads this slot, so it must sit at offset 48 and be valid: for
    /// faithful built-ins it routes to the mirror/box free path.
    pub tp_dealloc: Option<crate::layout::destructor>, // 48
    pub tp_vectorcall_offset: PySsizeT, // 56
    pub tp_getattr: *mut c_void, // 64
    pub tp_setattr: *mut c_void, // 72
    pub tp_as_async: *mut c_void, // 80
    pub tp_repr: *mut c_void, // 88
    pub tp_as_number: *mut c_void, // 96
    pub tp_as_sequence: *mut c_void, // 104
    pub tp_as_mapping: *mut c_void, // 112
    pub tp_hash: *mut c_void, // 120
    pub tp_call: *mut c_void, // 128
    pub tp_str: *mut c_void, // 136
    pub tp_getattro: *mut c_void, // 144
    pub tp_setattro: *mut c_void, // 152
    pub tp_as_buffer: *mut c_void, // 160
    /// CPython `unsigned long tp_flags`. 64-bit, at offset 168.
    pub tp_flags: u64, // 168
    pub tp_doc: *const c_char, // 176
    pub tp_traverse: *mut c_void, // 184
    pub tp_clear: *mut c_void, // 192
    pub tp_richcompare: *mut c_void, // 200
    pub tp_weaklistoffset: PySsizeT, // 208
    pub tp_iter: *mut c_void, // 216
    pub tp_iternext: *mut c_void, // 224
    pub tp_methods: *mut c_void, // 232
    pub tp_members: *mut c_void, // 240
    pub tp_getset: *mut c_void, // 248
    pub tp_base: *mut PyTypeObject, // 256
    pub tp_dict: *mut PyObject, // 264
    pub tp_descr_get: *mut c_void, // 272
    pub tp_descr_set: *mut c_void, // 280
    pub tp_dictoffset: PySsizeT, // 288
    pub tp_init: *mut c_void, // 296
    pub tp_alloc: *mut c_void, // 304
    pub tp_new: *mut c_void, // 312
    pub tp_free: *mut c_void, // 320
    pub tp_is_gc: *mut c_void, // 328
    pub tp_bases: *mut PyObject, // 336
    pub tp_mro: *mut PyObject, // 344
    pub tp_cache: *mut PyObject, // 352
    pub tp_subclasses: *mut c_void, // 360
    pub tp_weaklist: *mut PyObject, // 368
    pub tp_del: *mut c_void, // 376
    pub tp_version_tag: u64, // 384 (unsigned int + pad)
    pub tp_finalize: *mut c_void, // 392
    pub tp_vectorcall: *mut c_void, // 400
    /// `unsigned char tp_watched` + `uint16_t tp_versions_used` + pad.
    pub tp_tail: [u8; 8], // 408
    // --- WeavePy private fields (offset >= 416, invisible to C) ---
    /// Extension-supplied [`crate::ffi::PyType_Slot`] table, or null.
    pub tp_slots: *mut PyType_Slot,
    /// Bridge to the WeavePy native type. Boxed `Rc<TypeObject>`.
    pub bridge: *mut Rc<TypeObject>,
    _filler: [usize; 2],
}

unsafe impl Sync for PyTypeObject {}

impl PyTypeObject {
    /// A fully-zeroed faithful type with only the head set. Used as the
    /// `..` base for the initialisers so each site spells out just the
    /// fields it cares about.
    pub const fn new_zeroed() -> Self {
        PyTypeObject {
            head: PyObject {
                ob_refcnt: IMMORTAL_REFCNT,
                ob_type: ptr::null_mut(),
            },
            ob_size: 0,
            tp_name: ptr::null(),
            tp_basicsize: 0,
            tp_itemsize: 0,
            tp_dealloc: None,
            tp_vectorcall_offset: 0,
            tp_getattr: ptr::null_mut(),
            tp_setattr: ptr::null_mut(),
            tp_as_async: ptr::null_mut(),
            tp_repr: ptr::null_mut(),
            tp_as_number: ptr::null_mut(),
            tp_as_sequence: ptr::null_mut(),
            tp_as_mapping: ptr::null_mut(),
            tp_hash: ptr::null_mut(),
            tp_call: ptr::null_mut(),
            tp_str: ptr::null_mut(),
            tp_getattro: ptr::null_mut(),
            tp_setattro: ptr::null_mut(),
            tp_as_buffer: ptr::null_mut(),
            tp_flags: 0,
            tp_doc: ptr::null(),
            tp_traverse: ptr::null_mut(),
            tp_clear: ptr::null_mut(),
            tp_richcompare: ptr::null_mut(),
            tp_weaklistoffset: 0,
            tp_iter: ptr::null_mut(),
            tp_iternext: ptr::null_mut(),
            tp_methods: ptr::null_mut(),
            tp_members: ptr::null_mut(),
            tp_getset: ptr::null_mut(),
            tp_base: ptr::null_mut(),
            tp_dict: ptr::null_mut(),
            tp_descr_get: ptr::null_mut(),
            tp_descr_set: ptr::null_mut(),
            tp_dictoffset: 0,
            tp_init: ptr::null_mut(),
            tp_alloc: ptr::null_mut(),
            tp_new: ptr::null_mut(),
            tp_free: ptr::null_mut(),
            tp_is_gc: ptr::null_mut(),
            tp_bases: ptr::null_mut(),
            tp_mro: ptr::null_mut(),
            tp_cache: ptr::null_mut(),
            tp_subclasses: ptr::null_mut(),
            tp_weaklist: ptr::null_mut(),
            tp_del: ptr::null_mut(),
            tp_version_tag: 0,
            tp_finalize: ptr::null_mut(),
            tp_vectorcall: ptr::null_mut(),
            tp_tail: [0; 8],
            tp_slots: ptr::null_mut(),
            bridge: ptr::null_mut(),
            _filler: [0; 2],
        }
    }
}

/// Cross-check the live [`PyTypeObject`] against the machine-checked
/// faithful spec in [`crate::layout`]. Compile-time; zero runtime cost.
const _: () = {
    use crate::layout::PyTypeObjectFull as F;
    assert!(std::mem::offset_of!(PyTypeObject, tp_name) == std::mem::offset_of!(F, tp_name));
    assert!(
        std::mem::offset_of!(PyTypeObject, tp_basicsize) == std::mem::offset_of!(F, tp_basicsize)
    );
    assert!(
        std::mem::offset_of!(PyTypeObject, tp_itemsize) == std::mem::offset_of!(F, tp_itemsize)
    );
    assert!(std::mem::offset_of!(PyTypeObject, tp_dealloc) == std::mem::offset_of!(F, tp_dealloc));
    assert!(std::mem::offset_of!(PyTypeObject, tp_flags) == std::mem::offset_of!(F, tp_flags));
    assert!(std::mem::offset_of!(PyTypeObject, tp_base) == std::mem::offset_of!(F, tp_base));
    assert!(
        std::mem::offset_of!(PyTypeObject, tp_finalize) == std::mem::offset_of!(F, tp_finalize)
    );
    assert!(
        std::mem::offset_of!(PyTypeObject, tp_vectorcall) == std::mem::offset_of!(F, tp_vectorcall)
    );
    // The private fields must begin at or after the faithful region.
    assert!(std::mem::offset_of!(PyTypeObject, tp_slots) >= std::mem::size_of::<F>());
};

/// Re-export of the C `PyType_Slot` shape.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PyType_Slot {
    pub slot: c_int,
    pub pfunc: *mut c_void,
}

/// Re-export of the C `PyType_Spec` shape.
#[repr(C)]
#[derive(Debug)]
pub struct PyType_Spec {
    pub name: *const c_char,
    pub basicsize: c_int,
    pub itemsize: c_int,
    pub flags: u32,
    pub slots: *mut PyType_Slot,
}

/// Heap-allocated wrapper around a [`PyTypeObject`] that owns its
/// `Rc<TypeObject>` bridge. Returned by `PyType_FromSpec` etc.
///
/// The [`SlotTable`] embedded here is the lookup-side mirror of the
/// extension-supplied `tp_slots` array. Call sites that want to
/// dispatch into a heap type's protocol slot (the buffer protocol,
/// vectorcall, descriptor `tp_descr_get`, generic allocation, …) all
/// route through [`crate::slottable::slot_table_for`] which reads
/// this field.
#[repr(C)]
pub struct PyTypeObjectBox {
    pub head: PyTypeObject,
    pub owned_name: Vec<u8>,
    /// O(1)-lookup table of slot pointers populated at FromSpec time.
    pub slot_table: SlotTable,
}

/// Convenience cast.
pub fn as_pyobject(ty: *mut PyTypeObject) -> *mut PyObject {
    ty as *mut PyObject
}

/// Convenience cast.
pub fn as_pytype(ob: *mut PyObject) -> *mut PyTypeObject {
    ob as *mut PyTypeObject
}

// ----------------------------------------------------------------
// Static type registry.
//
// We pre-allocate a `PyTypeObject` per built-in WeavePy class.
// At interpreter startup the bridge slot is populated from
// `BuiltinTypes`. Extensions that compile against `Python.h` see
// these symbols as `extern PyTypeObject *`-style data — but we
// expose them via `static`s with a `Sync`-safe `UnsafeCell` wrapper.
// ----------------------------------------------------------------

/// Sync wrapper used for the static type table.
#[repr(transparent)]
pub struct StaticType(pub UnsafeCell<PyTypeObject>);

unsafe impl Sync for StaticType {}

impl StaticType {
    pub const fn new() -> Self {
        Self(UnsafeCell::new(PyTypeObject::new_zeroed()))
    }

    pub fn as_ptr(&self) -> *mut PyTypeObject {
        self.0.get()
    }
}

macro_rules! decl_static_type {
    ($($vis:vis $name:ident);* $(;)?) => {
        $(
            #[no_mangle]
            $vis static $name: StaticType = StaticType::new();
        )*
    };
}

// Static types we expose to extensions. Names match CPython.
decl_static_type! {
    pub PyType_Type;
    pub PyBaseObject_Type;
    pub PyLong_Type;
    pub PyFloat_Type;
    pub PyBool_Type;
    pub PyComplex_Type;
    pub PyUnicode_Type;
    pub PyBytes_Type;
    pub PyByteArray_Type;
    pub PyTuple_Type;
    pub PyList_Type;
    pub PyDict_Type;
    pub PySet_Type;
    pub PyFrozenSet_Type;
    pub PyRange_Type;
    pub PyModule_Type;
    pub _PyNone_Type;
    pub _PyNotImplemented_Type;
    pub PyEllipsis_Type;
    pub PyCapsule_Type;
    pub PySlice_Type;
    pub PyFunction_Type;
    pub PyCFunction_Type;
    pub PyMethod_Type;
    pub PyGen_Type;
    pub PyCoro_Type;
    pub PyAsyncGen_Type;
}

/// Initialise the static type table from the running interpreter's
/// [`weavepy_vm::builtin_types::BuiltinTypes`]. Idempotent; the
/// initialisation is gated on a `Once`-style mutex so concurrent
/// callers see a fully-populated table.
pub fn init_static_types() {
    static INIT_LOCK: Mutex<bool> = Mutex::new(false);
    let mut done = INIT_LOCK.lock().unwrap();
    if *done {
        return;
    }
    *done = true;
    let bt = weavepy_vm::builtin_types::builtin_types();

    fn install(slot: &StaticType, name: &'static [u8], rc: Rc<TypeObject>) {
        unsafe {
            let ty = &mut *slot.as_ptr();
            ty.head.ob_refcnt = IMMORTAL_REFCNT;
            ty.head.ob_type = PyType_Type.as_ptr();
            ty.tp_name = name.as_ptr() as *const c_char;
            // `_Py_Dealloc` (stock inlined `Py_DECREF`) reads
            // `Py_TYPE(inst)->tp_dealloc`; route to the host free path so
            // a stock extension that drops the last ref to one of our
            // objects releases the mirror/box instead of jumping through
            // a garbage slot.
            ty.tp_dealloc = Some(crate::object::_PyWeavePy_Dealloc);
            ty.bridge = Box::into_raw(Box::new(rc));
        }
    }

    install(&PyType_Type, b"type\0", bt.type_.clone());
    install(&PyBaseObject_Type, b"object\0", bt.object_.clone());
    install(&PyLong_Type, b"int\0", bt.int_.clone());
    install(&PyFloat_Type, b"float\0", bt.float_.clone());
    install(&PyBool_Type, b"bool\0", bt.bool_.clone());
    install(&PyUnicode_Type, b"str\0", bt.str_.clone());
    install(&PyBytes_Type, b"bytes\0", bt.bytes_.clone());
    install(&PyByteArray_Type, b"bytearray\0", bt.bytearray_.clone());
    install(&PyTuple_Type, b"tuple\0", bt.tuple_.clone());
    install(&PyList_Type, b"list\0", bt.list_.clone());
    install(&PyDict_Type, b"dict\0", bt.dict_.clone());
    install(&PySet_Type, b"set\0", bt.set_.clone());
    install(&PyFrozenSet_Type, b"frozenset\0", bt.frozenset_.clone());
    install(&PyRange_Type, b"range\0", bt.range_.clone());
    install(&PyModule_Type, b"module\0", bt.module_.clone());
    install(&_PyNone_Type, b"NoneType\0", bt.none_type.clone());
    install(&PyFunction_Type, b"function\0", bt.function_.clone());
    install(&PyGen_Type, b"generator\0", bt.generator_.clone());
    install(&PyCoro_Type, b"coroutine\0", bt.coroutine_.clone());
    install(
        &PyAsyncGen_Type,
        b"async_generator\0",
        bt.async_generator_.clone(),
    );

    // The complex/NotImplemented/Ellipsis/Capsule/CFunction/Slice/Method
    // types don't correspond directly to BuiltinTypes entries;
    // we synthesise minimal native types so `type(Py_None) is _PyNone_Type`
    // (and friends) round-trips correctly.
    fn install_synth(slot: &StaticType, name: &'static [u8], display: &str) {
        let rc = TypeObject::new_builtin(
            display,
            vec![weavepy_vm::builtin_types::builtin_types().object_.clone()],
        )
        .expect("synthetic builtin type must linearise");
        install(slot, name, rc);
    }
    install_synth(&PyComplex_Type, b"complex\0", "complex");
    install_synth(
        &_PyNotImplemented_Type,
        b"NotImplementedType\0",
        "NotImplementedType",
    );
    install_synth(&PyEllipsis_Type, b"ellipsis\0", "ellipsis");
    install_synth(&PyCapsule_Type, b"PyCapsule\0", "PyCapsule");
    install_synth(&PySlice_Type, b"slice\0", "slice");
    install_synth(
        &PyCFunction_Type,
        b"builtin_function_or_method\0",
        "builtin_function_or_method",
    );
    install_synth(&PyMethod_Type, b"method\0", "method");

    // Set the `Py_TPFLAGS_*_SUBCLASS` fast-subclass bits (and a
    // baseline `Py_TPFLAGS_DEFAULT`) on the built-in static types.
    // Stock CPython 3.13 headers inline the `PyX_Check` family as
    // `PyType_FastSubclass(Py_TYPE(o), Py_TPFLAGS_X_SUBCLASS)`, reading
    // `tp_flags` directly, and the *full*-API macros (`PyTuple_GET_ITEM`
    // etc.) `assert(PyX_Check(o))` in non-`NDEBUG` builds. Without these
    // bits a stock extension aborts the moment it touches one of our
    // objects. (RFC 0043 WS3/WS4.)
    use crate::layout::tpflags;
    unsafe fn add_flags(slot: &StaticType, flags: u64) {
        unsafe {
            (*slot.as_ptr()).tp_flags |= tpflags::DEFAULT | flags;
        }
    }
    unsafe {
        add_flags(&PyType_Type, tpflags::TYPE_SUBCLASS | tpflags::BASETYPE);
        add_flags(&PyBaseObject_Type, tpflags::BASETYPE);
        add_flags(&PyLong_Type, tpflags::LONG_SUBCLASS | tpflags::BASETYPE);
        // bool is an int subclass, so `PyLong_Check(True)` must hold.
        add_flags(&PyBool_Type, tpflags::LONG_SUBCLASS);
        add_flags(&PyList_Type, tpflags::LIST_SUBCLASS | tpflags::BASETYPE);
        add_flags(&PyTuple_Type, tpflags::TUPLE_SUBCLASS | tpflags::BASETYPE);
        add_flags(&PyBytes_Type, tpflags::BYTES_SUBCLASS | tpflags::BASETYPE);
        add_flags(
            &PyUnicode_Type,
            tpflags::UNICODE_SUBCLASS | tpflags::BASETYPE,
        );
        add_flags(&PyDict_Type, tpflags::DICT_SUBCLASS | tpflags::BASETYPE);
        // Types that have no fast-subclass bit still want DEFAULT.
        add_flags(&PyFloat_Type, tpflags::BASETYPE);
        add_flags(&PyComplex_Type, tpflags::BASETYPE);
        add_flags(&PyByteArray_Type, tpflags::BASETYPE);
        add_flags(&PySet_Type, tpflags::BASETYPE);
        add_flags(&PyFrozenSet_Type, 0);
    }
}

/// Map a runtime [`Object`] to the static [`PyTypeObject`] pointer
/// representing its type. Used by [`crate::object::into_owned`] to
/// fill in the `ob_type` slot.
pub fn type_for_object(o: &Object) -> *mut PyTypeObject {
    use Object as O;
    match o {
        O::None => _PyNone_Type.as_ptr(),
        O::Bool(_) => PyBool_Type.as_ptr(),
        O::Int(_) | O::Long(_) => PyLong_Type.as_ptr(),
        O::Float(_) => PyFloat_Type.as_ptr(),
        O::Complex(_) => PyComplex_Type.as_ptr(),
        O::Str(_) => PyUnicode_Type.as_ptr(),
        O::Bytes(_) => PyBytes_Type.as_ptr(),
        O::ByteArray(_) => PyByteArray_Type.as_ptr(),
        O::Tuple(_) => PyTuple_Type.as_ptr(),
        O::List(_) => PyList_Type.as_ptr(),
        O::Dict(_) => PyDict_Type.as_ptr(),
        O::Set(_) => PySet_Type.as_ptr(),
        O::FrozenSet(_) => PyFrozenSet_Type.as_ptr(),
        O::Range(_) => PyRange_Type.as_ptr(),
        O::Module(_) => PyModule_Type.as_ptr(),
        O::Function(_) => PyFunction_Type.as_ptr(),
        O::Builtin(_) => PyCFunction_Type.as_ptr(),
        O::BoundMethod(_) => PyMethod_Type.as_ptr(),
        O::Generator(_) => PyGen_Type.as_ptr(),
        O::Coroutine(_) => PyCoro_Type.as_ptr(),
        O::AsyncGenerator(_) => PyAsyncGen_Type.as_ptr(),
        O::Slice(_) => PySlice_Type.as_ptr(),
        O::Type(t) => find_type_ptr(t).unwrap_or_else(|| PyType_Type.as_ptr()),
        O::Instance(inst) => {
            find_type_ptr(&inst.cls()).unwrap_or_else(|| PyBaseObject_Type.as_ptr())
        }
        // RFC 0045 (wave 3): capsules round-trip as their retained box in
        // `into_owned`, but report the faithful `PyCapsule_Type` for any
        // direct `Py_TYPE`-style query that reaches here.
        O::Capsule(_) => PyCapsule_Type.as_ptr(),
        _ => PyBaseObject_Type.as_ptr(),
    }
}

/// Walk the static type registry plus the heap-type registry
/// looking for a slot whose bridge matches `t`. Used by
/// [`type_for_object`]. Linear in the number of registered types
/// (small static set + however many `PyType_FromSpec` produced).
fn find_type_ptr(t: &Rc<TypeObject>) -> Option<*mut PyTypeObject> {
    let target = Rc::as_ptr(t);
    for slot in STATIC_TYPE_TABLE {
        let p = slot.as_ptr();
        unsafe {
            let bridge = (*p).bridge;
            if !bridge.is_null() && Rc::as_ptr(&*bridge) == target {
                return Some(p);
            }
        }
    }
    let from_heap = HEAP_TYPES.with(|cell| {
        for &p in cell.borrow().iter() {
            unsafe {
                let bridge = (*p).bridge;
                if !bridge.is_null() && Rc::as_ptr(&*bridge) == target {
                    return Some(p);
                }
            }
        }
        None
    });
    if from_heap.is_some() {
        return from_heap;
    }
    // Readied stock types (RFC 0044): match on the bridge and return
    // the extension's own pointer so instances carry its `ob_type`.
    READIED_TYPES.with(|cell| {
        for rt in cell.borrow().iter() {
            if Rc::as_ptr(&rt.bridge) == target {
                return Some(rt.ext_ptr);
            }
        }
        None
    })
}

thread_local! {
    /// Registry of heap-allocated `PyTypeObject *` produced by
    /// `PyType_FromSpec[WithBases]`. Looked up by [`find_type_ptr`]
    /// when an `Object::Instance` is materialised back into a
    /// boxed `*mut PyObject`, so the box's `ob_type` matches the
    /// extension's declared type.
    ///
    /// Heap types live forever (`Box::leak`'d at construction),
    /// so we store raw pointers — they're stable for the process
    /// lifetime.
    static HEAP_TYPES: std::cell::RefCell<Vec<*mut PyTypeObject>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Register a heap-allocated type pointer so subsequent
/// `Object::Instance` boxes get the extension's declared
/// `ob_type` instead of falling back to `PyBaseObject_Type`.
pub fn register_heap_type(p: *mut PyTypeObject) {
    if p.is_null() {
        return;
    }
    HEAP_TYPES.with(|cell| cell.borrow_mut().push(p));
}

/// Static table used by [`find_type_ptr`]. Listed once so we don't
/// drift the macro-declared slots and the lookup table apart.
static STATIC_TYPE_TABLE: &[&StaticType] = &[
    &PyType_Type,
    &PyBaseObject_Type,
    &PyLong_Type,
    &PyFloat_Type,
    &PyBool_Type,
    &PyComplex_Type,
    &PyUnicode_Type,
    &PyBytes_Type,
    &PyByteArray_Type,
    &PyTuple_Type,
    &PyList_Type,
    &PyDict_Type,
    &PySet_Type,
    &PyFrozenSet_Type,
    &PyRange_Type,
    &PyModule_Type,
    &_PyNone_Type,
    &_PyNotImplemented_Type,
    &PyEllipsis_Type,
    &PyCapsule_Type,
    &PySlice_Type,
    &PyFunction_Type,
    &PyCFunction_Type,
    &PyMethod_Type,
    &PyGen_Type,
    &PyCoro_Type,
    &PyAsyncGen_Type,
];

/// Borrow the bridged native type from a [`PyTypeObject`].
///
/// SAFETY: `ty` must be either null or a valid pointer to a
/// statically- or heap-allocated type whose `bridge` has been
/// initialised.
pub unsafe fn bridge_type(ty: *mut PyTypeObject) -> Option<Rc<TypeObject>> {
    if ty.is_null() {
        return None;
    }
    // Readied stock types (RFC 0044) keep their bridge in a side
    // registry — their struct is only 416 bytes and has no `bridge`
    // field to read. Check that first.
    if let Some(rt) = readied_for(ty) {
        return Some(rt.bridge.clone());
    }
    let bridge = unsafe { (*ty).bridge };
    if bridge.is_null() {
        return None;
    }
    Some(unsafe { (*bridge).clone() })
}

/// Find the static [`PyTypeObject`] pointer that bridges to `t`,
/// installing one on demand for user-defined classes (e.g. heap
/// types created without `PyType_FromSpec` — usually never; this is
/// a fallback path).
pub fn install_user_type(t: &Rc<TypeObject>) -> *mut PyTypeObject {
    if let Some(p) = find_type_ptr(t) {
        return p;
    }
    let owned_name = format!("{}\0", t.name).into_bytes();
    let bx = Box::new(PyTypeObjectBox {
        head: PyTypeObject {
            head: PyObject {
                ob_refcnt: IMMORTAL_REFCNT,
                ob_type: PyType_Type.as_ptr(),
            },
            tp_name: owned_name.as_ptr() as *const c_char,
            tp_basicsize: std::mem::size_of::<crate::object::PyObjectBox>() as PySsizeT,
            tp_dealloc: Some(crate::object::_PyWeavePy_Dealloc),
            bridge: Box::into_raw(Box::new(t.clone())),
            ..PyTypeObject::new_zeroed()
        },
        owned_name,
        slot_table: SlotTable::empty(),
    });
    let p = Box::leak(bx);
    let ty_ptr = &mut p.head as *mut PyTypeObject;
    // Cache so subsequent calls with the same native `Rc` return the
    // same pointer instead of leaking a fresh box every time
    // (`PyExc_*` aliases — e.g. `SystemError` → `runtime_error` —
    // would otherwise install distinct slots for the same type).
    register_heap_type(ty_ptr);
    ty_ptr
}

// ----------------------------------------------------------------
// PyType_FromSpec — the heart of "extension defines a class".
// ----------------------------------------------------------------

pub const PY_TPFLAGS_HEAPTYPE: u32 = 1 << 9;
pub const PY_TPFLAGS_BASETYPE: u32 = 1 << 10;
pub const PY_TPFLAGS_HAVE_GC: u32 = 1 << 14;
pub const PY_TPFLAGS_DEFAULT: u32 = 1 << 18;
pub const PY_TPFLAGS_HAVE_VECTORCALL: u32 = 1 << 11;
pub const PY_TPFLAGS_DISALLOW_INSTANTIATION: u32 = 1 << 7;
/// `Py_TPFLAGS_READY` — set on `tp_flags` once a type is finalised.
pub const PY_TPFLAGS_READY: u64 = 1 << 12;

/// Assemble a heap/readied type's `__dict__`: `__doc__` / `__module__`
/// / `__qualname__`, the method/getset/member descriptors, and the
/// synthesised dunder shims that forward to the C slots. Shared by
/// [`PyType_FromMetaclass`] and [`PyType_Ready`] (RFC 0044, WS2) so the
/// two type-definition styles converge on identical dispatch.
fn assemble_type_dict(
    qualified: &str,
    bare: &str,
    slot_table: &SlotTable,
    methods: &[crate::module::MethodEntry],
    getset_pairs: Vec<(String, Object)>,
    member_pairs: Vec<(String, Object)>,
    doc: Option<&str>,
) -> DictData {
    let mut dict = DictData::new();
    if let Some(d) = doc {
        dict.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_str(d.to_owned()),
        );
    }
    dict.insert(
        DictKey(Object::from_static("__module__")),
        if let Some(idx) = qualified.rfind('.') {
            Object::from_str(qualified[..idx].to_owned())
        } else {
            Object::from_static("builtins")
        },
    );
    dict.insert(
        DictKey(Object::from_static("__qualname__")),
        Object::from_str(bare.to_owned()),
    );
    for entry in methods {
        dict.insert(
            DictKey(Object::from_str(entry.name.clone())),
            entry.bind_unbound(),
        );
    }
    for (name, obj) in getset_pairs {
        dict.insert(DictKey(Object::from_str(name)), obj);
    }
    for (name, obj) in member_pairs {
        dict.insert(DictKey(Object::from_str(name)), obj);
    }
    let dunder_pairs = crate::dunder_shim::install_dunder_shims(slot_table, qualified.to_owned());
    for (name, obj) in dunder_pairs {
        dict.insert(DictKey(Object::from_str(name)), obj);
    }
    dict
}

// ----------------------------------------------------------------
// Readied stock types (RFC 0044, WS2).
//
// A stock extension defines a *static* `PyTypeObject` (exactly 416
// bytes — the CPython layout, with no room for WeavePy's private
// `bridge`/`slot_table` trailing fields) and calls `PyType_Ready`.
// We therefore cannot stash the bridge in the caller's struct; the
// bridge + decoded slot table live in this side registry, keyed by
// the extension's own type pointer (which is what flows through
// `PyModule_AddObject`, instance `ob_type`, `type(x)`, …).
//
// Entries are `Box::leak`'d (types live for the process lifetime,
// exactly like `HEAP_TYPES`), so the `&'static` borrows handed out by
// `bridge_type` / `slot_table_for` stay valid.
// ----------------------------------------------------------------

/// WeavePy-owned data backing a readied stock type.
pub struct ReadiedType {
    /// The extension's own `&MyType` pointer (the canonical identity).
    pub ext_ptr: *mut PyTypeObject,
    /// Bridge to the native type.
    pub bridge: Rc<TypeObject>,
    /// Slots decoded from the faithful struct + method suites.
    pub slot_table: SlotTable,
}

unsafe impl Send for ReadiedType {}
unsafe impl Sync for ReadiedType {}

thread_local! {
    /// Map from an extension type pointer to its readied data.
    static READIED_BY_PTR: std::cell::RefCell<std::collections::HashMap<usize, &'static ReadiedType>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    /// Insertion-ordered list for the `Rc<TypeObject>` → pointer scan.
    static READIED_TYPES: std::cell::RefCell<Vec<&'static ReadiedType>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

thread_local! {
    /// Types whose instances get a faithful **inline `tp_basicsize`
    /// body** (RFC 0045, wave 3): C-extension types finalised by
    /// `PyType_FromSpec` / `PyType_Ready` that declare storage beyond the
    /// object head. Membership is the opt-in gate that
    /// [`crate::object::into_owned`] and
    /// [`crate::genericalloc::PyType_GenericAlloc`] consult; a type absent
    /// from this set keeps the wave-1/2 `PyObjectBox` instance shape, so
    /// the change is purely additive (pure-Python classes and every
    /// dict-backed fixture are unaffected).
    static INLINE_TYPES: std::cell::RefCell<std::collections::HashSet<usize>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

/// Register `ty` as an inline-instance type iff it declares storage
/// beyond `PyObject_HEAD` (`tp_basicsize > sizeof(PyObject)`) — i.e. it
/// has real inline fields a stock reader pokes at fixed offsets (the
/// `PyArrayObject` shape). Called at `PyType_FromSpec` / `PyType_Ready`
/// finalisation. Types that keep all state in `__dict__`
/// (`tp_basicsize <= sizeof(PyObject)`, which is every current fixture)
/// are not registered and keep the legacy box (RFC 0045, WS1).
pub fn maybe_register_inline_type(ty: *mut PyTypeObject) {
    if ty.is_null() {
        return;
    }
    let basicsize = unsafe { (*ty).tp_basicsize } as usize;
    if basicsize > std::mem::size_of::<PyObject>() {
        INLINE_TYPES.with(|s| s.borrow_mut().insert(ty as usize));
    }
}

/// True if instances of `ty` use a faithful inline `tp_basicsize` body
/// (RFC 0045, wave 3). O(1) hash lookup; false for every non-extension
/// type, so the wave-1/2 paths are unchanged for them.
pub fn is_inline_instance_type(ty: *mut PyTypeObject) -> bool {
    if ty.is_null() {
        return false;
    }
    INLINE_TYPES.with(|s| s.borrow().contains(&(ty as usize)))
}

/// Look up the readied-type data for an extension type pointer.
fn readied_for(ty: *mut PyTypeObject) -> Option<&'static ReadiedType> {
    if ty.is_null() {
        return None;
    }
    READIED_BY_PTR.with(|m| m.borrow().get(&(ty as usize)).copied())
}

/// The decoded slot table for a readied stock type, or `None` if `ty`
/// was not readied via [`PyType_Ready`]. Used by
/// [`crate::slottable::slot_table_for`] so readied static types (which
/// don't carry the `Py_TPFLAGS_HEAPTYPE` bit and have no embedded
/// `PyTypeObjectBox`) still expose their slots for direct dispatch
/// (buffer protocol, vectorcall, GC traverse, …).
pub fn readied_slot_table(ty: *mut PyTypeObject) -> Option<&'static SlotTable> {
    readied_for(ty).map(|rt| &rt.slot_table)
}

/// True if `ty` is a stock type finalised through [`PyType_Ready`]
/// (RFC 0044). Used by [`crate::genericalloc::PyType_GenericAlloc`] to
/// decide whether a freshly-allocated instance should carry a real
/// `Object::Instance` payload (so the extension's `tp_init` /
/// `PyObject_SetAttrString` operate on a genuine instance dict).
pub fn is_readied_type(ty: *mut PyTypeObject) -> bool {
    readied_for(ty).is_some()
}

/// The native bridge type for a readied stock type, if any.
pub fn readied_bridge(ty: *mut PyTypeObject) -> Option<Rc<TypeObject>> {
    readied_for(ty).map(|rt| rt.bridge.clone())
}

/// The live `PyTypeObject *` backing `cls`, if `cls` is bridged from a
/// C type — static, heap (`PyType_FromSpec`), or readied
/// (`PyType_Ready`). Public so the GC bridge (RFC 0044, WS4) can reach
/// an instance's `tp_traverse` / `tp_clear` slots; returns `None` for a
/// pure-Python class (no C `PyTypeObject` exists to consult).
pub fn type_ptr_for_class(cls: &Rc<TypeObject>) -> Option<*mut PyTypeObject> {
    find_type_ptr(cls)
}

#[no_mangle]
pub unsafe extern "C" fn PyType_FromSpec(spec: *mut PyType_Spec) -> *mut PyObject {
    unsafe { PyType_FromSpecWithBases(spec, ptr::null_mut()) }
}

#[no_mangle]
pub unsafe extern "C" fn PyType_FromSpecWithBases(
    spec: *mut PyType_Spec,
    bases: *mut PyObject,
) -> *mut PyObject {
    unsafe { PyType_FromMetaclass(ptr::null_mut(), ptr::null_mut(), spec, bases) }
}

#[no_mangle]
pub unsafe extern "C" fn PyType_FromMetaclass(
    _metaclass: *mut PyTypeObject,
    _module: *mut PyObject,
    spec: *mut PyType_Spec,
    bases: *mut PyObject,
) -> *mut PyObject {
    crate::interp::ensure_initialised();
    if spec.is_null() {
        crate::errors::set_runtime_error("PyType_FromSpec called with null spec");
        return ptr::null_mut();
    }
    let spec_ref = unsafe { &*spec };
    let raw_name = if spec_ref.name.is_null() {
        b"<anonymous>\0".as_ptr() as *const c_char
    } else {
        spec_ref.name
    };
    let name_cstr = unsafe { CStr::from_ptr(raw_name) };
    let qualified = name_cstr.to_string_lossy().into_owned();
    let bare = qualified
        .rsplit('.')
        .next()
        .unwrap_or(&qualified)
        .to_owned();

    // ----------------------------------------------------------------
    // Resolve the base type list. Default to `object` if `bases` is
    // null or empty. A `Py_tp_base` / `Py_tp_bases` slot in the spec
    // wins over the explicit argument (matches CPython).
    // ----------------------------------------------------------------
    let mut spec_base: Option<Rc<TypeObject>> = None;
    let mut spec_bases: Option<Vec<Rc<TypeObject>>> = None;
    let mut slot_ptr = spec_ref.slots;
    if !slot_ptr.is_null() {
        let mut p = slot_ptr;
        loop {
            let slot = unsafe { *p };
            if slot.slot == 0 {
                break;
            }
            match slot.slot {
                x if x == crate::slottable::Py_tp_base => {
                    if !slot.pfunc.is_null() {
                        let ty_ptr = slot.pfunc as *mut PyTypeObject;
                        if let Some(t) = unsafe { bridge_type(ty_ptr) } {
                            spec_base = Some(t);
                        }
                    }
                }
                x if x == crate::slottable::Py_tp_bases => {
                    if !slot.pfunc.is_null() {
                        let bases_obj =
                            unsafe { crate::object::clone_object(slot.pfunc as *mut PyObject) };
                        if let Object::Tuple(items) = bases_obj {
                            spec_bases = Some(
                                items
                                    .iter()
                                    .filter_map(|item| match item {
                                        Object::Type(t) => Some(t.clone()),
                                        _ => None,
                                    })
                                    .collect(),
                            );
                        }
                    }
                }
                _ => {}
            }
            p = unsafe { p.add(1) };
        }
        slot_ptr = spec_ref.slots;
    }

    let arg_bases: Vec<Rc<TypeObject>> = if bases.is_null() {
        Vec::new()
    } else {
        let cloned = unsafe { crate::object::clone_object(bases) };
        match cloned {
            Object::Tuple(items) => items
                .iter()
                .filter_map(|item| match item {
                    Object::Type(t) => Some(t.clone()),
                    _ => None,
                })
                .collect(),
            Object::Type(t) => vec![t],
            _ => Vec::new(),
        }
    };

    let bases_resolved: Vec<Rc<TypeObject>> = if let Some(list) = spec_bases {
        list
    } else if !arg_bases.is_empty() {
        arg_bases
    } else if let Some(b) = spec_base {
        vec![b]
    } else {
        vec![weavepy_vm::builtin_types::builtin_types().object_.clone()]
    };
    let bases_resolved = if bases_resolved.is_empty() {
        vec![weavepy_vm::builtin_types::builtin_types().object_.clone()]
    } else {
        bases_resolved
    };

    // ----------------------------------------------------------------
    // First pass: scan the slot table, populate the SlotTable with
    // every recognised slot, and harvest doc / methods / getsets /
    // members for the dict.
    // ----------------------------------------------------------------
    let mut slot_table = SlotTable::empty();
    let mut methods: Vec<crate::module::MethodEntry> = Vec::new();
    let mut getset_pairs: Vec<(String, Object)> = Vec::new();
    let mut member_pairs: Vec<(String, Object)> = Vec::new();
    let mut doc: Option<String> = None;
    if !slot_ptr.is_null() {
        loop {
            let slot = unsafe { *slot_ptr };
            if slot.slot == 0 {
                break;
            }
            match slot.slot {
                x if x == crate::slottable::Py_tp_doc => {
                    if !slot.pfunc.is_null() {
                        let s = unsafe { CStr::from_ptr(slot.pfunc as *const c_char) };
                        doc = Some(s.to_string_lossy().into_owned());
                    }
                }
                x if x == crate::slottable::Py_tp_methods => {
                    if !slot.pfunc.is_null() {
                        methods.extend(unsafe {
                            crate::module::collect_methods(
                                slot.pfunc as *mut crate::module::PyMethodDef,
                            )
                        });
                    }
                }
                x if x == crate::slottable::Py_tp_getset => {
                    if !slot.pfunc.is_null() {
                        getset_pairs.extend(unsafe {
                            crate::getset::collect_getsets(
                                slot.pfunc as *mut crate::getset::PyGetSetDef,
                            )
                        });
                    }
                }
                x if x == crate::slottable::Py_tp_members => {
                    if !slot.pfunc.is_null() {
                        member_pairs.extend(unsafe {
                            crate::getset::collect_members(
                                slot.pfunc as *mut crate::getset::PyMemberDef,
                            )
                        });
                    }
                }
                x if x == crate::slottable::Py_tp_base || x == crate::slottable::Py_tp_bases => {
                    // Already consumed in the bases pass.
                }
                _ => {
                    slot_table.install(slot.slot, slot.pfunc);
                }
            }
            slot_ptr = unsafe { slot_ptr.add(1) };
        }
    }

    // ----------------------------------------------------------------
    // Build the type's dict: doc + module + methods + getset/member
    // descriptors + synthesised dunder shims. Shared with the
    // `PyType_Ready` path (RFC 0044, WS2) via [`assemble_type_dict`].
    // ----------------------------------------------------------------
    let dict = assemble_type_dict(
        &qualified,
        &bare,
        &slot_table,
        &methods,
        getset_pairs,
        member_pairs,
        doc.as_deref(),
    );

    let ty = match TypeObject::new_user(&bare, bases_resolved, dict) {
        Ok(ty) => ty,
        Err(_) => {
            crate::errors::set_runtime_error("could not linearise base classes");
            return ptr::null_mut();
        }
    };
    let owned_name = format!("{qualified}\0").into_bytes();
    let bx = Box::new(PyTypeObjectBox {
        head: PyTypeObject {
            head: PyObject {
                ob_refcnt: IMMORTAL_REFCNT,
                ob_type: PyType_Type.as_ptr(),
            },
            tp_name: owned_name.as_ptr() as *const c_char,
            tp_basicsize: spec_ref.basicsize as PySsizeT,
            tp_itemsize: spec_ref.itemsize as PySsizeT,
            tp_flags: (spec_ref.flags | PY_TPFLAGS_HEAPTYPE) as u64,
            tp_dealloc: Some(crate::object::_PyWeavePy_Dealloc),
            tp_slots: spec_ref.slots,
            bridge: Box::into_raw(Box::new(ty)),
            ..PyTypeObject::new_zeroed()
        },
        owned_name,
        slot_table,
    });
    let leaked = Box::leak(bx);
    let ty_ptr = &mut leaked.head as *mut PyTypeObject;
    register_heap_type(ty_ptr);
    // RFC 0045 (wave 3): a heap type that declares inline fields beyond
    // the object head gets faithful `tp_basicsize` instance storage.
    maybe_register_inline_type(ty_ptr);
    ty_ptr as *mut PyObject
}

/// `PyType_GetSlot(ty, slot)` — fetch a slot pointer from `ty`'s
/// SlotTable.
#[no_mangle]
pub unsafe extern "C" fn PyType_GetSlot(ty: *mut PyTypeObject, slot: c_int) -> *mut c_void {
    let Some(table) = (unsafe { crate::slottable::slot_table_for(ty) }) else {
        return ptr::null_mut();
    };
    table.get(slot).as_void()
}

/// `PyType_HasFeature(type, flag)` — check a `Py_TPFLAGS_*` bit.
#[no_mangle]
pub unsafe extern "C" fn PyType_HasFeature(ty: *mut PyTypeObject, flag: u32) -> c_int {
    if ty.is_null() {
        return 0;
    }
    let f = unsafe { (*ty).tp_flags };
    let flag = flag as u64;
    if (f & flag) == flag {
        1
    } else {
        0
    }
}

/// `PyType_GetFlags(type)` — return the type's `tp_flags` field.
/// CPython returns `unsigned long` (`c_ulong`, 64-bit on LP64).
#[no_mangle]
pub unsafe extern "C" fn PyType_GetFlags(ty: *mut PyTypeObject) -> std::os::raw::c_ulong {
    if ty.is_null() {
        return 0;
    }
    unsafe { (*ty).tp_flags as std::os::raw::c_ulong }
}

/// `PyType_GetQualName(type)` — return the type's qualified name as
/// a fresh `str` object.
#[no_mangle]
pub unsafe extern "C" fn PyType_GetQualName(ty: *mut PyTypeObject) -> *mut PyObject {
    if ty.is_null() {
        return ptr::null_mut();
    }
    let n = unsafe { (*ty).tp_name };
    if n.is_null() {
        return crate::object::into_owned(Object::from_static(""));
    }
    let s = unsafe { CStr::from_ptr(n) }.to_string_lossy().into_owned();
    let bare = s.rsplit('.').next().unwrap_or(&s).to_owned();
    crate::object::into_owned(Object::from_str(bare))
}

#[no_mangle]
pub unsafe extern "C" fn PyType_FromModuleAndSpec(
    _module: *mut PyObject,
    spec: *mut PyType_Spec,
    bases: *mut PyObject,
) -> *mut PyObject {
    unsafe { PyType_FromSpecWithBases(spec, bases) }
}

/// True if `ty` is a WeavePy-owned type object (a static built-in or a
/// `PyType_FromSpec` heap type) — i.e. its struct carries the private
/// `bridge`/`slot_table` trailing fields and is already "ready". Decided
/// purely by pointer identity so we never read past a 416-byte stock
/// struct.
fn is_weavepy_owned_type(ty: *mut PyTypeObject) -> bool {
    for slot in STATIC_TYPE_TABLE {
        if slot.as_ptr() == ty {
            return true;
        }
    }
    HEAP_TYPES.with(|cell| cell.borrow().contains(&ty))
}

/// Decode a faithfully-laid-out `PyTypeObject` (a stock extension's
/// statically-initialised type + its method suites) into a
/// [`SlotTable`] plus the dict ingredients (RFC 0044, WS2).
struct Harvested {
    slot_table: SlotTable,
    methods: Vec<crate::module::MethodEntry>,
    getset_pairs: Vec<(String, Object)>,
    member_pairs: Vec<(String, Object)>,
    doc: Option<String>,
    base: Option<Rc<TypeObject>>,
}

/// Read every populated slot of a faithful `PyTypeObject` into a
/// [`SlotTable`]. The direct `tp_*` function pointers map to their
/// `Py_tp_*` ids; each non-null method suite (`tp_as_number`, …) is
/// decomposed into its `Py_nb_*` / `Py_sq_*` / `Py_mp_*` / `Py_am_*` /
/// `Py_bf_*` ids at the faithful offsets pinned in [`crate::layout`].
///
/// # Safety
/// `ty` must point at a readable, faithfully-laid-out `PyTypeObject`
/// (at least the 416-byte CPython prefix).
unsafe fn harvest_faithful(ty: *mut PyTypeObject) -> Harvested {
    use crate::slottable as ids;
    let mut t = SlotTable::empty();

    unsafe fn put(t: &mut SlotTable, id: c_int, p: *mut c_void) {
        if !p.is_null() {
            t.install(id, p);
        }
    }

    let tref = unsafe { &*ty };

    // Direct type-level slots.
    unsafe {
        put(&mut t, ids::Py_tp_call, tref.tp_call);
        put(&mut t, ids::Py_tp_init, tref.tp_init);
        put(&mut t, ids::Py_tp_new, tref.tp_new);
        put(&mut t, ids::Py_tp_iter, tref.tp_iter);
        put(&mut t, ids::Py_tp_iternext, tref.tp_iternext);
        put(&mut t, ids::Py_tp_richcompare, tref.tp_richcompare);
        put(&mut t, ids::Py_tp_getattro, tref.tp_getattro);
        put(&mut t, ids::Py_tp_setattro, tref.tp_setattro);
        put(&mut t, ids::Py_tp_descr_get, tref.tp_descr_get);
        put(&mut t, ids::Py_tp_descr_set, tref.tp_descr_set);
        put(&mut t, ids::Py_tp_hash, tref.tp_hash);
        put(&mut t, ids::Py_tp_repr, tref.tp_repr);
        put(&mut t, ids::Py_tp_str, tref.tp_str);
        put(&mut t, ids::Py_tp_traverse, tref.tp_traverse);
        put(&mut t, ids::Py_tp_clear, tref.tp_clear);
        put(&mut t, ids::Py_tp_alloc, tref.tp_alloc);
        put(&mut t, ids::Py_tp_free, tref.tp_free);
        put(&mut t, ids::Py_tp_getattr, tref.tp_getattr);
        put(&mut t, ids::Py_tp_setattr, tref.tp_setattr);
    }

    // Number suite.
    if !tref.tp_as_number.is_null() {
        let n = unsafe { &*(tref.tp_as_number as *const crate::layout::PyNumberMethods) };
        unsafe {
            put(&mut t, ids::Py_nb_add, n.nb_add);
            put(&mut t, ids::Py_nb_subtract, n.nb_subtract);
            put(&mut t, ids::Py_nb_multiply, n.nb_multiply);
            put(&mut t, ids::Py_nb_remainder, n.nb_remainder);
            put(&mut t, ids::Py_nb_divmod, n.nb_divmod);
            put(&mut t, ids::Py_nb_power, n.nb_power);
            put(&mut t, ids::Py_nb_negative, n.nb_negative);
            put(&mut t, ids::Py_nb_positive, n.nb_positive);
            put(&mut t, ids::Py_nb_absolute, n.nb_absolute);
            put(&mut t, ids::Py_nb_bool, n.nb_bool);
            put(&mut t, ids::Py_nb_invert, n.nb_invert);
            put(&mut t, ids::Py_nb_lshift, n.nb_lshift);
            put(&mut t, ids::Py_nb_rshift, n.nb_rshift);
            put(&mut t, ids::Py_nb_and, n.nb_and);
            put(&mut t, ids::Py_nb_xor, n.nb_xor);
            put(&mut t, ids::Py_nb_or, n.nb_or);
            put(&mut t, ids::Py_nb_int, n.nb_int);
            put(&mut t, ids::Py_nb_float, n.nb_float);
            put(&mut t, ids::Py_nb_inplace_add, n.nb_inplace_add);
            put(&mut t, ids::Py_nb_inplace_subtract, n.nb_inplace_subtract);
            put(&mut t, ids::Py_nb_inplace_multiply, n.nb_inplace_multiply);
            put(&mut t, ids::Py_nb_inplace_remainder, n.nb_inplace_remainder);
            put(&mut t, ids::Py_nb_inplace_power, n.nb_inplace_power);
            put(&mut t, ids::Py_nb_inplace_lshift, n.nb_inplace_lshift);
            put(&mut t, ids::Py_nb_inplace_rshift, n.nb_inplace_rshift);
            put(&mut t, ids::Py_nb_inplace_and, n.nb_inplace_and);
            put(&mut t, ids::Py_nb_inplace_xor, n.nb_inplace_xor);
            put(&mut t, ids::Py_nb_inplace_or, n.nb_inplace_or);
            put(&mut t, ids::Py_nb_floor_divide, n.nb_floor_divide);
            put(&mut t, ids::Py_nb_true_divide, n.nb_true_divide);
            put(
                &mut t,
                ids::Py_nb_inplace_floor_divide,
                n.nb_inplace_floor_divide,
            );
            put(
                &mut t,
                ids::Py_nb_inplace_true_divide,
                n.nb_inplace_true_divide,
            );
            put(&mut t, ids::Py_nb_index, n.nb_index);
            put(&mut t, ids::Py_nb_matrix_multiply, n.nb_matrix_multiply);
            put(
                &mut t,
                ids::Py_nb_inplace_matrix_multiply,
                n.nb_inplace_matrix_multiply,
            );
        }
    }

    // Sequence suite.
    if !tref.tp_as_sequence.is_null() {
        let s = unsafe { &*(tref.tp_as_sequence as *const crate::layout::PySequenceMethods) };
        unsafe {
            put(&mut t, ids::Py_sq_length, s.sq_length);
            put(&mut t, ids::Py_sq_concat, s.sq_concat);
            put(&mut t, ids::Py_sq_repeat, s.sq_repeat);
            put(&mut t, ids::Py_sq_item, s.sq_item);
            put(&mut t, ids::Py_sq_ass_item, s.sq_ass_item);
            put(&mut t, ids::Py_sq_contains, s.sq_contains);
            put(&mut t, ids::Py_sq_inplace_concat, s.sq_inplace_concat);
            put(&mut t, ids::Py_sq_inplace_repeat, s.sq_inplace_repeat);
        }
    }

    // Mapping suite.
    if !tref.tp_as_mapping.is_null() {
        let m = unsafe { &*(tref.tp_as_mapping as *const crate::layout::PyMappingMethods) };
        unsafe {
            put(&mut t, ids::Py_mp_length, m.mp_length);
            put(&mut t, ids::Py_mp_subscript, m.mp_subscript);
            put(&mut t, ids::Py_mp_ass_subscript, m.mp_ass_subscript);
        }
    }

    // Async suite.
    if !tref.tp_as_async.is_null() {
        let a = unsafe { &*(tref.tp_as_async as *const crate::layout::PyAsyncMethods) };
        unsafe {
            put(&mut t, ids::Py_am_await, a.am_await);
            put(&mut t, ids::Py_am_aiter, a.am_aiter);
            put(&mut t, ids::Py_am_anext, a.am_anext);
            put(&mut t, ids::Py_am_send, a.am_send);
        }
    }

    // Buffer suite.
    if !tref.tp_as_buffer.is_null() {
        let b = unsafe { &*(tref.tp_as_buffer as *const crate::layout::PyBufferProcs) };
        unsafe {
            put(&mut t, ids::Py_bf_getbuffer, b.bf_getbuffer);
            put(&mut t, ids::Py_bf_releasebuffer, b.bf_releasebuffer);
        }
    }

    // Descriptor tables + doc + base for the dict / linearisation.
    let methods = if tref.tp_methods.is_null() {
        Vec::new()
    } else {
        unsafe {
            crate::module::collect_methods(tref.tp_methods as *mut crate::module::PyMethodDef)
        }
    };
    let getset_pairs = if tref.tp_getset.is_null() {
        Vec::new()
    } else {
        unsafe { crate::getset::collect_getsets(tref.tp_getset as *mut crate::getset::PyGetSetDef) }
    };
    let member_pairs = if tref.tp_members.is_null() {
        Vec::new()
    } else {
        unsafe {
            crate::getset::collect_members(tref.tp_members as *mut crate::getset::PyMemberDef)
        }
    };
    let doc = if tref.tp_doc.is_null() {
        None
    } else {
        Some(
            unsafe { CStr::from_ptr(tref.tp_doc) }
                .to_string_lossy()
                .into_owned(),
        )
    };
    let base = if tref.tp_base.is_null() {
        None
    } else {
        unsafe { bridge_type(tref.tp_base) }
    };

    Harvested {
        slot_table: t,
        methods,
        getset_pairs,
        member_pairs,
        doc,
        base,
    }
}

/// `PyType_Ready(t)` — finalise a type object.
///
/// For WeavePy's own types (static built-ins, `PyType_FromSpec` heap
/// types) this is a no-op: they are ready the moment their bridge is
/// installed. For a **stock extension's statically-initialised
/// `PyTypeObject`** (RFC 0044, WS2) it harvests the faithful struct +
/// method suites into a [`SlotTable`], builds the bridged native type
/// with synthesised dunder shims, and registers it in the readied-type
/// side table — then writes `ob_type` and the `Py_TPFLAGS_READY` bit
/// back into the caller's struct (both at offsets inside the faithful
/// 416-byte region, so a stock struct is never overrun).
#[no_mangle]
pub unsafe extern "C" fn PyType_Ready(t: *mut PyTypeObject) -> c_int {
    if t.is_null() {
        return 0;
    }
    crate::interp::ensure_initialised();
    // Idempotent: already readied, or one of our own ready types.
    if readied_for(t).is_some() || is_weavepy_owned_type(t) {
        return 0;
    }

    let h = unsafe { harvest_faithful(t) };

    // Resolve name (qualified + bare) from tp_name.
    let raw_name = unsafe { (*t).tp_name };
    let qualified = if raw_name.is_null() {
        "<readied>".to_owned()
    } else {
        unsafe { CStr::from_ptr(raw_name) }
            .to_string_lossy()
            .into_owned()
    };
    let bare = qualified
        .rsplit('.')
        .next()
        .unwrap_or(&qualified)
        .to_owned();

    let bases = vec![h
        .base
        .clone()
        .unwrap_or_else(|| weavepy_vm::builtin_types::builtin_types().object_.clone())];

    let dict = assemble_type_dict(
        &qualified,
        &bare,
        &h.slot_table,
        &h.methods,
        h.getset_pairs,
        h.member_pairs,
        h.doc.as_deref(),
    );

    let ty = match TypeObject::new_user(&bare, bases, dict) {
        Ok(ty) => ty,
        Err(_) => {
            crate::errors::set_runtime_error("PyType_Ready: could not linearise bases");
            return -1;
        }
    };

    let readied: &'static ReadiedType = Box::leak(Box::new(ReadiedType {
        ext_ptr: t,
        bridge: ty,
        slot_table: h.slot_table,
    }));
    READIED_BY_PTR.with(|m| m.borrow_mut().insert(t as usize, readied));
    READIED_TYPES.with(|v| v.borrow_mut().push(readied));
    // RFC 0045 (wave 3): a readied static type that declares inline
    // fields beyond the object head gets faithful `tp_basicsize`
    // instance storage (the `PyArrayObject` shape).
    maybe_register_inline_type(t);

    // Write-back into the caller's struct — both offsets live inside
    // the faithful 416-byte CPython prefix, so a stock static type is
    // never overrun.
    unsafe {
        (*t).head.ob_type = PyType_Type.as_ptr();
        (*t).head.ob_refcnt = IMMORTAL_REFCNT;
        (*t).tp_flags |= PY_TPFLAGS_READY;
        if (*t).tp_dealloc.is_none() {
            (*t).tp_dealloc = Some(crate::object::_PyWeavePy_Dealloc);
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn PyType_IsSubtype(a: *mut PyTypeObject, b: *mut PyTypeObject) -> c_int {
    let (Some(a), Some(b)) = (unsafe { bridge_type(a) }, unsafe { bridge_type(b) }) else {
        return 0;
    };
    if a.is_subclass_of(&b) {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_TypeCheck(o: *mut PyObject, ty: *mut PyTypeObject) -> c_int {
    if o.is_null() || ty.is_null() {
        return 0;
    }
    let head = unsafe { &*o };
    if std::ptr::eq(head.ob_type, ty) {
        return 1;
    }
    let Some(other) = (unsafe { bridge_type(head.ob_type) }) else {
        return 0;
    };
    let Some(t) = (unsafe { bridge_type(ty) }) else {
        return 0;
    };
    c_int::from(other.is_subclass_of(&t))
}

#[no_mangle]
pub unsafe extern "C" fn PyType_GetName(ty: *mut PyTypeObject) -> *const c_char {
    if ty.is_null() {
        return ptr::null();
    }
    unsafe { (*ty).tp_name }
}
