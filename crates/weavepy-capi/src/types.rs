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
use std::rc::Rc;
use std::sync::Mutex;

use weavepy_vm::object::Object;
use weavepy_vm::types::TypeObject;

use crate::object::{PyObject, PySsizeT, IMMORTAL_REFCNT};

/// Layout of a type object as the C side sees it.
///
/// The first field shadows [`PyObject`] exactly. Subsequent fields
/// are deliberately a *subset* of CPython's full `PyTypeObject` —
/// extensions that compile against `Py_LIMITED_API` only see the
/// header through opaque accessors, so we don't need to expose the
/// hundred-odd CPython slots verbatim. The `_bridge` slot at the end
/// stores the `Rc<TypeObject>` we use for native dispatch.
#[repr(C)]
pub struct PyTypeObject {
    pub head: PyObject,
    /// Type's qualified name (`module.Name` for heap types).
    pub tp_name: *const c_char,
    /// Reserved for future use; mirrors CPython's `tp_basicsize`.
    pub tp_basicsize: PySsizeT,
    pub tp_itemsize: PySsizeT,
    pub tp_flags: u32,
    /// Extension-supplied [`crate::ffi::PyType_Slot`] table, or
    /// null if the type wasn't built from a spec.
    pub tp_slots: *mut PyType_Slot,
    /// Bridge to the WeavePy native type. Boxed
    /// `Rc<TypeObject>` (Rc keeps refcount); the box is leaked
    /// when the type is materialised. For heap types whose lifetime
    /// is bound to an extension's scope we drop this box on
    /// `tp_free`; static types have a sentinel that's never freed.
    pub bridge: *mut Rc<TypeObject>,
    /// Static type vs. heap-allocated type marker. Set to
    /// `IMMORTAL_REFCNT` for static types so the refcount machinery
    /// is a no-op.
    _filler: [usize; 4],
}

unsafe impl Sync for PyTypeObject {}

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
#[repr(C)]
pub struct PyTypeObjectBox {
    pub head: PyTypeObject,
    pub owned_name: Vec<u8>,
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
        Self(UnsafeCell::new(PyTypeObject {
            head: PyObject {
                ob_refcnt: IMMORTAL_REFCNT,
                ob_type: ptr::null_mut(),
            },
            tp_name: ptr::null(),
            tp_basicsize: 0,
            tp_itemsize: 0,
            tp_flags: 0,
            tp_slots: ptr::null_mut(),
            bridge: ptr::null_mut(),
            _filler: [0; 4],
        }))
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
            find_type_ptr(&inst.class).unwrap_or_else(|| PyBaseObject_Type.as_ptr())
        }
        _ => PyBaseObject_Type.as_ptr(),
    }
}

/// Walk the static type registry looking for a slot whose bridge
/// matches `t`. Used by [`type_for_object`]. Linear in the number
/// of static types, which is small (~25).
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
    None
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
            tp_itemsize: 0,
            tp_flags: 0,
            tp_slots: ptr::null_mut(),
            bridge: Box::into_raw(Box::new(t.clone())),
            _filler: [0; 4],
        },
        owned_name,
    });
    let p = Box::leak(bx);
    &mut p.head as *mut PyTypeObject
}

// ----------------------------------------------------------------
// PyType_FromSpec — the heart of "extension defines a class".
// ----------------------------------------------------------------

const Py_TPFLAGS_BASETYPE: u32 = 1 << 10;
const Py_TPFLAGS_HEAPTYPE: u32 = 1 << 9;

