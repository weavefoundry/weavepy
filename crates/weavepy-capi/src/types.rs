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

use weavepy_vm::object::{BoundMethod, DictData, DictKey, Object};
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
    // RFC 0047 (wave 5): the umbrella type for WeavePy's native iterators
    // (`Object::Iter` — list/tuple/range/dict/set/... iterators). Macro-heavy
    // Cython reads `Py_TYPE(it)->tp_iternext` *directly* in its `for` loop and
    // `next()` codegen; without a non-NULL slot a compiled `for x in range(n)`
    // (numpy.random's `SeedSequence.generate_state`) jumps to its error label
    // with no exception set. The type carries `tp_iter` (return self) +
    // `tp_iternext` (→ `PyIter_Next`).
    pub PySeqIter_Type;
    // RFC 0046 (wave 4): types numpy's `_multiarray_umath` references by
    // address (`Py_TYPE(x) == &PyMemoryView_Type`, descriptor/proxy slots).
    pub PyMemoryView_Type;
    pub PyDictProxy_Type;
    pub PyGetSetDescr_Type;
    pub PyMemberDescr_Type;
    pub PyMethodDescr_Type;
    // RFC 0047 (wave 5): `wrapper_descriptor` (slot wrappers like
    // `object.__init__`); numpy.random / pandas reference the type by
    // address. A synthetic minimal type satisfies symbol + identity.
    pub PyWrapperDescr_Type;
    // RFC 0046 (wave 4): tags a `PyModuleDef` returned by
    // `PyModuleDef_Init`, so the loader can recognise a multi-phase
    // (PEP 489) extension and run its create/exec slots.
    pub PyModuleDef_Type;
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

    // `complex` bridges to the VM's real builtin `complex` (like `float`),
    // so `PyComplex_Type`'s bridged type is *identity-equal* to the VM
    // `complex`. numpy's `complex128` dual-inherits from it
    // (`tp_bases == (complexfloating, complex)`), and
    // `isinstance(np.complex128(z), complex)` / `issubclass(...)` only hold
    // when the MRO's `complex` entry *is* the builtin one.
    install(&PyComplex_Type, b"complex\0", bt.complex_.clone());

    // The NotImplemented/Ellipsis/Capsule/CFunction/Slice/Method types
    // don't correspond directly to BuiltinTypes entries; we synthesise
    // minimal native types so `type(Py_None) is _PyNone_Type` (and
    // friends) round-trips correctly.
    fn install_synth(slot: &StaticType, name: &'static [u8], display: &str) {
        let rc = TypeObject::new_builtin(
            display,
            vec![weavepy_vm::builtin_types::builtin_types().object_.clone()],
        )
        .expect("synthetic builtin type must linearise");
        install(slot, name, rc);
    }
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
    // RFC 0046 (wave 4): numpy references these by address. Synthetic
    // minimal types are enough for symbol resolution + pointer identity.
    install_synth(&PyMemoryView_Type, b"memoryview\0", "memoryview");
    install_synth(&PyDictProxy_Type, b"mappingproxy\0", "mappingproxy");
    install_synth(
        &PyGetSetDescr_Type,
        b"getset_descriptor\0",
        "getset_descriptor",
    );
    install_synth(
        &PyMemberDescr_Type,
        b"member_descriptor\0",
        "member_descriptor",
    );
    install_synth(
        &PyMethodDescr_Type,
        b"method_descriptor\0",
        "method_descriptor",
    );
    install_synth(
        &PyWrapperDescr_Type,
        b"wrapper_descriptor\0",
        "wrapper_descriptor",
    );
    install_synth(&PyModuleDef_Type, b"moduledef\0", "moduledef");
    install_synth(&PySeqIter_Type, b"iterator\0", "iterator");

    // RFC 0047 (wave 5): wire the iteration protocol (`tp_iter` → self,
    // `tp_iternext` → `PyIter_Next`) onto the iterator umbrella type and the
    // generator type. Stock Cython reads these slots off `Py_TYPE(it)`
    // directly when compiling `for`/`next()`, so a WeavePy iterator handed to
    // a C extension must advertise them or the loop silently errors.
    unsafe {
        crate::builtin_slots::install_iterator(&PySeqIter_Type);
        crate::builtin_slots::install_iterator(&PyGen_Type);
    }

    // RFC 0047 (wave 5): wire the descriptor `tp_descr_get` onto the
    // callable types so a WeavePy function / instance-binding builtin method
    // found via `_PyType_Lookup` *binds* to the instance. Stock Cython's
    // special-method protocol (`with`, `for`, operator dunders) reads this
    // slot directly off `Py_TYPE(descr)`; without it a bound special method
    // (a `threading.Lock`'s `__exit__`) was used unbound and called with no
    // `self`, failing inside extension module init.
    unsafe {
        (*PyFunction_Type.as_ptr()).tp_descr_get = callable_descr_get as *mut c_void;
        (*PyCFunction_Type.as_ptr()).tp_descr_get = callable_descr_get as *mut c_void;
        (*PyMethodDescr_Type.as_ptr()).tp_descr_get = callable_descr_get as *mut c_void;
    }

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
        // The iterator umbrella type needs the baseline DEFAULT feature bit so
        // `PyType_HasFeature`/`PyIter_Check` behave; no fast-subclass bit.
        add_flags(&PySeqIter_Type, 0);
    }

    // RFC 0047 (wave 5): faithful `tp_basicsize` / `tp_itemsize` for the
    // static built-ins. A Cython extension imports a builtin and validates
    // it with `__Pyx_ImportType`, which reads `Py_TYPE(builtins.X)->
    // tp_basicsize` and raises `ValueError("X size changed, may indicate
    // binary incompatibility. Expected N from C header, got M from
    // PyObject")` when the live value is smaller than the `sizeof(...)` its
    // stock CPython 3.13 headers baked in (numpy.random's `bit_generator`
    // checks `builtins.type` == `sizeof(PyHeapTypeObject)` = 928, and a
    // zero tripped it). WeavePy stores these objects as managed
    // `PyObjectBox` mirrors, so the field is **reporting-only**: it never
    // diverts allocation, which is gated by the separate `INLINE_TYPES`
    // registry these are never entered into. The values mirror stock
    // CPython 3.13 (`<type>.__basicsize__` / `.__itemsize__`) byte-for-byte.
    unsafe fn set_size(slot: &StaticType, basicsize: PySsizeT, itemsize: PySsizeT) {
        unsafe {
            let ty = &mut *slot.as_ptr();
            ty.tp_basicsize = basicsize;
            ty.tp_itemsize = itemsize;
        }
    }
    unsafe {
        set_size(&PyType_Type, 928, 40);
        set_size(&PyBaseObject_Type, 16, 0);
        set_size(&PyLong_Type, 24, 4);
        set_size(&PyFloat_Type, 24, 0);
        set_size(&PyBool_Type, 24, 4);
        set_size(&PyComplex_Type, 32, 0);
        set_size(&PyUnicode_Type, 64, 0);
        set_size(&PyBytes_Type, 33, 1);
        set_size(&PyByteArray_Type, 56, 0);
        set_size(&PyTuple_Type, 24, 8);
        set_size(&PyList_Type, 40, 0);
        set_size(&PyDict_Type, 48, 0);
        set_size(&PySet_Type, 200, 0);
        set_size(&PyFrozenSet_Type, 200, 0);
    }

    // RFC 0046 (wave 4): give the exported value built-ins a faithful
    // `tp_new`. A C type that subclasses one of them (numpy's
    // `float64 ← float`, `str_ ← str`, `bytes_ ← bytes`) inherits and may
    // directly call the base's `tp_new`; a NULL slot is a jump through
    // address 0 (`np.float64(1.0)` and numpy's import self-checks crash).
    crate::builtin_new::install_builtin_constructors();

    // RFC 0047 (wave 5): populate the C-level protocol suites
    // (`tp_as_sequence`/`tp_as_mapping`/`tp_iter`) on the built-in
    // containers. Macro-heavy Cython reads these slots directly off the
    // type struct (`__Pyx_PyObject_GetItem` → `tp_as_mapping->mp_subscript`),
    // so without them a WeavePy list/tuple/dict appears "not subscriptable"
    // to an extension even though the VM handles the operation.
    crate::builtin_slots::install();

    // RFC 0047 (wave 5): populate `tp_as_number` on the exported numeric
    // built-ins (`int`/`float`/`bool`/`complex`). Macro-heavy Cython casts a
    // scalar to a C integer/double by reading `Py_TYPE(x)->tp_as_number->nb_int`
    // (`__Pyx_PyNumber_IntOrLong`) / `nb_float` directly; a NULL suite made
    // `<int64_t>(some_float)` raise "an integer is required" — the exact break
    // in pandas' `Timedelta("1 day")` string parser (`cast_from_unit`).
    crate::builtin_slots::install_numbers();

    // RFC 0047 (wave 5): wire `tp_repr` / `tp_str` / `tp_hash` on every
    // exported built-in static type. Stock Cython/C code reaches the
    // stringify + hash slots *directly* off `Py_TYPE(o)` — e.g. pandas'
    // `lib.ensure_string_array` compiles `f"{val}"` on an `int`/`float`
    // element to `Py_TYPE(val)->tp_repr(val)` (via `PyObject_Format` →
    // `object.__format__` → `PyObject_Str` → `tp_str`/`tp_repr`), and dict
    // /set keying reads `Py_TYPE(key)->tp_hash`. A NULL slot is a jump
    // through address 0 (`s.astype(str)` on an int64 Series crashed with
    // `pc=0x0` inside `ensure_string_array.cold`). The `synth_*` bridges
    // forward to `PyObject_Repr` / `PyObject_Str` / `PyObject_Hash`, which
    // dispatch on the runtime `Object` enum and never re-read these C slots,
    // so the forward is recursion-safe. Only fill a slot left NULL so a
    // faithful per-type slot (datetime, `PyType_FromSpec`) is untouched;
    // `list`/`dict`/`set` get the hash bridge too, which raises
    // `unhashable type` via the VM exactly as CPython's
    // `PyObject_HashNotImplemented` does.
    unsafe {
        for slot in STATIC_TYPE_TABLE {
            let ty = &mut *slot.as_ptr();
            if ty.tp_repr.is_null() {
                ty.tp_repr = synth_tp_repr as *mut c_void;
            }
            if ty.tp_str.is_null() {
                ty.tp_str = synth_tp_str as *mut c_void;
            }
            if ty.tp_hash.is_null() {
                ty.tp_hash = synth_tp_hash as *mut c_void;
            }
        }
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
        O::MemoryView(_) => PyMemoryView_Type.as_ptr(),
        O::Module(_) => PyModule_Type.as_ptr(),
        O::Function(_) => PyFunction_Type.as_ptr(),
        O::Builtin(_) => PyCFunction_Type.as_ptr(),
        O::BoundMethod(_) => PyMethod_Type.as_ptr(),
        O::Generator(_) => PyGen_Type.as_ptr(),
        O::Iter(_) => PySeqIter_Type.as_ptr(),
        O::Coroutine(_) => PyCoro_Type.as_ptr(),
        O::AsyncGenerator(_) => PyAsyncGen_Type.as_ptr(),
        O::Slice(_) => PySlice_Type.as_ptr(),
        O::Type(t) => find_type_ptr(t).unwrap_or_else(|| PyType_Type.as_ptr()),
        O::Instance(inst) => {
            let cls = inst.cls();
            // RFC 0029 (wave 5): an instance crosses wearing its *real* type,
            // not a bare `object`. A pure-Python subclass of a faithful C base
            // — pytz's `UTC ← BaseTzInfo ← datetime.tzinfo`, consumed by
            // pandas' `cdef tzinfo utc_pytz = pytz.utc` (a Cython
            // `__Pyx_TypeTest(obj, datetime.tzinfo)`) — only passes the C-side
            // subtype check if `Py_TYPE(obj)`'s `tp_base` chain reaches the
            // `tzinfo` shell. `install_user_type` builds that chain (minting
            // intermediate bases), so use it as the final fallback rather than
            // collapsing every unregistered instance to `object`.
            find_type_ptr(&cls)
                .or_else(|| synth_type_for_class(&cls))
                .unwrap_or_else(|| install_user_type(&cls))
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
    // RFC 0029 (wave 5): the `datetime` module's classes resolve to the
    // faithful, size-correct C types (minted on first use). Authoritative
    // and identity-checked, so it runs *before* the generic registry scan
    // and never collides with a coincidentally-named user class.
    if let Some(p) = crate::datetime_api::faithful_type_for_class(t) {
        return Some(p);
    }
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
    let from_heap = HEAP_TYPES.lock().ok().and_then(|g| {
        for &addr in g.iter() {
            let p = addr as *mut PyTypeObject;
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
                if std::env::var_os("WEAVEPY_TRACE_TYPEPTR").is_some() {
                    eprintln!(
                        "[FINDPTR] readied match name={:?} ext_ptr={:p} bridge={:p}",
                        rt.bridge.name, rt.ext_ptr, target
                    );
                }
                return Some(rt.ext_ptr);
            }
        }
        None
    })
}

