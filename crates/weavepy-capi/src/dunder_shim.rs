//! Bridge layer between C-API type slots and Python-level dunder
//! dispatch.
//!
//! When `PyType_FromSpec` builds a heap type, we have two ways to
//! make a slot reachable:
//!
//! 1. **Direct**: store the slot pointer in the
//!    [`SlotTable`](crate::slottable::SlotTable) so call sites that
//!    bypass the dunder protocol (the buffer protocol, vectorcall,
//!    `PyObject_GenericGetAttr`'s descriptor chain) can dispatch
//!    through it. This is the only access path for a few slots
//!    (`bf_getbuffer`, `tp_traverse`, …).
//!
//! 2. **Dunder shim**: insert a synthesised method into the type's
//!    dict whose body forwards to the C slot. This is how the
//!    operator slots reach the VM dispatcher: when user code does
//!    `obj + other`, the VM looks up `__add__` on `type(obj)`, finds
//!    a `BuiltinFn` we installed at FromSpec time, and calls it. The
//!    BuiltinFn calls the original C `nb_add` slot.
//!
//! [`install_dunder_shims`] performs the second step: given a freshly
//! decoded [`SlotTable`], it returns a vector of `(name, Object)`
//! pairs to insert into the type's dict.

use std::os::raw::c_int;

use weavepy_vm::error::{type_error, RuntimeError};
use weavepy_vm::object::{BuiltinFn, DictKey, Object};
use weavepy_vm::sync::Rc;

use crate::object::PyObject;
use crate::slottable::{self as ids, SlotPtr, SlotTable};

// ----------------------------------------------------------------
// Calling-convention adapters.
// ----------------------------------------------------------------

type UnaryFunc = unsafe extern "C" fn(*mut PyObject) -> *mut PyObject;
type BinaryFunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject;
type TernaryFunc =
    unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject;
type Inquiry = unsafe extern "C" fn(*mut PyObject) -> c_int;
type LenFunc = unsafe extern "C" fn(*mut PyObject) -> isize;
type SsizeArgFunc = unsafe extern "C" fn(*mut PyObject, isize) -> *mut PyObject;
type SsizeObjArgProc = unsafe extern "C" fn(*mut PyObject, isize, *mut PyObject) -> c_int;
type ObjObjProc = unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int;
type ObjObjArgProc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int;
type RichCmpFunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, c_int) -> *mut PyObject;
type ReprFunc = unsafe extern "C" fn(*mut PyObject) -> *mut PyObject;
type GetIterFunc = unsafe extern "C" fn(*mut PyObject) -> *mut PyObject;
type IterNextFunc = unsafe extern "C" fn(*mut PyObject) -> *mut PyObject;
type GetAttroFunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject;
type SetAttroFunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int;
type HashFunc = unsafe extern "C" fn(*mut PyObject) -> isize;
type InitProc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int;
type DescrGetFunc =
    unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject;
type DescrSetFunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int;
type NewFunc = unsafe extern "C" fn(
    *mut crate::types::PyTypeObject,
    *mut PyObject,
    *mut PyObject,
) -> *mut PyObject;

// ----------------------------------------------------------------
// Dunder synthesis.
// ----------------------------------------------------------------