/// Slot identifiers we recognise. Numbers come from CPython's
/// `Include/typeslots.h`. Unknown slots are accepted but have no
/// effect — extensions that depend on those slots will see
/// `PyType_GetSlot` return null, which mirrors what CPython does
/// for unsupported slots in the limited API.
mod slot_ids {
    pub const Py_tp_doc: i32 = 56;
    pub const Py_tp_methods: i32 = 65;
    pub const Py_tp_repr: i32 = 66;
    pub const Py_tp_str: i32 = 70;
    pub const Py_tp_init: i32 = 61;
    pub const Py_tp_new: i32 = 65;
    pub const Py_tp_call: i32 = 50;
    pub const Py_tp_dealloc: i32 = 52;
    pub const Py_tp_iter: i32 = 63;
    pub const Py_tp_iternext: i32 = 64;
    pub const Py_tp_richcompare: i32 = 67;
    pub const Py_tp_getattro: i32 = 59;
    pub const Py_tp_setattro: i32 = 69;
    pub const Py_tp_hash: i32 = 60;
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

    // Resolve the base type list. Default to `object` if `bases`
    // is null or empty.
    let base_types: Vec<Rc<TypeObject>> = if bases.is_null() {
        vec![weavepy_vm::builtin_types::builtin_types().object_.clone()]
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
            _ => vec![],
        }
    };
    let bases_resolved = if base_types.is_empty() {
        vec![weavepy_vm::builtin_types::builtin_types().object_.clone()]
    } else {
        base_types
    };

    // Walk the slot table, recording the methods defined on the
    // type and a pointer to the doc string if any.
    let mut methods: Vec<crate::module::MethodEntry> = Vec::new();
    let mut doc: Option<String> = None;
    let mut slot_ptr = spec_ref.slots;
    if !slot_ptr.is_null() {
        loop {
            let slot = unsafe { *slot_ptr };
            if slot.slot == 0 {
                break;
            }
            match slot.slot {
                x if x == slot_ids::Py_tp_doc => {
                    if !slot.pfunc.is_null() {
                        let s = unsafe { CStr::from_ptr(slot.pfunc as *const c_char) };
                        doc = Some(s.to_string_lossy().into_owned());
                    }
                }
                x if x == slot_ids::Py_tp_methods => {
                    if !slot.pfunc.is_null() {
                        methods.extend(unsafe {
                            crate::module::collect_methods(
                                slot.pfunc as *mut crate::module::PyMethodDef,
                            )
                        });
                    }
                }
                _ => { /* unsupported slot: silently accepted */ }
            }
            slot_ptr = unsafe { slot_ptr.add(1) };
        }
    }

    let mut dict = weavepy_vm::object::DictData::new();
    for entry in &methods {
        dict.insert(
            weavepy_vm::object::DictKey(Object::from_str(entry.name.clone())),
            entry.bind_unbound(),
        );
    }
    if let Some(d) = doc.as_ref() {
        dict.insert(
            weavepy_vm::object::DictKey(Object::from_static("__doc__")),
            Object::from_str(d.clone()),
        );
    }
    dict.insert(
        weavepy_vm::object::DictKey(Object::from_static("__module__")),
        if let Some(idx) = qualified.rfind('.') {
            Object::from_str(qualified[..idx].to_owned())
        } else {
            Object::from_static("builtins")
        },
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
            tp_flags: spec_ref.flags | Py_TPFLAGS_HEAPTYPE,
            tp_slots: spec_ref.slots,
            bridge: Box::into_raw(Box::new(ty)),
            _filler: [0; 4],
        },
        owned_name,
    });
    let leaked = Box::leak(bx);
    &mut leaked.head as *mut PyTypeObject as *mut PyObject
}

#[no_mangle]
pub unsafe extern "C" fn PyType_FromModuleAndSpec(
    _module: *mut PyObject,
    spec: *mut PyType_Spec,
    bases: *mut PyObject,
) -> *mut PyObject {
    unsafe { PyType_FromSpecWithBases(spec, bases) }
}

#[no_mangle]
pub unsafe extern "C" fn PyType_Ready(_t: *mut PyTypeObject) -> c_int {
    // Type objects in WeavePy are always "ready" the moment their
    // bridge is installed. CPython uses `PyType_Ready` to lazily
    // populate slot tables; we don't have that complication.
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