/// Registry of heap-allocated `PyTypeObject *` produced by
/// `PyType_FromSpec[WithBases]` (and the bridged `PyExc_*` statics,
/// installed via [`install_user_type`]). Looked up by [`find_type_ptr`]
/// when an `Object::Instance` is materialised back into a boxed
/// `*mut PyObject`, so the box's `ob_type` matches the extension's
/// declared type, and by [`is_weavepy_owned_type`] to decide whether
/// `bridge_type` may read the trailing `bridge` field.
///
/// RFC 0046 (wave 4): process-global, **not** thread-local. A heap
/// type's identity is a property of the process, not of one OS thread:
/// the boxes are `Box::leak`'d (immortal, stable pointers) and their
/// bridge is an `Arc<TypeObject>` (`Send + Sync`). The `PyExc_*`
/// statics in particular are published once (under a global lock) on
/// whatever thread first initialises the runtime, then read from
/// *every* thread — so a thread-local registry made
/// `clone_object(PyExc_ValueError)` resolve to a foreign proxy on any
/// other thread, collapsing `PyErr_SetString(PyExc_ValueError, …)` to a
/// bare `RuntimeError`. Stored as `usize` addresses so the `static` is
/// `Send` (mirrors [`crate::object`]'s `MINTED` set). The interpreter
/// runs single-threaded under the GIL, so the mutex is uncontended.
static HEAP_TYPES: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// Register a heap-allocated type pointer so subsequent
/// `Object::Instance` boxes get the extension's declared
/// `ob_type` instead of falling back to `PyBaseObject_Type`.
pub fn register_heap_type(p: *mut PyTypeObject) {
    if p.is_null() {
        return;
    }
    if let Ok(mut g) = HEAP_TYPES.lock() {
        if !g.contains(&(p as usize)) {
            g.push(p as usize);
        }
    }
}

// ---------------------------------------------------------------------------
// RFC 0047 (wave 5): synthesize a faithful C `PyTypeObject` for a *Python*
// class that crosses into a C extension.
//
// WeavePy classes (incl. stdlib ones written in Python like
// `itertools.cycle`) have no `PyType_FromSpec`-registered C type, so a
// bare `Object::Instance` previously crossed into C wearing
// `PyBaseObject_Type`. Macro-heavy Cython then reads protocol slots
// (`Py_TYPE(it)->tp_iternext`, `->tp_call`, …) straight off that struct,
// finds them NULL, and silently errors (`numpy.random.SeedSequence.
// generate_state` iterates `cycle(self.pool)`).
//
// We mint one immortal type per class, populated from the class's dunders
// with bridges that forward to the recursion-safe abstract C-API (which
// dispatches on the Rust `Object`, never re-reading these slots). Scoped
// to iterables/iterators to keep the blast radius small: every other
// instance keeps the historic `PyBaseObject_Type` crossing.

unsafe extern "C" fn synth_tp_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abstract_::PyObject_GetIter(o) }
}
unsafe extern "C" fn synth_tp_iternext(o: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abstract_::PyIter_Next(o) }
}
unsafe extern "C" fn synth_tp_call(
    o: *mut PyObject,
    a: *mut PyObject,
    k: *mut PyObject,
) -> *mut PyObject {
    unsafe { crate::abstract_::PyObject_Call(o, a, k) }
}
unsafe extern "C" fn synth_tp_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abstract_::PyObject_Repr(o) }
}
unsafe extern "C" fn synth_tp_str(o: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abstract_::PyObject_Str(o) }
}
unsafe extern "C" fn synth_tp_hash(o: *mut PyObject) -> crate::object::PyHashT {
    unsafe { crate::abstract_::PyObject_Hash(o) }
}