/// Build the `(name, callable)` pairs to install into the type's
/// dict from a populated [`SlotTable`]. Returns an empty vec if the
/// table has no relevant slots.
pub fn install_dunder_shims(table: &SlotTable, type_name: String) -> Vec<(String, Object)> {
    let mut out = Vec::new();
    let name = Rc::<str>::from(type_name.as_str());

    // Numeric protocol → __add__ / __sub__ / __mul__ / __matmul__ / …
    install_binary(&mut out, table, ids::Py_nb_add, "__add__", &name);
    install_binary(&mut out, table, ids::Py_nb_subtract, "__sub__", &name);
    install_binary(&mut out, table, ids::Py_nb_multiply, "__mul__", &name);
    install_binary(&mut out, table, ids::Py_nb_remainder, "__mod__", &name);
    install_binary(&mut out, table, ids::Py_nb_divmod, "__divmod__", &name);
    install_binary(
        &mut out,
        table,
        ids::Py_nb_floor_divide,
        "__floordiv__",
        &name,
    );
    install_binary(
        &mut out,
        table,
        ids::Py_nb_true_divide,
        "__truediv__",
        &name,
    );
    install_binary(&mut out, table, ids::Py_nb_lshift, "__lshift__", &name);
    install_binary(&mut out, table, ids::Py_nb_rshift, "__rshift__", &name);
    install_binary(&mut out, table, ids::Py_nb_and, "__and__", &name);
    install_binary(&mut out, table, ids::Py_nb_xor, "__xor__", &name);
    install_binary(&mut out, table, ids::Py_nb_or, "__or__", &name);
    install_binary(
        &mut out,
        table,
        ids::Py_nb_matrix_multiply,
        "__matmul__",
        &name,
    );

    // Reflected numeric dunders (`__radd__`, `__rmul__`, …). CPython has no
    // dedicated reflected C slot — the *same* `nb_*` slot serves both
    // directions: `binary_op1` tries the right operand's `nb_*` with the
    // operands in their original order when the left operand's slot declines.
    // A numpy scalar/array defines `nb_multiply` but no Python-level
    // `__rmul__`, so `1 * np.float64(2)` — where the VM `int` on the left
    // cannot handle the numpy RHS — must still reach the RHS's `nb_multiply`.
    // The reflected shim forwards to the forward slot with the operands
    // swapped back into original (left, right) order (see
    // [`install_binary_reflected`]).
    install_binary_reflected(&mut out, table, ids::Py_nb_add, "__radd__", &name);
    install_binary_reflected(&mut out, table, ids::Py_nb_subtract, "__rsub__", &name);
    install_binary_reflected(&mut out, table, ids::Py_nb_multiply, "__rmul__", &name);
    install_binary_reflected(&mut out, table, ids::Py_nb_remainder, "__rmod__", &name);
    install_binary_reflected(&mut out, table, ids::Py_nb_divmod, "__rdivmod__", &name);
    install_binary_reflected(
        &mut out,
        table,
        ids::Py_nb_floor_divide,
        "__rfloordiv__",
        &name,
    );
    install_binary_reflected(
        &mut out,
        table,
        ids::Py_nb_true_divide,
        "__rtruediv__",
        &name,
    );
    install_binary_reflected(&mut out, table, ids::Py_nb_lshift, "__rlshift__", &name);
    install_binary_reflected(&mut out, table, ids::Py_nb_rshift, "__rrshift__", &name);
    install_binary_reflected(&mut out, table, ids::Py_nb_and, "__rand__", &name);
    install_binary_reflected(&mut out, table, ids::Py_nb_xor, "__rxor__", &name);
    install_binary_reflected(&mut out, table, ids::Py_nb_or, "__ror__", &name);
    install_binary_reflected(
        &mut out,
        table,
        ids::Py_nb_matrix_multiply,
        "__rmatmul__",
        &name,
    );

    install_binary(&mut out, table, ids::Py_nb_inplace_add, "__iadd__", &name);
    install_binary(
        &mut out,
        table,
        ids::Py_nb_inplace_subtract,
        "__isub__",
        &name,
    );
    install_binary(
        &mut out,
        table,
        ids::Py_nb_inplace_multiply,
        "__imul__",
        &name,
    );
    install_binary(
        &mut out,
        table,
        ids::Py_nb_inplace_remainder,
        "__imod__",
        &name,
    );
    install_binary(
        &mut out,
        table,
        ids::Py_nb_inplace_floor_divide,
        "__ifloordiv__",
        &name,
    );
    install_binary(
        &mut out,
        table,
        ids::Py_nb_inplace_true_divide,
        "__itruediv__",
        &name,
    );
    install_binary(
        &mut out,
        table,
        ids::Py_nb_inplace_lshift,
        "__ilshift__",
        &name,
    );
    install_binary(
        &mut out,
        table,
        ids::Py_nb_inplace_rshift,
        "__irshift__",
        &name,
    );
    install_binary(&mut out, table, ids::Py_nb_inplace_and, "__iand__", &name);
    install_binary(&mut out, table, ids::Py_nb_inplace_xor, "__ixor__", &name);
    install_binary(&mut out, table, ids::Py_nb_inplace_or, "__ior__", &name);
    install_binary(
        &mut out,
        table,
        ids::Py_nb_inplace_matrix_multiply,
        "__imatmul__",
        &name,
    );

    install_unary(&mut out, table, ids::Py_nb_negative, "__neg__", &name);
    install_unary(&mut out, table, ids::Py_nb_positive, "__pos__", &name);
    install_unary(&mut out, table, ids::Py_nb_absolute, "__abs__", &name);
    install_unary(&mut out, table, ids::Py_nb_invert, "__invert__", &name);
    install_unary(&mut out, table, ids::Py_nb_int, "__int__", &name);
    install_unary(&mut out, table, ids::Py_nb_float, "__float__", &name);
    install_unary(&mut out, table, ids::Py_nb_index, "__index__", &name);

    install_inquiry(&mut out, table, ids::Py_nb_bool, "__bool__", &name);

    install_ternary(&mut out, table, ids::Py_nb_power, "__pow__", &name);
    install_ternary(&mut out, table, ids::Py_nb_inplace_power, "__ipow__", &name);

    // Sequence protocol → __len__ / __getitem__ / __setitem__ / …
    install_lenfunc(&mut out, table, ids::Py_sq_length, "__len__", &name);
    install_ssize_arg(&mut out, table, ids::Py_sq_item, "__getitem__", &name);
    install_ssize_obj_arg(&mut out, table, ids::Py_sq_ass_item, "__setitem__", &name);
    install_obj_obj(&mut out, table, ids::Py_sq_contains, "__contains__", &name);
    // `__add__`/`__mul__` (and their in-place forms) are shared between the
    // number and sequence protocols. CPython resolves the dunder to the
    // *number* slot when the type defines one, falling back to the sequence
    // slot only when it does not (`slot_nb_add` precedes `slot_sq_concat` in
    // `slotdefs`). A numpy ndarray defines BOTH `nb_add` (real elementwise
    // add) and `sq_concat` (`array_concat`, which deliberately raises
    // "Concatenation operation is not implemented"); installing the concat
    // shim unconditionally last would clobber the numeric one and make
    // `arr + arr` raise. Only install the sequence shim when the numeric
    // counterpart is absent.
    if table.get(ids::Py_nb_add).is_null() {
        install_binary(&mut out, table, ids::Py_sq_concat, "__add__", &name);
    }
    if table.get(ids::Py_nb_multiply).is_null() {
        install_ssize_arg(&mut out, table, ids::Py_sq_repeat, "__mul__", &name);
    }
    if table.get(ids::Py_nb_inplace_add).is_null() {
        install_binary(
            &mut out,
            table,
            ids::Py_sq_inplace_concat,
            "__iadd__",
            &name,
        );
    }
    if table.get(ids::Py_nb_inplace_multiply).is_null() {
        install_ssize_arg(
            &mut out,
            table,
            ids::Py_sq_inplace_repeat,
            "__imul__",
            &name,
        );
    }

    // Mapping protocol takes precedence over sq_item where both are
    // defined: install the mapping shim last so its dunder
    // overwrites the sq variant in `out`.
    install_lenfunc(&mut out, table, ids::Py_mp_length, "__len__", &name);
    install_binary(&mut out, table, ids::Py_mp_subscript, "__getitem__", &name);
    install_obj_obj_arg(
        &mut out,
        table,
        ids::Py_mp_ass_subscript,
        "__setitem__",
        &name,
    );

    // Type-level protocol.
    install_repr(&mut out, table, ids::Py_tp_repr, "__repr__", &name);
    install_repr(&mut out, table, ids::Py_tp_str, "__str__", &name);
    install_get_iter(&mut out, table, ids::Py_tp_iter, "__iter__", &name);
    install_iter_next(&mut out, table, ids::Py_tp_iternext, "__next__", &name);
    install_richcmp(&mut out, table, ids::Py_tp_richcompare, &name);
    install_call(&mut out, table, ids::Py_tp_call, "__call__", &name);
    install_init(&mut out, table, ids::Py_tp_init, "__init__", &name);
    install_hash(&mut out, table, ids::Py_tp_hash, "__hash__", &name);
    install_getattro(
        &mut out,
        table,
        ids::Py_tp_getattro,
        "__getattribute__",
        &name,
    );
    install_setattro(&mut out, table, ids::Py_tp_setattro, "__setattr__", &name);

    // Descriptor protocol → __get__ / __set__ (RFC 0044, WS3).
    install_descr_get(&mut out, table, ids::Py_tp_descr_get, &name);
    install_descr_set(&mut out, table, ids::Py_tp_descr_set, &name);

    // Async protocol → __await__ / __aiter__ / __anext__ (RFC 0044, WS3).
    install_get_iter(&mut out, table, ids::Py_am_await, "__await__", &name);
    install_get_iter(&mut out, table, ids::Py_am_aiter, "__aiter__", &name);
    install_anext(&mut out, table, ids::Py_am_anext, "__anext__", &name);

    // Construction → __new__ (RFC 0044, WS3).
    install_new(&mut out, table, ids::Py_tp_new, &name);

    out
}

