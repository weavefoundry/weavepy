//! `PyObject_*`, `PyNumber_*`, `PySequence_*`, `PyMapping_*` —
//! the "abstract object" protocol.
//!
//! These functions translate to native operations on
//! [`weavepy_vm::object::Object`]. Calls that need an active
//! interpreter (e.g. attribute access through user-defined
//! `__getattr__`, function invocation) reach into
//! [`crate::interp`].

use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::ptr;
use weavepy_vm::sync::Rc;

use weavepy_vm::error::RuntimeError;
use weavepy_vm::object::{DictKey, Object};

use crate::object::{PyHashT, PyObject, PySsizeT};

// ---- TEMP recursion diagnostic (remove after fix) -----------------
thread_local! {
    static WP_RCMP_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}
struct WpDepthGuard;
impl WpDepthGuard {
    fn enter(where_: &str, a: *mut PyObject, b: *mut PyObject) -> Self {
        let d = WP_RCMP_DEPTH.with(|c| {
            let n = c.get() + 1;
            c.set(n);
            n
        });
        if d > 120 {
            let ta = wp_ty_name(a);
            let tb = wp_ty_name(b);
            panic!("WP recursion guard tripped at {where_} depth={d} a_type={ta} b_type={tb}");
        }
        WpDepthGuard
    }
}
impl Drop for WpDepthGuard {
    fn drop(&mut self) {
        WP_RCMP_DEPTH.with(|c| c.set(c.get().saturating_sub(1)));
    }
}
fn wp_ty_name(o: *mut PyObject) -> String {
    if o.is_null() {
        return "<null>".to_string();
    }
    let ty = unsafe { (*o).ob_type };
    if ty.is_null() {
        return "<null-type>".to_string();
    }
    let name = unsafe { (*(ty as *mut crate::layout::PyTypeObjectFull)).tp_name };
    if name.is_null() {
        return "<null-name>".to_string();
    }
    unsafe { CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned()
}
// ---- end TEMP -----------------------------------------------------

// ----------------------------------------------------------------
// PyObject_* helpers.
// ----------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PyObject_Repr(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(o) };
    // RFC 0046 (wave 4): a *foreign* object's `repr` must come from its own
    // `tp_repr` (numpy's `dtype` prints as `dtype('float64')`); the VM-side
    // `repr_for` only sees an opaque `Object::Foreign` and would emit the
    // debug `<foreign … at 0x…>` placeholder.
    if matches!(obj, Object::Foreign(_)) {
        let r = unsafe { foreign_repr_or_str(o, true) };
        if !r.is_null() {
            return r;
        }
        // NULL with a pending exception: the slot *raised* — propagate,
        // like CPython. NULL without one: no usable slot — placeholder.
        if crate::errors::pending().is_some() {
            return ptr::null_mut();
        }
    }
    // A VM object with a Python-level `__repr__` (a user/extension class
    // instance, or a class with a metaclass `__repr__`) must dispatch that
    // dunder — the same way the `repr()` builtin does — so C code calling
    // `PyObject_Repr` agrees with the bytecode path. `repr_for` only knows
    // the built-in shapes and would emit a `<Foo object>` placeholder for
    // everything else (this is how Cython's `repr(...)` on a pure-Python
    // instance used to lose its real value). Containers dispatch too: their
    // *elements* need the same dunder treatment (pandas' ujson stringifies a
    // MultiIndex key tuple holding a `Timestamp`, whose Cython `__repr__`
    // the native renderer cannot run).
    if matches!(
        obj,
        Object::Instance(_) | Object::Type(_) | Object::Tuple(_) | Object::List(_)
    ) {
        match crate::interp::ensure_active(|| {
            crate::interp::with_interp_mut(|interp| interp.repr_object(&obj))
        }) {
            Some(Ok(s)) => return crate::object::into_owned(Object::from_str(s)),
            Some(Err(e)) => {
                if std::env::var_os("WEAVEPY_TRACE_REPRERR").is_some() {
                    if let Object::Instance(i) = &obj {
                        let r = i.cls().lookup("__repr__");
                        eprintln!(
                            "[REPRERR] instance of {} repr_lookup={:?} err={e:?}",
                            i.cls().name,
                            r.as_ref().map(|x| x.type_name_owned()),
                        );
                    } else {
                        eprintln!("[REPRERR] obj={} err={e:?}", obj.type_name());
                    }
                }
                crate::errors::set_pending_from_runtime(e);
                return ptr::null_mut();
            }
            None => {}
        }
    }
    let s = repr_for(&obj);
    crate::object::into_owned(Object::from_str(s))
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Str(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(o) };
    // RFC 0046 (wave 4): a foreign object's `str` comes from its `tp_str`
    // (falling back to `tp_repr`, exactly as CPython's `PyObject_Str`).
    if matches!(obj, Object::Foreign(_)) {
        let r = unsafe { foreign_repr_or_str(o, false) };
        if !r.is_null() {
            return r;
        }
        // Propagate a raised `tp_str`/`tp_repr` (see `PyObject_Repr`).
        if crate::errors::pending().is_some() {
            return ptr::null_mut();
        }
    }
    // Dispatch a Python-level `__str__` (defined *or inherited*) for VM
    // instances and metaclass-`__str__` classes, matching the `str()`
    // builtin. Without this, Cython code doing `str(obj)` on a pure-Python
    // instance — e.g. `pytz.tzinfo.BaseTzInfo.__str__` returning the zone
    // name inside pandas' `tz_standardize` — got the `<Foo object>`
    // placeholder from `str_for`, corrupting the value. Containers route
    // through the interpreter too so their *elements'* dunders run (pandas'
    // ujson `str(...)`s MultiIndex key tuples holding `Timestamp`s).
    if matches!(
        obj,
        Object::Instance(_) | Object::Type(_) | Object::Tuple(_) | Object::List(_)
    ) {
        match crate::interp::ensure_active(|| {
            crate::interp::with_interp_mut(|interp| interp.str_object(&obj))
        }) {
            Some(Ok(s)) => return crate::object::into_owned(Object::from_str(s)),
            Some(Err(e)) => {
                crate::errors::set_pending_from_runtime(e);
                return ptr::null_mut();
            }
            None => {}
        }
    }
    let s = str_for(&obj);
    crate::object::into_owned(Object::from_str(s))
}

/// CPython-faithful `repr`/`str` for a *foreign* extension object
/// (RFC 0046, wave 4): call `tp_repr` (when `want_repr`) or `tp_str`,
/// `tp_str` falling back to `tp_repr` as CPython does. Returns a new
/// reference, or null when no slot is defined (caller uses the VM
/// placeholder).
///
/// # Safety
/// `o` must be a live, non-null `PyObject*` whose `ob_type` is readable.
unsafe fn foreign_repr_or_str(o: *mut PyObject, want_repr: bool) -> *mut PyObject {
    let ty = unsafe { (*o).ob_type } as *mut crate::layout::PyTypeObjectFull;
    if ty.is_null() {
        return ptr::null_mut();
    }
    // CPython bakes inherited slots into each subtype during `PyType_Ready`
    // (`inherit_slots`). WeavePy's `PyType_Ready` does not, so a stock
    // subclass such as numpy's `Float64DType` carries a NULL `tp_repr` even
    // though its base `np.dtype` defines `arraydescr_repr`. Walk the
    // `tp_base` chain to recover the inherited slot, mirroring the effect of
    // `inherit_slots` for the repr/str path.
    let slot = unsafe { inherited_repr_str_slot(ty, want_repr) };
    if slot.is_null() {
        return ptr::null_mut();
    }
    // The resolved slot may be one of WeavePy's own `synth_tp_repr`/
    // `synth_tp_str` bridges (installed on static types like `PyType_Type`
    // whose CPython slot we don't reimplement). Those bridges forward to
    // `PyObject_Repr`/`PyObject_Str`, which for a *foreign* object comes
    // right back here — an unbounded loop (`repr` of a numpy heap type
    // walked `tp_base` up to `type`, found the bridge, and overflowed the
    // stack). Treat a bridge hit as "no foreign slot" so the caller falls
    // back to the VM placeholder.
    if crate::types::is_synth_repr_str_slot(slot) {
        return ptr::null_mut();
    }
    let f: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject =
        unsafe { std::mem::transmute(slot) };
    // A raised slot error stays pending for the caller to propagate —
    // CPython's `PyObject_Str`/`PyObject_Repr` return NULL when
    // `tp_str`/`tp_repr` raise. numpy's generic-unit `datetime64` scalar
    // *must* raise ValueError from `str()` (RFC 0075 WS8,
    // test_casting_unittests.test_numeric_to_times); the old swallow-and-
    // placeholder path turned that contract into `<foreign …>` text.
    unsafe { f(o) }
}

/// Resolve the effective `tp_repr` (when `want_repr`) or `tp_str` for `ty`,
/// walking the `tp_base` chain when the slot is NULL on the subtype. `str`
/// with no `tp_str` anywhere in the chain falls back to `tp_repr`, exactly
/// as CPython's `PyObject_Str`.
///
/// # Safety
/// `ty` must be a live, non-null `PyTypeObjectFull*` with a readable
/// (possibly NULL-terminated) `tp_base` chain.
unsafe fn inherited_repr_str_slot(
    ty: *mut crate::layout::PyTypeObjectFull,
    want_repr: bool,
) -> *mut std::os::raw::c_void {
    unsafe fn walk(
        mut ty: *mut crate::layout::PyTypeObjectFull,
        repr: bool,
    ) -> *mut std::os::raw::c_void {
        // Bound the walk defensively against a cyclic/corrupt base chain.
        for _ in 0..256 {
            if ty.is_null() {
                break;
            }
            let s = if repr {
                unsafe { (*ty).tp_repr }
            } else {
                unsafe { (*ty).tp_str }
            };
            if !s.is_null() {
                return s;
            }
            ty = unsafe { (*ty).tp_base };
        }
        ptr::null_mut()
    }
    let primary = unsafe { walk(ty, want_repr) };
    if !primary.is_null() {
        return primary;
    }
    if !want_repr {
        return unsafe { walk(ty, true) };
    }
    ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_ASCII(o: *mut PyObject) -> *mut PyObject {
    // CPython's `ascii()`: repr, then escape every non-ASCII code point
    // (`\xhh` / `\uxxxx` / `\Uxxxxxxxx`) — PyUnicode_FromFormat's %A
    // relies on the escaping (test_capi.test_unicode.test_from_format).
    let r = unsafe { PyObject_Repr(o) };
    if r.is_null() {
        return r;
    }
    let cs = unsafe { crate::strings::PyUnicode_AsUTF8(r) };
    if cs.is_null() {
        // Not a UTF-8-representable repr (lone surrogates): keep the
        // repr as-is rather than failing outright.
        crate::errors::clear_thread_local();
        return r;
    }
    let s = unsafe { core::ffi::CStr::from_ptr(cs) }.to_string_lossy();
    if s.is_ascii() {
        return r;
    }
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        let cp = ch as u32;
        if cp < 0x80 {
            out.push(ch);
        } else if cp <= 0xFF {
            let _ = write!(out, "\\x{cp:02x}");
        } else if cp <= 0xFFFF {
            let _ = write!(out, "\\u{cp:04x}");
        } else {
            let _ = write!(out, "\\U{cp:08x}");
        }
    }
    unsafe { crate::object::Py_DecRef(r) };
    let c = match std::ffi::CString::new(out) {
        Ok(c) => c,
        Err(_) => return ptr::null_mut(),
    };
    unsafe { crate::strings::PyUnicode_FromString(c.as_ptr()) }
}