/// Address of the VM-forwarding `tp_hash` bridge. `hash_via_slot` uses it to
/// short-circuit the shim for a *foreign* object: such an object's VM hash
/// (`py_hash_value` → `foreign::hash` → `fwd_hash` → `hash_via_slot`) would
/// otherwise re-enter this bridge (`synth_tp_hash` → `PyObject_Hash` →
/// `hash_public` → `py_hash_value`) and ping-pong `VM → C → VM` until the
/// stack overflows. The bridge adds nothing over the VM's own dispatch for a
/// foreign value, so it must be treated as "no native slot".
pub(crate) fn synth_tp_hash_addr() -> *mut c_void {
    synth_tp_hash as *mut c_void
}
unsafe extern "C" fn synth_length(o: *mut PyObject) -> PySsizeT {
    unsafe { crate::abstract_::PyObject_Length(o) }
}
unsafe extern "C" fn synth_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe { crate::abstract_::PyObject_GetItem(o, k) }
}
unsafe extern "C" fn synth_ass_subscript(
    o: *mut PyObject,
    k: *mut PyObject,
    v: *mut PyObject,
) -> c_int {
    unsafe { crate::abstract_::PyObject_SetItem(o, k, v) }
}

/// `tp_descr_get` for WeavePy callables (plain functions, instance-binding
/// builtin methods, method descriptors) that cross into a C extension as a
/// type-dict entry.
///
/// CPython's special-method protocol — the one Cython emits for `with`,
/// `for`, operators (`__Pyx_PyObject_LookupSpecial`) — does
/// `res = _PyType_Lookup(tp, name); f = Py_TYPE(res)->tp_descr_get; res =
/// f(res, obj, tp);` to **bind** the found descriptor to the instance. With
/// no `tp_descr_get` wired on the function/method types the descriptor was
/// taken *unbound*, so a bound special method (e.g. a lock's `__exit__`) was
/// then called with `self` missing — surfacing as `AttributeError`/`TypeError`
/// deep inside an extension's module init. This mirrors CPython's
/// `func_descr_get` / `method_get`: bind to `obj` (yielding a `method`), or
/// return the descriptor unchanged for class access (`obj == NULL`/`None`) and
/// for a non-instance-binding builtin (a static/module function).
unsafe extern "C" fn callable_descr_get(
    descr: *mut PyObject,
    obj: *mut PyObject,
    _type: *mut PyObject,
) -> *mut PyObject {
    if std::env::var_os("WEAVEPY_TRACE_CTOR").is_some() {
        let dty = if descr.is_null() {
            ptr::null_mut()
        } else {
            unsafe { (*descr).ob_type }
        };
        eprintln!(
            "[DESCRGET] descr={descr:p} descr.ob_type={dty:p} obj={obj:p} type={_type:p}"
        );
    }
    let trace = std::env::var_os("WEAVEPY_TRACE_CTOR").is_some();
    if descr.is_null() {
        return ptr::null_mut();
    }
    // Class access (`Type.method`) yields the descriptor unchanged.
    if obj.is_null() {
        unsafe { crate::object::Py_IncRef(descr) };
        return descr;
    }
    if trace {
        eprintln!("[DESCRGET] step=clone_obj");
    }
    let receiver = unsafe { crate::object::clone_object(obj) };
    if trace {
        eprintln!("[DESCRGET] step=clone_obj_done recv={}", receiver.type_name());
    }
    if matches!(receiver, Object::None) {
        unsafe { crate::object::Py_IncRef(descr) };
        return descr;
    }
    if trace {
        eprintln!("[DESCRGET] step=clone_descr");
    }
    let d = unsafe { crate::object::clone_object(descr) };
    if trace {
        eprintln!("[DESCRGET] step=clone_descr_done d={}", d.type_name());
    }
    let bind = match &d {
        Object::Function(_) => true,
        Object::Builtin(b) => b.binds_instance,
        _ => false,
    };
    if !bind {
        unsafe { crate::object::Py_IncRef(descr) };
        return descr;
    }
    if trace {
        eprintln!("[DESCRGET] step=make_bound");
    }
    let bound = Object::BoundMethod(Rc::new(BoundMethod::new(receiver, d)));
    if trace {
        eprintln!("[DESCRGET] step=into_owned");
    }
    let r = crate::object::into_owned(bound);
    if trace {
        eprintln!("[DESCRGET] step=done r={r:p}");
    }
    r
}

/// Serialises synth-type creation so a class crossing concurrently mints
/// exactly one type.
static SYNTH_LOCK: Mutex<()> = Mutex::new(());

/// True when `cls` (or any base) defines `name` as a non-`None` attribute.
fn class_has_dunder(cls: &Rc<TypeObject>, name: &str) -> bool {
    !matches!(cls.lookup(name), None | Some(Object::None))
}

/// Populate a synthesised mirror's C-level protocol slots from `cls`'s
/// Python dunder methods. Macro-heavy extension code (Cython's
/// `__Pyx_PyObject_GetItem` → `Py_TYPE(o)->tp_as_mapping->mp_subscript`,
/// `__Pyx_PyObject_Call` → `tp_call`, the `for`/`with` slot reads) consults
/// these slots *directly* off `Py_TYPE(obj)`, bypassing the abstract API. A
/// mirror that leaves them NULL therefore looks e.g. "not subscriptable" /
/// "not callable" to an extension even though the VM implements the operation
/// (pandas' `lib.pyx` does `Literal[_NoDefault.no_default]` — a `__getitem__`
/// on the frozen `typing._SpecialForm` — during single-pass module exec).
/// Shared by [`synth_type_for_class`] and [`install_user_type`] so every
/// Python-class crossing exposes a faithful slot table regardless of which
/// mirror-minting path built it. Each `synth_*` bridge forwards back into the
/// VM's dispatch.
fn synth_protocol_slots(ty: &mut PyTypeObject, cls: &Rc<TypeObject>) {
    if class_has_dunder(cls, "__iter__") {
        ty.tp_iter = synth_tp_iter as *mut c_void;
    }
    if class_has_dunder(cls, "__next__") {
        ty.tp_iternext = synth_tp_iternext as *mut c_void;
        // CPython iterators answer `iter(it) is it`; advertise tp_iter too.
        if ty.tp_iter.is_null() {
            ty.tp_iter = synth_tp_iter as *mut c_void;
        }
    }
    if class_has_dunder(cls, "__call__") {
        ty.tp_call = synth_tp_call as *mut c_void;
    }
    if class_has_dunder(cls, "__repr__") {
        ty.tp_repr = synth_tp_repr as *mut c_void;
    }
    if class_has_dunder(cls, "__str__") {
        ty.tp_str = synth_tp_str as *mut c_void;
    }
    if class_has_dunder(cls, "__hash__") {
        ty.tp_hash = synth_tp_hash as *mut c_void;
    }
    let has_len = class_has_dunder(cls, "__len__");
    let has_getitem = class_has_dunder(cls, "__getitem__");
    let has_setitem =
        class_has_dunder(cls, "__setitem__") || class_has_dunder(cls, "__delitem__");
    if has_len || has_getitem || has_setitem {
        let mut mm: crate::layout::PyMappingMethods = unsafe { std::mem::zeroed() };
        if has_len {
            mm.mp_length = synth_length as *mut c_void;
        }
        if has_getitem {
            mm.mp_subscript = synth_subscript as *mut c_void;
        }
        if has_setitem {
            mm.mp_ass_subscript = synth_ass_subscript as *mut c_void;
        }
        ty.tp_as_mapping = Box::into_raw(Box::new(mm)) as *mut c_void;
        if has_len {
            let mut sm: crate::layout::PySequenceMethods = unsafe { std::mem::zeroed() };
            sm.sq_length = synth_length as *mut c_void;
            ty.tp_as_sequence = Box::into_raw(Box::new(sm)) as *mut c_void;
        }
    }
}