fn install_unary(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    name: &str,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    let mname = name.to_owned();
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let tn = type_name.clone();
    let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: UnaryFunc = unsafe { slot.cast() };
        let self_p = primary_self(args, static_name, &tn)?;
        invoke_unary(func, self_p)
    };
    out.push((
        mname,
        Object::Builtin(Rc::new(BuiltinFn {
            name: static_name,
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        })),
    ));
}

fn install_binary(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    name: &str,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    let mname = name.to_owned();
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let tn = type_name.clone();
    let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: BinaryFunc = unsafe { slot.cast() };
        let (a, b) = binary_args(args, static_name, &tn)?;
        invoke_binary(func, a, b)
    };
    out.push((
        mname,
        Object::Builtin(Rc::new(BuiltinFn {
            name: static_name,
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        })),
    ));
}

/// Reflected sibling of [`install_binary`]: installs `__radd__`-style
/// dunders that forward to a *forward* `nb_*` slot. A reflected dunder
/// `self.__rop__(other)` computes `other OP self`; the C slot computes
/// `left OP right`, so the operands — which arrive bound as `(self, other)`
/// — are swapped back to `(other, self)` before the call. CPython reaches
/// the same slot through `binary_op1` trying the right operand's `nb_*`
/// with the operands in their original order.
fn install_binary_reflected(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    name: &str,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    let mname = name.to_owned();
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let tn = type_name.clone();
    let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: BinaryFunc = unsafe { slot.cast() };
        let (self_p, other_p) = binary_args(args, static_name, &tn)?;
        invoke_binary(func, other_p, self_p)
    };
    out.push((
        mname,
        Object::Builtin(Rc::new(BuiltinFn {
            name: static_name,
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        })),
    ));
}