fn repr_for(o: &Object) -> String {
    use Object as O;
    match o {
        O::None => "None".to_owned(),
        O::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
        O::Int(i) => i.to_string(),
        O::Long(big) => big.to_string(),
        O::Float(f) => crate::numbers_format::format_float(*f),
        O::Str(s) => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'")),
        O::Bytes(b) => format!("b'{}'", String::from_utf8_lossy(b)),
        O::Tuple(items) => {
            let inner: Vec<String> = items.iter().map(repr_for).collect();
            if items.len() == 1 {
                format!("({},)", inner[0])
            } else {
                format!("({})", inner.join(", "))
            }
        }
        O::List(rc) => {
            let inner: Vec<String> = rc.borrow().iter().map(repr_for).collect();
            format!("[{}]", inner.join(", "))
        }
        O::Dict(rc) => {
            let inner: Vec<String> = rc
                .borrow()
                .iter()
                .map(|(k, v)| format!("{}: {}", repr_for(&k.0), repr_for(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        O::Type(t) => format!("<class '{}'>", t.name),
        O::Module(m) => format!("<module '{}'>", m.name),
        _ => format!("{o:?}"),
    }
}

fn str_for(o: &Object) -> String {
    if let Object::Str(s) = o {
        return s.to_string();
    }
    repr_for(o)
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_GetAttr(o: *mut PyObject, attr: *mut PyObject) -> *mut PyObject {
    if o.is_null() || attr.is_null() {
        return ptr::null_mut();
    }
    // RFC 0069 WS5 — attribute dispatch can recurse through extension
    // `tp_getattro` chains that never charge `Py_EnterRecursiveCall`.
    if c_stack_exhausted(" while getting an attribute") {
        return ptr::null_mut();
    }
    let key = match unsafe { crate::object::clone_object(attr) } {
        Object::Str(s) => s.to_string(),
        _ => {
            crate::errors::set_type_error("attribute name must be string");
            return ptr::null_mut();
        }
    };
    do_getattr(o, &key)
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_GetAttrString(
    o: *mut PyObject,
    attr: *const c_char,
) -> *mut PyObject {
    if o.is_null() || attr.is_null() {
        return ptr::null_mut();
    }
    let key = unsafe { CStr::from_ptr(attr) }
        .to_string_lossy()
        .into_owned();
    do_getattr(o, &key)
}

fn trace_resolved(key: &str, v: &Object) {
    if std::env::var_os("WEAVEPY_TRACE_GETATTR").is_none() {
        return;
    }
    let detail = match v {
        Object::Type(t) => {
            let p = crate::types::type_ptr_for_class(t);
            format!("Type(name={:?}, ptr={:?})", t.name, p)
        }
        Object::Foreign(s) => format!("Foreign(ptr={:?})", s.ptr),
        other => type_name(other).to_owned(),
    };
    eprintln!("[GETATTR] key={key:?} resolved -> {detail}");
}

fn do_getattr(o: *mut PyObject, key: &str) -> *mut PyObject {
    let obj = unsafe { crate::object::clone_object(o) };
    if std::env::var_os("WEAVEPY_TRACE_GETATTR").is_some() {
        let extra = match &obj {
            Object::Type(t) => {
                let has = t.lookup(key).is_some();
                format!(" [Type name={:?} lookup_has={}]", t.name, has)
            }
            _ => String::new(),
        };
        eprintln!(
            "[GETATTR] key={key:?} on {}{} -> resolving",
            type_name(&obj),
            extra
        );
    }
    // RFC 0046 (wave 4): a foreign extension object resolves attributes
    // through its own slots, never through the VM's `Foreign` arm (which
    // would loop back here via the foreign getattr hook). See
    // [`foreign_getattr_dispatch`].
    if matches!(obj, Object::Foreign(_)) {
        return foreign_getattr_dispatch(o, &obj, key);
    }
    // Fast path: the handful of container/instance shapes `attr_lookup`
    // resolves without re-entering the interpreter.
    if let Some(v) = attr_lookup(&obj, key) {
        trace_resolved(key, &v);
        return crate::object::into_owned(v);
    }
    // RFC 0046 (wave 4): everything else — functions, builtins, generators,
    // foreign extension objects, and every genuine miss — resolves through
    // the VM's full `LOAD_ATTR` machinery, so the C-API agrees with the
    // bytecode path on both the value and (on failure) the *exact*
    // exception. numpy reads `dispatcher.__qualname__` / `__name__` on a
    // plain `function` through here while wrapping `__array_function__`
    // implementations; the legacy `_ => None` arm wrongly reported
    // "'function' object has no attribute '__qualname__'".
    match crate::interp::ensure_active(|| {
        crate::interp::with_interp_mut(|interp| interp.load_attr_public(&obj, key))
    }) {
        Some(Ok(v)) => {
            trace_resolved(key, &v);
            crate::object::into_owned(v)
        }
        Some(Err(e)) => {
            crate::errors::set_pending_from_runtime(e);
            ptr::null_mut()
        }
        None => {
            crate::errors::set_pending(
                Some(
                    weavepy_vm::builtin_types::builtin_types()
                        .attribute_error
                        .clone(),
                ),
                Object::from_str(format!(
                    "'{}' object has no attribute '{}'",
                    type_name(&obj),
                    key
                )),
            );
            ptr::null_mut()
        }
    }
}

/// Resolve `name` on a *foreign* extension object (RFC 0046, wave 4),
/// mirroring CPython's `PyObject_GetAttr` dispatch:
///
/// 1. A **custom** `tp_getattro` (one the extension installed itself, e.g.
///    `ndarray`'s) is the object's own resolution — call it directly.
/// 2. Otherwise (the slot is null or our generic `PyObject_GenericGetAttr`)
///    resolve through the bridged type's harvested descriptors via the VM
///    ([`Interpreter::resolve_foreign_via_type`]). This invokes getset
///    getters / binds methods with the foreign object as `self`, and never
///    re-enters the foreign getattr hook — so there is no recursion.
fn foreign_getattr_dispatch(o: *mut PyObject, obj: &Object, key: &str) -> *mut PyObject {
    let tp = unsafe { (*o).ob_type };
    if !tp.is_null() {
        let getattro = unsafe { (*tp).tp_getattro };
        let generic = crate::genericalloc::PyObject_GenericGetAttr
            as unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject
            as usize;
        if !getattro.is_null() && getattro as usize != generic {
            let name_obj = crate::object::into_owned(Object::from_str(key));
            let f: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject =
                unsafe { std::mem::transmute(getattro) };
            let r = unsafe { f(o, name_obj) };
            unsafe { crate::object::Py_DecRef(name_obj) };
            return r;
        }
    }
    match crate::interp::ensure_active(|| {
        crate::interp::with_interp_mut(|interp| interp.resolve_foreign_via_type(obj, key))
    }) {
        Some(Some(Ok(v))) => crate::object::into_owned(v),
        Some(Some(Err(e))) => {
            crate::errors::set_pending_from_runtime(e);
            ptr::null_mut()
        }
        _ => {
            crate::errors::set_pending(
                Some(
                    weavepy_vm::builtin_types::builtin_types()
                        .attribute_error
                        .clone(),
                ),
                Object::from_str(format!(
                    "'{}' object has no attribute '{}'",
                    type_name(obj),
                    key
                )),
            );
            ptr::null_mut()
        }
    }
}

/// Apply the descriptor protocol for an attribute `raw` resolved in the
/// MRO of type `t`, as `type.__getattribute__` does (`__get__(None, t)`):
///
/// * a `classmethod` binds to the class itself (`BoundMethod(t, func)`),
/// * a `staticmethod` unwraps to its plain function,
/// * everything else (plain functions, properties, data) is returned as-is
///   — on a *type* receiver a bare function is the unbound function and a
///   property descriptor returns itself, matching CPython.
fn bind_type_attr(t: &weavepy_vm::sync::Rc<weavepy_vm::types::TypeObject>, raw: Object) -> Object {
    match raw {
        Object::ClassMethod(inner) => Object::BoundMethod(weavepy_vm::sync::Rc::new(
            weavepy_vm::object::BoundMethod::new(Object::Type(t.clone()), inner.func()),
        )),
        Object::StaticMethod(inner) => inner.func(),
        other => other,
    }
}

fn attr_lookup(o: &Object, key: &str) -> Option<Object> {
    match o {
        Object::Module(m) => {
            let kk = DictKey(Object::from_str(key));
            m.dict.borrow().get(&kk).cloned()
        }
        Object::Dict(rc) => {
            let kk = DictKey(Object::from_str(key));
            rc.borrow().get(&kk).cloned()
        }
        Object::Type(t) => {
            // Mirror `type.__getattribute__`: a class/static method found
            // in the type's MRO is bound via its descriptor `__get__(None,
            // t)` before being returned. Without this, the C-API getattr
            // hands back the raw `classmethod`/`staticmethod` wrapper (not
            // callable the way CPython's bound result is), which breaks
            // Cython's class-creation path — e.g. `EnumType.__prepare__`
            // fetched while building a `class X(Enum)` inside a `.pyx`.
            let raw = t.lookup(key)?;
            // A member/getset found in the class's own MRO serves
            // *instances*; on the class itself the metatype's data
            // descriptor wins (`C.__dict__` is `type.__dict__['__dict__']
            // .__get__(C)` → a mappingproxy, not the raw
            // member_descriptor). Defer to the VM's full `LOAD_ATTR`
            // (RFC 0066 WS6: zope.interface's C `implementedBy` does
            // `PyObject_GetItem(PyObject_GetAttr(cls, "__dict__"), …)`
            // and subscripted the raw descriptor).
            if matches!(raw, Object::SlotDescriptor(_)) {
                return None;
            }
            Some(bind_type_attr(t, raw))
        }
        Object::Instance(inst) => {
            // A *bound* super proxy (`super(C, obj)`) has a custom
            // `tp_getattro` in CPython (`super_getattro`): attribute access
            // walks `__self_class__`'s MRO *after* `__thisclass__`, never the
            // proxy's own (`super`) class. This fast path resolves against
            // `inst.cls()` — for a super proxy that is the `super` type, whose
            // own builtin `__init__` (`super_init_impl`) rejects keyword
            // arguments. Real Cython hits this: `pandas.TimeRE.__init__` calls
            // `super().__init__(locale_time=...)` through `PyObject_GetAttr`,
            // which landed here and wrongly bound `super.__init__` instead of
            // `_strptime.TimeRE.__init__`. Defer to the VM's full `LOAD_ATTR`
            // (return `None` -> `load_attr_public` in [`do_getattr`]), which
            // performs the proper super MRO walk.
            {
                let d = inst.dict.borrow();
                let is_super_proxy = matches!(
                    d.get(&DictKey(Object::from_static("__self_class__"))),
                    Some(Object::Type(_))
                ) && matches!(
                    d.get(&DictKey(Object::from_static("__thisclass__"))),
                    Some(Object::Type(_))
                ) && !matches!(
                    d.get(&DictKey(Object::from_static("__self__"))),
                    Some(Object::None) | None
                );
                if is_super_proxy {
                    return None;
                }
            }
            let kk = DictKey(Object::from_str(key));
            if let Some(v) = inst.dict.borrow().get(&kk).cloned() {
                return Some(v);
            }
            // Walk the MRO and invoke descriptor protocol if the
            // resolved attribute is a property, classmethod, or
            // staticmethod. Mirror the VM's `LOAD_ATTR` dispatcher.
            let raw = inst.cls().lookup(key)?;
            match &raw {
                Object::Property(p) => {
                    let getter = p.fget();
                    if matches!(getter, Object::None) {
                        return Some(raw);
                    }
                    crate::interp::ensure_active(|| {
                        crate::interp::with_interp_mut(|interp| {
                            interp
                                .call_object(getter, std::slice::from_ref(o), &[])
                                .ok()
                        })
                    })
                    .flatten()
                }
                Object::StaticMethod(inner) => Some(inner.func()),
                Object::ClassMethod(inner) => {
                    let class = Object::Type(inst.cls());
                    Some(Object::BoundMethod(weavepy_vm::sync::Rc::new(
                        weavepy_vm::object::BoundMethod::new(class, inner.func()),
                    )))
                }
                Object::Function(_) | Object::Builtin(_) => {
                    Some(Object::BoundMethod(weavepy_vm::sync::Rc::new(
                        weavepy_vm::object::BoundMethod::new(o.clone(), raw.clone()),
                    )))
                }
                // A member/getset (`Object::SlotDescriptor`) or a custom
                // `__get__` data descriptor must run its descriptor protocol
                // — a `__slots__` member in particular stores its value in
                // the instance's *slot storage*, not `inst.dict`, so it is
                // not resolvable here. Defer to the VM's full `LOAD_ATTR`
                // (returning `None` falls through to `load_attr_public` in
                // [`do_getattr`]). The previous `_ => Some(raw)` arm returned
                // the raw `member_descriptor`, which broke real Cython's
                // PEP 489 create slot (`spec.name` -> `PyModule_NewObject`
                // got the descriptor, not the name).
                Object::SlotDescriptor(_) => None,
                Object::Instance(ci) if ci.cls().lookup("__get__").is_some() => None,
                _ => Some(raw),
            }
        }
        _ => None,
    }
}

fn type_name(o: &Object) -> &'static str {
    use Object as O;
    match o {
        O::None => "NoneType",
        O::Bool(_) => "bool",
        O::Int(_) | O::Long(_) => "int",
        O::Float(_) => "float",
        O::Complex(_) => "complex",
        O::Str(_) => "str",
        O::Bytes(_) => "bytes",
        O::ByteArray(_) => "bytearray",
        O::Tuple(_) => "tuple",
        O::List(_) => "list",
        O::Dict(_) => "dict",
        O::Set(_) => "set",
        O::FrozenSet(_) => "frozenset",
        O::Range(_) => "range",
        O::Module(_) => "module",
        O::Type(_) => "type",
        O::Function(_) | O::Builtin(_) => "function",
        O::BoundMethod(_) => "method",
        O::Generator(_) => "generator",
        O::Coroutine(_) => "coroutine",
        O::Slice(_) => "slice",
        _ => "object",
    }
}

/// Best-effort human-readable name for a callable, for tracing only.
fn callable_label(o: &Object) -> String {
    use Object as O;
    match o {
        O::Function(f) => f.code().qualname.clone(),
        O::Builtin(b) => b.name.to_string(),
        O::Type(t) => format!("type:{}", t.name),
        O::BoundMethod(bm) => format!("bound:{}", callable_label(&bm.function)),
        O::Instance(i) => format!("inst:{}", i.cls().name),
        other => type_name(other).to_string(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_SetAttr(
    o: *mut PyObject,
    attr: *mut PyObject,
    value: *mut PyObject,
) -> c_int {
    if o.is_null() || attr.is_null() {
        return -1;
    }
    let key = match unsafe { crate::object::clone_object(attr) } {
        Object::Str(s) => s.to_string(),
        _ => return -1,
    };
    do_setattr(o, &key, value)
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_SetAttrString(
    o: *mut PyObject,
    attr: *const c_char,
    value: *mut PyObject,
) -> c_int {
    if o.is_null() || attr.is_null() {
        return -1;
    }
    let key = unsafe { CStr::from_ptr(attr) }
        .to_string_lossy()
        .into_owned();
    do_setattr(o, &key, value)
}

fn do_setattr(o: *mut PyObject, key: &str, value: *mut PyObject) -> c_int {
    let obj = unsafe { crate::object::clone_object(o) };
    // RFC 0029 (wave 5): route through the VM's full `STORE_ATTR`/`DELETE_ATTR`
    // dispatch — the same logic bytecode runs — so a metaclass `__setattr__`,
    // a data descriptor (`property` setter), and most importantly *class*
    // attribute assignment land correctly. pandas' `timestamps.pyx` does
    // `Timestamp.min = Timestamp(...)` / `Timestamp.resolution = Timedelta(...)`
    // at init via `PyObject_SetAttr` on the *type*; the dict fast-paths below
    // only know modules/dicts/instances and rejected a type with "object has
    // no settable attributes". The native fallback still applies when no
    // interpreter is active (pure C-side construction before any VM frame).
    if let Some(res) = crate::interp::ensure_active(|| {
        crate::interp::with_interp_mut(|interp| {
            if value.is_null() {
                interp.delete_attr_public(&obj, key)
            } else {
                let v = unsafe { crate::object::clone_object(value) };
                interp.store_attr_public(&obj, key, v)
            }
        })
    }) {
        return match res {
            Ok(()) => 0,
            Err(e) => {
                crate::errors::set_pending_from_runtime(e);
                -1
            }
        };
    }
    match obj {
        Object::Module(m) => {
            let v = if value.is_null() {
                m.dict
                    .borrow_mut()
                    .shift_remove(&DictKey(Object::from_str(key)));
                return 0;
            } else {
                unsafe { crate::object::clone_object(value) }
            };
            m.dict
                .borrow_mut()
                .insert(DictKey(Object::from_str(key)), v);
            0
        }
        Object::Dict(rc) => {
            if value.is_null() {
                rc.borrow_mut()
                    .shift_remove(&DictKey(Object::from_str(key)));
            } else {
                let v = unsafe { crate::object::clone_object(value) };
                rc.borrow_mut().insert(DictKey(Object::from_str(key)), v);
            }
            0
        }
        Object::Instance(inst) => {
            if value.is_null() {
                inst.dict
                    .borrow_mut()
                    .shift_remove(&DictKey(Object::from_str(key)));
            } else {
                let v = unsafe { crate::object::clone_object(value) };
                inst.dict
                    .borrow_mut()
                    .insert(DictKey(Object::from_str(key)), v);
            }
            0
        }
        _ => {
            crate::errors::set_type_error("object has no settable attributes");
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_HasAttr(o: *mut PyObject, attr: *mut PyObject) -> c_int {
    let p = unsafe { PyObject_GetAttr(o, attr) };
    if p.is_null() {
        crate::errors::clear_thread_local();
        0
    } else {
        unsafe { crate::object::Py_DecRef(p) };
        1
    }
}

/// `PyObject_HasAttrWithError(o, attr)` (CPython 3.13) — like
/// [`PyObject_HasAttr`] but *propagates* a non-`AttributeError` failure
/// rather than swallowing it: 1 = present, 0 = absent (the `AttributeError`
/// is cleared), -1 = a different error remains set. Cython's import lookup
/// (`__Pyx__Import_Lookup`) uses this to probe an already-imported module
/// for the names in a `from … import …`.
#[no_mangle]
pub unsafe extern "C" fn PyObject_HasAttrWithError(o: *mut PyObject, attr: *mut PyObject) -> c_int {
    let p = unsafe { PyObject_GetAttr(o, attr) };
    if !p.is_null() {
        unsafe { crate::object::Py_DecRef(p) };
        return 1;
    }
    if unsafe { crate::errors::PyErr_Occurred() }.is_null() {
        return 0;
    }
    let attr_err = unsafe { crate::errors::PyExc_AttributeError };
    if attr_err.is_null() || unsafe { crate::errors::PyErr_ExceptionMatches(attr_err) } != 0 {
        crate::errors::clear_thread_local();
        return 0;
    }
    -1
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_HasAttrString(o: *mut PyObject, attr: *const c_char) -> c_int {
    let p = unsafe { PyObject_GetAttrString(o, attr) };
    if p.is_null() {
        crate::errors::clear_thread_local();
        0
    } else {
        unsafe { crate::object::Py_DecRef(p) };
        1
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_DelAttrString(o: *mut PyObject, attr: *const c_char) -> c_int {
    unsafe { PyObject_SetAttrString(o, attr, ptr::null_mut()) }
}

/// After a C-level call returns, refresh the macro-visible size field of
/// any faithful `set`/`dict` mirror passed as a positional argument.
///
/// RFC 0047 (wave 5): a mutating method reached through its *unbound* type
/// method — Cython's `__Pyx_CallUnboundCMethod` path, e.g.
/// `set.difference_update(s, other)` — hands the container in as `args[0]`
/// and mutates the prefix's native store in place. The inlined
/// `PySet_GET_SIZE` / `PyDict_GET_SIZE` Cython emits next reads the body
/// field directly (there is no C-API call to hook), so the count has to be
/// re-published here. Cheap for non-container args: [`sync_container_size`]
/// gates on the mirror magic before any type comparison.
///
/// # Safety
/// `args` may be null; if non-null it must have a readable `ob_type`.
unsafe fn sync_arg_container_sizes(args: *mut PyObject) {
    if args.is_null() {
        return;
    }
    let trace = std::env::var_os("WEAVEPY_TRACE_SETSEED").is_some();
    match unsafe { crate::object::clone_object(args) } {
        Object::Tuple(items) => {
            if trace {
                eprintln!("[SYNC_ARGS] tuple len={}", items.len());
            }
            for i in 0..items.len() {
                let e = unsafe { crate::containers::PyTuple_GetItem(args, i as PySsizeT) };
                if trace {
                    eprintln!(
                        "[SYNC_ARGS]   arg[{}]={:p} mirror={} set={}",
                        i,
                        e,
                        unsafe { crate::mirror::is_mirror(e) },
                        unsafe { crate::mirror::is_faithful_set(e) },
                    );
                }
                unsafe { crate::mirror::sync_container_size(e) };
            }
        }
        Object::List(rc) => {
            let n = rc.borrow().len();
            for i in 0..n {
                let e = unsafe { crate::containers::PyList_GetItem(args, i as PySsizeT) };
                unsafe { crate::mirror::sync_container_size(e) };
            }
        }
        other => {
            if trace {
                eprintln!("[SYNC_ARGS] non-seq args type={}", other.type_name());
            }
        }
    }
}

/// Dispatch a *foreign* callable — an object the extension minted itself,
/// whose callable protocol lives in its own `tp_call` slot — exactly like
/// CPython's `_PyObject_MakeTpCall`: straight through the slot, no VM
/// round-trip. pybind11's `instancemethod` wrapper (around every
/// registered method) and its capsule-bearing `cpp_function`s chain
/// `tp_call` → `PyObject_Call` → `tp_call` entirely at this level;
/// cloning them into the VM yields an opaque proxy the VM cannot call
/// (RFC 0066 WS6 — matplotlib `_image`'s classic `py::enum_`, whose
/// `__int__`/`__eq__`/`__repr__` all resolve to such wrappers while
/// `m.def` renders a default argument).
///
/// Returns `None` when `callable` is VM-owned, has no `tp_call`, or the
/// slot is the VM-forwarding bridge (`synth_tp_call` would re-enter the
/// caller). `args` may be NULL (a fresh empty tuple is minted); `kwargs`
/// may be NULL.
unsafe fn foreign_tp_call(
    callable: *mut PyObject,
    args: *mut PyObject,
    kwargs: *mut PyObject,
) -> Option<*mut PyObject> {
    if crate::object::is_weavepy_owned(callable) {
        return None;
    }
    let tp = unsafe { (*callable).ob_type } as *mut crate::types::PyTypeObject;
    if tp.is_null() {
        return None;
    }
    let tp_call = unsafe { (*tp).tp_call };
    if tp_call.is_null() || crate::types::is_synth_call_slot(tp_call) {
        return None;
    }
    let f: unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject =
        unsafe { std::mem::transmute(tp_call) };
    // `tp_call` requires a real (possibly empty) args tuple.
    let (args_owned, args_ptr) = if args.is_null() {
        (
            true,
            crate::object::into_owned(Object::new_tuple(Vec::new())),
        )
    } else {
        (false, args)
    };
    let r = unsafe { f(callable, args_ptr, kwargs) };
    if args_owned {
        unsafe { crate::object::Py_DecRef(args_ptr) };
    }
    Some(r)
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Call(
    callable: *mut PyObject,
    args: *mut PyObject,
    kwargs: *mut PyObject,
) -> *mut PyObject {
    if callable.is_null() {
        crate::errors::set_type_error("PyObject_Call: callable is NULL");
        return ptr::null_mut();
    }
    if let Some(r) = unsafe { foreign_tp_call(callable, args, kwargs) } {
        unsafe { sync_arg_container_sizes(args) };
        return r;
    }
    let target = unsafe { crate::object::clone_object(callable) };
    let arg_vec = if args.is_null() {
        Vec::new()
    } else {
        match unsafe { crate::object::clone_object(args) } {
            Object::Tuple(items) => items.iter().cloned().collect(),
            Object::List(rc) => rc.borrow().clone(),
            other => vec![other],
        }
    };
    let kwarg_pairs = if kwargs.is_null() {
        Vec::new()
    } else {
        match unsafe { crate::object::clone_object(kwargs) } {
            Object::Dict(rc) => rc
                .borrow()
                .iter()
                .map(|(k, v)| (key_string(&k.0), v.clone()))
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        }
    };

    if std::env::var_os("WEAVEPY_TRACE_CALL").is_some() {
        let keys: Vec<&str> = kwarg_pairs.iter().map(|(k, _)| k.as_str()).collect();
        eprintln!(
            "[TRACE_CALL] target={} name={} nargs={} kwargs={:?} (kwptr_null={})",
            type_name(&target),
            callable_label(&target),
            arg_vec.len(),
            keys,
            kwargs.is_null()
        );
    }

    let result = invoke_callable(target, arg_vec, kwarg_pairs);
    unsafe { sync_arg_container_sizes(args) };
    result
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_CallObject(
    callable: *mut PyObject,
    args: *mut PyObject,
) -> *mut PyObject {
    unsafe { PyObject_Call(callable, args, ptr::null_mut()) }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_CallNoArgs(callable: *mut PyObject) -> *mut PyObject {
    unsafe { PyObject_Call(callable, ptr::null_mut(), ptr::null_mut()) }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_CallOneArg(
    callable: *mut PyObject,
    arg: *mut PyObject,
) -> *mut PyObject {
    if callable.is_null() {
        return ptr::null_mut();
    }
    if !crate::object::is_weavepy_owned(callable) {
        let args = unsafe { crate::containers::PyTuple_New(1) };
        unsafe {
            crate::object::Py_IncRef(arg);
            crate::containers::PyTuple_SetItem(args, 0, arg);
        }
        let r = unsafe { foreign_tp_call(callable, args, ptr::null_mut()) };
        unsafe { crate::object::Py_DecRef(args) };
        if let Some(r) = r {
            unsafe { crate::mirror::sync_container_size(arg) };
            return r;
        }
    }
    let target = unsafe { crate::object::clone_object(callable) };
    let arg_obj = if arg.is_null() {
        Object::None
    } else {
        unsafe { crate::object::clone_object(arg) }
    };
    let result = invoke_callable(target, vec![arg_obj], Vec::new());
    unsafe { crate::mirror::sync_container_size(arg) };
    result
}

/// `PyObject_CallTwoArgs(callable, a, b)` — convenience for the
/// common two-positional-arg call. CPython 3.11+ exposes this.
#[no_mangle]
pub unsafe extern "C" fn PyObject_CallTwoArgs(
    callable: *mut PyObject,
    a: *mut PyObject,
    b: *mut PyObject,
) -> *mut PyObject {
    if callable.is_null() {
        return ptr::null_mut();
    }
    let target = unsafe { crate::object::clone_object(callable) };
    let arg_a = if a.is_null() {
        Object::None
    } else {
        unsafe { crate::object::clone_object(a) }
    };
    let arg_b = if b.is_null() {
        Object::None
    } else {
        unsafe { crate::object::clone_object(b) }
    };
    invoke_callable(target, vec![arg_a, arg_b], Vec::new())
}

fn key_string(o: &Object) -> String {
    match o {
        Object::Str(s) => s.to_string(),
        _ => format!("{o:?}"),
    }
}

fn invoke_callable(
    target: Object,
    args: Vec<Object>,
    kwargs: Vec<(String, Object)>,
) -> *mut PyObject {
    let result: Result<Object, RuntimeError> = match target {
        // A WeavePy builtin (incl. a foreign C function bridged through
        // `PyModule_Create`/`PyCFunction_NewEx`) carries a separate
        // keyword-aware entry point. The C-API call surface
        // (`PyObject_Call`/`PyObject_Vectorcall`) MUST route through it
        // when keywords are present — Cython emits `np.array(x, dtype=…)`
        // / `np.zeros(n, dtype=…)` as vectorcall sites, and dropping the
        // keywords here silently defaulted every dtype to float64.
        //
        // Prefer the *interpreter's* builtin dispatch when one is active:
        // `Interpreter::call` carries the protocol fast paths (`len` →
        // `do_len_call`, `str` → `do_str_call`, `abs`, `iter`, …) that
        // resolve dunders synthesised from bridged C slots. The raw
        // `bf.call` closure cannot — pandas' `roll_apply(…, len, raw=True)`
        // hands the *builtin* `len` an ndarray-slice instance whose length
        // lives in numpy's C `sq_length`, and `Object::len` alone has no
        // way to reach it.
        Object::Builtin(bf) => {
            let routed = crate::interp::with_interp_mut(|interp| {
                interp.call_object(Object::Builtin(bf.clone()), &args, &kwargs)
            });
            match routed {
                Some(r) => r,
                None => invoke_builtin(&bf, &args, &kwargs),
            }
        }
        Object::Type(_) | Object::Function(_) | Object::BoundMethod(_) => {
            // For non-Builtin callables we need the VM to do the
            // dispatch (locals, frame setup, etc.).
            let r = crate::interp::with_interp_mut(|interp| {
                interp.call_object(target.clone(), &args, &kwargs)
            });
            match r {
                Some(r) => r,
                None => Err(weavepy_vm::error::runtime_error(
                    "PyObject_Call: no active interpreter",
                )),
            }
        }
        Object::None => Err(weavepy_vm::error::type_error(
            "PyObject_Call: NoneType is not callable",
        )),
        other => {
            // Best-effort: maybe `__call__` is defined.
            if let Some(call) = attr_lookup(&other, "__call__") {
                invoke_callable_inner(call, args, kwargs)
            } else {
                if std::env::var_os("WEAVEPY_TRACE_NOTCALL").is_some() {
                    eprintln!(
                        "[NOTCALL] capi invoke_callable {} nargs={}\n{}",
                        type_name(&other),
                        args.len(),
                        std::backtrace::Backtrace::force_capture()
                    );
                }
                Err(weavepy_vm::error::type_error(format!(
                    "'{}' object is not callable",
                    type_name(&other)
                )))
            }
        }
    };
    // The call may have mutated a C-resident bytearray through the VM
    // (aiohttp's parser: `self._buf.extend(...)` then macro-reads the
    // buffer); re-publish the struct fields before C code resumes.
    crate::mirror::sync_bytearray_boxes();
    match result {
        Ok(v) => crate::object::into_owned(v),
        Err(err) => {
            install_runtime_error(err);
            ptr::null_mut()
        }
    }
}

fn invoke_callable_inner(
    target: Object,
    args: Vec<Object>,
    kwargs: Vec<(String, Object)>,
) -> Result<Object, RuntimeError> {
    match target {
        Object::Builtin(bf) => invoke_builtin(&bf, &args, &kwargs),
        _ => {
            let r = crate::interp::with_interp_mut(|interp| {
                interp.call_object(target.clone(), &args, &kwargs)
            });
            r.unwrap_or_else(|| Err(weavepy_vm::error::runtime_error("no active interpreter")))
        }
    }
}

/// Invoke a WeavePy [`BuiltinFn`] honouring keyword arguments, mirroring
/// the VM's own builtin dispatch (`crate::interp` / `Interpreter::call`):
/// prefer the keyword-aware entry point, fall back to the positional one
/// only when there are no keywords, and otherwise raise the CPython
/// "takes no keyword arguments" `TypeError`.
fn invoke_builtin(
    bf: &weavepy_vm::object::BuiltinFn,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    if let Some(call_kw) = bf.call_kw.as_ref() {
        call_kw(args, kwargs)
    } else if kwargs.is_empty() {
        (bf.call)(args)
    } else {
        Err(weavepy_vm::error::type_error(format!(
            "{}() takes no keyword arguments",
            bf.name
        )))
    }
}

fn install_runtime_error(err: RuntimeError) {
    // Delegate to the centralised bridge, which preserves a real exception
    // *instance* verbatim (keeping custom attributes such as numpy's
    // `_UFuncBinaryResolutionError.ufunc`/`.dtypes`). The previous inline
    // version stringified the message, dropping those attributes and making a
    // later `str(exc)` raise `AttributeError`.
    crate::errors::set_pending_from_runtime(err);
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_IsTrue(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return -1;
    }
    let obj = unsafe { crate::object::clone_object(o) };
    // RFC 0046 (wave 4): a *foreign* object (a numpy scalar such as
    // `np.bool_`, a 0-d array, …) is opaque to the VM. Cloning it yields an
    // `Object::Foreign`, which `truthy`'s catch-all reports as `true` — so a
    // *false* `np.bool_` would test truthy. numpy's `polyfit` does
    // `if rank != order and not full:` where `rank != order` is exactly an
    // `np.bool_`; the false positive raised a spurious `RankWarning` that
    // `_mac_os_check` escalates to a hard `RuntimeError` on import. Dispatch
    // through the object's own `nb_bool` / `mp_length` / `sq_length` slots,
    // faithful to CPython's `PyObject_IsTrue`.
    if matches!(obj, Object::Foreign(_)) {
        return unsafe { foreign_is_true(o) };
    }
    // A *faithful instance* wearing a real C type whose `tp_as_number->nb_bool`
    // is defined (a numpy `ndarray` crosses as an `Object::Instance`, not a
    // `Foreign`) must drive that slot — CPython's `PyObject_IsTrue`. A
    // multi-element array's `nb_bool` raises `ValueError` ("truth value ...
    // ambiguous"); the naive `truthy` catch-all (`_ => true`) reported every
    // instance truthy, silently dropping numpy's error so
    // `PyObject_RichCompareBool(scalar, array)` (pandas `array_equivalent_object`,
    // `Series.equals` over object arrays) returned a bogus match.
    if matches!(obj, Object::Instance(_)) && unsafe { type_has_nb_bool(o) } {
        return unsafe { foreign_is_true(o) };
    }
    // A pure-Python instance whose truthiness is defined by a Python-level
    // `__bool__`/`__len__` has *no* C `nb_bool` slot (pandas' `NAType` is
    // `class NAType(C_NAType)` — a Python subclass of a `cdef` base — so
    // `__bool__` never reaches `tp_as_number`). The `truthy` catch-all below
    // reports every such instance true, silently dropping the dunder: a
    // reflected `str == pd.NA` yields `pd.NA`, and
    // `PyObject_RichCompareBool` then called `IsTrue(pd.NA)` → *true*, so
    // `pd.NA` bogusly "matched" every key in pandas' khash routines
    // (`ismember_object`/`isin`, `value_counts`, `unique`). Route these
    // through the VM's `obj_truthy` (CPython's `PyObject_IsTrue`: `nb_bool`
    // then `sq_length`/`mp_length`) so the `__bool__` runs and its
    // "boolean value of NA is ambiguous" error propagates as `-1`.
    if let Object::Instance(inst) = &obj {
        let cls = inst.cls();
        if cls.lookup("__bool__").is_some() || cls.lookup("__len__").is_some() {
            if let Some(res) = crate::interp::ensure_active(|| {
                crate::interp::with_interp_mut(|interp| interp.op_truth(&obj))
            }) {
                return match res {
                    Ok(b) => c_int::from(b),
                    Err(e) => {
                        crate::errors::set_pending_from_runtime(e);
                        -1
                    }
                };
            }
        }
    }
    truthy(&obj).into()
}

/// True when `o`'s C type defines an `nb_bool` slot (numpy `ndarray`,
/// numpy scalars, …). Used to decide whether a faithful instance's
/// truthiness must go through the CPython slot chain rather than the naive
/// native `truthy`.
///
/// # Safety
/// `o` must be a live, non-null `PyObject*` whose `ob_type` is readable.
unsafe fn type_has_nb_bool(o: *mut PyObject) -> bool {
    let ty = unsafe { (*o).ob_type } as *mut crate::layout::PyTypeObjectFull;
    if ty.is_null() {
        return false;
    }
    let nb = unsafe { (*ty).tp_as_number };
    !nb.is_null() && !unsafe { (*nb).nb_bool }.is_null()
}

/// CPython-faithful truthiness for a *foreign* extension object
/// (RFC 0046, wave 4): consult `nb_bool`, then `mp_length`, then
/// `sq_length`, defaulting to true when none is defined — exactly the
/// fallback chain in CPython's `PyObject_IsTrue`.
///
/// # Safety
/// `o` must be a live, non-null `PyObject*` whose `ob_type` is readable.
unsafe fn foreign_is_true(o: *mut PyObject) -> c_int {
    let ty = unsafe { (*o).ob_type } as *mut crate::layout::PyTypeObjectFull;
    if ty.is_null() {
        return 1;
    }
    // `nb_bool` (inquiry): `int (*)(PyObject*)` returning 1 / 0 / -1.
    let nb = unsafe { (*ty).tp_as_number };
    if !nb.is_null() {
        let slot = unsafe { (*nb).nb_bool };
        if !slot.is_null() {
            let f: unsafe extern "C" fn(*mut PyObject) -> c_int =
                unsafe { std::mem::transmute(slot) };
            return unsafe { f(o) };
        }
    }
    // `mp_length` / `sq_length` (lenfunc): `Py_ssize_t (*)(PyObject*)`;
    // truthy iff non-zero, propagating a negative (error) result.
    let mp = unsafe { (*ty).tp_as_mapping };
    if !mp.is_null() {
        let slot = unsafe { (*mp).mp_length };
        if !slot.is_null() {
            return len_to_truth(unsafe {
                let f: unsafe extern "C" fn(*mut PyObject) -> PySsizeT = std::mem::transmute(slot);
                f(o)
            });
        }
    }
    let sq = unsafe { (*ty).tp_as_sequence };
    if !sq.is_null() {
        let slot = unsafe { (*sq).sq_length };
        if !slot.is_null() {
            return len_to_truth(unsafe {
                let f: unsafe extern "C" fn(*mut PyObject) -> PySsizeT = std::mem::transmute(slot);
                f(o)
            });
        }
    }
    1
}

/// CPython-faithful `int()` for a *foreign* extension object (numpy
/// scalars such as `np.int64`): consult `nb_int`, then `nb_index`, read
/// straight off `tp_as_number` (the slots `attr_lookup` cannot see on an
/// opaque foreign object). Returns a new reference, the slot's pending
/// error (null with the exception set), or — when neither slot exists —
/// null with *no* pending error so the caller raises its own TypeError.
///
/// # Safety
/// `o` must be a live, non-null `PyObject*` whose `ob_type` is readable.
unsafe fn foreign_as_int(o: *mut PyObject) -> *mut PyObject {
    let ty = unsafe { (*o).ob_type } as *mut crate::layout::PyTypeObjectFull;
    if ty.is_null() {
        return ptr::null_mut();
    }
    let nb = unsafe { (*ty).tp_as_number };
    if nb.is_null() {
        return ptr::null_mut();
    }
    for slot in [unsafe { (*nb).nb_int }, unsafe { (*nb).nb_index }] {
        if !slot.is_null() {
            let f: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject =
                unsafe { std::mem::transmute(slot) };
            // The slot result (or its pending error) is authoritative; do
            // not fall through to the next slot once one is present.
            return unsafe { f(o) };
        }
    }
    ptr::null_mut()
}

/// CPython-faithful `PyNumber_Index` for a *foreign* extension object: call
/// its `tp_as_number->nb_index` slot directly (the slot `attr_lookup`
/// cannot see on an opaque foreign object). Unlike [`foreign_as_int`], this
/// consults **only** `nb_index` — CPython's `PyNumber_Index` never falls
/// back to `nb_int`. Returns a new reference on success, NULL with a
/// pending error when the slot raised, or — when no `nb_index` exists —
/// null with *no* pending error so the caller raises its own TypeError.
///
/// # Safety
/// `o` must be a live, non-null `PyObject*` whose `ob_type` is readable.
unsafe fn foreign_nb_index(o: *mut PyObject) -> *mut PyObject {
    let ty = unsafe { (*o).ob_type } as *mut crate::layout::PyTypeObjectFull;
    if ty.is_null() {
        return ptr::null_mut();
    }
    let nb = unsafe { (*ty).tp_as_number };
    if nb.is_null() {
        return ptr::null_mut();
    }
    let slot = unsafe { (*nb).nb_index };
    if slot.is_null() {
        return ptr::null_mut();
    }
    let f: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject =
        unsafe { std::mem::transmute(slot) };
    unsafe { f(o) }
}

/// CPython-faithful `float()` for a *foreign* extension object (numpy's
/// `float64`/`float32`): call its `tp_as_number->nb_float` slot directly.
/// CPython's `PyFloat_AsDouble` reads `nb_float` off the type, but the
/// getattro-based `__float__` lookup used for WeavePy-owned objects walks
/// numpy's *own* C dict and misses the dunder inherited from the mirror
/// base (the same blind spot `complex128.__complex__` hit). Returns a new
/// reference on success, NULL with a pending error when the slot raised,
/// or — when no `nb_float` exists — null with *no* pending error so the
/// caller falls through to its own protocol/error.
///
/// # Safety
/// `o` must be a live, non-null `PyObject*` whose `ob_type` is readable.
pub(crate) unsafe fn foreign_nb_float(o: *mut PyObject) -> *mut PyObject {
    let ty = unsafe { (*o).ob_type } as *mut crate::layout::PyTypeObjectFull;
    if ty.is_null() {
        return ptr::null_mut();
    }
    let nb = unsafe { (*ty).tp_as_number };
    if nb.is_null() {
        return ptr::null_mut();
    }
    let slot = unsafe { (*nb).nb_float };
    if slot.is_null() {
        return ptr::null_mut();
    }
    let f: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject =
        unsafe { std::mem::transmute(slot) };
    unsafe { f(o) }
}

/// The four unary `tp_as_number` slots, selecting which field
/// [`foreign_unary`] dispatches to.
#[derive(Clone, Copy)]
enum UnarySlot {
    Negative,
    Positive,
    Absolute,
    Invert,
}

/// Call a *foreign* extension object's unary `tp_as_number` slot
/// (`nb_negative`/`nb_positive`/`nb_absolute`/`nb_invert`) directly.
///
/// These slots are invisible to [`attr_lookup`] on an opaque foreign
/// object, yet a stock Cython type defines them — CPython's
/// `PyNumber_Negative` and friends dispatch straight through
/// `Py_TYPE(o)->tp_as_number->nb_*`. This matters for numpy's
/// **object-dtype** unary ufunc loop: `np.negative(arr)` /
/// `arr.__neg__()` call `PyNumber_Negative` once per element, so the old
/// `_ => null` arm planted raw NULL `PyObject*`s in the output object
/// array. Iterating them from Python coerced NULL→`None` (masking the
/// bug), but pandas' C-level `assert_series_equal` formatting dereferenced
/// the NULL and segfaulted. pandas `Timedelta` (a Cython class) is the
/// concrete trigger (`-Series([Timedelta(...)], dtype=object)`).
///
/// Returns a new reference on success, NULL with a pending error when the
/// slot raised, or NULL with *no* pending error when the slot is absent so
/// the caller can fall back to the dunder path / raise its own TypeError.
///
/// # Safety
/// `o` must be a live, non-null `PyObject*` whose `ob_type` is readable.
unsafe fn foreign_unary(o: *mut PyObject, which: UnarySlot) -> *mut PyObject {
    let ty = unsafe { (*o).ob_type } as *mut crate::layout::PyTypeObjectFull;
    if ty.is_null() {
        return ptr::null_mut();
    }
    let nb = unsafe { (*ty).tp_as_number };
    if nb.is_null() {
        return ptr::null_mut();
    }
    let slot = unsafe {
        match which {
            UnarySlot::Negative => (*nb).nb_negative,
            UnarySlot::Positive => (*nb).nb_positive,
            UnarySlot::Absolute => (*nb).nb_absolute,
            UnarySlot::Invert => (*nb).nb_invert,
        }
    };
    if slot.is_null() {
        return ptr::null_mut();
    }
    let f: unsafe extern "C" fn(*mut PyObject) -> *mut PyObject =
        unsafe { std::mem::transmute(slot) };
    unsafe { f(o) }
}

/// Shared fallback for the unary numeric ops when the operand is not one of
/// WeavePy's native numeric scalars.
///
/// Routes through the VM's full unary-dunder dispatch — identical to what
/// the `UNARY_OP` bytecode (`-x`/`~x`) or the `abs()` builtin would do: a
/// *foreign* extension operand resolves `__neg__`/`__abs__`/… through its
/// type's method table (the path that makes scalar `-Timedelta(...)` work),
/// and a VM / user instance through its `__op__`. This is what the mirrored
/// foreign `tp_as_number` slot cannot always do — a stock Cython
/// `pandas.Timedelta` has no `nb_negative` slot in WeavePy's mirror, so the
/// old slot-only path (and the even older `_ => null` arm) returned NULL,
/// which numpy's object-dtype unary ufunc loop planted straight into the
/// output array.
///
/// When no interpreter is active (a C extension calling in before any VM
/// frame), fall back to the foreign object's unary `tp_as_number` slot, then
/// the CPython unary-operator TypeError.
///
/// # Safety
/// `o` must be a live, non-null `PyObject*`; `other` is its cloned VM view.
unsafe fn unary_fallback(
    o: *mut PyObject,
    other: &Object,
    which: UnarySlot,
    err: &str,
) -> *mut PyObject {
    let vm = crate::interp::ensure_active(|| {
        crate::interp::with_interp_mut(|interp| match which {
            UnarySlot::Negative => interp.op_unary_public(other, weavepy_compiler::UnaryKind::Neg),
            UnarySlot::Positive => interp.op_unary_public(other, weavepy_compiler::UnaryKind::Pos),
            UnarySlot::Invert => interp.op_unary_public(other, weavepy_compiler::UnaryKind::Invert),
            UnarySlot::Absolute => interp.abs_public(other),
        })
    });
    match vm {
        Some(Ok(v)) => return crate::object::into_owned(v),
        Some(Err(e)) => {
            crate::errors::set_pending_from_runtime(e);
            return ptr::null_mut();
        }
        None => {}
    }
    // No active interpreter: dispatch the foreign object's unary slot directly.
    if matches!(other, Object::Foreign(_)) {
        let r = unsafe { foreign_unary(o, which) };
        if !r.is_null() || crate::errors::pending().is_some() {
            return r;
        }
    }
    crate::errors::set_type_error(err);
    ptr::null_mut()
}

/// Map a `lenfunc` result to a `PyObject_IsTrue` code: negative is an
/// error (passed through), zero is false, positive is true.
fn len_to_truth(n: PySsizeT) -> c_int {
    match n.cmp(&0) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Not(o: *mut PyObject) -> c_int {
    let r = unsafe { PyObject_IsTrue(o) };
    if r < 0 {
        -1
    } else {
        c_int::from(r == 0)
    }
}

fn truthy(o: &Object) -> bool {
    use Object as O;
    match o {
        O::None => false,
        O::Bool(b) => *b,
        O::Int(i) => *i != 0,
        O::Long(b) => !(**b == num_bigint::BigInt::from(0)),
        O::Float(f) => *f != 0.0,
        O::Str(s) => !s.is_empty(),
        O::Bytes(b) => !b.is_empty(),
        O::Tuple(items) => !items.is_empty(),
        O::List(rc) => !rc.borrow().is_empty(),
        O::Dict(rc) => !rc.borrow().is_empty(),
        O::Set(rc) => !rc.borrow().is_empty(),
        _ => true,
    }
}

/// Route a rich comparison through the VM's `do_richcompare`
/// ([`Interpreter::rich_compare_public`]). This is the equivalent of a
/// native type's `tp_richcompare` slot: it handles recursive container
/// comparison (tuple/list ordering, per-element `__eq__`), built-in
/// scalars, and user / `cdef`-class `__op__`/`__rop__` overloads — the
/// cases the capi's scalar-only `compare_objects` cannot.
///
/// Returns `Some(result)` when an interpreter handled the comparison
/// (a new reference, or NULL with a pending error when a dunder raised /
/// the ordering is unsupported), or `None` when no VM is active so the
/// caller can fall back to its native scalar path.
///
/// # Safety
/// `a` and `b` must be live, non-null `PyObject*`.
pub(crate) unsafe fn richcompare_via_vm(
    a: *mut PyObject,
    b: *mut PyObject,
    op: c_int,
) -> Option<*mut PyObject> {
    let kind = weavepy_compiler::CompareKind::from_arg(op as u32)?;
    let oa = unsafe { crate::object::clone_object(a) };
    let ob = unsafe { crate::object::clone_object(b) };
    crate::interp::ensure_active(|| {
        crate::interp::with_interp_mut(|interp| interp.rich_compare_public(&oa, &ob, kind))
    })
    .map(|res| match res {
        Ok(v) => crate::object::into_owned(v),
        Err(e) => {
            crate::errors::set_pending_from_runtime(e);
            ptr::null_mut()
        }
    })
}

/// Slot-semantics variant of [`richcompare_via_vm`] backing the
/// synthesized `tp_richcompare` bridge: dispatch only `a`'s own dunder for
/// `op` (CPython's `slot_tp_richcompare`), returning `NotImplemented` when
/// it is absent or declines. Never raises the final unsupported-ordering
/// `TypeError` — that belongs to the calling `do_richcompare` protocol.
///
/// # Safety
/// `a` and `b` must be live, non-null `PyObject*`.
pub(crate) unsafe fn richcompare_slot_via_vm(
    a: *mut PyObject,
    b: *mut PyObject,
    op: c_int,
) -> Option<*mut PyObject> {
    let kind = weavepy_compiler::CompareKind::from_arg(op as u32)?;
    let oa = unsafe { crate::object::clone_object(a) };
    let ob = unsafe { crate::object::clone_object(b) };
    crate::interp::ensure_active(|| {
        crate::interp::with_interp_mut(|interp| interp.rich_compare_slot_public(&oa, &ob, kind))
    })
    .map(|res| match res {
        Ok(v) => crate::object::into_owned(v),
        Err(e) => {
            crate::errors::set_pending_from_runtime(e);
            ptr::null_mut()
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_RichCompare(
    a: *mut PyObject,
    b: *mut PyObject,
    op: c_int,
) -> *mut PyObject {
    if a.is_null() || b.is_null() {
        crate::errors::set_type_error("bad argument to internal function");
        return ptr::null_mut();
    }
    if std::env::var_os("WEAVEPY_CMP_BT").is_some() {
        let oa = unsafe { crate::object::clone_object(a) };
        let ob = unsafe { crate::object::clone_object(b) };
        let na = type_name(&oa);
        let nb = type_name(&ob);
        if na == "NoneType" || nb == "NoneType" {
            eprintln!(
                "[CMP_BT] op={} '{}' vs '{}'\n{:?}",
                op,
                na,
                nb,
                std::backtrace::Backtrace::force_capture()
            );
        }
    }
    let _wpg = WpDepthGuard::enter("PyObject_RichCompare", a, b);
    // RFC 0047 (wave 5): CPython's `do_richcompare` dispatches through the
    // operands' `tp_richcompare` slots first — this is how a *foreign*
    // object (a numpy scalar's comparison, `float32 < float`) is compared.
    // WeavePy previously only knew native scalars, so foreign ordering was
    // a hard "not supported".
    let r = unsafe { richcompare_via_slot(a, b, op) };
    if r.is_null() {
        return ptr::null_mut();
    }
    if r != crate::singletons::not_implemented_ptr() {
        return r;
    }
    unsafe { crate::object::Py_DecRef(r) };
    // RFC 0047 (wave 5): the C `tp_richcompare` slots declined (or are
    // absent — WeavePy-native tuples/lists carry no C slot). Route through
    // the VM's `do_richcompare` so container ordering, per-element
    // comparison, and native operator overloads resolve exactly as the
    // `COMPARE_OP` bytecode would. Cython's import-time `(major, minor)`
    // version-tuple checks (`sys.version_info[:2] >= (3, 9)`) land here.
    if let Some(out) = unsafe { richcompare_via_vm(a, b, op) } {
        return out;
    }
    // No interpreter active (very early init): native scalar fallback —
    // built-in scalars / identity for ==,!=.
    let rb = unsafe { PyObject_RichCompareBool(a, b, op) };
    if rb < 0 {
        // No native ordering and no slot: `==`/`!=` already resolved to
        // identity inside `RichCompareBool`; an ordering op is unsupported.
        let oa = unsafe { crate::object::clone_object(a) };
        let ob = unsafe { crate::object::clone_object(b) };
        let sym = match op {
            0 => "<",
            1 => "<=",
            4 => ">",
            5 => ">=",
            _ => "compare",
        };
        if std::env::var_os("WEAVEPY_CMP_BT").is_some() {
            eprintln!(
                "[CMP_BT] '{}' between '{}' and '{}'\n{:?}",
                sym,
                type_name(&oa),
                type_name(&ob),
                std::backtrace::Backtrace::force_capture()
            );
        }
        crate::errors::set_type_error(format!(
            "'{}' not supported between instances of '{}' and '{}'",
            sym,
            type_name(&oa),
            type_name(&ob)
        ));
        return ptr::null_mut();
    }
    let truth = if rb != 0 {
        crate::singletons::true_ptr()
    } else {
        crate::singletons::false_ptr()
    };
    unsafe { crate::object::Py_IncRef(truth) };
    truth
}

/// CPython `do_richcompare` over the operands' `tp_richcompare` slots: try
/// `type(a)`'s slot with `op`, then (reflected, when `type(b)` differs)
/// `type(b)`'s with the swapped op, honouring the `NotImplemented`
/// protocol. Returns a new reference on success, NULL with a pending error
/// when a slot raised, or the (incref'd) `NotImplemented` singleton when
/// both decline / are absent (the caller then applies the native default).
///
/// # Safety
/// `a` and `b` must be live, non-null `PyObject*` with readable `ob_type`.
pub(crate) unsafe fn richcompare_via_slot(
    a: *mut PyObject,
    b: *mut PyObject,
    op: c_int,
) -> *mut PyObject {
    unsafe { richcompare_via_slot_masked(a, b, op, ptr::null_mut()) }
}

/// [`richcompare_via_slot`] with one slot address treated as absent.
///
/// The VM→C compare bridge (`foreign.rs::fwd_compare`) passes the address
/// of the VM-forwarding `synth_tp_richcompare` bridge here: the VM's
/// `rich_compare_obj` is the *caller* on that path, so a slot that merely
/// re-enters the VM for the same pair (a VM class like pandas
/// `CategoricalDtype` wears the bridge as its C `tp_richcompare`) must be
/// skipped — invoking it would ping-pong `VM → C → VM` for the same
/// operands until the native stack overflows. Skipping it exactly matches
/// CPython, where such a class's `tp_richcompare` is `slot_tp_richcompare`
/// dispatching to the same Python dunders the VM caller is about to try.
///
/// # Safety
/// `a` and `b` must be live, non-null `PyObject*` with readable `ob_type`.
pub(crate) unsafe fn richcompare_via_slot_masked(
    a: *mut PyObject,
    b: *mut PyObject,
    op: c_int,
    masked: *mut std::ffi::c_void,
) -> *mut PyObject {
    type RichCmpFunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, c_int) -> *mut PyObject;
    // `_Py_SwappedOp`: Py_LT<->Py_GT, Py_LE<->Py_GE, Py_EQ/Py_NE unchanged.
    const SWAPPED: [c_int; 6] = [4, 5, 2, 3, 0, 1];

    let richcompare_slot = |o: *mut PyObject| -> *mut std::ffi::c_void {
        let ty = unsafe { (*o).ob_type } as *mut crate::layout::PyTypeObjectFull;
        if ty.is_null() {
            return ptr::null_mut();
        }
        let slot = unsafe { (*ty).tp_richcompare };
        if !masked.is_null() && slot == masked {
            return ptr::null_mut();
        }
        slot
    };

    if !(0..=5).contains(&op) {
        return ptr::null_mut();
    }
    let not_impl = crate::singletons::not_implemented_ptr();
    let ta = unsafe { (*a).ob_type };
    let tb = unsafe { (*b).ob_type };
    let slot_a = richcompare_slot(a);
    let mut slot_b = if ta == tb {
        ptr::null_mut()
    } else {
        richcompare_slot(b)
    };
    // CPython `do_richcompare` subclass priority: a proper subclass on the
    // right gets its (reflected) slot consulted first, and not again after.
    if !slot_b.is_null()
        && slot_b != slot_a
        && unsafe { crate::types::PyType_IsSubtype(tb, ta) } != 0
    {
        let f: RichCmpFunc = unsafe { std::mem::transmute(slot_b) };
        let r = unsafe { f(b, a, SWAPPED[op as usize]) };
        if r.is_null() {
            return ptr::null_mut();
        }
        if r != not_impl {
            return r;
        }
        unsafe { crate::object::Py_DecRef(r) };
        slot_b = ptr::null_mut();
    }
    if !slot_a.is_null() {
        let f: RichCmpFunc = unsafe { std::mem::transmute(slot_a) };
        let r = unsafe { f(a, b, op) };
        if r.is_null() {
            return ptr::null_mut();
        }
        if r != not_impl {
            return r;
        }
        unsafe { crate::object::Py_DecRef(r) };
    }
    if !slot_b.is_null() {
        let f: RichCmpFunc = unsafe { std::mem::transmute(slot_b) };
        let r = unsafe { f(b, a, SWAPPED[op as usize]) };
        if r.is_null() {
            return ptr::null_mut();
        }
        if r != not_impl {
            return r;
        }
        unsafe { crate::object::Py_DecRef(r) };
    }
    unsafe { crate::object::Py_IncRef(not_impl) };
    not_impl
}

/// Invoke an object's own `tp_hash` slot directly, bypassing the VM hash
/// router. This is the C side of the VM→C `fwd_hash` bridge (foreign.rs):
/// the VM has already decided the operand is foreign and is asking C for its
/// native hash. Routing through `PyObject_Hash` here would bounce straight
/// back into the VM (`hash_public` → `py_hash_value` → `foreign::hash` →
/// here), an unbounded ping-pong that overflows the stack — exactly the numpy
/// scalar case (`hash(np.int64(0))`). Consulting only the type slot lets a
/// numpy `int64`/`float64` hash like the equal Python scalar so numpy's
/// `np.roll` `shifts` dict (keyed by Python-int axes, probed with numpy ints)
/// resolves instead of raising `KeyError`.
///
/// Returns `None` when the type carries no `tp_hash` (an unhashable foreign
/// type); the caller then falls back to an identity hash. When the slot is
/// present its result is returned verbatim (a `-1` return leaves the slot's
/// pending exception set, mirroring CPython).
pub(crate) unsafe fn hash_via_slot(o: *mut PyObject) -> Option<PyHashT> {
    type HashFunc = unsafe extern "C" fn(*mut PyObject) -> PyHashT;
    if o.is_null() {
        return None;
    }
    let ty = unsafe { (*o).ob_type } as *mut crate::layout::PyTypeObjectFull;
    if ty.is_null() {
        return None;
    }
    let slot = unsafe { (*ty).tp_hash };
    if slot.is_null() {
        return None;
    }
    // A foreign object whose C `tp_hash` is WeavePy's own VM-forwarding
    // bridge would ping-pong forever: `fwd_hash → hash_via_slot →
    // synth_tp_hash → PyObject_Hash → hash_public → py_hash_value(Foreign)
    // → foreign::hash → fwd_hash`. The bridge is inherited by a numpy scalar
    // that subclasses a WeavePy builtin (`np.float64 : float`,
    // `np.complex128 : complex`), so hash it by *value* — exactly the
    // float/complex `__hash__` CPython inherits (which reads the shared C
    // body) — preserving `hash(np.float64(x)) == hash(x)`. Any other kind
    // returns `None`, so the caller falls back to an identity hash, matching
    // `object.__hash__`.
    if slot == crate::types::synth_tp_hash_addr() {
        return unsafe { foreign_numeric_value_hash(o) };
    }
    let f: HashFunc = unsafe { std::mem::transmute(slot) };
    Some(unsafe { f(o) })
}

/// Value-based hash for a foreign scalar whose C `tp_hash` is WeavePy's own
/// VM-forwarding bridge (inherited from a builtin numeric base). Builds the
/// native `float`/`complex` value and hashes it through the VM's single hash
/// source of truth so it agrees bit-for-bit with the equal Python scalar
/// (`hash(np.float64(x)) == hash(x)`). Returns `None` for a non-numeric kind,
/// leaving the caller to fall back to an identity hash (CPython's
/// `object.__hash__`).
///
/// Classification goes through the *number protocol*, not a subtype test: a
/// numpy scalar's single `tp_base` chain is the numpy hierarchy
/// (`np.float64 → np.floating → … → object`) and its bridged VM type does not
/// re-expose Python `float`/`complex`, so `PyType_IsSubtype` cannot see the
/// relationship. Reading through the *complex* protocol subsumes both cases:
/// a real scalar reports a zero imaginary part, and `hash(complex(x, 0)) ==
/// hash(x)`, so a zero imag is hashed as a plain float — matching CPython's
/// `complex_hash` (and hence the inherited `float`/`complex` `__hash__`) for
/// numpy float *and* complex scalars alike. (Probing `__float__` first would
/// misclassify a complex scalar: numpy's `complex.__float__` returns the real
/// part with a `ComplexWarning` rather than raising.)
///
/// # Safety
/// `o` must be a live `PyObject*` whose `ob_type` is readable.
unsafe fn foreign_numeric_value_hash(o: *mut PyObject) -> Option<PyHashT> {
    // Clear any stale pending error so our own probe's error signal is
    // unambiguous.
    let _ = crate::errors::take_pending();
    let re = unsafe { crate::numbers::PyComplex_RealAsDouble(o) };
    if crate::errors::take_pending().is_some() {
        return None; // not a numeric scalar -> identity fallback
    }
    // numpy's complex scalar exposes neither a `__complex__` nor a working
    // `PyComplex_ImagAsDouble` (its `__float__` yields only the real part),
    // so read the imaginary component from the `.imag` attribute — every
    // numpy numeric scalar carries it (`0.0` for a real scalar). A
    // missing/failing attribute is treated as real. `hash(complex(x, 0)) ==
    // hash(x)`, so a zero imag hashes as a plain float, matching CPython.
    let im = unsafe { foreign_attr_double(o, b"imag\0".as_ptr().cast()) }.unwrap_or(0.0);
    // CPython 3.10+ hashes a NaN by the *object's* identity
    // (`_Py_HashDouble(inst, v)` → `PyObject_GenericHash(inst)`), so two
    // distinct NaN-valued scalars hash apart while the same object hashes
    // stably. The foreign pointer is that identity.
    if re.is_nan() || im.is_nan() {
        let h = (o as usize).rotate_right(4) as PyHashT;
        return Some(if h == -1 { -2 } else { h });
    }
    let value = if im == 0.0 {
        Object::Float(re)
    } else {
        Object::new_complex(re, im)
    };
    match weavepy_vm::builtins::hash_object(&value) {
        // CPython reserves `-1` for "error"; a real hash of `-1` becomes `-2`.
        Ok(Object::Int(h)) => Some(if h == -1 { -2 } else { h as PyHashT }),
        _ => None,
    }
}

/// Read numeric attribute `name` off a foreign scalar as an `f64`, or `None`
/// when the attribute is absent or not float-convertible. Consumes any pending
/// error so the probe stays side-effect free.
///
/// # Safety
/// `o` must be a live `PyObject*` and `name` a NUL-terminated C string.
unsafe fn foreign_attr_double(o: *mut PyObject, name: *const std::os::raw::c_char) -> Option<f64> {
    let attr = unsafe { PyObject_GetAttrString(o, name) };
    if attr.is_null() {
        let _ = crate::errors::take_pending();
        return None;
    }
    let d = unsafe { crate::numbers::PyFloat_AsDouble(attr) };
    let err = crate::errors::take_pending().is_some();
    unsafe { crate::object::Py_DecRef(attr) };
    if err {
        None
    } else {
        Some(d)
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_RichCompareBool(
    a: *mut PyObject,
    b: *mut PyObject,
    op: c_int,
) -> c_int {
    if a.is_null() || b.is_null() {
        return -1;
    }
    // CPython's `PyObject_RichCompareBool` opens with an identity shortcut so
    // that identity implies equality (e.g. `[nan].index(nan)` finds the NaN,
    // and pandas relies on it for object-array membership): when the operands
    // are the *same* object, `Py_EQ` is trivially true and `Py_NE` trivially
    // false without consulting `__eq__`.
    if std::ptr::eq(a, b) {
        if op == 2 {
            return 1;
        }
        if op == 3 {
            return 0;
        }
    }
    let _wpg = WpDepthGuard::enter("PyObject_RichCompareBool", a, b);
    let oa = unsafe { crate::object::clone_object(a) };
    let ob = unsafe { crate::object::clone_object(b) };
    let _cmp_trace = std::env::var_os("WEAVEPY_TRACE_CMP").is_some();
    if _cmp_trace {
        eprintln!(
            "[CMP] op={} a={:?}<{}> b={:?}<{}> a_id=0x{:x} b_id=0x{:x}",
            op,
            oa.repr(),
            oa.type_name_owned(),
            ob.repr(),
            ob.type_name_owned(),
            a as usize,
            b as usize,
        );
    }
    if std::env::var_os("WEAVEPY_CMP_BT").is_some() {
        let na = type_name(&oa);
        let nb = type_name(&ob);
        if (na == "NoneType" || nb == "NoneType") && (na != nb) {
            eprintln!(
                "[CMP_BT bool] op={} '{}' vs '{}'\n{:?}",
                op,
                na,
                nb,
                std::backtrace::Backtrace::force_capture()
            );
        }
    }
    // Fast path: a pair of native scalars the ordering table can resolve
    // directly (int/float/str/bytes/bool). Equivalent to the faithful
    // compare-then-`IsTrue` path below but avoids a VM round-trip.
    if let Some(o) = compare_objects(&oa, &ob) {
        let r = match op {
            0 => i32::from(o == std::cmp::Ordering::Less),
            1 => i32::from(o != std::cmp::Ordering::Greater),
            2 => i32::from(o == std::cmp::Ordering::Equal),
            3 => i32::from(o != std::cmp::Ordering::Equal),
            4 => i32::from(o == std::cmp::Ordering::Greater),
            5 => i32::from(o != std::cmp::Ordering::Less),
            _ => -1,
        };
        if _cmp_trace {
            eprintln!("[CMP]   -> fastpath ord={o:?} result={r}");
        }
        return r;
    }
    if _cmp_trace {
        eprintln!("[CMP]   -> no fastpath (types not both scalar); falling to slot/vm");
    }
    // General path — CPython's `PyObject_RichCompare(v, w, op)` followed by
    // `PyObject_IsTrue`. This is mandatory for objects with a custom
    // comparison (a pandas `Period`/`Timestamp` C `tp_richcompare`, a
    // WeavePy-native container, an instance's `__eq__`): the earlier
    // structural `eq_value` shortcut here compared two equal-but-distinct
    // `Period`s as unequal, so `pandas._libs.ops.vec_compare` (which calls
    // this with `Py_EQ`) returned all-`False` for object-dtype Period arrays.
    // Inlined (rather than delegating to `PyObject_RichCompare`) so the
    // no-interpreter fallback below cannot recurse back into this function.
    let slot_r = unsafe { richcompare_via_slot(a, b, op) };
    if slot_r.is_null() {
        return -1;
    }
    if slot_r != crate::singletons::not_implemented_ptr() {
        let truth = unsafe { PyObject_IsTrue(slot_r) };
        unsafe { crate::object::Py_DecRef(slot_r) };
        return truth;
    }
    unsafe { crate::object::Py_DecRef(slot_r) };
    if let Some(r) = unsafe { richcompare_via_vm(a, b, op) } {
        if r.is_null() {
            return -1;
        }
        let truth = unsafe { PyObject_IsTrue(r) };
        unsafe { crate::object::Py_DecRef(r) };
        return truth;
    }
    // No `tp_richcompare` slot and no active interpreter (very early init):
    // last-resort structural equality for `==`/`!=`, matching CPython's
    // default object comparison (identity-based) closely enough for the
    // scalar bootstrap. Ordering without a resolver stays unsupported.
    match op {
        2 => i32::from(oa.eq_value(&ob)),
        3 => i32::from(!oa.eq_value(&ob)),
        _ => -1,
    }
}

fn compare_objects(a: &Object, b: &Object) -> Option<std::cmp::Ordering> {
    use Object as O;
    match (a, b) {
        (O::Int(x), O::Int(y)) => Some(x.cmp(y)),
        (O::Float(x), O::Float(y)) => x.partial_cmp(y),
        (O::Str(x), O::Str(y)) => Some(x.as_ref().cmp(y.as_ref())),
        (O::Bytes(x), O::Bytes(y)) => Some(x.cmp(y)),
        (O::Bool(x), O::Bool(y)) => Some(x.cmp(y)),
        (O::Long(x), O::Long(y)) => Some(x.cmp(y)),
        // NB: these two arms must stay separate — an or-pattern binding
        // `(Int(x), Float(y)) | (Float(y), Int(x))` erases the operand
        // *order*, silently answering `2.0 < 1` as `1 < 2.0` (numpy's
        // object-array sort/partition compares through this fast path).
        (O::Int(x), O::Float(y)) => (*x as f64).partial_cmp(y),
        (O::Float(x), O::Int(y)) => x.partial_cmp(&(*y as f64)),
        _ => None,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Hash(o: *mut PyObject) -> PyHashT {
    if o.is_null() {
        return -1;
    }
    let _wpg = WpDepthGuard::enter("PyObject_Hash", o, ptr::null_mut());
    let obj = unsafe { crate::object::clone_object(o) };
    // RFC 0047 (wave 5): route through the VM's `do_hash_call` (the same
    // path the `hash()` builtin uses) so a value hashed from inside a C
    // extension matches the VM's CPython-faithful hash bit-for-bit. Cython's
    // `__hash__` idiom `hash(tuple(self._items))` compares the C-API result
    // against a VM-computed hash, so the two must agree.
    if let Some(res) =
        crate::interp::ensure_active(|| crate::interp::with_interp_mut(|i| i.hash_public(&obj)))
    {
        return match res {
            Ok(h) => {
                if h == -1 {
                    -2
                } else {
                    h as PyHashT
                }
            }
            Err(e) => {
                crate::errors::set_pending_from_runtime(e);
                -1
            }
        };
    }
    // No interpreter active (very early init): fall back to a structural
    // hash so callers still get a stable, non-error value.
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    DictKey(obj).hash(&mut hasher);
    let h = hasher.finish() as PyHashT;
    if h == -1 {
        -2
    } else {
        h
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Type(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let head = unsafe { &*o };
    let ty = head.ob_type;
    if ty.is_null() {
        return ptr::null_mut();
    }
    unsafe { crate::object::Py_IncRef(ty as *mut PyObject) };
    ty as *mut PyObject
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_IsInstance(o: *mut PyObject, cls: *mut PyObject) -> c_int {
    if o.is_null() || cls.is_null() {
        return 0;
    }
    let ob = unsafe { crate::object::clone_object(o) };
    let classinfo = unsafe { crate::object::clone_object(cls) };
    // Route through the interpreter's full `isinstance()` protocol so ABCMeta
    // virtual subclasses (`numbers.Number`, `collections.abc`, pandas'
    // `ABCSeries`/`ABCIndex`), tuples of classinfos and PEP 604 unions all
    // resolve exactly as the `isinstance` builtin would. CPython's
    // `PyObject_IsInstance` shares its implementation with the builtin, so the
    // C-API and bytecode paths must never diverge: a Cython `isinstance(x,
    // numbers.Number)` (pandas `NAType.__add__`/`__pow__`) has to see a plain
    // `int` as a number, which only the metaclass `__instancecheck__` reveals.
    match crate::interp::ensure_active(|| {
        crate::interp::with_interp_mut(|interp| interp.isinstance_public(&ob, &classinfo))
    }) {
        Some(Ok(true)) => 1,
        Some(Ok(false)) => 0,
        Some(Err(e)) => {
            crate::errors::set_pending_from_runtime(e);
            -1
        }
        // No active interpreter (very early import machinery) — fall back to
        // the structural MRO walk. ABC/tuple classinfos are vanishingly
        // unlikely this early, and this preserves best-effort behaviour.
        None => isinstance_structural(&ob, &classinfo),
    }
}

/// Best-effort structural `isinstance` used only when no interpreter is
/// active (before/without a running VM). Handles tuples of classinfos and a
/// bare MRO subclass check; it deliberately cannot see ABC-registered virtual
/// subclasses — that requires the interpreter path above.
fn isinstance_structural(ob: &Object, classinfo: &Object) -> c_int {
    if let Object::Tuple(items) = classinfo {
        for it in items.iter() {
            if isinstance_structural(ob, it) == 1 {
                return 1;
            }
        }
        return 0;
    }
    let class = match classinfo {
        Object::Type(t) => t,
        _ => return 0,
    };
    let actual = match ob {
        Object::Instance(inst) => Some(inst.cls()),
        Object::Type(_) => Some(weavepy_vm::builtin_types::builtin_types().type_.clone()),
        _ => weavepy_vm::builtin_types::builtin_types()
            .by_name(type_name(ob))
            .clone(),
    };
    actual.map_or(0, |a| i32::from(a.is_subclass_of(class)))
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_IsSubclass(o: *mut PyObject, cls: *mut PyObject) -> c_int {
    if o.is_null() || cls.is_null() {
        return 0;
    }
    let derived = unsafe { crate::object::clone_object(o) };
    let classinfo = unsafe { crate::object::clone_object(cls) };
    // Same reasoning as `PyObject_IsInstance`: dispatch through the VM's
    // `issubclass()` protocol so metaclass `__subclasscheck__` (ABCMeta) and
    // tuple/union classinfos are honoured.
    match crate::interp::ensure_active(|| {
        crate::interp::with_interp_mut(|interp| interp.issubclass_public(&derived, &classinfo))
    }) {
        Some(Ok(true)) => 1,
        Some(Ok(false)) => 0,
        Some(Err(e)) => {
            crate::errors::set_pending_from_runtime(e);
            -1
        }
        None => {
            let Object::Type(oa) = &derived else { return 0 };
            let Object::Type(oc) = &classinfo else {
                return 0;
            };
            i32::from(oa.is_subclass_of(oc))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Length(o: *mut PyObject) -> PySsizeT {
    if o.is_null() {
        return -1;
    }
    let obj = unsafe { crate::object::clone_object(o) };
    if let Some(n) = sequence_len(&obj) {
        return n;
    }
    if std::env::var_os("WEAVEPY_LEN_DBG").is_some() {
        eprintln!(
            "[LEN_DBG] PyObject_Length o={:p} obj_variant={} weavepy_owned={} mirror={}",
            o,
            obj.type_name(),
            crate::object::is_weavepy_owned(o),
            unsafe { crate::mirror::is_mirror(o) },
        );
    }
    // Genuinely foreign extension objects (numpy `ndarray`/`dtype`, Cython
    // `cdef class` instances) carry their length in their *own* C
    // `tp_as_sequence->sq_length` / `tp_as_mapping->mp_length` slot; read it
    // directly, exactly like CPython's `PyObject_Size`.
    if matches!(obj, Object::Foreign(_)) {
        if let Some(n) = unsafe { foreign_len(o) } {
            return n;
        }
        // A genuinely foreign object with *no* length slot fails here with
        // `TypeError`, exactly like CPython's `PyObject_Size`. It must NOT
        // fall through to the interpreter route below: the VM's Foreign
        // lane resolves `len()` by forwarding straight back through
        // `fwd_length` → `PyObject_Length`, so the two sides bounce until
        // the C stack faults (RFC 0069 WS5 — numpy's sequence discovery
        // under `np.asarray(wrapper)` calls `len()` on a foreign numpy
        // scalar; test_protocols' census row rode that cycle to a SIGBUS).
        crate::errors::set_type_error(format!("object of type '{}' has no len()", type_name(&obj)));
        return -1;
    }
    // Any other VM object — a `list`/`dict`/… *subclass* instance, a
    // generator, … — resolves `__len__` through the interpreter. Routing an
    // instance through `foreign_len` would invoke our own generic
    // `sq_length` bridge, which calls straight back into `PyObject_Length`:
    // unbounded recursion (numpy's `np.array(frozenlist, dtype=…)` once
    // `PySequence_Check` reports the subclass a sequence).
    if let Some(res) = crate::interp::ensure_active(|| {
        crate::interp::with_interp_mut(|interp| interp.len_object(&obj))
    }) {
        return match res {
            Ok(n) => n as PySsizeT,
            Err(e) => {
                crate::errors::set_pending_from_runtime(e);
                -1
            }
        };
    }
    // No active interpreter: last-ditch bridged length slot.
    if let Some(n) = unsafe { foreign_len(o) } {
        return n;
    }
    crate::errors::set_type_error(format!("object of type '{}' has no len()", type_name(&obj)));
    -1
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Size(o: *mut PyObject) -> PySsizeT {
    unsafe { PyObject_Length(o) }
}

/// Read `len(o)` from a foreign type's `tp_as_sequence->sq_length` or
/// `tp_as_mapping->mp_length` slot. Returns `None` when neither slot is
/// present (the object genuinely has no length).
///
/// # Safety
/// `o` must be a live `PyObject*`.
unsafe fn foreign_len(o: *mut PyObject) -> Option<PySsizeT> {
    type LenFunc = unsafe extern "C" fn(*mut PyObject) -> PySsizeT;
    let ty = unsafe { (*o).ob_type } as *mut crate::layout::PyTypeObjectFull;
    if ty.is_null() {
        return None;
    }
    let seq = unsafe { (*ty).tp_as_sequence };
    if !seq.is_null() {
        let slot = unsafe { (*seq).sq_length };
        if !slot.is_null() {
            let f: LenFunc = unsafe { std::mem::transmute(slot) };
            return Some(unsafe { f(o) });
        }
    }
    let map = unsafe { (*ty).tp_as_mapping };
    if !map.is_null() {
        let slot = unsafe { (*map).mp_length };
        if !slot.is_null() {
            let f: LenFunc = unsafe { std::mem::transmute(slot) };
            return Some(unsafe { f(o) });
        }
    }
    None
}

fn sequence_len(o: &Object) -> Option<PySsizeT> {
    use Object as O;
    Some(match o {
        O::Str(s) => s.chars().count() as PySsizeT,
        O::Bytes(b) => b.len() as PySsizeT,
        O::ByteArray(rc) => rc.borrow().len() as PySsizeT,
        O::Tuple(items) => items.len() as PySsizeT,
        O::List(rc) => rc.borrow().len() as PySsizeT,
        O::Dict(rc) => rc.borrow().len() as PySsizeT,
        O::Set(rc) => rc.borrow().len() as PySsizeT,
        O::FrozenSet(s) => s.len() as PySsizeT,
        _ => return None,
    })
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_GetItem(o: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    if o.is_null() || key.is_null() {
        crate::errors::set_type_error("bad argument to internal function");
        return ptr::null_mut();
    }
    // RFC 0069 WS5 — subscript dispatch recurses through extension
    // `mp_subscript`/`sq_item` chains (numpy's sequence discovery)
    // that never charge `Py_EnterRecursiveCall`.
    if c_stack_exhausted(" while subscripting an object") {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(o) };
    let k = unsafe { crate::object::clone_object(key) };
    // RFC 0047 (wave 5): route through the VM's full `__getitem__` dispatch
    // — the same logic `BINARY_SUBSCR` runs — so instance dunders, foreign
    // `mp_subscript`/`sq_item` slot wrappers (numpy `ndarray`/`flatiter`),
    // metaclass `__getitem__`, and PEP 585 aliases all resolve identically.
    if let Some(res) = crate::interp::ensure_active(|| {
        crate::interp::with_interp_mut(|interp| interp.subscr_get_public(&obj, &k))
    }) {
        return match res {
            Ok(v) => crate::object::into_owned(v),
            Err(e) => {
                crate::errors::set_pending_from_runtime(e);
                ptr::null_mut()
            }
        };
    }
    // No active interpreter: native-only fallback.
    match get_item(&obj, &k) {
        Ok(v) => crate::object::into_owned(v),
        Err(err) => {
            install_runtime_error(err);
            ptr::null_mut()
        }
    }
}

fn get_item(o: &Object, k: &Object) -> Result<Object, RuntimeError> {
    use Object as O;
    match o {
        O::Dict(rc) => rc
            .borrow()
            .get(&DictKey(k.clone()))
            .cloned()
            .ok_or_else(|| weavepy_vm::error::key_error(format!("{k:?}"))),
        O::List(rc) => match k {
            O::Int(i) => rc
                .borrow()
                .get(*i as usize)
                .cloned()
                .ok_or_else(|| weavepy_vm::error::index_error("list index out of range")),
            _ => Err(weavepy_vm::error::type_error(
                "list indices must be integers",
            )),
        },
        O::Tuple(items) => match k {
            O::Int(i) => items
                .get(*i as usize)
                .cloned()
                .ok_or_else(|| weavepy_vm::error::index_error("tuple index out of range")),
            _ => Err(weavepy_vm::error::type_error(
                "tuple indices must be integers",
            )),
        },
        O::Str(s) => match k {
            O::Int(i) => {
                let idx = *i as usize;
                s.chars()
                    .nth(idx)
                    .map(|c| Object::from_str(c.to_string()))
                    .ok_or_else(|| weavepy_vm::error::index_error("string index out of range"))
            }
            _ => Err(weavepy_vm::error::type_error(
                "string indices must be integers",
            )),
        },
        _ => Err(weavepy_vm::error::type_error("object is not subscriptable")),
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_SetItem(
    o: *mut PyObject,
    key: *mut PyObject,
    v: *mut PyObject,
) -> c_int {
    if o.is_null() || key.is_null() {
        return -1;
    }
    let obj = unsafe { crate::object::clone_object(o) };
    let k = unsafe { crate::object::clone_object(key) };
    let val = if v.is_null() {
        return unsafe { PyObject_DelItem(o, key) };
    } else {
        unsafe { crate::object::clone_object(v) }
    };
    // RFC 0047 (wave 5): route through the VM's full `__setitem__` dispatch
    // — the same logic `STORE_SUBSCR` runs — so instance dunders and foreign
    // `mp_ass_subscript`/`sq_ass_item` slot wrappers (numpy `ndarray`) work.
    if let Some(res) = crate::interp::ensure_active(|| {
        crate::interp::with_interp_mut(|interp| interp.subscr_set_public(&obj, &k, val.clone()))
    }) {
        return match res {
            Ok(()) => {
                unsafe { crate::mirror::sync_dict_ma_used(o) };
                // RFC 0047 (wave 5): the VM updated the shared prefix `Rc`,
                // but a faithful list mirror's inline `ob_item` buffer — read
                // directly by Cython's `__Pyx_PyList_GetItemRefFast` /
                // `PyList_GET_ITEM` macros — is now stale. The VM→C flush only
                // runs on *entry* to `ensure_active` (before this write), and
                // the next C read is an inlined macro that bypasses every
                // WeavePy function, so nothing would republish it. Re-sync now
                // so a `lst[i] = x` performed while C iterates `lst` (exactly
                // `guess_datetime_format`'s `format_guess[i] = attr_format`
                // inside `for i, _ in enumerate(format_guess)`) is visible.
                unsafe { crate::mirror::sync_list_ob_item(o) };
                0
            }
            Err(e) => {
                crate::errors::set_pending_from_runtime(e);
                -1
            }
        };
    }
    // No active interpreter: native-only fallback.
    match obj {
        Object::Dict(rc) => {
            rc.borrow_mut().insert(DictKey(k), val);
            unsafe { crate::mirror::sync_dict_ma_used(o) };
            0
        }
        Object::List(rc) => match k {
            Object::Int(i) => {
                let idx = i as usize;
                let len = rc.borrow().len();
                if idx < len {
                    rc.borrow_mut()[idx] = val;
                    // Keep the faithful mirror's `ob_item` coherent (see the
                    // active-interpreter arm above).
                    unsafe { crate::mirror::sync_list_ob_item(o) };
                    0
                } else {
                    crate::errors::set_value_error("list assignment index out of range");
                    -1
                }
            }
            _ => {
                crate::errors::set_type_error("list indices must be integers");
                -1
            }
        },
        _ => {
            crate::errors::set_type_error("object does not support item assignment");
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_DelItem(o: *mut PyObject, key: *mut PyObject) -> c_int {
    if o.is_null() || key.is_null() {
        crate::errors::set_type_error("bad argument to internal function");
        return -1;
    }
    let obj = unsafe { crate::object::clone_object(o) };
    let k = unsafe { crate::object::clone_object(key) };
    // RFC 0047 (wave 5): route through the VM's full `__delitem__` dispatch
    // — the same logic `DELETE_SUBSCR` runs — so instance dunders and foreign
    // `mp_ass_subscript`(NULL) slot wrappers resolve identically.
    if let Some(res) = crate::interp::ensure_active(|| {
        crate::interp::with_interp_mut(|interp| interp.subscr_del_public(&obj, &k))
    }) {
        return match res {
            Ok(()) => {
                unsafe { crate::mirror::sync_dict_ma_used(o) };
                // A list deletion shifts every trailing element and shrinks
                // `ob_size`; republish so inline macro reads stay coherent.
                unsafe { crate::mirror::sync_list_ob_item(o) };
                0
            }
            Err(e) => {
                crate::errors::set_pending_from_runtime(e);
                -1
            }
        };
    }
    // No active interpreter: native-only fallback.
    match obj {
        Object::Dict(rc) => {
            if rc.borrow_mut().shift_remove(&DictKey(k)).is_some() {
                unsafe { crate::mirror::sync_dict_ma_used(o) };
                0
            } else {
                crate::errors::set_value_error("KeyError");
                -1
            }
        }
        _ => {
            crate::errors::set_type_error("object does not support item deletion");
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Dir(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(o) };
    // Defer to the VM's `dir()` builtin: it walks the class MRO as CPython's
    // `object.__dir__` does, so class-level attributes (properties, methods)
    // are enumerated too. pandas' ujson serializes arbitrary objects by
    // iterating `PyObject_Dir` and getattr-ing each name — an instance-dict-
    // only listing silently dropped every `@property` (and with it the
    // recursion `test_default_handler` relies on for its OverflowError).
    match weavepy_vm::builtins::b_dir(std::slice::from_ref(&obj)) {
        Ok(list) => crate::object::into_owned(list),
        Err(e) => {
            crate::errors::set_pending_from_runtime(e);
            ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_GetIter(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(o) };
    let r = crate::interp::with_interp_mut(|interp| interp.iter_object(obj));
    match r {
        Some(Ok(it)) => crate::object::into_owned(it),
        Some(Err(err)) => {
            install_runtime_error(err);
            ptr::null_mut()
        }
        None => {
            crate::errors::set_runtime_error("PyObject_GetIter: no active interpreter");
            ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyIter_Next(it: *mut PyObject) -> *mut PyObject {
    if it.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(it) };
    let r = crate::interp::with_interp_mut(|interp| interp.iter_next_object(obj));
    match r {
        Some(Ok(Some(v))) => crate::object::into_owned(v),
        Some(Ok(None)) => ptr::null_mut(),
        Some(Err(err)) => {
            install_runtime_error(err);
            ptr::null_mut()
        }
        None => ptr::null_mut(),
    }
}

/// `PyIter_Send(iter, arg, presult)` — the 3.10+ send protocol (RFC 0072
/// WS3). Cython's coroutine machinery resumes a delegated *exact*
/// generator/coroutine through this (`__Pyx_PyGen_Send`); the extension
/// links it with `-undefined dynamic_lookup`, so a missing symbol is a
/// jump through NULL at the first `await` of a VM awaitable.
///
/// Returns `PYGEN_NEXT` (1) with a yielded value, `PYGEN_RETURN` (0)
/// with the completed iterator's return value (no exception left
/// pending, per CPython), or `PYGEN_ERROR` (-1) with the exception set.
#[no_mangle]
pub unsafe extern "C" fn PyIter_Send(
    iter: *mut PyObject,
    arg: *mut PyObject,
    presult: *mut *mut PyObject,
) -> c_int {
    if presult.is_null() {
        crate::errors::set_runtime_error("PyIter_Send: NULL result pointer");
        return -1;
    }
    unsafe { *presult = ptr::null_mut() };
    if iter.is_null() {
        crate::errors::set_runtime_error("PyIter_Send: NULL iterator");
        return -1;
    }
    let obj = unsafe { crate::object::clone_object(iter) };
    let sent = if arg.is_null() {
        Object::None
    } else {
        unsafe { crate::object::clone_object(arg) }
    };
    let r = crate::interp::with_interp_mut(|interp| interp.send_object_capi(obj, sent));
    match r {
        Some(Ok(v)) => {
            unsafe { *presult = crate::object::into_owned(v) };
            1 // PYGEN_NEXT
        }
        Some(Err(weavepy_vm::error::RuntimeError::PyException(pe)))
            if pe.type_name() == "StopIteration" =>
        {
            let value = match &pe.instance {
                Object::Instance(inst) => inst
                    .slot_get("value")
                    .filter(|v| !matches!(v, Object::None))
                    .or_else(|| match inst.slot_get("args") {
                        Some(Object::Tuple(t)) => t.first().cloned(),
                        _ => None,
                    })
                    .unwrap_or(Object::None),
                _ => Object::None,
            };
            unsafe { *presult = crate::object::into_owned(value) };
            0 // PYGEN_RETURN
        }
        Some(Err(err)) => {
            install_runtime_error(err);
            -1 // PYGEN_ERROR
        }
        None => {
            crate::errors::set_runtime_error("PyIter_Send: no active interpreter");
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyIter_NextItem(it: *mut PyObject, finished: *mut c_int) -> *mut PyObject {
    if !finished.is_null() {
        unsafe {
            *finished = 0;
        }
    }
    let r = unsafe { PyIter_Next(it) };
    if r.is_null() && !finished.is_null() {
        if crate::errors::pending().is_none() {
            unsafe {
                *finished = 1;
            }
        }
    }
    r
}

// ----------------------------------------------------------------
// PyNumber_*
// ----------------------------------------------------------------

fn binop(a: *mut PyObject, b: *mut PyObject, op: BinOp) -> *mut PyObject {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    let oa = unsafe { crate::object::clone_object(a) };
    let ob = unsafe { crate::object::clone_object(b) };
    if std::env::var_os("WEAVEPY_TSDBG").is_some() {
        eprintln!(
            "[TSDBG] binop {op:?} a_type={} a_kind={} b_type={} b_kind={}",
            unsafe { crate::object::debug_type_name(a) },
            oa.type_name(),
            unsafe { crate::object::debug_type_name(b) },
            ob.type_name()
        );
    }
    if let Some(v) = apply_binop(&oa, &ob, op) {
        return crate::object::into_owned(v);
    }
    // RFC 0046 (wave 4): when either operand is a *foreign* extension
    // object, dispatch through the operands' `tp_as_number` slots
    // (CPython's `binary_op1`) — a numpy scalar's `nb_subtract`, an
    // extension type's `nb_add`. Without this, `float32 - float32`
    // (numpy's import-time `getlimits` math) is a hard "unsupported
    // operand". Native operands fall through to the VM below.
    let either_foreign = matches!(oa, Object::Foreign(_)) || matches!(ob, Object::Foreign(_));
    if either_foreign {
        let r = unsafe { number_slot_binop(a, b, op) };
        if r.is_null() {
            // A slot raised; its exception is pending.
            if std::env::var_os("WEAVEPY_TSDBG").is_some() {
                eprintln!("[TSDBG] binop {op:?} foreign-slot RESULT=NULL (slot raised)");
            }
            return ptr::null_mut();
        }
        if r == crate::singletons::not_implemented_ptr() {
            unsafe { crate::object::Py_DecRef(r) };
            if std::env::var_os("WEAVEPY_TSDBG").is_some() {
                eprintln!("[TSDBG] binop {op:?} foreign-slot RESULT=NotImplemented");
            }
            crate::errors::set_type_error(format!("unsupported operand type for {op:?}"));
            return ptr::null_mut();
        }
        if std::env::var_os("WEAVEPY_TSDBG").is_some() {
            eprintln!("[TSDBG] binop {op:?} foreign-slot RESULT type={}", unsafe {
                crate::object::debug_type_name(r)
            });
        }
        return r;
    }
    // RFC 0047 (wave 5): both operands are WeavePy-native, so dispatch the
    // full VM binary-op protocol — `str % args` formatting (Cython's
    // `PyUnicode_Format` routes here), sequence concat/repeat, and user /
    // `cdef` class `__op__`/`__rop__` overloads — exactly as the
    // `BINARY_OP` bytecode would. The native scalar fast path above only
    // knew built-in numeric/`str+str` combinations.
    let kind = binop_kind(op);
    match crate::interp::ensure_active(|| {
        crate::interp::with_interp_mut(|interp| interp.binary_op_public(&oa, &ob, kind))
    }) {
        Some(Ok(v)) => crate::object::into_owned(v),
        Some(Err(e)) => {
            crate::errors::set_pending_from_runtime(e);
            ptr::null_mut()
        }
        None => {
            crate::errors::set_type_error(format!("unsupported operand type for {op:?}"));
            ptr::null_mut()
        }
    }
}

/// Map the C-API [`BinOp`] tag to the VM's [`weavepy_compiler::BinOpKind`]
/// so [`binop`] can defer native operands to the bytecode dispatcher.
fn binop_kind(op: BinOp) -> weavepy_compiler::BinOpKind {
    use weavepy_compiler::BinOpKind as K;
    match op {
        BinOp::Add => K::Add,
        BinOp::Sub => K::Sub,
        BinOp::Mul => K::Mult,
        BinOp::TrueDiv => K::Div,
        BinOp::FloorDiv => K::FloorDiv,
        BinOp::Rem => K::Mod,
        BinOp::Pow => K::Pow,
        BinOp::And => K::BitAnd,
        BinOp::Or => K::BitOr,
        BinOp::Xor => K::BitXor,
        BinOp::Lshift => K::LShift,
        BinOp::Rshift => K::RShift,
    }
}

/// CPython `binary_op1` over the operands' `tp_as_number` slots: try
/// `type(a)`'s slot, then (when `type(b)` differs) `type(b)`'s, honouring
/// the `NotImplemented` decline protocol — both slots are invoked as
/// `slot(a, b)` (the slot itself resolves which operand is its own type).
/// Returns a new reference on success, NULL with a pending error when a
/// slot raised, or the (incref'd) `NotImplemented` singleton when both
/// decline / are absent.
///
/// # Safety
/// `a` and `b` must be live, non-null `PyObject*` with readable `ob_type`.
unsafe fn number_slot_binop(a: *mut PyObject, b: *mut PyObject, op: BinOp) -> *mut PyObject {
    type BinaryFunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject;
    type TernaryFunc =
        unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject;

    unsafe fn number_suite(o: *mut PyObject) -> *mut crate::layout::PyNumberMethods {
        let ty = unsafe { (*o).ob_type } as *mut crate::layout::PyTypeObjectFull;
        if ty.is_null() {
            return ptr::null_mut();
        }
        unsafe { (*ty).tp_as_number }
    }
    let slot_of = |nb: *mut crate::layout::PyNumberMethods| -> *mut std::ffi::c_void {
        if nb.is_null() {
            return ptr::null_mut();
        }
        unsafe {
            match op {
                BinOp::Add => (*nb).nb_add,
                BinOp::Sub => (*nb).nb_subtract,
                BinOp::Mul => (*nb).nb_multiply,
                BinOp::TrueDiv => (*nb).nb_true_divide,
                BinOp::FloorDiv => (*nb).nb_floor_divide,
                BinOp::Rem => (*nb).nb_remainder,
                BinOp::Pow => (*nb).nb_power,
                BinOp::And => (*nb).nb_and,
                BinOp::Or => (*nb).nb_or,
                BinOp::Xor => (*nb).nb_xor,
                BinOp::Lshift => (*nb).nb_lshift,
                BinOp::Rshift => (*nb).nb_rshift,
            }
        }
    };
    let invoke = |slot: *mut std::ffi::c_void| -> *mut PyObject {
        if matches!(op, BinOp::Pow) {
            // `nb_power` is a ternaryfunc; pass `None` for the modulus.
            let f: TernaryFunc = unsafe { std::mem::transmute(slot) };
            unsafe { f(a, b, crate::singletons::none_ptr()) }
        } else {
            let f: BinaryFunc = unsafe { std::mem::transmute(slot) };
            unsafe { f(a, b) }
        }
    };

    // Resolve `op`'s number slot the way a readied type's *flattened*
    // `tp_as_number` would: the object's own type suite, else the first
    // non-NULL slot inherited from a `tp_base` ancestor. CPython's
    // `PyType_Ready` bakes inherited `nb_*` into every subtype, but WeavePy
    // may not have readied a Cython-extension-over-pure-Python subclass
    // (pandas `Timedelta(_Timedelta(datetime.timedelta))`), leaving the
    // leaf's own `nb_add`/`nb_subtract`/… NULL while the Cython base still
    // carries them. Walking `tp_base` here reproduces the flattening at call
    // time (the RFC 0046 §2.7 stop-gap, applied to the numeric suite) so a
    // reflected `numpy.timedelta64 + Timedelta` finds `_Timedelta`'s slot.
    let resolve_slot = |o: *mut PyObject| -> *mut std::ffi::c_void {
        let mut ty = unsafe { (*o).ob_type } as *mut crate::layout::PyTypeObjectFull;
        let mut guard = 0;
        while !ty.is_null() && guard < 100 {
            let s = slot_of(unsafe { (*ty).tp_as_number });
            if !s.is_null() {
                return s;
            }
            ty = unsafe { (*ty).tp_base };
            guard += 1;
        }
        ptr::null_mut()
    };

    let not_impl = crate::singletons::not_implemented_ptr();
    let ta = unsafe { (*a).ob_type };
    let slot_a = resolve_slot(a);
    let slot_b_raw = resolve_slot(b);
    // CPython's `binary_op1` drops the reflected slot when it is the *same*
    // function as the left slot (both inherited from a shared base), so the
    // slot never runs twice. Compare the resolved pointers, not the types.
    let slot_b = if slot_b_raw == slot_a {
        ptr::null_mut()
    } else {
        slot_b_raw
    };
    if std::env::var_os("WEAVEPY_SLOTDBG").is_some() {
        let nsa = unsafe { number_suite(a) };
        let base_a = unsafe {
            let tf = ta as *mut crate::layout::PyTypeObjectFull;
            if tf.is_null() {
                ptr::null_mut()
            } else {
                (*tf).tp_base
            }
        };
        eprintln!(
            "[SLOTDBG] {op:?} a_ty={:p} ({}) tp_as_number={:p} nb_add={:p} nb_sub={:p} tp_base={:p} readied={}",
            ta,
            unsafe { crate::object::debug_type_name(a) },
            nsa,
            if nsa.is_null() { ptr::null_mut() } else { unsafe { (*nsa).nb_add } },
            if nsa.is_null() { ptr::null_mut() } else { unsafe { (*nsa).nb_subtract } },
            base_a,
            crate::types::readied_slot_table(ta).is_some(),
        );
        let tb = unsafe { (*b).ob_type };
        let nsb = unsafe { number_suite(b) };
        let base_b = unsafe {
            let tf = tb as *mut crate::layout::PyTypeObjectFull;
            if tf.is_null() {
                ptr::null_mut()
            } else {
                (*tf).tp_base
            }
        };
        eprintln!(
            "[SLOTDBG]   b_ty={:p} ({}) tp_as_number={:p} tp_base={:p} readied={} inline={} mirror={} op_slot={:p} nb_rem={:p} nb_fdiv={:p}",
            tb,
            unsafe { crate::object::debug_type_name(b) },
            nsb,
            base_b,
            crate::types::readied_slot_table(tb as *mut crate::types::PyTypeObject).is_some(),
            crate::types::is_inline_instance_type(tb as *mut crate::types::PyTypeObject),
            unsafe { crate::mirror::is_mirror(b) },
            slot_of(nsb),
            if nsb.is_null() { ptr::null_mut() } else { unsafe { (*nsb).nb_remainder } },
            if nsb.is_null() { ptr::null_mut() } else { unsafe { (*nsb).nb_floor_divide } },
        );
    }

    let dbg = std::env::var_os("WEAVEPY_TSDBG").is_some();
    for (idx, slot) in [slot_a, slot_b].into_iter().enumerate() {
        if slot.is_null() {
            if dbg {
                eprintln!("[TSDBG]   slot[{idx}] (a=0,b=1) is NULL for {op:?}");
            }
            continue;
        }
        let r = invoke(slot);
        if r.is_null() {
            if dbg {
                eprintln!("[TSDBG]   slot[{idx}] {op:?} RAISED");
            }
            return ptr::null_mut();
        }
        if r != not_impl {
            if dbg {
                eprintln!("[TSDBG]   slot[{idx}] {op:?} returned type={}", unsafe {
                    crate::object::debug_type_name(r)
                });
            }
            return r;
        }
        if dbg {
            eprintln!("[TSDBG]   slot[{idx}] {op:?} returned NotImplemented");
        }
        unsafe { crate::object::Py_DecRef(r) };
    }
    unsafe { crate::object::Py_IncRef(not_impl) };
    not_impl
}

#[derive(Clone, Copy, Debug)]
enum BinOp {
    Add,
    Sub,
    Mul,
    TrueDiv,
    FloorDiv,
    Rem,
    Pow,
    And,
    Or,
    Xor,
    Lshift,
    Rshift,
}

fn apply_binop(a: &Object, b: &Object, op: BinOp) -> Option<Object> {
    use Object as O;
    match (a, b) {
        (O::Int(x), O::Int(y)) => match op {
            // CPython ints are arbitrary precision. Two `i64`s always fit
            // in `i128` for +/-/*, so promote via `int_from_i128` (which
            // re-demotes to `Int` when the product still fits) instead of
            // the old `wrapping_*`. Silent wraparound didn't just give
            // wrong answers — it defeated C extensions' overflow
            // *detection*: Cython's `x * 1_000_000_000` computes in C
            // `long long`, and on overflow falls back to
            // `Py_TYPE(x)->tp_as_number->nb_multiply` expecting a promoted
            // big int (pandas `Timedelta(days=10**6)` relies on this to
            // raise `OutOfBoundsTimedelta`).
            BinOp::Add => Some(weavepy_vm::object::int_from_i128(*x as i128 + *y as i128)),
            BinOp::Sub => Some(weavepy_vm::object::int_from_i128(*x as i128 - *y as i128)),
            BinOp::Mul => Some(weavepy_vm::object::int_from_i128(*x as i128 * *y as i128)),
            BinOp::TrueDiv => {
                if *y == 0 {
                    return None;
                }
                Some(O::Float(*x as f64 / *y as f64))
            }
            // Floor-division / remainder: defer zero-division (VM raises),
            // the sole i64 overflow (`i64::MIN // -1`, which would panic),
            // and — since `i64::div_euclid`/`rem_euclid` don't match
            // Python's floor semantics for mixed signs — every case to the
            // VM's faithful arbitrary-precision implementation.
            BinOp::FloorDiv | BinOp::Rem => None,
            // `**` can overflow i64, grow without bound, or (negative
            // exponent) produce a float — hand the whole thing to the VM.
            BinOp::Pow => None,
            // Bitwise of two machine ints is always a machine int and
            // matches Python's infinite two's-complement within i64.
            BinOp::And => Some(O::Int(x & y)),
            BinOp::Or => Some(O::Int(x | y)),
            BinOp::Xor => Some(O::Int(x ^ y)),
            // Shifts can grow past i64 (`1 << 100`) or take a negative
            // count; defer to the VM for the faithful arbitrary-precision
            // result rather than truncating.
            BinOp::Lshift | BinOp::Rshift => None,
        },
        (O::Float(x), O::Float(y)) => match op {
            // `fresh_float`: a NaN result gets a fresh identity tag, matching
            // CPython allocating a new object per operation.
            BinOp::Add => Some(weavepy_vm::object::fresh_float(x + y)),
            BinOp::Sub => Some(weavepy_vm::object::fresh_float(x - y)),
            BinOp::Mul => Some(weavepy_vm::object::fresh_float(x * y)),
            // Division by zero must raise ZeroDivisionError, not return
            // inf/nan — defer to the VM which raises faithfully.
            BinOp::TrueDiv => {
                if *y == 0.0 {
                    return None;
                }
                Some(weavepy_vm::object::fresh_float(x / y))
            }
            // Floor-division and `%` carry Python's floored-sign semantics
            // (result takes the divisor's sign; `//` floors the quotient —
            // not C truncation, not Rust's `rem_euclid`), and `**` of a
            // negative base with a fractional exponent produces a *complex*
            // result. All three live in the VM's faithful implementation.
            BinOp::FloorDiv | BinOp::Rem | BinOp::Pow => None,
            // Bitwise/shift on floats is a TypeError; let the VM raise it.
            BinOp::And | BinOp::Or | BinOp::Xor | BinOp::Lshift | BinOp::Rshift => None,
        },
        (O::Float(x), O::Int(y)) => apply_binop(&O::Float(*x), &O::Float(*y as f64), op),
        (O::Int(x), O::Float(y)) => apply_binop(&O::Float(*x as f64), &O::Float(*y), op),
        (O::Str(x), O::Str(y)) if matches!(op, BinOp::Add) => {
            let mut s = String::with_capacity(x.len() + y.len());
            s.push_str(x);
            s.push_str(y);
            Some(O::from_str(s))
        }
        _ => None,
    }
}

/// A `tp_as_number` binary-slot bridge for WeavePy's built-in numeric
/// types. Cython reads these slots off `Py_TYPE(x)->tp_as_number` and
/// calls them **directly** (e.g. `__Pyx_PyInt_MultiplyObjC`'s overflow
/// fallback), so a NULL slot is a hard crash (`blr NULL`). Unlike the
/// public [`PyNumber_Add`] & friends this never re-enters the *foreign*
/// `tp_as_number` dispatch (which would recurse, since *this* is one of
/// those slots): a foreign or otherwise-unhandled operand yields
/// `NotImplemented` so CPython's `binary_op1` protocol tries the other
/// operand's slot.
///
/// # Safety
/// `a` and `b` must be live, non-null `PyObject*`.
unsafe fn number_slot_native(a: *mut PyObject, b: *mut PyObject, op: BinOp) -> *mut PyObject {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    let oa = unsafe { crate::object::clone_object(a) };
    let ob = unsafe { crate::object::clone_object(b) };
    // Native scalar fast path (promotes int overflow to big-int).
    if let Some(v) = apply_binop(&oa, &ob, op) {
        return crate::object::into_owned(v);
    }
    // Decline foreign operands so the foreign type's own slot can run —
    // and, crucially, so we don't recurse back through `binop`.
    if matches!(oa, Object::Foreign(_)) || matches!(ob, Object::Foreign(_)) {
        let ni = crate::singletons::not_implemented_ptr();
        unsafe { crate::object::Py_IncRef(ni) };
        return ni;
    }
    // Both operands native but the fast path declined (big-int `//`/`%`/
    // `**`, `str % tuple`, sequence concat/repeat): route the faithful VM
    // binary-op protocol, mapping "no applicable rule" to NotImplemented.
    match crate::interp::ensure_active(|| {
        crate::interp::with_interp_mut(|interp| interp.binary_op_public(&oa, &ob, binop_kind(op)))
    }) {
        Some(Ok(v)) => crate::object::into_owned(v),
        Some(Err(e)) => {
            crate::errors::set_pending_from_runtime(e);
            ptr::null_mut()
        }
        None => {
            let ni = crate::singletons::not_implemented_ptr();
            unsafe { crate::object::Py_IncRef(ni) };
            ni
        }
    }
}

/// Generate a `#[no_mangle]` `binaryfunc` bridge for each numeric slot.
macro_rules! nb_binary_slot {
    ($name:ident, $op:expr) => {
        /// `binaryfunc` bridge installed into built-in numeric
        /// `tp_as_number` suites; see [`number_slot_native`].
        ///
        /// # Safety
        /// `a`/`b` must be valid `PyObject*` (the slot ABI contract).
        pub unsafe extern "C" fn $name(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
            unsafe { number_slot_native(a, b, $op) }
        }
    };
}

nb_binary_slot!(nb_slot_add, BinOp::Add);
nb_binary_slot!(nb_slot_subtract, BinOp::Sub);
nb_binary_slot!(nb_slot_multiply, BinOp::Mul);
nb_binary_slot!(nb_slot_remainder, BinOp::Rem);
nb_binary_slot!(nb_slot_floor_divide, BinOp::FloorDiv);
nb_binary_slot!(nb_slot_true_divide, BinOp::TrueDiv);
nb_binary_slot!(nb_slot_lshift, BinOp::Lshift);
nb_binary_slot!(nb_slot_rshift, BinOp::Rshift);
nb_binary_slot!(nb_slot_and, BinOp::And);
nb_binary_slot!(nb_slot_or, BinOp::Or);
nb_binary_slot!(nb_slot_xor, BinOp::Xor);

/// `ternaryfunc` bridge for `nb_power`. `a ** b` compiles to
/// `nb_power(a, b, Py_None)`; the 3-arg `pow(a, b, m)` form passes a real
/// modulus. Without a modulus we defer to the shared numeric slot path;
/// with one we fall back to the full [`PyNumber_Power`] protocol.
///
/// # Safety
/// `a`/`b` must be valid `PyObject*`; `m` may be `Py_None`/NULL/modulus.
pub unsafe extern "C" fn nb_slot_power(
    a: *mut PyObject,
    b: *mut PyObject,
    m: *mut PyObject,
) -> *mut PyObject {
    let no_mod = m.is_null() || m == crate::singletons::none_ptr();
    if no_mod {
        return unsafe { number_slot_native(a, b, BinOp::Pow) };
    }
    unsafe { PyNumber_Power(a, b, m) }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Add(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    binop(a, b, BinOp::Add)
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Subtract(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    binop(a, b, BinOp::Sub)
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Multiply(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    binop(a, b, BinOp::Mul)
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_TrueDivide(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    binop(a, b, BinOp::TrueDiv)
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_FloorDivide(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    binop(a, b, BinOp::FloorDiv)
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Remainder(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    binop(a, b, BinOp::Rem)
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Power(
    a: *mut PyObject,
    b: *mut PyObject,
    _mod_: *mut PyObject,
) -> *mut PyObject {
    binop(a, b, BinOp::Pow)
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Negative(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(o) };
    if std::env::var_os("WEAVEPY_TSDBG").is_some() {
        eprintln!(
            "[TSDBG] PyNumber_Negative in_type={} cloned={:?}",
            unsafe { crate::object::debug_type_name(o) },
            obj.type_name()
        );
    }
    let res = match obj {
        Object::Int(i) => crate::object::into_owned(Object::Int(-i)),
        Object::Float(f) => crate::object::into_owned(Object::Float(-f)),
        Object::Long(b) => crate::object::into_owned(Object::Long(Rc::new((*b).clone() * -1))),
        // A foreign Cython/numpy operand (pandas `Timedelta`, a numpy
        // scalar) dispatches through `nb_negative`; a VM/user object
        // through `__neg__`. The old `_ => null` return planted NULLs in
        // numpy object arrays (`np.negative(arr)`) and later segfaulted.
        ref other => unsafe {
            unary_fallback(
                o,
                other,
                UnarySlot::Negative,
                "bad operand type for unary -",
            )
        },
    };
    if std::env::var_os("WEAVEPY_TSDBG").is_some() && !res.is_null() {
        eprintln!("[TSDBG] PyNumber_Negative RESULT type={}", unsafe {
            crate::object::debug_type_name(res)
        });
    }
    res
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Positive(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(o) };
    match obj {
        // `+x` is the identity for the native numeric scalars (CPython's
        // `long_pos`/`float_pos` return the operand unchanged).
        Object::Int(_) | Object::Bool(_) | Object::Float(_) | Object::Long(_) => {
            crate::object::into_owned(obj)
        }
        ref other => unsafe {
            unary_fallback(
                o,
                other,
                UnarySlot::Positive,
                "bad operand type for unary +",
            )
        },
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Absolute(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(o) };
    match obj {
        Object::Int(i) => crate::object::into_owned(Object::Int(i.abs())),
        Object::Float(f) => crate::object::into_owned(Object::Float(f.abs())),
        Object::Long(b) => {
            let abs = if b.sign() == num_bigint::Sign::Minus {
                (*b).clone() * -1
            } else {
                (*b).clone()
            };
            crate::object::into_owned(Object::Long(Rc::new(abs)))
        }
        // Foreign operand → `nb_absolute`; VM/user object → `__abs__`.
        // (Same NULL-in-object-array hazard as `PyNumber_Negative`.)
        ref other => unsafe {
            unary_fallback(o, other, UnarySlot::Absolute, "bad operand type for abs()")
        },
    }
}

/// CPython's `_PyLong_FromBytes`: decode the buffer as an ASCII integer
/// literal (base 10) with the full string-parser rules — whitespace, sign,
/// `_` separators, arbitrary precision.
fn long_from_ascii_bytes(bytes: &[u8]) -> *mut PyObject {
    match std::ffi::CString::new(bytes) {
        Ok(cs) => unsafe { crate::numbers::PyLong_FromString(cs.as_ptr(), ptr::null_mut(), 10) },
        Err(_) => {
            crate::errors::set_value_error("invalid literal for int() with base 10");
            ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Long(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Int(i) => crate::object::into_owned(Object::Int(i)),
        Object::Bool(b) => crate::object::into_owned(Object::Int(i64::from(b))),
        // CPython's `float.__int__` rejects NaN (`ValueError`) and infinities
        // (`OverflowError`) and promotes a large magnitude to an
        // arbitrary-precision int rather than truncating to `i64`.
        Object::Float(f) => crate::numbers::float_to_int_object(f),
        Object::Long(big) => crate::object::into_owned(Object::Long(big)),
        Object::Str(s) => {
            // `int(str)` is arbitrary-precision in CPython. `s.parse::<i64>()`
            // rejected anything past i64::MAX (and any leading/trailing
            // whitespace, sign, or `_` separator), so `int("47393996303418497800")`
            // raised "invalid literal for int()" instead of yielding a big int.
            // pandas' `maybe_convert_numeric` does `int(s)` and then range-checks
            // itself, so that spurious failure surfaced as the wrong message
            // ("invalid literal…" instead of "Integer out of range…"). Delegate
            // to the full base-10 string parser, which produces an `Object::Long`
            // on overflow and matches CPython's whitespace/sign/underscore rules
            // and error text.
            match std::ffi::CString::new(s.as_bytes()) {
                Ok(cs) => unsafe {
                    crate::numbers::PyLong_FromString(cs.as_ptr(), ptr::null_mut(), 10)
                },
                Err(_) => {
                    // Embedded NUL — not a valid integer literal.
                    crate::errors::set_value_error("invalid literal for int() with base 10");
                    ptr::null_mut()
                }
            }
        }
        // CPython's `PyNumber_Long` accepts bytes/bytearray via
        // `_PyLong_FromBytes` — ASCII digits parsed base-10. numpy's
        // `LONG_setitem` funnels `np.array([b"423"], dtype="l")` through
        // here (RFC 0075 WS8, test_ufunc.test_addition_string_types[U-l]);
        // the missing arms drew the "not 'bytes'" TypeError whose message
        // ironically lists bytes-like as accepted.
        Object::Bytes(b) => long_from_ascii_bytes(&b),
        Object::ByteArray(b) => {
            let bytes = b.borrow().clone();
            long_from_ascii_bytes(&bytes)
        }
        other => {
            // RFC 0046 (wave 4): CPython's `PyNumber_Long` consults
            // `nb_int`, then `nb_index`, then `__trunc__`. A numpy scalar /
            // foreign object (or a user instance) reaches us here, so try
            // `__int__` then `__index__` via the dunder shim — the same
            // route `PyNumber_Index` already uses for `__index__`.
            //
            // RFC 0047 (wave 5): a *foreign* extension object is opaque to
            // `attr_lookup`, so dispatch through its `nb_int`/`nb_index`
            // slots directly (real numpy calls `int(np.int64(...))` during
            // `_multiarray_umath` init — the hermetic wave-4 gate's
            // `zeros @ ones` never exercised it).
            if matches!(other, Object::Foreign(_)) {
                let r = unsafe { foreign_as_int(o) };
                if !r.is_null() || crate::errors::pending().is_some() {
                    return r;
                }
            }
            for attr in ["__int__", "__index__"] {
                if let Some(dunder) = attr_lookup(&other, attr) {
                    // `attr_lookup` binds an instance method to its receiver
                    // (returns a `BoundMethod`), so it already carries `self`
                    // and must be called with no positional args — passing `o`
                    // again raised "__int__() takes 1 positional argument but 2
                    // were given" (numpy's object→int cast of a custom
                    // `__int__` object). A non-bound callable still needs `o`.
                    let is_bound = matches!(dunder, Object::BoundMethod(_));
                    let dunder_o = crate::object::into_owned(dunder);
                    let result = if is_bound {
                        unsafe { PyObject_CallNoArgs(dunder_o) }
                    } else {
                        unsafe { PyObject_CallOneArg(dunder_o, o) }
                    };
                    unsafe { crate::object::Py_DecRef(dunder_o) };
                    return result;
                }
            }
            if std::env::var_os("WEAVEPY_DEBUG_INT").is_some() {
                eprintln!(
                    "[PyNumber_Long] cannot convert to int: type={} debug={:?}",
                    other.type_name(),
                    other
                );
            }
            // CPython (`long_new_impl` → `PyNumber_Long`): an object with no
            // `nb_int`/`nb_index`/`__trunc__` that is not a string/bytes
            // raises this exact text (pandas' `_astype_nansafe` on a string
            // array with `pd.NA` asserts the message via `pytest.raises`).
            crate::errors::set_type_error(format!(
                "int() argument must be a string, a bytes-like object or a real number, not '{}'",
                other.type_name()
            ));
            ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Float(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(o) };
    match &obj {
        // CPython fast-paths an exact `float`; the other numeric builtins
        // convert through `nb_float`/`nb_index` to the identical double, so
        // short-circuit them here too.
        Object::Float(f) => return crate::object::into_owned(Object::Float(*f)),
        Object::Int(i) => return crate::object::into_owned(Object::Float(*i as f64)),
        Object::Long(big) => {
            use num_traits::ToPrimitive;
            return crate::object::into_owned(Object::Float(big.to_f64().unwrap_or(f64::INFINITY)));
        }
        Object::Bool(b) => return crate::object::into_owned(Object::Float(f64::from(*b as i32))),
        _ => {}
    }
    match unsafe { crate::numbers::float_number_protocol(o, &obj) } {
        // Fresh object in CPython — a *canonical* NaN gets a fresh identity
        // tag; an exotic payload is preserved verbatim (the bits are the
        // value — `np.float64(struct.unpack(...))` must round-trip, see
        // `PyFloat_FromDouble`).
        crate::numbers::FloatProtocol::Value(v) => {
            crate::object::into_owned(Object::Float(weavepy_vm::object::tag_unpacked_nan(v)))
        }
        crate::numbers::FloatProtocol::Raised => ptr::null_mut(),
        crate::numbers::FloatProtocol::NoProtocol => {
            // CPython's `PyNumber_Float` parses a `str`/`bytes` argument via
            // `PyFloat_FromString` before giving up.
            if matches!(obj, Object::Str(_) | Object::Bytes(_)) {
                return unsafe { crate::wave4::PyFloat_FromString(o) };
            }
            // The `float()` builtin's distinctive message (differs from
            // `PyFloat_AsDouble`'s "must be real number, not X") — pandas'
            // groupby-`corr` over an object column matches on this exact
            // wording ("must be a string or a.* number").
            crate::errors::set_type_error(format!(
                "float() argument must be a string or a real number, not '{}'",
                unsafe { crate::object::debug_type_name(o) }
            ));
            ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    let obj = unsafe { crate::object::clone_object(o) };
    // Native numerics.
    if matches!(
        obj,
        Object::Int(_) | Object::Long(_) | Object::Float(_) | Object::Bool(_) | Object::Complex(_)
    ) {
        return 1;
    }
    // CPython's `PyNumber_Check` is exactly `nb_index || nb_int || nb_float`.
    // A foreign object / faithful instance wearing a real C type (numpy
    // scalars) exposes these slots; read them straight off the type.
    let ty = unsafe { (*o).ob_type } as *mut crate::layout::PyTypeObjectFull;
    if !ty.is_null() {
        let nb = unsafe { (*ty).tp_as_number };
        if !nb.is_null()
            && (!unsafe { (*nb).nb_index }.is_null()
                || !unsafe { (*nb).nb_int }.is_null()
                || !unsafe { (*nb).nb_float }.is_null())
        {
            return 1;
        }
    }
    // A pure-Python numeric class (`decimal.Decimal`, `fractions.Fraction`, a
    // user class with `__int__`/`__float__`/`__index__`) carries its
    // conversions as VM dunders, not populated C slots. `is_scalar(Decimal(x))`
    // — and hence `pd.isna(Decimal("NaN"))`, `to_numeric`, and Decimal
    // extension arrays — hinge on this returning True.
    if let Object::Instance(inst) = &obj {
        let cls = inst.cls();
        let has = |name: &str| !matches!(cls.lookup(name), None | Some(Object::None));
        if has("__index__") || has("__int__") || has("__float__") {
            return 1;
        }
    }
    0
}

// ----------------------------------------------------------------
// PySequence_*
// ----------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PySequence_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    let obj = unsafe { crate::object::clone_object(o) };
    // CPython: `PyDict_Check(s)` short-circuits to 0 (a mapping is never a
    // sequence even though it carries `mp_subscript`).
    if matches!(obj, Object::Dict(_)) {
        return 0;
    }
    // CPython returns true iff `tp_as_sequence->sq_item != NULL`: the built-in
    // sequences (`list`/`tuple`/`str`/`bytes`/`bytearray`/`range`) *and their
    // subclasses*. A `set` has no `sq_item`. A plain class defining
    // `__getitem__` *does* get `sq_item` (its slotdef maps `__getitem__` to both
    // `sq_item` and `mp_subscript`) — handled by the `Object::Instance` arm.
    if sequence_object_has_sq_item(&obj) {
        return 1;
    }
    // Anything that wears a real C type reports its sequence protocol through
    // `tp_as_sequence->sq_item`; read it straight off `ob_type`, exactly like
    // CPython's `PySequence_Check`. This is what distinguishes a numpy `ndarray`
    // (a real C type with `sq_item` set) from a numpy *scalar* (`np.int64`, NULL
    // `sq_item`). pandas' `is_scalar` (`lib.pyx`) relies on
    // `PySequence_Check(ndarray) == 1` to reject arrays *before* the
    // `PyNumber_Check` fallback — an `ndarray` carries numeric `nb_*` slots, so
    // `PyNumber_Check(ndarray)` is 1 just like CPython, and only this sequence
    // check keeps `is_scalar(ndarray)` False.
    let ty = unsafe { (*o).ob_type } as *mut crate::layout::PyTypeObjectFull;
    if !ty.is_null() {
        let seq = unsafe { (*ty).tp_as_sequence };
        if !seq.is_null() && !unsafe { (*seq).sq_item }.is_null() {
            return 1;
        }
    }
    // A pure-VM class that defines `__getitem__` gets a `sq_item` wrapper in
    // CPython, so `PySequence_Check` reports True. WeavePy carries `__getitem__`
    // as a VM dunder rather than a populated C `sq_item`, so consult the class
    // directly. A dict subclass is excluded, matching CPython's `PyDict_Check`
    // short-circuit (`Py_TPFLAGS_DICT_SUBCLASS`).
    //
    // This is what keeps `is_scalar(Series)` False: a `Series` also exposes
    // `__float__`, so `PyNumber_Check(Series)` is True, and without this sequence
    // check pandas' `_isna(Series)` takes the scalar `libmissing.checknull`
    // branch and returns a bool — breaking `Series.isna()` and hence every
    // `dict_to_mgr` construction, `reset_index`, `combine_first`, etc.
    if let Object::Instance(inst) = &obj {
        if !matches!(inst.native.get(), Some(Object::Dict(_))) {
            // Reached only when the C type's `sq_item` is NULL. A *pure-VM*
            // class (`Series`/`Index`/`Categorical`, all with `sq_item` NULL in
            // WeavePy) defines `__getitem__` as a Python `function`, which
            // CPython would back with a real `sq_item` slot → sequence; return
            // 1 so `is_scalar(Series)` stays False. A WeavePy *mirror* of a
            // foreign object whose type is a genuine mapping-not-sequence
            // (numpy's parametric dtypes — datetime64/timedelta64/'S'/'U'/
            // structured — carry numpy's `descr_subscript` as `mp_subscript`
            // with a NULL `sq_item`) exposes `__getitem__` as a C bridge
            // (`Object::Builtin`) instead; CPython's `PySequence_Check` is 0
            // for those, so returning 1 here made numpy's `np.array([dtype],
            // dtype=object)` (reached by `DataFrame.dtypes` →
            // `combine_first`/`quantile`) call `PySequence_Fast(dtype)` and
            // raise "Could not convert object to sequence". A foreign sequence
            // that really has `sq_item` (numpy's `_array_converter`) returns 1
            // at the C-slot check above and never reaches here.
            if matches!(
                inst.cls().lookup("__getitem__"),
                Some(Object::Function(_) | Object::BoundMethod(_))
            ) {
                return 1;
            }
        }
    }
    0
}

/// CPython's `PySequence_Check` predicate for native objects: a value has a
/// sequence `sq_item` slot iff it is a built-in sequence or a subclass of one.
///
/// numpy's array coercion (`np.array(x, dtype=…)`) leans on this: an object
/// that fails the check is treated as a **scalar** and handed to the dtype's
/// `int()`/`float()` setter, so a false negative for a `list` subclass (e.g.
/// pandas' `FrozenList`, passed to `np.array(codes, dtype="int64")` when
/// building a `MultiIndex` engine) surfaces as "cannot convert to int".
fn sequence_object_has_sq_item(o: &Object) -> bool {
    use Object as O;
    match o {
        O::List(_) | O::Tuple(_) | O::Str(_) | O::Bytes(_) | O::ByteArray(_) | O::Range(_) => true,
        // A subclass of a built-in sequence *is* that sequence — it wraps the
        // primitive in `native` — so it inherits `sq_item` just like CPython.
        O::Instance(inst) => matches!(
            inst.native.get(),
            Some(
                O::List(_) | O::Tuple(_) | O::Str(_) | O::Bytes(_) | O::ByteArray(_) | O::Range(_)
            )
        ),
        _ => false,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_Length(o: *mut PyObject) -> PySsizeT {
    unsafe { PyObject_Length(o) }
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_Size(o: *mut PyObject) -> PySsizeT {
    unsafe { PyObject_Length(o) }
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_GetItem(o: *mut PyObject, i: PySsizeT) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    // RFC 0069 WS5: structurally recursive abstract entry — extension code
    // (numpy's dimension discovery) recurses through here without
    // `Py_EnterRecursiveCall`, and the stable-slot lane below bypasses
    // `PyObject_GetItem`'s guard, so this entry needs its own.
    if c_stack_exhausted(" while getting an item") {
        return ptr::null_mut();
    }
    // RFC 0069 WS5: on CPython, the new reference returned for a tuple/list
    // element is a pointer the *container itself* also owns, so the common
    // extension idiom `stash[i] = PySequence_GetItem(seq, i); Py_DECREF(item)`
    // leaves a valid pointer for as long as the container lives. numpy's
    // `_vec_string_with_args` builds its broadcast list exactly that way —
    // a fresh per-call box died on that DECREF and `PyArray_FromAny` then
    // discovered a dtype from freed memory (the test_defchararray census
    // SIGSEGV). Serve elements from the same stable-slot lanes the
    // borrowed-reference getters use (faithful `ob_item` / borrowed-item
    // cache), upgraded to a new reference.
    let obj = unsafe { crate::object::clone_object(o) };
    let stable = match &obj {
        Object::Tuple(items) => {
            let n = items.len() as PySsizeT;
            let idx = if i < 0 { i + n } else { i };
            (0..n)
                .contains(&idx)
                .then(|| unsafe { crate::containers::PyTuple_GetItem(o, idx) })
        }
        Object::List(rc) => {
            let n = rc.borrow().len() as PySsizeT;
            let idx = if i < 0 { i + n } else { i };
            (0..n)
                .contains(&idx)
                .then(|| unsafe { crate::containers::PyList_GetItem(o, idx) })
        }
        _ => None,
    };
    if let Some(p) = stable {
        if !p.is_null() {
            unsafe { crate::object::Py_IncRef(p) };
            return p;
        }
        // Slot read failed (e.g. staged container): fall through to the
        // generic path, clearing whatever the borrowed getter set.
        crate::errors::clear_thread_local();
    }
    let key = crate::object::into_owned(Object::Int(i as i64));
    let result = unsafe { PyObject_GetItem(o, key) };
    unsafe { crate::object::Py_DecRef(key) };
    result
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_SetItem(
    o: *mut PyObject,
    i: PySsizeT,
    v: *mut PyObject,
) -> c_int {
    if o.is_null() {
        return -1;
    }
    let key = crate::object::into_owned(Object::Int(i as i64));
    let result = unsafe { PyObject_SetItem(o, key, v) };
    unsafe { crate::object::Py_DecRef(key) };
    result
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_Contains(o: *mut PyObject, v: *mut PyObject) -> c_int {
    if o.is_null() || v.is_null() {
        return -1;
    }
    let obj = unsafe { crate::object::clone_object(o) };
    let needle = unsafe { crate::object::clone_object(v) };
    // A `list`/`tuple` haystack: a shallow `eq_value` hit is definitive, but a
    // *miss* is not — instance elements compare by `__eq__` through the VM
    // (CPython's `PySequence_Contains` calls `PyObject_RichCompare`). Cython's
    // `other.time() in self.end` (pandas `BusinessHour._apply`, a tuple of
    // `datetime.time` instances) lands here and must dispatch.
    fn plain_scalar(o: &Object) -> bool {
        matches!(
            o,
            Object::None
                | Object::Bool(_)
                | Object::Int(_)
                | Object::Long(_)
                | Object::Float(_)
                | Object::Complex(_)
                | Object::Str(_)
                | Object::WStr(_)
                | Object::Bytes(_)
                | Object::ByteArray(_)
        )
    }
    let seq_items: Option<Vec<Object>> = match &obj {
        Object::List(rc) => Some(rc.borrow().clone()),
        Object::Tuple(items) => Some(items.iter().cloned().collect()),
        _ => None,
    };
    if let Some(items) = seq_items {
        if items.iter().any(|x| x.eq_value(&needle)) {
            return 1;
        }
        if plain_scalar(&needle) && items.iter().all(plain_scalar) {
            return 0;
        }
        let res = crate::interp::ensure_active(|| {
            crate::interp::with_interp_mut(|interp| interp.py_contains(&obj, &needle))
        });
        return match res {
            Some(Ok(found)) => i32::from(found),
            Some(Err(e)) => {
                crate::errors::set_pending_from_runtime(e);
                -1
            }
            None => 0,
        };
    }
    match obj {
        Object::Str(s) => match needle {
            Object::Str(n) => i32::from(s.contains(n.as_ref())),
            _ => 0,
        },
        Object::Set(rc) => i32::from(rc.borrow().contains(&DictKey(needle))),
        Object::FrozenSet(s) => i32::from(s.contains(&DictKey(needle))),
        // `key in dict`. CPython dispatches the dict's `sq_contains`; Cython
        // compiles `val in <module-global dict>` (pandas' `_try_infer_map`'s
        // `if val in _TYPE_MAP`) to `PySequence_Contains`, *not*
        // `PyDict_Contains`. Without this arm the dict fell through to the old
        // `_ => -1` — an error return with no exception set — which surfaced as
        // `infer_dtype` failing with "C extension reported failure without
        // setting an exception" for *every* input (the function's first act is
        // `_try_infer_map`).
        Object::Dict(rc) => i32::from(rc.borrow().contains_key(&DictKey(needle))),
        // Everything else (mappingproxy, dict views, ranges, bytes, a user
        // `__contains__`, a foreign object's `sq_contains`) resolves through
        // the VM's containment, matching CPython's `sq_contains` /
        // `_PySequence_IterSearch` dispatch and — crucially — installing a real
        // exception on failure instead of the bare `-1`.
        other => {
            let res = crate::interp::ensure_active(|| {
                crate::interp::with_interp_mut(|interp| interp.py_contains(&other, &needle))
            });
            match res {
                Some(Ok(found)) => i32::from(found),
                Some(Err(e)) => {
                    crate::errors::set_pending_from_runtime(e);
                    -1
                }
                // No interpreter active (pure C-side): best-effort native test.
                None => match other.contains(&needle) {
                    Ok(found) => i32::from(found),
                    Err(e) => {
                        crate::errors::set_pending_from_runtime(e);
                        -1
                    }
                },
            }
        }
    }
}

/// Collect every item of an arbitrary iterable `o` by driving the VM's
/// iterator protocol (`iter()` then repeated `next()`), exactly as
/// CPython's `PySequence_List`/`PySequence_Tuple` do via `PyObject_GetIter`
/// + `PyIter_Next`. Returns the items, or `None` with a pending exception
/// when `o` is not iterable or an element access raised.
///
/// # Safety
/// `o` must be a live, non-null `PyObject*`.
pub(crate) unsafe fn collect_iterable(o: *mut PyObject) -> Option<Vec<Object>> {
    let it = unsafe { PyObject_GetIter(o) };
    if it.is_null() {
        // Not iterable — `PyObject_GetIter` set the TypeError.
        return None;
    }
    let mut items = Vec::new();
    loop {
        let item = unsafe { PyIter_Next(it) };
        if item.is_null() {
            // Exhausted (no error) or an element raised (error pending).
            break;
        }
        items.push(unsafe { crate::object::clone_object(item) });
        unsafe { crate::object::Py_DecRef(item) };
    }
    unsafe { crate::object::Py_DecRef(it) };
    if unsafe { crate::errors::PyErr_Occurred() }.is_null() {
        Some(items)
    } else {
        None
    }
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_List(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(o) };
    match obj {
        Object::List(rc) => crate::object::into_owned(Object::new_list(rc.borrow().clone())),
        Object::Tuple(items) => {
            crate::object::into_owned(Object::new_list(items.iter().cloned().collect()))
        }
        // CPython's `PySequence_List(o)` is `o` coerced through the iterator
        // protocol, *not* a no-op for non-sequences. Cython's
        // `list(self)` (`cdef class` `__richcmp__`, `__hash__`, …) compiles
        // straight to `PySequence_List`, so returning an empty list here
        // silently corrupted every `list(cdef_instance)`.
        _ => match unsafe { collect_iterable(o) } {
            Some(items) => crate::object::into_owned(Object::new_list(items)),
            None => ptr::null_mut(),
        },
    }
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_Tuple(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let obj = unsafe { crate::object::clone_object(o) };
    match obj {
        Object::List(rc) => crate::object::into_owned(Object::new_tuple(rc.borrow().clone())),
        Object::Tuple(items) => crate::object::into_owned(Object::Tuple(items)),
        // As with `PySequence_List`, coerce any iterable via its iterator
        // protocol (`tuple(self)` → `PySequence_Tuple`).
        _ => match unsafe { collect_iterable(o) } {
            Some(items) => crate::object::into_owned(Object::new_tuple(items)),
            None => ptr::null_mut(),
        },
    }
}

// ----------------------------------------------------------------
// PyMapping_*
// ----------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PyMapping_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(unsafe { crate::object::clone_object(o) }, Object::Dict(_)).into()
}

#[no_mangle]
pub unsafe extern "C" fn PyMapping_Length(o: *mut PyObject) -> PySsizeT {
    unsafe { PyObject_Length(o) }
}

#[no_mangle]
pub unsafe extern "C" fn PyMapping_Size(o: *mut PyObject) -> PySsizeT {
    unsafe { PyObject_Length(o) }
}

#[no_mangle]
pub unsafe extern "C" fn PyMapping_GetItemString(
    o: *mut PyObject,
    key: *const c_char,
) -> *mut PyObject {
    if o.is_null() || key.is_null() {
        return ptr::null_mut();
    }
    let k = crate::object::into_owned(Object::from_str(
        unsafe { CStr::from_ptr(key) }
            .to_string_lossy()
            .into_owned(),
    ));
    let result = unsafe { PyObject_GetItem(o, k) };
    unsafe { crate::object::Py_DecRef(k) };
    result
}

#[no_mangle]
pub unsafe extern "C" fn PyMapping_HasKey(o: *mut PyObject, key: *mut PyObject) -> c_int {
    if o.is_null() || key.is_null() {
        return 0;
    }
    let p = unsafe { PyObject_GetItem(o, key) };
    if p.is_null() {
        crate::errors::clear_thread_local();
        0
    } else {
        unsafe { crate::object::Py_DecRef(p) };
        1
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyMapping_HasKeyString(o: *mut PyObject, key: *const c_char) -> c_int {
    if o.is_null() || key.is_null() {
        return 0;
    }
    let k = crate::object::into_owned(Object::from_str(
        unsafe { CStr::from_ptr(key) }
            .to_string_lossy()
            .into_owned(),
    ));
    let result = unsafe { PyMapping_HasKey(o, k) };
    unsafe { crate::object::Py_DecRef(k) };
    result
}

#[no_mangle]
pub unsafe extern "C" fn PyMapping_SetItemString(
    o: *mut PyObject,
    key: *const c_char,
    v: *mut PyObject,
) -> c_int {
    if o.is_null() || key.is_null() {
        return -1;
    }
    let k = crate::object::into_owned(Object::from_str(
        unsafe { CStr::from_ptr(key) }
            .to_string_lossy()
            .into_owned(),
    ));
    let result = unsafe { PyObject_SetItem(o, k, v) };
    unsafe { crate::object::Py_DecRef(k) };
    result
}

#[no_mangle]
pub unsafe extern "C" fn PyMapping_DelItemString(o: *mut PyObject, key: *const c_char) -> c_int {
    if o.is_null() || key.is_null() {
        return -1;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Dict(rc) => {
            let key_s = unsafe { CStr::from_ptr(key) }
                .to_string_lossy()
                .into_owned();
            let dk = DictKey(Object::from_str(key_s.clone()));
            if rc.borrow_mut().shift_remove(&dk).is_some() {
                unsafe { crate::mirror::sync_dict_ma_used(o) };
                0
            } else {
                crate::errors::set_pending(
                    Some(weavepy_vm::builtin_types::builtin_types().key_error.clone()),
                    Object::from_str(key_s),
                );
                -1
            }
        }
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyMapping_DelItem(o: *mut PyObject, k: *mut PyObject) -> c_int {
    if o.is_null() || k.is_null() {
        return -1;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Dict(rc) => {
            let dk = DictKey(unsafe { crate::object::clone_object(k) });
            if rc.borrow_mut().shift_remove(&dk).is_some() {
                0
            } else {
                crate::errors::set_pending(
                    Some(weavepy_vm::builtin_types::builtin_types().key_error.clone()),
                    dk.0,
                );
                -1
            }
        }
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyMapping_Keys(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Dict(rc) => {
            let items: Vec<Object> = rc.borrow().keys().map(|k| k.0.clone()).collect();
            crate::object::into_owned(Object::new_list(items))
        }
        _ => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyMapping_Values(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Dict(rc) => {
            let items: Vec<Object> = rc.borrow().values().cloned().collect();
            crate::object::into_owned(Object::new_list(items))
        }
        _ => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyMapping_Items(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Dict(rc) => {
            let items: Vec<Object> = rc
                .borrow()
                .iter()
                .map(|(k, v)| Object::new_tuple(vec![k.0.clone(), v.clone()]))
                .collect();
            crate::object::into_owned(Object::new_list(items))
        }
        _ => ptr::null_mut(),
    }
}

// ----------------------------------------------------------------
// RFC 0029 — additional `PyObject_*` surface.
// ----------------------------------------------------------------

/// `_PyObject_LookupAttr(obj, name, &result)` — CPython-private
/// helper that distinguishes "attribute missing" (returns 0,
/// `*result = NULL`) from "attribute lookup raised" (returns -1).
/// numpy and pluggy depend on this helper heavily.
#[no_mangle]
pub unsafe extern "C" fn _PyObject_LookupAttr(
    o: *mut PyObject,
    attr: *mut PyObject,
    result: *mut *mut PyObject,
) -> c_int {
    if !result.is_null() {
        unsafe { *result = ptr::null_mut() };
    }
    if o.is_null() || attr.is_null() {
        return -1;
    }
    let key = match unsafe { crate::object::clone_object(attr) } {
        Object::Str(s) => s.to_string(),
        _ => return -1,
    };
    let obj = unsafe { crate::object::clone_object(o) };
    match attr_lookup(&obj, &key) {
        Some(v) => {
            if !result.is_null() {
                unsafe { *result = crate::object::into_owned(v) };
            }
            1
        }
        None => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn _PyObject_LookupAttrId(
    o: *mut PyObject,
    name: *const c_char,
    result: *mut *mut PyObject,
) -> c_int {
    if !result.is_null() {
        unsafe { *result = ptr::null_mut() };
    }
    if o.is_null() || name.is_null() {
        return -1;
    }
    let key = unsafe { CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned();
    let obj = unsafe { crate::object::clone_object(o) };
    match attr_lookup(&obj, &key) {
        Some(v) => {
            if !result.is_null() {
                unsafe { *result = crate::object::into_owned(v) };
            }
            1
        }
        None => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn _PyObject_GenericGetAttrWithDict(
    o: *mut PyObject,
    attr: *mut PyObject,
    _dict: *mut PyObject,
    _suppress: c_int,
) -> *mut PyObject {
    unsafe { PyObject_GetAttr(o, attr) }
}

#[no_mangle]
pub unsafe extern "C" fn _PyObject_GenericSetAttrWithDict(
    o: *mut PyObject,
    attr: *mut PyObject,
    value: *mut PyObject,
    _dict: *mut PyObject,
) -> c_int {
    unsafe { PyObject_SetAttr(o, attr, value) }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_GetAttrId(
    o: *mut PyObject,
    name: *const c_char,
) -> *mut PyObject {
    unsafe { PyObject_GetAttrString(o, name) }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_DelAttr(o: *mut PyObject, attr: *mut PyObject) -> c_int {
    unsafe { PyObject_SetAttr(o, attr, ptr::null_mut()) }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_LengthHint(o: *mut PyObject, default: PySsizeT) -> PySsizeT {
    let n = unsafe { PyObject_Length(o) };
    if n < 0 {
        crate::errors::clear_thread_local();
        return default;
    }
    n
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Bytes(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Bytes(_) => unsafe {
            crate::object::Py_IncRef(o);
            o
        },
        Object::Str(s) => crate::object::into_owned(Object::Bytes(s.as_bytes().into())),
        Object::ByteArray(b) => crate::object::into_owned(Object::Bytes(b.borrow().clone().into())),
        _ => unsafe { crate::strings::PyBytes_FromObject(o) },
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyObject_Format(o: *mut PyObject, spec: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    // The format spec defaults to the empty string when NULL (CPython's
    // `PyObject_Format`). A non-str spec is a `TypeError`, matching CPython.
    let spec_str = if spec.is_null() {
        String::new()
    } else {
        match unsafe { crate::object::clone_object(spec) } {
            Object::Str(s) => s.to_string(),
            other => {
                crate::errors::set_type_error(format!(
                    "format() argument 2 must be str, not {}",
                    other.type_name_owned()
                ));
                return ptr::null_mut();
            }
        }
    };
    let obj = unsafe { crate::object::clone_object(o) };
    // Route through the VM's full `__format__` dispatch (the same logic the
    // `FORMAT_VALUE` bytecode / `format()` builtin run): user `__format__`,
    // built-in subclass native formatting, foreign-scalar `__format__`, and
    // the numeric/`str` format mini-language. Cython lowers every f-string
    // conversion `f'{x:spec}'` to this call, so the spec must be honoured.
    match crate::interp::ensure_active(|| {
        crate::interp::with_interp_mut(|interp| interp.format_public(&obj, &spec_str))
    }) {
        Some(Ok(s)) => return crate::object::into_owned(Object::from_str(s)),
        Some(Err(e)) => {
            crate::errors::set_pending_from_runtime(e);
            return ptr::null_mut();
        }
        // No active interpreter (pure C-side construction before any VM
        // frame): fall back to `str(o)`, the pre-RFC behaviour.
        None => {}
    }
    unsafe { PyObject_Str(o) }
}

// ----------------------------------------------------------------
// RFC 0029 / RFC 0069 WS5 — recursion guards.
// ----------------------------------------------------------------
//
// CPython charges every `Py_EnterRecursiveCall` against
// `tstate->c_recursion_remaining` (initialized to its
// `Py_C_RECURSION_LIMIT` analogue) so unbounded *C-level* recursion —
// numpy's recursive sequence discovery, extension↔bridge dunder
// ping-pong — raises `RecursionError` long before the native stack
// faults. The previous stub always succeeded, so such recursion rode
// the real C stack into SIGBUS (RFC 0066's crash census). The counter
// lives in the byte-faithful thread-state body (mypyc/Cython touch the
// field directly; one budget serves both access styles), and a
// native-stack headroom probe backstops paths whose per-frame C
// footprint is too large for a counted limit to catch in time.

/// Native-stack headroom floor: a guarded C-API entry below this
/// remaining-stack figure raises `RecursionError` instead of letting
/// the next C frame chain fault. Sized like the interpreter's own
/// `stacker::maybe_grow` red zone (512 KiB) — comfortably more than
/// any single extension frame chain observed in the census.
const C_STACK_HEADROOM: usize = 512 * 1024;

/// `true` when the calling thread's native stack still has headroom.
/// Threads whose stack bounds are unknown to the probe report `true`
/// (the counted budget still applies). While a greenlet is current the
/// probe is skipped outright: execution runs on the greenlet's own
/// mmap'd stack, so `stacker`'s figure (measured against the OS
/// thread's stack bounds) is garbage — sqlalchemy's `greenlet_spawn`
/// legs raised spurious `RecursionError`s from exactly this misread.
/// Greenlet stacks are sized for a full recursion limit's worth of
/// frames (see `greenlet_native::stack_size`), and the counted budget
/// still applies.
#[inline]
fn c_stack_headroom_ok() -> bool {
    weavepy_vm::stdlib::greenlet_native::on_greenlet_stack()
        || stacker::remaining_stack().is_none_or(|r| r >= C_STACK_HEADROOM)
}

/// RFC 0069 WS5 — the shared overflow check for structurally recursive
/// abstract entries (`PyObject_GetItem` / `PySequence_GetItem` /
/// `PyObject_GetAttr`): these recurse through extension code without
/// necessarily passing `Py_EnterRecursiveCall`, so the headroom probe
/// is the boundary's own guard. Returns `true` (with a pending
/// `RecursionError`) when the caller must fail the call.
#[inline]
fn c_stack_exhausted(where_: &str) -> bool {
    if c_stack_headroom_ok() {
        return false;
    }
    crate::errors::set_recursion_error(format!("maximum recursion depth exceeded{where_}"));
    true
}

#[no_mangle]
pub unsafe extern "C" fn Py_EnterRecursiveCall(where_: *const c_char) -> c_int {
    let slot = crate::pystate::c_recursion_remaining_slot();
    // SAFETY: the slot is a live, thread-local `i32` for the calling
    // thread (see `pystate::store_ptr`).
    let remaining = unsafe { *slot } - 1;
    unsafe { *slot = remaining };
    if remaining > 0 && c_stack_headroom_ok() {
        return 0;
    }
    // Roll the failed activation back (the caller does not pair a
    // failing enter with `Py_LeaveRecursiveCall`).
    unsafe { *slot = remaining + 1 };
    let suffix = if where_.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(where_) }
            .to_string_lossy()
            .into_owned()
    };
    crate::errors::set_recursion_error(format!("maximum recursion depth exceeded{suffix}"));
    -1
}

#[no_mangle]
pub unsafe extern "C" fn Py_LeaveRecursiveCall() {
    let slot = crate::pystate::c_recursion_remaining_slot();
    // SAFETY: as in `Py_EnterRecursiveCall`.
    unsafe { *slot += 1 };
}

#[no_mangle]
pub unsafe extern "C" fn _Py_CheckRecursionLimit() -> c_int {
    1000
}

// ----------------------------------------------------------------
// RFC 0029 — additional `PyNumber_*` surface.
// ----------------------------------------------------------------

/// `PyNumber_Index(o)` — call `__index__` and return the result
/// (or raise TypeError if the object can't be losslessly turned
/// into an int). Heavily used by numpy for size-arg coercion.
#[no_mangle]
pub unsafe extern "C" fn PyNumber_Index(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        crate::errors::set_type_error("PyNumber_Index: NULL");
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Int(_) | Object::Long(_) | Object::Bool(_) => unsafe {
            crate::object::Py_IncRef(o);
            o
        },
        Object::Float(_) | Object::Complex(_) => {
            crate::errors::set_type_error(
                "__index__ returned non-int (the object cannot be interpreted as an integer)",
            );
            ptr::null_mut()
        }
        other => {
            // RFC 0047 (wave 5): a *foreign* extension scalar (numpy's
            // `np.int32`/`np.intp`) carries `__index__` in its C `nb_index`
            // slot, invisible to `attr_lookup`. CPython's `PyNumber_Index`
            // reads `nb_index` directly; numpy's scalar comparison routes
            // the operand through here (`np.intp(3) != 3` calls
            // `PyNumber_Index` on the scalar), as does any size-arg coercion
            // of a numpy integer. The hermetic wave-4 gate never exercised
            // it because `zeros @ ones` passes only native ints.
            if matches!(other, Object::Foreign(_)) {
                let r = unsafe { foreign_nb_index(o) };
                if !r.is_null() || crate::errors::pending().is_some() {
                    return r;
                }
            }
            // Try `__index__` via the dunder shim.
            let attr = "__index__";
            let dunder = match attr_lookup(&unsafe { crate::object::clone_object(o) }, attr) {
                Some(d) => d,
                None => {
                    crate::errors::set_type_error("object cannot be interpreted as an integer");
                    return ptr::null_mut();
                }
            };
            let dunder_o = crate::object::into_owned(dunder);
            let result = unsafe { PyObject_CallOneArg(dunder_o, o) };
            unsafe { crate::object::Py_DecRef(dunder_o) };
            result
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_AsSsize_t(o: *mut PyObject, _exc: *mut PyObject) -> PySsizeT {
    if o.is_null() {
        crate::errors::set_type_error("PyNumber_AsSsize_t: NULL");
        return -1;
    }
    let idx = unsafe { PyNumber_Index(o) };
    if idx.is_null() {
        return -1;
    }
    let v = unsafe { crate::numbers::PyLong_AsLong(idx) };
    unsafe { crate::object::Py_DecRef(idx) };
    v as PySsizeT
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Divmod(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    let q = unsafe { PyNumber_FloorDivide(a, b) };
    if q.is_null() {
        return ptr::null_mut();
    }
    let r = unsafe { PyNumber_Remainder(a, b) };
    if r.is_null() {
        unsafe { crate::object::Py_DecRef(q) };
        return ptr::null_mut();
    }
    let tuple = crate::object::into_owned(Object::new_tuple(vec![
        unsafe { crate::object::clone_object(q) },
        unsafe { crate::object::clone_object(r) },
    ]));
    unsafe { crate::object::Py_DecRef(q) };
    unsafe { crate::object::Py_DecRef(r) };
    tuple
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_MatrixMultiply(
    a: *mut PyObject,
    b: *mut PyObject,
) -> *mut PyObject {
    // Default: delegate to __matmul__ via the type lookup if
    // available. For now, error out on missing operator.
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    let lhs = unsafe { crate::object::clone_object(a) };
    let m = match attr_lookup(&lhs, "__matmul__") {
        Some(m) => m,
        None => {
            crate::errors::set_type_error("unsupported operand type for @");
            return ptr::null_mut();
        }
    };
    let m_o = crate::object::into_owned(m);
    let result = unsafe { PyObject_CallTwoArgs(m_o, a, b) };
    unsafe { crate::object::Py_DecRef(m_o) };
    result
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Lshift(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    binop(a, b, BinOp::Lshift)
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Rshift(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    binop(a, b, BinOp::Rshift)
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_And(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    binop(a, b, BinOp::And)
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Or(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    binop(a, b, BinOp::Or)
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_Xor(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    binop(a, b, BinOp::Xor)
}

/// `~o` — the bitwise inverse. `~x == -x - 1` at arbitrary precision, so
/// big ints invert faithfully (the prior `!PyLong_AsLong(o)` truncated to
/// 64 bits and overflowed on big ints). Foreign / user types dispatch to
/// `__invert__`.
#[no_mangle]
pub unsafe extern "C" fn PyNumber_Invert(o: *mut PyObject) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::Int(i) => crate::object::into_owned(Object::Int(!i)),
        Object::Bool(b) => crate::object::into_owned(Object::Int(!i64::from(b))),
        Object::Long(big) => {
            let inv = -((*big).clone() + num_bigint::BigInt::from(1));
            crate::object::into_owned(Object::int_from_bigint(inv))
        }
        // Foreign operand → `nb_invert` slot; VM/user object → `__invert__`.
        ref other => unsafe {
            unary_fallback(o, other, UnarySlot::Invert, "bad operand type for unary ~")
        },
    }
}

// In-place variants (`+=`, `-=`, …). CPython's `BINARY_IOP1` first tries the
// left operand's `nb_inplace_<op>` slot; only if it is absent / returns
// `NotImplemented` does it fall back to the immutable binary form. For
// WeavePy's own immutable scalars there is no in-place slot, so the fallback
// is correct — but a *mutable foreign* object (a numpy `ndarray`) has a real
// `nb_inplace_add` that mutates the array *in place* and returns a new
// reference to itself. Delegating straight to `PyNumber_Add` skipped that
// slot and produced a fresh array, so compiled-Cython code relying on
// in-place aliasing silently lost the mutation — e.g. pandas' MultiIndex
// engine `BaseMultiIndexCodesEngine._extract_level_codes` does
// `codes += 1; codes[codes > 0] += 1` on arrays still held in a Python list,
// and without true in-place semantics every `MultiIndex.get_indexer`
// (hence `union`/`intersection`/`difference`, reindex, joins) mis-encoded its
// target and returned all `-1` (187 failures in `indexes/multi/test_setops`).

/// Try the left operand's `nb_inplace_<op>` slot (walking `tp_base` for an
/// un-flattened Cython subtype, as [`number_slot_binop`] does). Returns
/// `Some(result)` when an in-place slot handled it (`result` is NULL with a
/// pending error if the slot raised), or `None` when there is no in-place slot
/// (or it declined with `NotImplemented`) so the caller uses the binary form.
///
/// # Safety
/// `a` and `b` must be live, non-null `PyObject*` with a readable `ob_type`.
unsafe fn number_inplace_slot(
    a: *mut PyObject,
    b: *mut PyObject,
    op: BinOp,
) -> Option<*mut PyObject> {
    type BinaryFunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject;
    type TernaryFunc =
        unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject;

    let slot_of = |nb: *mut crate::layout::PyNumberMethods| -> *mut std::ffi::c_void {
        if nb.is_null() {
            return ptr::null_mut();
        }
        unsafe {
            match op {
                BinOp::Add => (*nb).nb_inplace_add,
                BinOp::Sub => (*nb).nb_inplace_subtract,
                BinOp::Mul => (*nb).nb_inplace_multiply,
                BinOp::TrueDiv => (*nb).nb_inplace_true_divide,
                BinOp::FloorDiv => (*nb).nb_inplace_floor_divide,
                BinOp::Rem => (*nb).nb_inplace_remainder,
                BinOp::Pow => (*nb).nb_inplace_power,
                BinOp::And => (*nb).nb_inplace_and,
                BinOp::Or => (*nb).nb_inplace_or,
                BinOp::Xor => (*nb).nb_inplace_xor,
                BinOp::Lshift => (*nb).nb_inplace_lshift,
                BinOp::Rshift => (*nb).nb_inplace_rshift,
            }
        }
    };

    let mut ty = unsafe { (*a).ob_type } as *mut crate::layout::PyTypeObjectFull;
    let mut slot = ptr::null_mut();
    let mut guard = 0;
    while !ty.is_null() && guard < 100 {
        let s = slot_of(unsafe { (*ty).tp_as_number });
        if !s.is_null() {
            slot = s;
            break;
        }
        ty = unsafe { (*ty).tp_base };
        guard += 1;
    }
    if slot.is_null() {
        return None;
    }

    let r = if matches!(op, BinOp::Pow) {
        // `nb_inplace_power` is a ternaryfunc; pass `None` for the modulus.
        let f: TernaryFunc = unsafe { std::mem::transmute(slot) };
        unsafe { f(a, b, crate::singletons::none_ptr()) }
    } else {
        let f: BinaryFunc = unsafe { std::mem::transmute(slot) };
        unsafe { f(a, b) }
    };
    if r.is_null() {
        // Slot raised — leave the pending exception for the caller.
        return Some(ptr::null_mut());
    }
    if r == crate::singletons::not_implemented_ptr() {
        unsafe { crate::object::Py_DecRef(r) };
        return None;
    }
    Some(r)
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_InPlaceAdd(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    if let Some(r) = unsafe { number_inplace_slot(a, b, BinOp::Add) } {
        return r;
    }
    unsafe { PyNumber_Add(a, b) }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_InPlaceSubtract(
    a: *mut PyObject,
    b: *mut PyObject,
) -> *mut PyObject {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    if let Some(r) = unsafe { number_inplace_slot(a, b, BinOp::Sub) } {
        return r;
    }
    unsafe { PyNumber_Subtract(a, b) }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_InPlaceMultiply(
    a: *mut PyObject,
    b: *mut PyObject,
) -> *mut PyObject {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    if let Some(r) = unsafe { number_inplace_slot(a, b, BinOp::Mul) } {
        return r;
    }
    unsafe { PyNumber_Multiply(a, b) }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_InPlaceTrueDivide(
    a: *mut PyObject,
    b: *mut PyObject,
) -> *mut PyObject {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    if let Some(r) = unsafe { number_inplace_slot(a, b, BinOp::TrueDiv) } {
        return r;
    }
    unsafe { PyNumber_TrueDivide(a, b) }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_InPlaceFloorDivide(
    a: *mut PyObject,
    b: *mut PyObject,
) -> *mut PyObject {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    if let Some(r) = unsafe { number_inplace_slot(a, b, BinOp::FloorDiv) } {
        return r;
    }
    unsafe { PyNumber_FloorDivide(a, b) }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_InPlaceRemainder(
    a: *mut PyObject,
    b: *mut PyObject,
) -> *mut PyObject {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    if let Some(r) = unsafe { number_inplace_slot(a, b, BinOp::Rem) } {
        return r;
    }
    unsafe { PyNumber_Remainder(a, b) }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_InPlacePower(
    a: *mut PyObject,
    b: *mut PyObject,
    c: *mut PyObject,
) -> *mut PyObject {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    // Only the two-argument form (`a **= b`, modulus `None`) can use the
    // in-place slot; a genuine three-argument modulus goes to the binary form.
    if c == crate::singletons::none_ptr() {
        if let Some(r) = unsafe { number_inplace_slot(a, b, BinOp::Pow) } {
            return r;
        }
    }
    unsafe { PyNumber_Power(a, b, c) }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_InPlaceMatrixMultiply(
    a: *mut PyObject,
    b: *mut PyObject,
) -> *mut PyObject {
    // `@=` — read `nb_inplace_matrix_multiply` directly (no `BinOp` variant).
    if !a.is_null() && !b.is_null() {
        let ty = unsafe { (*a).ob_type } as *mut crate::layout::PyTypeObjectFull;
        if !ty.is_null() {
            let nb = unsafe { (*ty).tp_as_number };
            if !nb.is_null() {
                let slot = unsafe { (*nb).nb_inplace_matrix_multiply };
                if !slot.is_null() {
                    type BinaryFunc =
                        unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject;
                    let f: BinaryFunc = unsafe { std::mem::transmute(slot) };
                    let r = unsafe { f(a, b) };
                    if r.is_null() {
                        return ptr::null_mut();
                    }
                    if r != crate::singletons::not_implemented_ptr() {
                        return r;
                    }
                    unsafe { crate::object::Py_DecRef(r) };
                }
            }
        }
    }
    unsafe { PyNumber_MatrixMultiply(a, b) }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_InPlaceLshift(
    a: *mut PyObject,
    b: *mut PyObject,
) -> *mut PyObject {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    if let Some(r) = unsafe { number_inplace_slot(a, b, BinOp::Lshift) } {
        return r;
    }
    unsafe { PyNumber_Lshift(a, b) }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_InPlaceRshift(
    a: *mut PyObject,
    b: *mut PyObject,
) -> *mut PyObject {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    if let Some(r) = unsafe { number_inplace_slot(a, b, BinOp::Rshift) } {
        return r;
    }
    unsafe { PyNumber_Rshift(a, b) }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_InPlaceAnd(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    if let Some(r) = unsafe { number_inplace_slot(a, b, BinOp::And) } {
        return r;
    }
    unsafe { PyNumber_And(a, b) }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_InPlaceOr(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    if let Some(r) = unsafe { number_inplace_slot(a, b, BinOp::Or) } {
        return r;
    }
    unsafe { PyNumber_Or(a, b) }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_InPlaceXor(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    if let Some(r) = unsafe { number_inplace_slot(a, b, BinOp::Xor) } {
        return r;
    }
    unsafe { PyNumber_Xor(a, b) }
}

#[no_mangle]
pub unsafe extern "C" fn PyNumber_ToBase(o: *mut PyObject, base: c_int) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let v = unsafe { crate::numbers::PyLong_AsLong(o) };
    if crate::errors::pending().is_some() {
        return ptr::null_mut();
    }
    let s = match base {
        2 => format!("{:#b}", v),
        8 => format!("{:#o}", v),
        16 => format!("{:#x}", v),
        _ => v.to_string(),
    };
    crate::object::into_owned(Object::from_str(s))
}

// ----------------------------------------------------------------
// RFC 0029 — additional `PySequence_*` surface.
// ----------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PySequence_Concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    if a.is_null() || b.is_null() {
        return ptr::null_mut();
    }
    match (unsafe { crate::object::clone_object(a) }, unsafe {
        crate::object::clone_object(b)
    }) {
        (Object::List(la), Object::List(lb)) => {
            let mut combined = la.borrow().clone();
            combined.extend(lb.borrow().iter().cloned());
            crate::object::into_owned(Object::new_list(combined))
        }
        (Object::Tuple(ia), Object::Tuple(ib)) => {
            let combined: Vec<Object> = ia.iter().cloned().chain(ib.iter().cloned()).collect();
            crate::object::into_owned(Object::new_tuple(combined))
        }
        _ => {
            crate::errors::set_type_error("PySequence_Concat: incompatible types");
            ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_Repeat(o: *mut PyObject, n: PySsizeT) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let count = n.max(0) as usize;
    match unsafe { crate::object::clone_object(o) } {
        Object::List(rc) => {
            let mut out = Vec::with_capacity(rc.borrow().len() * count);
            for _ in 0..count {
                out.extend(rc.borrow().iter().cloned());
            }
            crate::object::into_owned(Object::new_list(out))
        }
        Object::Tuple(items) => {
            let mut out = Vec::with_capacity(items.len() * count);
            for _ in 0..count {
                out.extend(items.iter().cloned());
            }
            crate::object::into_owned(Object::new_tuple(out))
        }
        // `str`/`bytes`/`bytearray` and *foreign* sequences (numpy/pandas
        // objects) carry repetition in their type's `sq_repeat`/`nb_multiply`
        // slot, invisible to the arms above. Route `o * count` through the
        // shared binop bridge exactly as `PyNumber_Multiply` does — the VM's
        // `Mult` protocol implements `str * int`, `bytes * int`, and the
        // foreign slot multiply, the same slot CPython's `PySequence_Repeat`
        // ultimately reaches. pandas' non-ISO datetime parser repeats a
        // `str` here (`"01-01-2013T…+0000"`); the old list/tuple-only match
        // wrongly raised "not a sequence".
        _ => {
            let count_obj = crate::object::into_owned(Object::Int(count as i64));
            let result = binop(o, count_obj, BinOp::Mul);
            unsafe { crate::object::Py_DecRef(count_obj) };
            result
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_InPlaceConcat(
    a: *mut PyObject,
    b: *mut PyObject,
) -> *mut PyObject {
    unsafe { PySequence_Concat(a, b) }
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_InPlaceRepeat(o: *mut PyObject, n: PySsizeT) -> *mut PyObject {
    unsafe { PySequence_Repeat(o, n) }
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_Count(o: *mut PyObject, v: *mut PyObject) -> PySsizeT {
    if o.is_null() || v.is_null() {
        return -1;
    }
    let target = unsafe { crate::object::clone_object(v) };
    match unsafe { crate::object::clone_object(o) } {
        Object::List(rc) => rc.borrow().iter().filter(|x| x.eq_value(&target)).count() as PySsizeT,
        Object::Tuple(items) => items.iter().filter(|x| x.eq_value(&target)).count() as PySsizeT,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_Index(o: *mut PyObject, v: *mut PyObject) -> PySsizeT {
    if o.is_null() || v.is_null() {
        return -1;
    }
    let target = unsafe { crate::object::clone_object(v) };
    match unsafe { crate::object::clone_object(o) } {
        Object::List(rc) => match rc.borrow().iter().position(|x| x.eq_value(&target)) {
            Some(idx) => idx as PySsizeT,
            None => {
                crate::errors::set_value_error("sequence.index(x): x not in sequence");
                -1
            }
        },
        Object::Tuple(items) => match items.iter().position(|x| x.eq_value(&target)) {
            Some(idx) => idx as PySsizeT,
            None => {
                crate::errors::set_value_error("sequence.index(x): x not in sequence");
                -1
            }
        },
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_GetSlice(
    o: *mut PyObject,
    lo: PySsizeT,
    hi: PySsizeT,
) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::List(rc) => {
            let v = rc.borrow();
            let lo = lo.max(0).min(v.len() as PySsizeT) as usize;
            let hi = hi.max(0).min(v.len() as PySsizeT) as usize;
            let lo = lo.min(hi);
            crate::object::into_owned(Object::new_list(v[lo..hi].to_vec()))
        }
        Object::Tuple(items) => {
            let lo = lo.max(0).min(items.len() as PySsizeT) as usize;
            let hi = hi.max(0).min(items.len() as PySsizeT) as usize;
            let lo = lo.min(hi);
            crate::object::into_owned(Object::new_tuple(items[lo..hi].to_vec()))
        }
        Object::Str(s) => {
            let chars: Vec<char> = s.chars().collect();
            let lo = lo.max(0).min(chars.len() as PySsizeT) as usize;
            let hi = hi.max(0).min(chars.len() as PySsizeT) as usize;
            let lo = lo.min(hi);
            let collected: String = chars[lo..hi].iter().collect();
            crate::object::into_owned(Object::from_str(collected))
        }
        obj => {
            // Generic path, like CPython (`mp_subscript` with a slice
            // object): `obj[lo:hi]` through the VM's full subscript
            // dispatch, so bytes/bytearray/memoryview, instance
            // `__getitem__`, and extension slot wrappers all slice.
            // RFC 0075 WS7 — grpcio's cygrpc server poller slices a
            // `bytes` tag through this entry point; the old
            // list/tuple/str-only table killed the serving thread with
            // a TypeError on the first inbound RPC.
            let key = Object::Slice(weavepy_vm::sync::Rc::new(weavepy_vm::object::PySlice {
                start: Object::Int(lo as i64),
                stop: Object::Int(hi as i64),
                step: Object::None,
            }));
            match crate::interp::ensure_active(|| {
                crate::interp::with_interp_mut(|interp| interp.subscr_get_public(&obj, &key))
            }) {
                Some(Ok(v)) => crate::object::into_owned(v),
                Some(Err(e)) => {
                    crate::errors::set_pending_from_runtime(e);
                    ptr::null_mut()
                }
                None => {
                    crate::errors::set_type_error("PySequence_GetSlice: not a sequence");
                    ptr::null_mut()
                }
            }
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_SetSlice(
    o: *mut PyObject,
    lo: PySsizeT,
    hi: PySsizeT,
    v: *mut PyObject,
) -> c_int {
    if o.is_null() {
        return -1;
    }
    let replacement: Vec<Object> = if v.is_null() {
        Vec::new()
    } else {
        match unsafe { crate::object::clone_object(v) } {
            Object::List(rc) => rc.borrow().clone(),
            Object::Tuple(items) => items.iter().cloned().collect(),
            _ => return -1,
        }
    };
    match unsafe { crate::object::clone_object(o) } {
        Object::List(rc) => {
            {
                let mut list = rc.borrow_mut();
                let len = list.len();
                let lo = (lo.max(0) as usize).min(len);
                let hi = (hi.max(0) as usize).min(len);
                let hi = hi.max(lo);
                list.splice(lo..hi, replacement);
            }
            // Keep the faithful mirror's `ob_item` coherent with the spliced
            // prefix `Rc` (see `PyObject_SetItem`); a slice replacement can
            // grow or shrink the list, so `ob_size` is republished too.
            unsafe { crate::mirror::sync_list_ob_item(o) };
            0
        }
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_DelSlice(
    o: *mut PyObject,
    lo: PySsizeT,
    hi: PySsizeT,
) -> c_int {
    unsafe { PySequence_SetSlice(o, lo, hi, ptr::null_mut()) }
}

#[no_mangle]
pub unsafe extern "C" fn PySequence_DelItem(o: *mut PyObject, idx: PySsizeT) -> c_int {
    if o.is_null() {
        return -1;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::List(rc) => {
            let mut list = rc.borrow_mut();
            let len = list.len();
            let i = if idx < 0 {
                (len as PySsizeT + idx) as usize
            } else {
                idx as usize
            };
            if i >= len {
                crate::errors::set_pending(
                    Some(
                        weavepy_vm::builtin_types::builtin_types()
                            .index_error
                            .clone(),
                    ),
                    Object::from_static("list assignment index out of range"),
                );
                return -1;
            }
            list.remove(i);
            drop(list);
            // Keep the faithful mirror's `ob_item` coherent (see
            // `PyObject_SetItem`); a deletion shifts the tail and shrinks
            // `ob_size`.
            unsafe { crate::mirror::sync_list_ob_item(o) };
            0
        }
        _ => -1,
    }
}