/// Mint (or return the cached) faithful C type for a Python class whose
/// instances drive a C-level protocol Cython reads off `Py_TYPE(obj)`:
/// iteration (`__iter__`/`__next__`) or the context-manager protocol
/// (`__enter__`/`__exit__`, looked up via `_PyType_Lookup` for `with`).
/// Returns `None` for every other class, leaving the historic
/// `PyBaseObject_Type` crossing in place to keep the blast radius small.
pub(crate) fn synth_type_for_class(cls: &Rc<TypeObject>) -> Option<*mut PyTypeObject> {
    let is_iter = class_has_dunder(cls, "__iter__") || class_has_dunder(cls, "__next__");
    let is_ctx = class_has_dunder(cls, "__enter__") || class_has_dunder(cls, "__exit__");
    if !is_iter && !is_ctx {
        return None;
    }
    let _guard = SYNTH_LOCK.lock().ok()?;
    // Another thread may have minted + registered it between our caller's
    // `find_type_ptr` miss and acquiring the lock.
    if let Some(p) = find_type_ptr(cls) {
        return Some(p);
    }

    let meta = metaclass_ptr(cls);
    // RFC 0045 (wave 5): a synthesised shell can itself subclass an inline C
    // type — numpy's `class MaskedArray(ndarray)` is iterable, so it reaches
    // *this* path (not `install_user_type`) yet must still get a faithful
    // `tp_basicsize`-wide body. Without inheriting the base's inline layout
    // its instances were plain 16-byte boxes; numpy's `PyArray_NewFromDescr`
    // then wrote the `PyArrayObject` fields over the Rust `obj` payload and a
    // later `clone_object` dereferenced the clobbered pointer (a SIGBUS in
    // `numpy.ma.core`'s `array_view → __array_finalize__`).
    let (inline_base_ptr, base_inline, basicsize) = inherit_inline_base_layout(cls, 16);
    let mut ty = PyTypeObject::new_zeroed();
    ty.head.ob_refcnt = IMMORTAL_REFCNT;
    ty.head.ob_type = meta;
    let cname = std::ffi::CString::new(cls.name.clone())
        .unwrap_or_else(|_| std::ffi::CString::new("object").unwrap());
    ty.tp_name = cname.into_raw() as *const c_char;
    ty.tp_basicsize = basicsize;
    ty.tp_dealloc = Some(crate::object::_PyWeavePy_Dealloc);
    // Mirror CPython's `PyType_Ready` `tp_alloc`/`tp_free` defaults so a
    // foreign C `tp_new` subclassing this synthesised type can allocate
    // through its `tp_alloc` slot (see `install_user_type`).
    {
        let alloc_fp: unsafe extern "C" fn(*mut PyTypeObject, PySsizeT) -> *mut PyObject =
            crate::genericalloc::PyType_GenericAlloc;
        let new_fp: unsafe extern "C" fn(
            *mut PyTypeObject,
            *mut PyObject,
            *mut PyObject,
        ) -> *mut PyObject = crate::genericalloc::PyType_GenericNew;
        let free_fp: unsafe extern "C" fn(*mut c_void) = crate::memory::PyObject_Free;
        ty.tp_alloc = alloc_fp as *mut c_void;
        // Inherit the solid (best_base) inline base's faithful `tp_new` so
        // its `cdef object` fields are initialised (see the matching note in
        // `install_user_type`). Non-inline shells keep the generic allocator.
        ty.tp_new = if base_inline {
            let inherited = inline_base_ptr
                .filter(|bp| !bp.is_null())
                .map(|bp| unsafe { (*bp).tp_new })
                .unwrap_or(ptr::null_mut());
            if inherited.is_null() {
                new_fp as *mut c_void
            } else {
                inherited
            }
        } else {
            new_fp as *mut c_void
        };
        ty.tp_free = free_fp as *mut c_void;
    }
    ty.tp_flags = crate::layout::tpflags::DEFAULT | crate::layout::tpflags::BASETYPE;
    // Point `tp_base` at the faithful inline base (so `PyType_IsSubtype`
    // and numpy's `PyArray_Check` walk to `ndarray`); a non-inline shell
    // keeps the historic bare-`object` base to bound the blast radius.
    ty.tp_base = if base_inline {
        inline_base_ptr.unwrap_or_else(|| PyBaseObject_Type.as_ptr())
    } else {
        PyBaseObject_Type.as_ptr()
    };
    ty.bridge = Box::into_raw(Box::new(cls.clone()));

    synth_protocol_slots(&mut ty, cls);

    let p = Box::into_raw(Box::new(ty));
    register_heap_type(p);
    // Mirror the inline base: instances need the same faithful inline body
    // (RFC 0045) so fixed-offset field reads/writes land on real
    // CPython-shaped memory rather than the Rust `obj` payload.
    if base_inline {
        maybe_register_inline_type(p);
    }
    Some(p)
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
    &PySeqIter_Type,
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
    // RFC 0046 (wave 4): the private `bridge` field sits at offset 424 —
    // *past* the 416-byte stock `PyTypeObject`. A foreign extension type
    // (numpy's metatypes, an un-readied static type) is exactly that stock
    // size, so reading `.bridge` would be an out-of-bounds heap read (it
    // surfaced as a misaligned-pointer fault on a numpy type). Only our own
    // static/heap type boxes carry the field; decide by pointer identity.
    if !is_weavepy_owned_type(ty) {
        return None;
    }
    let bridge = unsafe { (*ty).bridge };
    if bridge.is_null() {
        return None;
    }
    Some(unsafe { (*bridge).clone() })
}

/// Resolve a `PyTypeObject*` to its bridged [`TypeObject`], readying it
/// on demand if it has not been bridged yet.
///
/// CPython's `PyType_Ready` finalises a type's **bases before the type
/// itself** (`type_ready` → `type_ready_mro`, which `PyType_Ready`s each
/// base). A stock extension that readies a subtype before its base —
/// numpy registers `Float16DType` before `PyType_Ready(&PyArrayDescr_Type)`
/// has run in some import orders — would otherwise lose the base entirely:
/// `bridge_type` returns `None`, the bridged type silently falls back to
/// `object`, and the subtype's MRO collapses to `[Self, object]`. That
/// dropped every getset the base declares (e.g. `numpy.dtype.type`).
///
/// SAFETY: `ty` must be null or a valid (own or stock) `PyTypeObject*`.
pub unsafe fn bridge_or_ready(ty: *mut PyTypeObject) -> Option<Rc<TypeObject>> {
    if ty.is_null() {
        return None;
    }
    if let Some(t) = unsafe { bridge_type(ty) } {
        return Some(t);
    }
    // Not yet bridged: ready it (idempotent) and retry. Foreign stock
    // types get harvested; our own types short-circuit inside PyType_Ready.
    unsafe { PyType_Ready(ty) };
    unsafe { bridge_type(ty) }
}

/// Mirror CPython's `inherit_special` fast-subclass bit assignment
/// (`Objects/typeobject.c`): a finalised type carries exactly one
/// `Py_TPFLAGS_*_SUBCLASS` bit, for the most-derived builtin in its
/// ancestry, so a stock extension's inlined `PyX_Check`
/// (`Py_TYPE(o)->tp_flags & Py_TPFLAGS_X_SUBCLASS`) classifies it without
/// an MRO walk. The else-if order matches CPython exactly. In particular
/// Cython's `__Pyx_ImportType` rejects `numpy.dtype` ("is not a type
/// object") unless its metaclass `numpy._DTypeMeta` — a `type` subclass —
/// carries `Py_TPFLAGS_TYPE_SUBCLASS`, and `__Pyx_Raise` rejects a
/// `cdef`-raised exception unless its class carries
/// `Py_TPFLAGS_BASE_EXC_SUBCLASS`.
fn fast_subclass_flags(t: &Rc<TypeObject>) -> u64 {
    use crate::layout::tpflags;
    let bt = weavepy_vm::builtin_types::builtin_types();
    if t.is_subclass_of(&bt.base_exception) {
        tpflags::BASE_EXC_SUBCLASS
    } else if t.is_subclass_of(&bt.type_) {
        tpflags::TYPE_SUBCLASS
    } else if t.is_subclass_of(&bt.int_) {
        tpflags::LONG_SUBCLASS
    } else if t.is_subclass_of(&bt.bytes_) {
        tpflags::BYTES_SUBCLASS
    } else if t.is_subclass_of(&bt.str_) {
        tpflags::UNICODE_SUBCLASS
    } else if t.is_subclass_of(&bt.tuple_) {
        tpflags::TUPLE_SUBCLASS
    } else if t.is_subclass_of(&bt.list_) {
        tpflags::LIST_SUBCLASS
    } else if t.is_subclass_of(&bt.dict_) {
        tpflags::DICT_SUBCLASS
    } else {
        0
    }
}