fn install_ternary(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    name: &str,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    let mname = name.to_owned();
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let tn = type_name.clone();
    let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: TernaryFunc = unsafe { slot.cast() };
        if args.len() < 2 || args.len() > 3 {
            return Err(type_error(format!(
                "{static_name}() takes 2 or 3 args ({} given)",
                args.len()
            )));
        }
        let self_p = crate::object::into_owned(args[0].clone());
        let b_p = crate::object::into_owned(args[1].clone());
        let c_p = if args.len() == 3 {
            crate::object::into_owned(args[2].clone())
        } else {
            unsafe { crate::object::Py_IncRef(crate::singletons::none_ptr()) };
            crate::singletons::none_ptr()
        };
        let raw = crate::interp::ensure_active(|| unsafe { func(self_p, b_p, c_p) });
        unsafe {
            crate::object::Py_DecRef(self_p);
            crate::object::Py_DecRef(b_p);
            crate::object::Py_DecRef(c_p);
        }
        unwrap_pyobject(raw)
    };
    let _ = tn;
    out.push((
        mname,
        Object::Builtin(Rc::new(BuiltinFn {
            name: static_name,
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        })),
    ));
}

fn install_inquiry(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    name: &str,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    let mname = name.to_owned();
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let tn = type_name.clone();
    let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: Inquiry = unsafe { slot.cast() };
        let self_p = primary_self(args, static_name, &tn)?;
        let r = crate::interp::ensure_active(|| unsafe { func(self_p) });
        unsafe { crate::object::Py_DecRef(self_p) };
        if r < 0 {
            return Err(take_pending_or_default());
        }
        Ok(Object::Bool(r != 0))
    };
    out.push((
        mname,
        Object::Builtin(Rc::new(BuiltinFn {
            name: static_name,
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        })),
    ));
}

fn install_lenfunc(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    name: &str,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    let mname = name.to_owned();
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let tn = type_name.clone();
    let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: LenFunc = unsafe { slot.cast() };
        let self_p = primary_self(args, static_name, &tn)?;
        let n = crate::interp::ensure_active(|| unsafe { func(self_p) });
        unsafe { crate::object::Py_DecRef(self_p) };
        if n < 0 {
            return Err(take_pending_or_default());
        }
        Ok(Object::Int(n as i64))
    };
    out.push((
        mname,
        Object::Builtin(Rc::new(BuiltinFn {
            name: static_name,
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        })),
    ));
}

fn install_ssize_arg(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    name: &str,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    let mname = name.to_owned();
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let tn = type_name.clone();
    let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: SsizeArgFunc = unsafe { slot.cast() };
        if args.len() != 2 {
            return Err(type_error(format!(
                "{static_name}() takes 2 args ({} given)",
                args.len()
            )));
        }
        let self_p = crate::object::into_owned(args[0].clone());
        let idx = match &args[1] {
            Object::Int(i) => *i as isize,
            Object::Bool(b) => isize::from(*b),
            _ => {
                unsafe { crate::object::Py_DecRef(self_p) };
                return Err(type_error(format!(
                    "{}.{} requires int index",
                    tn, static_name
                )));
            }
        };
        let raw = crate::interp::ensure_active(|| unsafe { func(self_p, idx) });
        unsafe { crate::object::Py_DecRef(self_p) };
        unwrap_pyobject(raw)
    };
    out.push((
        mname,
        Object::Builtin(Rc::new(BuiltinFn {
            name: static_name,
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        })),
    ));
}

fn install_ssize_obj_arg(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    name: &str,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    let mname = name.to_owned();
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let tn = type_name.clone();
    let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: SsizeObjArgProc = unsafe { slot.cast() };
        if args.len() != 3 {
            return Err(type_error(format!(
                "{static_name}() takes 3 args ({} given)",
                args.len()
            )));
        }
        let self_p = crate::object::into_owned(args[0].clone());
        let idx = match &args[1] {
            Object::Int(i) => *i as isize,
            Object::Bool(b) => isize::from(*b),
            _ => {
                unsafe { crate::object::Py_DecRef(self_p) };
                return Err(type_error(format!(
                    "{}.{} requires int index",
                    tn, static_name
                )));
            }
        };
        let value_p = crate::object::into_owned(args[2].clone());
        let r = crate::interp::ensure_active(|| unsafe { func(self_p, idx, value_p) });
        unsafe {
            crate::object::Py_DecRef(self_p);
            crate::object::Py_DecRef(value_p);
        }
        if r < 0 {
            return Err(take_pending_or_default());
        }
        Ok(Object::None)
    };
    out.push((
        mname,
        Object::Builtin(Rc::new(BuiltinFn {
            name: static_name,
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        })),
    ));
}

fn install_obj_obj(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    name: &str,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    let mname = name.to_owned();
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let tn = type_name.clone();
    let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: ObjObjProc = unsafe { slot.cast() };
        let (a, b) = binary_args(args, static_name, &tn)?;
        let r = crate::interp::ensure_active(|| unsafe { func(a, b) });
        unsafe {
            crate::object::Py_DecRef(a);
            crate::object::Py_DecRef(b);
        }
        if r < 0 {
            return Err(take_pending_or_default());
        }
        Ok(Object::Bool(r != 0))
    };
    out.push((
        mname,
        Object::Builtin(Rc::new(BuiltinFn {
            name: static_name,
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        })),
    ));
}

fn install_obj_obj_arg(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    name: &str,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    let mname = name.to_owned();
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let tn = type_name.clone();
    let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: ObjObjArgProc = unsafe { slot.cast() };
        if args.len() != 3 {
            return Err(type_error(format!(
                "{static_name}() takes 3 args ({} given)",
                args.len()
            )));
        }
        let self_p = crate::object::into_owned(args[0].clone());
        let key_p = crate::object::into_owned(args[1].clone());
        let val_p = crate::object::into_owned(args[2].clone());
        let r = crate::interp::ensure_active(|| unsafe { func(self_p, key_p, val_p) });
        unsafe {
            crate::object::Py_DecRef(self_p);
            crate::object::Py_DecRef(key_p);
            crate::object::Py_DecRef(val_p);
        }
        let _ = tn;
        if r < 0 {
            return Err(take_pending_or_default());
        }
        Ok(Object::None)
    };
    out.push((
        mname,
        Object::Builtin(Rc::new(BuiltinFn {
            name: static_name,
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        })),
    ));
}

fn install_repr(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    name: &str,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    let mname = name.to_owned();
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let tn = type_name.clone();
    let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: ReprFunc = unsafe { slot.cast() };
        let self_p = primary_self(args, static_name, &tn)?;
        let raw = crate::interp::ensure_active(|| unsafe { func(self_p) });
        unsafe { crate::object::Py_DecRef(self_p) };
        unwrap_pyobject(raw)
    };
    out.push((
        mname,
        Object::Builtin(Rc::new(BuiltinFn {
            name: static_name,
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        })),
    ));
}

fn install_get_iter(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    name: &str,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    let mname = name.to_owned();
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let tn = type_name.clone();
    let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: GetIterFunc = unsafe { slot.cast() };
        let self_p = primary_self(args, static_name, &tn)?;
        let raw = crate::interp::ensure_active(|| unsafe { func(self_p) });
        unsafe { crate::object::Py_DecRef(self_p) };
        unwrap_pyobject(raw)
    };
    out.push((
        mname,
        Object::Builtin(Rc::new(BuiltinFn {
            name: static_name,
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        })),
    ));
}

fn install_iter_next(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    name: &str,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    let mname = name.to_owned();
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let tn = type_name.clone();
    let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: IterNextFunc = unsafe { slot.cast() };
        let self_p = primary_self(args, static_name, &tn)?;
        let raw = crate::interp::ensure_active(|| unsafe { func(self_p) });
        unsafe { crate::object::Py_DecRef(self_p) };
        if raw.is_null() {
            // CPython convention: a NULL return without an exception
            // means StopIteration.
            if let Some(p) = crate::errors::take_pending() {
                return Err(crate::errors::to_runtime_error(p));
            }
            return Err(weavepy_vm::error::RuntimeError::PyException(
                weavepy_vm::error::PyException::new(weavepy_vm::builtin_types::make_exception(
                    "StopIteration",
                    String::new(),
                )),
            ));
        }
        let out = unsafe { crate::object::clone_object(raw) };
        unsafe { crate::object::Py_DecRef(raw) };
        Ok(out)
    };
    out.push((
        mname,
        Object::Builtin(Rc::new(BuiltinFn {
            name: static_name,
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        })),
    ));
}

fn install_richcmp(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    for (op, name) in [
        (0, "__lt__"),
        (1, "__le__"),
        (2, "__eq__"),
        (3, "__ne__"),
        (4, "__gt__"),
        (5, "__ge__"),
    ] {
        let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
        let tn = type_name.clone();
        let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
            let func: RichCmpFunc = unsafe { slot.cast() };
            let (a, b) = binary_args(args, static_name, &tn)?;
            let raw = crate::interp::ensure_active(|| unsafe { func(a, b, op) });
            unsafe {
                crate::object::Py_DecRef(a);
                crate::object::Py_DecRef(b);
            }
            unwrap_pyobject(raw)
        };
        out.push((
            name.to_owned(),
            Object::Builtin(Rc::new(BuiltinFn {
                name: static_name,
                binds_instance: true,
                call: Box::new(f),
                call_kw: None,
            })),
        ));
    }
}