/// The canonical C `PyTypeObject*` for `t`'s **metaclass**, used as a
/// type mirror's `ob_type`.
///
/// CPython code (and Cython-generated code in particular) reads a class's
/// metaclass straight off `Py_TYPE(cls)`: `type(Enum)` must be `EnumType`,
/// `type(np.dtype)` must be `numpy._DTypeMeta`, and so on. Historically
/// every WeavePy type mirror hard-coded `ob_type = PyType_Type` (bare
/// `type`), so `__Pyx_CalculateMetaclass((Enum,))` resolved to `type`,
/// `type.__prepare__` returned a plain dict instead of `EnumType`'s
/// `_EnumDict`, and a Cython module defining a Python `class X(Enum)`
/// failed its multi-phase init with
/// `AttributeError: 'dict' object has no attribute '_member_names'`.
///
/// The metaclass mirror is minted on demand. Recursion bottoms out at the
/// static `PyType_Type`: `type`'s own metaclass is `type`, and every
/// well-formed metaclass chain terminates there.
fn metaclass_ptr(t: &Rc<TypeObject>) -> *mut PyTypeObject {
    let mc = t.metaclass_or_type();
    let bt = weavepy_vm::builtin_types::builtin_types();
    if Rc::ptr_eq(&mc, &bt.type_) || Rc::ptr_eq(&mc, t) {
        return PyType_Type.as_ptr();
    }
    if let Some(p) = find_type_ptr(&mc) {
        return p;
    }
    install_user_type(&mc)
}

/// Resolve `t`'s first base to its canonical C `PyTypeObject*` and decide
/// whether `t` inherits that base's **inline `tp_basicsize` storage**.
///
/// A pure-Python (VM) class can subclass an *inline* C extension type —
/// pandas' `class NaTType(_NaT)` (the `_NaT ← datetime` shell) or numpy's
/// `class MaskedArray(ndarray)`. CPython inherits `tp_basicsize`, so the
/// subclass instance carries the base's faithful C layout: a stock
/// `tp_new` packs fields at fixed offsets and the extension reads
/// `((BaseObject *)self)->field` directly. The mirror must therefore get a
/// faithful inline body, not a `PyObjectBox` (whose Rust payload sits
/// exactly where the extension expects `self->field`, so those writes
/// corrupt it — RFC 0045 / 0029).
///
/// Returns `(base_ptr, base_inline, basicsize)`. When the base is inline,
/// `basicsize` is the base's `tp_basicsize`; otherwise it is
/// `non_inline_default`. An unregistered pure-Python intermediate base
/// (pytz `UTC ← BaseTzInfo ← tzinfo`) is minted via `install_user_type`
/// so the `tp_base` chain reaches the faithful root; the `object` root
/// (no bases) yields `None`. Shared by [`install_user_type`] and
/// [`synth_type_for_class`] so a VM subclass of an inline C type gets a
/// faithful body regardless of which mirror-minting path it takes.
fn inherit_inline_base_layout(
    t: &Rc<TypeObject>,
    non_inline_default: PySsizeT,
) -> (Option<*mut PyTypeObject>, bool, PySsizeT) {
    // Resolve a direct base to its canonical C `PyTypeObject*`, minting a
    // mirror for a pure-Python intermediate base (one that itself has
    // bases) so the layout probe reaches the faithful root; the `object`
    // root (no bases) yields `None`.
    let resolve = |b: &Rc<TypeObject>| -> Option<*mut PyTypeObject> {
        type_ptr_for_class(b).or_else(|| {
            if b.bases.borrow().is_empty() {
                None
            } else {
                Some(install_user_type(b))
            }
        })
    };

    let bases = t.bases.borrow();
    let first_ptr = bases.first().and_then(&resolve);

    // CPython's `best_base()`: the "solid base" is the base with the widest
    // instance layout (`tp_basicsize`), scanned across *all* bases — not
    // just the first. A VM class that lists an inline C extension type
    // anywhere in its bases must inherit that faithful inline body. pandas'
    // `class Block(PandasObject, libinternals.Block)` puts its inline
    // Cython base *second* (the first base is the pure-Python
    // `PandasObject`); probing only `bases.first()` left `Block` — and its
    // subclasses `NumpyBlock`/`NumericBlock`/`ObjectBlock` — on a 16-byte
    // `PyObjectBox`, so Cython's fixed-offset field writes (`_mgr_locs`,
    // `values`, `refs`) clobbered the Rust `obj` payload and the later free
    // dereferenced the garbage (SIGBUS).
    let mut solid: Option<*mut PyTypeObject> = None;
    let mut solid_size: PySsizeT = 0;
    for b in bases.iter() {
        if let Some(bp) = resolve(b) {
            if is_inline_instance_type(bp) {
                let sz = unsafe { (*bp).tp_basicsize };
                if solid.is_none() || sz > solid_size {
                    solid = Some(bp);
                    solid_size = sz;
                }
            }
        }
    }

    match solid {
        Some(sp) => (Some(sp), true, solid_size),
        None => (first_ptr, false, non_inline_default),
    }
}