fn install_call(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    name: &str,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    let mname = name.to_owned();
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let _ = type_name;
    let f_pos = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: TernaryFunc = unsafe { slot.cast() };
        if args.is_empty() {
            return Err(type_error(format!("{static_name}() requires self")));
        }
        let self_p = crate::object::into_owned(args[0].clone());
        let arg_tuple = crate::object::into_owned(Object::new_tuple(args[1..].to_vec()));
        // No keyword arguments → CPython hands the slot a NULL `kwds`.
        let kw: *mut PyObject = std::ptr::null_mut();
        let raw = crate::interp::ensure_active(|| unsafe { func(self_p, arg_tuple, kw) });
        unsafe {
            crate::object::Py_DecRef(self_p);
            crate::object::Py_DecRef(arg_tuple);
        }
        unwrap_pyobject(raw)
    };
    let f_kw =
        move |args: &[Object], kwargs: &[(String, Object)]| -> Result<Object, RuntimeError> {
            let func: TernaryFunc = unsafe { slot.cast() };
            if args.is_empty() {
                return Err(type_error(format!("{static_name}() requires self")));
            }
            let self_p = crate::object::into_owned(args[0].clone());
            let arg_tuple = crate::object::into_owned(Object::new_tuple(args[1..].to_vec()));
            let kw_p = kwds_ptr(kwargs);
            let raw = crate::interp::ensure_active(|| unsafe { func(self_p, arg_tuple, kw_p) });
            unsafe {
                crate::object::Py_DecRef(self_p);
                crate::object::Py_DecRef(arg_tuple);
                crate::object::Py_DecRef(kw_p);
            }
            unwrap_pyobject(raw)
        };
    out.push((
        mname,
        Object::Builtin(Rc::new(BuiltinFn {
            name: static_name,
            binds_instance: true,
            call: Box::new(f_pos),
            call_kw: Some(Box::new(f_kw)),
        })),
    ));
}

fn install_init(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    name: &str,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    let mname = name.to_owned();
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let _ = type_name;
    let f_pos = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: InitProc = unsafe { slot.cast() };
        if args.is_empty() {
            return Err(type_error(format!("{static_name}() requires self")));
        }
        let self_p = crate::object::into_owned(args[0].clone());
        let arg_tuple = crate::object::into_owned(Object::new_tuple(args[1..].to_vec()));
        // No keyword arguments → CPython hands `tp_init` a NULL `kwds`.
        let kw: *mut PyObject = std::ptr::null_mut();
        let r = crate::interp::ensure_active(|| unsafe { func(self_p, arg_tuple, kw) });
        unsafe {
            crate::object::Py_DecRef(self_p);
            crate::object::Py_DecRef(arg_tuple);
        }
        if r < 0 {
            return Err(take_pending_or_default());
        }
        Ok(Object::None)
    };
    let f_kw =
        move |args: &[Object], kwargs: &[(String, Object)]| -> Result<Object, RuntimeError> {
            let func: InitProc = unsafe { slot.cast() };
            if args.is_empty() {
                return Err(type_error(format!("{static_name}() requires self")));
            }
            let self_p = crate::object::into_owned(args[0].clone());
            let arg_tuple = crate::object::into_owned(Object::new_tuple(args[1..].to_vec()));
            let kw_p = kwds_ptr(kwargs);
            let r = crate::interp::ensure_active(|| unsafe { func(self_p, arg_tuple, kw_p) });
            unsafe {
                crate::object::Py_DecRef(self_p);
                crate::object::Py_DecRef(arg_tuple);
                crate::object::Py_DecRef(kw_p);
            }
            if r < 0 {
                return Err(take_pending_or_default());
            }
            Ok(Object::None)
        };
    out.push((
        mname,
        Object::Builtin(Rc::new(BuiltinFn {
            name: static_name,
            binds_instance: true,
            call: Box::new(f_pos),
            call_kw: Some(Box::new(f_kw)),
        })),
    ));
}

fn install_hash(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    name: &str,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    let mname = name.to_owned();
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let tn = type_name.clone();
    let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: HashFunc = unsafe { slot.cast() };
        let self_p = primary_self(args, static_name, &tn)?;
        let h = crate::interp::ensure_active(|| unsafe { func(self_p) });
        unsafe { crate::object::Py_DecRef(self_p) };
        if h == -1 {
            return Err(take_pending_or_default());
        }
        Ok(Object::Int(h as i64))
    };
    out.push((
        mname,
        Object::Builtin(Rc::new(BuiltinFn {
            name: static_name,
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        })),
    ));
}

fn install_getattro(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    name: &str,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    let mname = name.to_owned();
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let tn = type_name.clone();
    let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: GetAttroFunc = unsafe { slot.cast() };
        if args.len() != 2 {
            return Err(type_error(format!(
                "{static_name}() takes 2 args ({} given)",
                args.len()
            )));
        }
        let self_p = crate::object::into_owned(args[0].clone());
        let name_p = crate::object::into_owned(args[1].clone());
        let raw = crate::interp::ensure_active(|| unsafe { func(self_p, name_p) });
        unsafe {
            crate::object::Py_DecRef(self_p);
            crate::object::Py_DecRef(name_p);
        }
        let _ = tn;
        unwrap_pyobject(raw)
    };
    out.push((
        mname,
        Object::Builtin(Rc::new(BuiltinFn {
            name: static_name,
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        })),
    ));
}

fn install_setattro(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    name: &str,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    let mname = name.to_owned();
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let _ = type_name;
    let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: SetAttroFunc = unsafe { slot.cast() };
        if args.len() != 3 {
            return Err(type_error(format!(
                "{static_name}() takes 3 args ({} given)",
                args.len()
            )));
        }
        let self_p = crate::object::into_owned(args[0].clone());
        let name_p = crate::object::into_owned(args[1].clone());
        let val_p = crate::object::into_owned(args[2].clone());
        let r = crate::interp::ensure_active(|| unsafe { func(self_p, name_p, val_p) });
        unsafe {
            crate::object::Py_DecRef(self_p);
            crate::object::Py_DecRef(name_p);
            crate::object::Py_DecRef(val_p);
        }
        if r < 0 {
            return Err(take_pending_or_default());
        }
        Ok(Object::None)
    };
    out.push((
        mname,
        Object::Builtin(Rc::new(BuiltinFn {
            name: static_name,
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        })),
    ));
}

/// `tp_descr_get` → `__get__(self, obj, type)`. Following CPython's
/// `wrap_descr_get`, a `None` `obj`/`type` is passed to the C slot as
/// `NULL` (extension descriptors test `obj == NULL` to mean "accessed
/// on the class").
fn install_descr_get(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    let tn = type_name.clone();
    let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: DescrGetFunc = unsafe { slot.cast() };
        if args.is_empty() {
            return Err(type_error(format!("{tn}.__get__() requires self")));
        }
        let self_p = crate::object::into_owned(args[0].clone());
        let obj_p = match args.get(1) {
            Some(Object::None) | None => std::ptr::null_mut(),
            Some(o) => crate::object::into_owned(o.clone()),
        };
        let type_p = match args.get(2) {
            Some(Object::None) | None => std::ptr::null_mut(),
            Some(o) => crate::object::into_owned(o.clone()),
        };
        let raw = crate::interp::ensure_active(|| unsafe { func(self_p, obj_p, type_p) });
        unsafe {
            crate::object::Py_DecRef(self_p);
            if !obj_p.is_null() {
                crate::object::Py_DecRef(obj_p);
            }
            if !type_p.is_null() {
                crate::object::Py_DecRef(type_p);
            }
        }
        unwrap_pyobject(raw)
    };
    out.push((
        "__get__".to_owned(),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__get__",
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        })),
    ));
}

/// `tp_descr_set` → `__set__(self, obj, value)`.
fn install_descr_set(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    let tn = type_name.clone();
    let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: DescrSetFunc = unsafe { slot.cast() };
        if args.len() != 3 {
            return Err(type_error(format!(
                "{tn}.__set__() takes 3 args ({} given)",
                args.len()
            )));
        }
        let self_p = crate::object::into_owned(args[0].clone());
        let obj_p = crate::object::into_owned(args[1].clone());
        let val_p = crate::object::into_owned(args[2].clone());
        let r = crate::interp::ensure_active(|| unsafe { func(self_p, obj_p, val_p) });
        unsafe {
            crate::object::Py_DecRef(self_p);
            crate::object::Py_DecRef(obj_p);
            crate::object::Py_DecRef(val_p);
        }
        if r < 0 {
            return Err(take_pending_or_default());
        }
        Ok(Object::None)
    };
    out.push((
        "__set__".to_owned(),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__set__",
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        })),
    ));
}

/// `am_anext` → `__anext__`. Like [`install_iter_next`] but a NULL
/// return without a pending exception raises `StopAsyncIteration`.
fn install_anext(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    name: &str,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    let mname = name.to_owned();
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let tn = type_name.clone();
    let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: IterNextFunc = unsafe { slot.cast() };
        let self_p = primary_self(args, static_name, &tn)?;
        let raw = crate::interp::ensure_active(|| unsafe { func(self_p) });
        unsafe { crate::object::Py_DecRef(self_p) };
        if raw.is_null() {
            if let Some(p) = crate::errors::take_pending() {
                return Err(crate::errors::to_runtime_error(p));
            }
            return Err(weavepy_vm::error::RuntimeError::PyException(
                weavepy_vm::error::PyException::new(weavepy_vm::builtin_types::make_exception(
                    "StopAsyncIteration",
                    String::new(),
                )),
            ));
        }
        let out = unsafe { crate::object::clone_object(raw) };
        unsafe { crate::object::Py_DecRef(raw) };
        Ok(out)
    };
    out.push((
        mname,
        Object::Builtin(Rc::new(BuiltinFn {
            name: static_name,
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        })),
    ));
}