/// Find the static [`PyTypeObject`] pointer that bridges to `t`,
/// installing one on demand for user-defined classes (e.g. heap
/// types created without `PyType_FromSpec` — usually never; this is
/// a fallback path).
pub fn install_user_type(t: &Rc<TypeObject>) -> *mut PyTypeObject {
    if let Some(p) = find_type_ptr(t) {
        return p;
    }
    // Resolve (minting if needed) the metaclass mirror *before* building
    // this type's box so `Py_TYPE(t)` reports the real metaclass.
    let meta = metaclass_ptr(t);
    let owned_name = format!("{}\0", t.name).into_bytes();
    // Stock extensions read `tp_flags` directly to classify a type. In
    // particular Cython's `__Pyx_Raise` gates `raise exc` on
    // `PyExceptionInstance_Check(x)` ≡ `Py_TYPE(x)->tp_flags &
    // Py_TPFLAGS_BASE_EXC_SUBCLASS`, so a WeavePy exception type published
    // here with `tp_flags == 0` makes every `raise RuntimeError(...)` from
    // a `cdef` method fail with "exception class must be a subclass of
    // BaseException". Mirror CPython: every readied type carries
    // DEFAULT | BASETYPE | READY, and an exception subclass also carries
    // the BASE_EXC_SUBCLASS fast-subclass bit.
    use crate::layout::tpflags;
    let flags =
        tpflags::DEFAULT | tpflags::BASETYPE | tpflags::READY | fast_subclass_flags(t);
    // RFC 0029 / 0045 (wave 5): inherit an *inline* C base's `tp_basicsize`
    // and inline-storage status (see [`inherit_inline_base_layout`]) so a VM
    // subclass of e.g. the `datetime` shell (pandas `NaTType`) or
    // `numpy.ndarray` gets a faithful body, not a `PyObjectBox` the base's
    // fixed-offset field writes would corrupt. A non-inline base keeps the
    // identity-box size.
    let (base_ptr, base_inline, basicsize) = inherit_inline_base_layout(
        t,
        std::mem::size_of::<crate::object::PyObjectBox>() as PySsizeT,
    );
    // RFC 0046 (wave 4/5): mirror CPython's `PyType_Ready` defaults for
    // `tp_alloc`/`tp_free` (inherited from `object`). A *foreign* C `tp_new`
    // that subclasses this VM type allocates through `subtype->tp_alloc(
    // subtype, 0)` directly — pandas' `class NAType(C_NAType)` runs the cdef
    // base's `C_NAType.__pyx_tp_new`, which calls `NAType->tp_alloc`. With a
    // NULL slot that is a jump through address 0 (SIGSEGV). `PyType_GenericAlloc`
    // mints a faithful instance body bound to this type's bridged class.
    let alloc_fp: unsafe extern "C" fn(*mut PyTypeObject, PySsizeT) -> *mut PyObject =
        crate::genericalloc::PyType_GenericAlloc;
    let new_fp: unsafe extern "C" fn(*mut PyTypeObject, *mut PyObject, *mut PyObject) -> *mut PyObject =
        crate::genericalloc::PyType_GenericNew;
    let free_fp: unsafe extern "C" fn(*mut c_void) = crate::memory::PyObject_Free;
    // RFC 0047 (wave 5): mirror CPython's `tp_new` inheritance. CPython's
    // `type_ready_inherit`/`inherit_special` copies `tp_new` down from
    // `tp_base` — which for a multiply-inheriting class is the *solid base*
    // (`best_base`), i.e. the base that owns the widest inline instance
    // layout, **not** the first base on the MRO. `inherit_inline_base_layout`
    // already resolved that solid base into `base_ptr`; adopt its faithful
    // `tp_new` so the C struct fields it declares are initialised
    // (Cython's `__pyx_tp_new` zeroes every `cdef object` field to `None`).
    //
    // This is the fix for pandas' `class MultiIndexUIntEngine(
    // BaseMultiIndexCodesEngine, UInt64Engine)`: the first MRO base
    // (`BaseMultiIndexCodesEngine`, no inline fields) has a `tp_new` that
    // leaves the inherited `IndexEngine` fields (`values`/`mask`/`mapping`)
    // NULL, so `IndexEngine.__init__`'s plain `__Pyx_DECREF(self->values)`
    // dereferenced NULL. The solid base (`UInt64Engine ← IndexEngine`) owns
    // those fields and its `tp_new` sets them to `None`. Only inline bases
    // carry a faithful C `tp_new` worth inheriting; a non-inline base keeps
    // the generic allocator (the shim still forwards to the base's captured
    // slot for those, unchanged).
    let tp_new_slot: *mut c_void = if base_inline {
        let inherited = base_ptr
            .filter(|bp| !bp.is_null())
            .map(|bp| unsafe { (*bp).tp_new })
            .unwrap_or(ptr::null_mut());
        if inherited.is_null() {
            new_fp as *mut c_void
        } else {
            inherited
        }
    } else {
        new_fp as *mut c_void
    };
    let bx = Box::new(PyTypeObjectBox {
        head: PyTypeObject {
            head: PyObject {
                ob_refcnt: IMMORTAL_REFCNT,
                ob_type: meta,
            },
            tp_name: owned_name.as_ptr() as *const c_char,
            tp_basicsize: basicsize,
            tp_base: base_ptr.unwrap_or(ptr::null_mut()),
            tp_dealloc: Some(crate::object::_PyWeavePy_Dealloc),
            tp_alloc: alloc_fp as *mut c_void,
            tp_new: tp_new_slot,
            tp_free: free_fp as *mut c_void,
            tp_flags: flags,
            bridge: Box::into_raw(Box::new(t.clone())),
            ..PyTypeObject::new_zeroed()
        },
        owned_name,
        slot_table: SlotTable::empty(),
    });
    let p = Box::leak(bx);
    // Derive the C-level protocol slots (`tp_as_mapping`/`mp_subscript`,
    // `tp_call`, `tp_iter`, …) from the class's dunders so an extension that
    // reads them straight off `Py_TYPE(obj)` (Cython's inlined subscript /
    // call / iteration helpers) sees a faithful table — not the NULL suite a
    // bare mirror would carry. Without this a frozen Python class crossing
    // here (e.g. `typing._SpecialForm`, which only defines `__getitem__` and
    // so misses the iter/ctx gate in `synth_type_for_class`) is "not
    // subscriptable" to a wheel's module-init code.
    synth_protocol_slots(&mut p.head, t);
    let ty_ptr = &mut p.head as *mut PyTypeObject;
    // Cache so subsequent calls with the same native `Rc` return the
    // same pointer instead of leaking a fresh box every time
    // (`PyExc_*` aliases — e.g. `SystemError` → `runtime_error` —
    // would otherwise install distinct slots for the same type).
    register_heap_type(ty_ptr);
    // Mirror the inline base: instances of this subclass need the same
    // faithful inline body (RFC 0045) so fixed-offset field reads/writes
    // land on real CPython-shaped memory.
    if base_inline {
        maybe_register_inline_type(ty_ptr);
    }
    ty_ptr
}

// ----------------------------------------------------------------
// PyType_FromSpec — the heart of "extension defines a class".
// ----------------------------------------------------------------

pub const PY_TPFLAGS_HEAPTYPE: u32 = 1 << 9;
/// `Py_TPFLAGS_IS_ABSTRACT` — set on abstract base classes. Used
/// transiently by [`crate::instance`] to neutralise a Cython
/// `@cython.freelist` `tp_dealloc`: the freelist stash is guarded by
/// `!HasFeature(Py_TPFLAGS_IS_ABSTRACT)` in **both** Cython codegen
/// variants (classic and type-specs), so temporarily setting it forces
/// the release (`tp_free`) branch instead of the raw-pointer stash.
pub const PY_TPFLAGS_IS_ABSTRACT: u32 = 1 << 20;
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
// Entries are `Box::leak`'d (readied types live for the process
// lifetime), so the `&'static` borrows handed out by `bridge_type` /
// `slot_table_for` stay valid. The map itself is thread-local: a stock
// extension readies its types and uses them on the same thread, so
// (unlike the cross-thread `PyExc_*` statics in the now-global
// `HEAP_TYPES`) no cross-thread visibility is required here.
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
    let registered = basicsize > std::mem::size_of::<PyObject>();
    if registered {
        INLINE_TYPES.with(|s| s.borrow_mut().insert(ty as usize));
    }
    if std::env::var_os("WEAVEPY_TRACE_CTOR").is_some() {
        eprintln!(
            "[CTOR] register_inline name={} ty={:p} basicsize={} sizeof_pyobj={} registered={}",
            ctor_trace_name(ty),
            ty,
            basicsize,
            std::mem::size_of::<PyObject>(),
            registered
        );
    }
}

/// Best-effort `tp_name` for constructor tracing (`WEAVEPY_TRACE_CTOR`).
pub fn ctor_trace_name(ty: *mut PyTypeObject) -> String {
    if ty.is_null() {
        return "<null>".to_owned();
    }
    let n = unsafe { (*ty).tp_name };
    if n.is_null() {
        return "<noname>".to_owned();
    }
    unsafe { CStr::from_ptr(n) }.to_string_lossy().into_owned()
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

/// Build a faithful C-level tuple of the canonical `PyTypeObject*` for
/// each class in `types`. Used to publish `tp_bases` / `tp_mro` on a
/// readied / spec-built type so Cython-generated code can walk them via
/// the `PyTuple_GET_SIZE` / `PyTuple_GET_ITEM` macros (direct struct
/// reads, no function call). Each entry must already have a registered
/// canonical pointer (built after the type itself is registered so its
/// own `tp_mro[0]` slot resolves). Returns NULL on allocation failure.
unsafe fn build_type_ptr_tuple(types: &[Rc<TypeObject>]) -> *mut PyObject {
    let tup = unsafe { crate::containers::PyTuple_New(types.len() as PySsizeT) };
    if tup.is_null() {
        return ptr::null_mut();
    }
    for (i, cls) in types.iter().enumerate() {
        // `into_owned(Object::Type)` resolves to the canonical
        // `PyTypeObject*` (static / heap / readied) and hands back an
        // owned reference, which `PyTuple_SetItem` then steals.
        let p = crate::object::into_owned(Object::Type(cls.clone()));
        unsafe { crate::containers::PyTuple_SetItem(tup, i as PySsizeT, p) };
    }
    tup
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
                        if let Some(t) = unsafe { bridge_or_ready(ty_ptr) } {
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
        Err(e) => {
            crate::errors::set_pending_from_runtime(e);
            return ptr::null_mut();
        }
    };
    let owned_name = format!("{qualified}\0").into_bytes();
    // RFC 0047 (wave 5): publish the C-level `tp_dict` backed by the
    // bridge's shared `DictData` (see the `PyType_Ready` path for the full
    // rationale — Cython's `__Pyx_SetVtable`/`__Pyx_GetVtable` read and
    // write `type->tp_dict` directly).
    let tp_dict_box = crate::object::into_owned(Object::Dict(ty.dict.clone()));
    // Snapshot the MRO / bases before `ty` is moved into the box; the
    // faithful C-level `tp_bases` / `tp_mro` are built after the type is
    // registered (so its own pointer resolves for the `tp_mro[0]` slot).
    let mro_for_c: Vec<Rc<TypeObject>> = ty.mro.borrow().clone();
    let bases_for_c: Vec<Rc<TypeObject>> = ty.bases.borrow().clone();
    // RFC 0047 (wave 5): stamp the `inherit_special` fast-subclass bit (see
    // `fast_subclass_flags`) so a spec-built metaclass / builtin subclass is
    // classified correctly by an extension's inlined `tp_flags` reads.
    let subclass_flags = fast_subclass_flags(&ty);
    let bx = Box::new(PyTypeObjectBox {
        head: PyTypeObject {
            head: PyObject {
                ob_refcnt: IMMORTAL_REFCNT,
                ob_type: PyType_Type.as_ptr(),
            },
            tp_name: owned_name.as_ptr() as *const c_char,
            tp_basicsize: spec_ref.basicsize as PySsizeT,
            tp_itemsize: spec_ref.itemsize as PySsizeT,
            tp_flags: (spec_ref.flags | PY_TPFLAGS_HEAPTYPE) as u64 | subclass_flags,
            tp_dealloc: Some(crate::object::_PyWeavePy_Dealloc),
            tp_slots: spec_ref.slots,
            tp_dict: tp_dict_box,
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
    // RFC 0047 (wave 5): publish faithful C-level `tp_base` / `tp_bases` /
    // `tp_mro` (CPython's `PyType_FromMetaclass` sets all three). Cython
    // and other extensions read them directly off the struct.
    unsafe {
        if let Some(bp) = bases_for_c.first().and_then(type_ptr_for_class) {
            (*ty_ptr).tp_base = bp;
        }
        let tp_bases_box = build_type_ptr_tuple(&bases_for_c);
        if !tp_bases_box.is_null() {
            (*ty_ptr).tp_bases = tp_bases_box;
        }
        let tp_mro_box = build_type_ptr_tuple(&mro_for_c);
        if !tp_mro_box.is_null() {
            (*ty_ptr).tp_mro = tp_mro_box;
        }
    }
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
pub(crate) fn is_weavepy_owned_type(ty: *mut PyTypeObject) -> bool {
    for slot in STATIC_TYPE_TABLE {
        if slot.as_ptr() == ty {
            return true;
        }
    }
    HEAP_TYPES
        .lock()
        .map(|g| g.contains(&(ty as usize)))
        .unwrap_or(false)
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
        unsafe { bridge_or_ready(tref.tp_base) }
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

/// Resolve a stock type's full `tp_bases` tuple into bridged VM types.
///
/// CPython's `PyType_Ready` linearises the MRO from **all** of a type's
/// bases, not just `tp_base`. NumPy's numeric scalars use *dual
/// inheritance* (`Objects/typeobject.c`-style `tp_bases`): e.g.
/// `numpy.float64.tp_base == numpy.floating` but
/// `numpy.float64.tp_bases == (numpy.floating, float)`, and likewise
/// `complex128 → (complexfloating, complex)`, `str_ → (str, character)`.
/// Following only `tp_base` truncates the MRO and — critically — drops the
/// Python parent, so `isinstance(np.float64(x), float)` and CPython's
/// `round()` (which requires a `float` subclass) both fail.
///
/// Returns the resolved bases in `tp_bases` order (each readied on
/// demand), or an empty vec when `tp_bases` is absent/unreadable so the
/// caller falls back to the single-`tp_base` path.
unsafe fn harvest_bases(ty: *mut PyTypeObject) -> Vec<Rc<TypeObject>> {
    let tp_bases = unsafe { (*ty).tp_bases };
    if tp_bases.is_null() {
        return Vec::new();
    }
    let n = unsafe { crate::containers::PyTuple_Size(tp_bases) };
    if n <= 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        let item = unsafe { crate::containers::PyTuple_GetItem(tp_bases, i) };
        if item.is_null() {
            continue;
        }
        if let Some(cls) = unsafe { bridge_or_ready(item as *mut PyTypeObject) } {
            out.push(cls);
        }
    }
    out
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

    let mut h = unsafe { harvest_faithful(t) };

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

    // MRO bases: prefer the full `tp_bases` when the type declares more
    // than one (numpy's dual-inherited scalars — `float64`, `complex128`,
    // `str_`, `bytes_`), so the Python parent lands in the linearised MRO
    // (`isinstance(np.float64(x), float)`, `round(...)`, …). Single-base
    // types keep the historical `tp_base`-only path.
    let multi_bases = unsafe { harvest_bases(t) };
    let single_base = h
        .base
        .clone()
        .unwrap_or_else(|| weavepy_vm::builtin_types::builtin_types().object_.clone());

    let dict = assemble_type_dict(
        &qualified,
        &bare,
        &h.slot_table,
        &h.methods,
        h.getset_pairs,
        h.member_pairs,
        h.doc.as_deref(),
    );

    let ty = if multi_bases.len() >= 2 {
        // Retry with only the primary `tp_base` if the full dual-inheritance
        // set has no consistent C3 linearisation — never worse than the
        // historical single-base MRO. `dict` is cloned for the retry.
        match TypeObject::new_user(&bare, multi_bases, dict.clone()) {
            Ok(ty) => ty,
            Err(_) => match TypeObject::new_user(&bare, vec![single_base], dict) {
                Ok(ty) => ty,
                Err(e) => {
                    crate::errors::set_pending_from_runtime(e);
                    return -1;
                }
            },
        }
    } else {
        match TypeObject::new_user(&bare, vec![single_base], dict) {
            Ok(ty) => ty,
            Err(e) => {
                crate::errors::set_pending_from_runtime(e);
                return -1;
            }
        }
    };

    // RFC 0047 (wave 5): publish the C-level `tp_dict`. Cython-generated
    // module init writes into `type->tp_dict` *directly*
    // (`__Pyx_SetVtable` does `PyDict_SetItem(type->tp_dict,
    // "__pyx_vtable__", capsule)`) and reads it back through
    // `__Pyx_GetVtable` for cpdef dispatch. We back it with a dict box
    // sharing the bridge's `DictData`, so a direct `tp_dict` mutation and
    // the VM's MRO lookup observe the same storage. The type is immortal,
    // so the box (refcount 1) lives for the process, matching CPython
    // where the type owns its `tp_dict`.
    let tp_dict_box = crate::object::into_owned(Object::Dict(ty.dict.clone()));

    // RFC 0047 (wave 5): faithful `inherit_slots`. The type dict above
    // carries only the subtype's *own* dunders (inherited behaviour is
    // reached through the MRO, exactly as CPython). But a Cython-generated
    // extension reads `Py_TYPE(self)->tp_*` and `…->tp_as_number->nb_add`
    // **directly off the C struct**, with no MRO walk, so the inherited
    // slots must be baked into both the decoded table (for direct-table
    // dispatch) and the faithful struct (for inlined reads). The base was
    // already readied + flattened during harvest, so one level of copy
    // carries the whole ancestor chain.
    let base_ptr = unsafe { (*t).tp_base };
    unsafe { crate::inherit::inherit_slots(t, &mut h.slot_table, base_ptr) };

    let readied: &'static ReadiedType = Box::leak(Box::new(ReadiedType {
        ext_ptr: t,
        bridge: ty,
        slot_table: h.slot_table,
    }));
    if std::env::var_os("WEAVEPY_TRACE_TYPEPTR").is_some() {
        eprintln!(
            "[READY] name={:?} ext_ptr={:p} bridge={:p}",
            bare,
            t,
            Rc::as_ptr(&readied.bridge)
        );
    }
    READIED_BY_PTR.with(|m| m.borrow_mut().insert(t as usize, readied));
    READIED_TYPES.with(|v| v.borrow_mut().push(readied));
    // RFC 0045 (wave 3): a readied static type that declares inline
    // fields beyond the object head gets faithful `tp_basicsize`
    // instance storage (the `PyArrayObject` shape).
    maybe_register_inline_type(t);

    // RFC 0047 (wave 5): publish faithful C-level `tp_base` / `tp_bases` /
    // `tp_mro`. Cython's `__Pyx_MergeVtables` reads `type->tp_base` and
    // indexes `type->tp_bases` through the `PyTuple_GET_SIZE` /
    // `PyTuple_GET_ITEM` macros (direct struct access), and
    // `__Pyx_setup_reduce` walks `tp_mro` — leaving any of them NULL
    // segfaults. Built *after* the type is registered so its own pointer
    // resolves for the `tp_mro[0]` self entry.
    let base_for_c: Option<*mut PyTypeObject> = readied
        .bridge
        .bases
        .borrow()
        .first()
        .and_then(type_ptr_for_class);
    let bases_for_c: Vec<Rc<TypeObject>> = readied.bridge.bases.borrow().clone();
    let mro_for_c: Vec<Rc<TypeObject>> = readied.bridge.mro.borrow().clone();
    let tp_bases_box = unsafe { build_type_ptr_tuple(&bases_for_c) };
    let tp_mro_box = unsafe { build_type_ptr_tuple(&mro_for_c) };

    // Write-back into the caller's struct — both offsets live inside
    // the faithful 416-byte CPython prefix, so a stock static type is
    // never overrun.
    //
    // RFC 0046 (wave 4): only *fill* a missing metaclass. CPython's
    // `PyType_Ready` sets `ob_type` to `Py_TYPE(tp_base)` (defaulting to
    // `&PyType_Type`) only when it is NULL; it must never clobber a
    // metaclass the extension pre-installed. numpy mallocs each DType
    // class with `ob_type = &PyArrayDTypeMeta_Type` (its metaclass) and
    // relies on the stock inlined `PyObject_TypeCheck(dt,
    // &PyArrayDTypeMeta_Type)` — a direct `ob_type` pointer compare — so
    // overwriting it with `&PyType_Type` made every DType fail
    // validation ("provided object … is not a DType").
    unsafe {
        if (*t).head.ob_type.is_null() {
            (*t).head.ob_type = PyType_Type.as_ptr();
        }
        (*t).head.ob_refcnt = IMMORTAL_REFCNT;
        (*t).tp_flags |= PY_TPFLAGS_READY;
        // RFC 0047 (wave 5): mirror CPython's `inherit_special` and stamp
        // the fast-subclass bit for the most-derived builtin in this type's
        // ancestry. A stock extension reads these directly off `tp_flags`
        // (`PyType_Check`, `PyExceptionInstance_Check`, …); without them
        // numpy.random's Cython rejects `numpy.dtype` whose metaclass
        // `_DTypeMeta` is a `type` subclass that here would carry no
        // `Py_TPFLAGS_TYPE_SUBCLASS`.
        (*t).tp_flags |= fast_subclass_flags(&readied.bridge);
        if (*t).tp_dict.is_null() {
            (*t).tp_dict = tp_dict_box;
        } else {
            crate::object::Py_DecRef(tp_dict_box);
        }
        if (*t).tp_base.is_null() {
            if let Some(bp) = base_for_c {
                (*t).tp_base = bp;
            }
        }
        if (*t).tp_bases.is_null() {
            (*t).tp_bases = tp_bases_box;
        } else if !tp_bases_box.is_null() {
            crate::object::Py_DecRef(tp_bases_box);
        }
        if (*t).tp_mro.is_null() {
            (*t).tp_mro = tp_mro_box;
        } else if !tp_mro_box.is_null() {
            crate::object::Py_DecRef(tp_mro_box);
        }
        if (*t).tp_dealloc.is_none() {
            (*t).tp_dealloc = Some(crate::object::_PyWeavePy_Dealloc);
        }
        // RFC 0046 (wave 4): CPython's `PyType_Ready` installs a default
        // `tp_free` (`PyObject_Free`) when the type provides none. An
        // extension `tp_dealloc` that ends with `Py_TYPE(self)->tp_free(self)`
        // — numpy's `boundarraymethod_dealloc` does — would otherwise call
        // through a NULL slot. Our `PyObject_Free` absorbs the free of a
        // faithful instance body (its block is owned by the native
        // instance) and `PyMem_Free`s a plain block.
        if (*t).tp_free.is_null() {
            let free_fp: unsafe extern "C" fn(*mut c_void) = crate::memory::PyObject_Free;
            (*t).tp_free = free_fp as *mut c_void;
        }
        // RFC 0046 (wave 4): likewise default `tp_alloc` to
        // `PyType_GenericAlloc`. CPython inherits it from `object`; an
        // extension that calls `subtype->tp_alloc(subtype, 0)` directly —
        // numpy's `arraydescr_new` does, to mint a fresh DType instance —
        // would otherwise jump through a NULL slot.
        if (*t).tp_alloc.is_null() {
            let alloc_fp: unsafe extern "C" fn(*mut PyTypeObject, PySsizeT) -> *mut PyObject =
                crate::genericalloc::PyType_GenericAlloc;
            (*t).tp_alloc = alloc_fp as *mut c_void;
        }
    }

    // RFC 0046 (wave 4): reflect a *foreign* metaclass onto the bridged
    // type so metatype-level descriptors resolve through the VM's
    // `load_attr_type` (CPython's `type_getattro` searches `Py_TYPE(type)`
    // first). numpy mallocs each DType class (`Float64DType`, …) with
    // `ob_type = &PyArrayDTypeMeta_Type`, whose `_DTypeMeta` exposes
    // `_legacy` / `_abstract` / the `type` property as getsets; a dtype's
    // `arraydescr_repr` / `.name` read `type(dtype)._legacy`. Without the
    // metaclass link `metaclass_or_type()` collapses to `type`, those reads
    // raise `AttributeError`, and dtype `repr`/`str`/`.name` degrade to the
    // foreign placeholder. Only adopt a genuine *foreign* metatype (never
    // our own `PyType_Type`, never the type itself), readied on demand so
    // its getsets are harvested.
    unsafe {
        let meta_ptr = (*t).head.ob_type;
        if !meta_ptr.is_null()
            && meta_ptr != t
            && meta_ptr != PyType_Type.as_ptr()
            && !is_weavepy_owned_type(meta_ptr)
        {
            if let Some(meta_t) = bridge_or_ready(meta_ptr) {
                readied.bridge.set_metaclass(meta_t);
            }
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn PyType_IsSubtype(a: *mut PyTypeObject, b: *mut PyTypeObject) -> c_int {
    if a.is_null() || b.is_null() {
        return 0;
    }
    // CPython-faithful C-level test first: `a` is a subtype of `b` if `b`
    // is reachable from `a` through the `tp_base` chain. A pure pointer
    // walk — correct no matter which interpreter minted either type, and
    // never an out-of-bounds read (every `PyTypeObject`, stock or ours,
    // carries `tp_base` at the standard offset). This is what makes the
    // process-global datetime shells (RFC 0029) answer
    // `PyDate_Check(datetime_instance)` correctly via their
    // `datetime → date → object` chain, where a bridge-identity
    // comparison would fail across the test harness's per-case
    // interpreters.
    if unsafe { c_base_chain_contains(a, b) } {
        return 1;
    }
    // Fall back to the bridged-MRO comparison for types whose faithful C
    // base chain is not populated (the common WeavePy bridged type).
    let (Some(a), Some(b)) = (unsafe { bridge_type(a) }, unsafe { bridge_type(b) }) else {
        return 0;
    };
    c_int::from(a.is_subclass_of(&b))
}

/// Walk `a`'s `tp_base` ancestry looking for `b` — the pointer-only core
/// of CPython's `PyType_IsSubtype`. Returns `false` (not "unknown") when
/// the chain runs out, so callers fall back to the bridged comparison.
///
/// # Safety
/// `a` must be null or a valid `PyTypeObject*`; the chain is acyclic and
/// terminates at a type whose `tp_base` is null (`object`).
unsafe fn c_base_chain_contains(a: *mut PyTypeObject, b: *mut PyTypeObject) -> bool {
    let mut cur = a;
    while !cur.is_null() {
        if std::ptr::eq(cur, b) {
            return true;
        }
        cur = unsafe { (*cur).tp_base };
    }
    false
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