/// `tp_new` → `__new__(cls, *args, **kwargs)`. Installed as a plain
/// `Builtin` (not a `staticmethod`-wrapped sentinel), so the VM's
/// construction path treats it as a user `__new__`: it is looked up on
/// the class and called with `cls` pushed in front of the constructor
/// args. The C `newfunc` receives the `*mut PyTypeObject` for `cls`.
fn install_new(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    type_name: &Rc<str>,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        return;
    }
    fn cls_ptr(
        arg: &Object,
        tn: &Rc<str>,
    ) -> Result<*mut crate::types::PyTypeObject, RuntimeError> {
        match arg {
            Object::Type(t) => Ok(crate::types::install_user_type(t)),
            _ => Err(type_error(format!("{tn}.__new__(X): X is not a type"))),
        }
    }
    let tn = type_name.clone();
    let f_pos = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: NewFunc = unsafe { slot.cast() };
        if args.is_empty() {
            return Err(type_error(
                "__new__() requires the type as its first argument",
            ));
        }
        let type_ptr = cls_ptr(&args[0], &tn)?;
        let arg_tuple = crate::object::into_owned(Object::new_tuple(args[1..].to_vec()));
        // No keyword arguments → CPython hands `tp_new` a NULL `kwds`.
        let kw: *mut PyObject = std::ptr::null_mut();
        let raw = crate::interp::ensure_active(|| unsafe { func(type_ptr, arg_tuple, kw) });
        unsafe {
            crate::object::Py_DecRef(arg_tuple);
        }
        unwrap_pyobject(raw)
    };
    let tn_kw = type_name.clone();
    let f_kw =
        move |args: &[Object], kwargs: &[(String, Object)]| -> Result<Object, RuntimeError> {
            let func: NewFunc = unsafe { slot.cast() };
            if args.is_empty() {
                return Err(type_error(
                    "__new__() requires the type as its first argument",
                ));
            }
            let type_ptr = cls_ptr(&args[0], &tn_kw)?;
            let arg_tuple = crate::object::into_owned(Object::new_tuple(args[1..].to_vec()));
            let kw_p = kwds_ptr(kwargs);
            let raw = crate::interp::ensure_active(|| unsafe { func(type_ptr, arg_tuple, kw_p) });
            unsafe {
                crate::object::Py_DecRef(arg_tuple);
                crate::object::Py_DecRef(kw_p);
            }
            unwrap_pyobject(raw)
        };
    out.push((
        "__new__".to_owned(),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__new__",
            binds_instance: false,
            call: Box::new(f_pos),
            call_kw: Some(Box::new(f_kw)),
        })),
    ));
}

// ----------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------

fn primary_self(
    args: &[Object],
    name: &'static str,
    type_name: &Rc<str>,
) -> Result<*mut PyObject, RuntimeError> {
    if args.is_empty() {
        return Err(type_error(format!(
            "{}.{}() requires self",
            type_name, name
        )));
    }
    Ok(crate::object::into_owned(args[0].clone()))
}

fn binary_args(
    args: &[Object],
    name: &'static str,
    type_name: &Rc<str>,
) -> Result<(*mut PyObject, *mut PyObject), RuntimeError> {
    if args.len() != 2 {
        return Err(type_error(format!(
            "{}.{}() takes 2 args ({} given)",
            type_name,
            name,
            args.len()
        )));
    }
    Ok((
        crate::object::into_owned(args[0].clone()),
        crate::object::into_owned(args[1].clone()),
    ))
}

fn invoke_unary(func: UnaryFunc, self_p: *mut PyObject) -> Result<Object, RuntimeError> {
    let raw = crate::interp::ensure_active(|| unsafe { func(self_p) });
    unsafe { crate::object::Py_DecRef(self_p) };
    unwrap_pyobject(raw)
}

fn invoke_binary(
    func: BinaryFunc,
    a: *mut PyObject,
    b: *mut PyObject,
) -> Result<Object, RuntimeError> {
    let raw = crate::interp::ensure_active(|| unsafe { func(a, b) });
    unsafe {
        crate::object::Py_DecRef(a);
        crate::object::Py_DecRef(b);
    }
    unwrap_pyobject(raw)
}

fn unwrap_pyobject(raw: *mut PyObject) -> Result<Object, RuntimeError> {
    if raw.is_null() {
        return Err(take_pending_or_default());
    }
    let obj = unsafe { crate::object::clone_object(raw) };
    unsafe { crate::object::Py_DecRef(raw) };
    Ok(obj)
}

fn take_pending_or_default() -> RuntimeError {
    if let Some(p) = crate::errors::take_pending() {
        crate::errors::to_runtime_error(p)
    } else {
        weavepy_vm::error::runtime_error(
            "C extension reported failure without setting an exception",
        )
    }
}

fn build_kw_dict(kwargs: &[(String, Object)]) -> weavepy_vm::object::DictData {
    let mut out = weavepy_vm::object::DictData::new();
    for (k, v) in kwargs {
        out.insert(DictKey(Object::from_str(k.clone())), v.clone());
    }
    out
}

/// Build the `kwds` argument for a C `(args, kwds)` slot (`tp_new`,
/// `tp_init`, ternary `METH_KEYWORDS`-style calls).
///
/// Returns **NULL** when there are no keyword arguments, exactly as
/// CPython's `type_call` / `PyObject_Call` hand the callee. Extensions
/// branch on `kwds != NULL` (numpy's `array_converter_new` raises
/// "Array creation helper doesn't support keywords." for any non-NULL,
/// non-empty `kwds`), and reading an *empty* WeavePy dict mirror through
/// the `PyDict_GET_SIZE` macro yields garbage — so a keyword-less call
/// must pass a genuine NULL, not a fresh empty dict.
fn kwds_ptr(kwargs: &[(String, Object)]) -> *mut PyObject {
    if kwargs.is_empty() {
        return std::ptr::null_mut();
    }
    let kw_dict = build_kw_dict(kwargs);
    crate::object::into_owned(Object::Dict(Rc::new(weavepy_vm::sync::RefCell::new(
        kw_dict,
    ))))
}

// Suppress dead-code on the unused SlotPtr re-export helper.
#[allow(dead_code)]
fn _slot_ptr_helper(p: SlotPtr) -> bool {
    p.is_null()
}
