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
pub fn install_dunder_shims(
    table: &SlotTable,
    type_name: String,
    new_null_raises: bool,
) -> Vec<(String, Object)> {
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
    // Reflected power. `nb_power` is the only reflectable numeric slot that is
    // a `ternaryfunc`, so `install_binary_reflected` (above) skips it and a
    // numpy ndarray/scalar ends up with `__pow__` but no `__rpow__`. Without
    // it `float ** ndarray` — pandas' `roperator.rpow` does `right ** left`
    // for masked-array reflected power — finds no reflected handler on the RHS
    // and raises `TypeError`. Synthesize `__rpow__` from the same slot with the
    // operands swapped and a `None` modulus (reflected power never carries one).
    install_ternary_reflected(&mut out, table, ids::Py_nb_power, "__rpow__", &name);

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
    install_delitem(&mut out, table, ids::Py_mp_ass_subscript, &name);

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
    // CPython's `add_operators` derives *both* `__setattr__` and
    // `__delattr__` from `tp_setattro` (deletion calls the slot with a
    // NULL value). Without the delete half, `del root.c1` on lxml's
    // `ObjectifiedElement` (whose Cython `__delattr__` unlinks the child
    // element) fell through to the VM's generic instance-dict delete and
    // raised AttributeError (RFC 0076 WS3).
    install_delattro(&mut out, table, ids::Py_tp_setattro, "__delattr__", &name);

    // Descriptor protocol → __get__ / __set__ (RFC 0044, WS3).
    install_descr_get(&mut out, table, ids::Py_tp_descr_get, &name);
    install_descr_set(&mut out, table, ids::Py_tp_descr_set, &name);

    // Async protocol → __await__ / __aiter__ / __anext__ (RFC 0044, WS3).
    install_get_iter(&mut out, table, ids::Py_am_await, "__await__", &name);
    install_get_iter(&mut out, table, ids::Py_am_aiter, "__aiter__", &name);
    install_anext(&mut out, table, ids::Py_am_anext, "__anext__", &name);

    // Construction → __new__ (RFC 0044, WS3).
    install_new(&mut out, table, ids::Py_tp_new, &name, new_null_raises);

    // PEP 688 → __buffer__ / __release_buffer__ (RFC 0076 WS1). CPython
    // 3.12+ synthesizes these wrappers for every buffer-exporting type
    // (`add_operators`' bufferprocs slotdefs); numpy's thread-safety
    // tests call `arr.__buffer__(inspect.BufferFlags.WRITABLE)` directly.
    install_buffer_dunders(&mut out, table, &name);

    out
}

/// Install `__buffer__(flags)` (and a paired `__release_buffer__`) when
/// the type exports `bf_getbuffer`. The shim crosses `self` into C and
/// builds a memoryview honouring the caller's exact flags via
/// [`crate::memoryview::PyMemoryView_FromObjectAndFlags`] — precisely
/// CPython's `__buffer__` wrapper around the C slot.
fn install_buffer_dunders(out: &mut Vec<(String, Object)>, table: &SlotTable, type_name: &Rc<str>) {
    if table.get(ids::Py_bf_getbuffer).is_null() {
        return;
    }
    let tn = type_name.clone();
    let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
        if args.len() != 2 {
            return Err(type_error(format!(
                "{}.__buffer__() takes 2 args ({} given)",
                tn,
                args.len()
            )));
        }
        let flags: c_int = match &args[1] {
            Object::Int(i) => *i as c_int,
            Object::Bool(b) => c_int::from(*b),
            // `inspect.BufferFlags` is an IntFlag instance — unwrap the
            // native int payload.
            other => match other.native_value() {
                Some(Object::Int(i)) => i as c_int,
                _ => {
                    return Err(type_error(
                        "__buffer__: flags must be an integer".to_owned(),
                    ))
                }
            },
        };
        let self_p = crate::object::into_owned(args[0].clone());
        let raw = crate::interp::ensure_active(|| unsafe {
            crate::memoryview::PyMemoryView_FromObjectAndFlags(self_p, flags)
        });
        unsafe { crate::object::Py_DecRef(self_p) };
        unwrap_pyobject(raw)
    };
    out.push((
        "__buffer__".to_owned(),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__buffer__",
            binds_instance: true,
            call: Box::new(f),
            call_kw: None,
        })),
    ));
    let g = move |args: &[Object]| -> Result<Object, RuntimeError> {
        // `__release_buffer__(self, view)`: release the memoryview taken
        // by `__buffer__` (CPython forwards to `bf_releasebuffer`; our
        // memoryview owns its `Py_buffer`, so releasing the view is the
        // faithful equivalent).
        if args.len() != 2 {
            return Err(type_error("__release_buffer__ takes 2 args".to_owned()));
        }
        if let Object::MemoryView(mv) = &args[1] {
            mv.release();
        }
        Ok(Object::None)
    };
    out.push((
        "__release_buffer__".to_owned(),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__release_buffer__",
            binds_instance: true,
            call: Box::new(g),
            call_kw: None,
        })),
    ));
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

/// Reflected sibling of [`install_ternary`] for `nb_power` → `__rpow__`.
/// `self.__rpow__(other)` computes `other ** self`, i.e. the ternary
/// `nb_power(other, self, None)`. Reflected power never carries a modulus —
/// the 3-arg `pow(base, exp, mod)` form only ever dispatches to the *forward*
/// `__pow__` — so the shim is effectively binary and always passes `None` for
/// the modulus. CPython reaches the same `nb_power` through `binary_op1`
/// trying the right operand's slot (with the operands in original order) when
/// the left operand declines; this shim reproduces that for a numpy
/// ndarray/scalar RHS, which exposes `nb_power` but no Python `__rpow__`.
fn install_ternary_reflected(
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
        // Bound as (self, other); reflected power computes `other ** self`.
        let (self_p, other_p) = binary_args(args, static_name, &tn)?;
        let none = crate::singletons::none_ptr();
        unsafe { crate::object::Py_IncRef(none) };
        let raw = crate::interp::ensure_active(|| unsafe { func(other_p, self_p, none) });
        unsafe {
            crate::object::Py_DecRef(self_p);
            crate::object::Py_DecRef(other_p);
            crate::object::Py_DecRef(none);
        }
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

/// `__delitem__` from `mp_ass_subscript`: CPython's slot wrapper calls the
/// same C function with a NULL value pointer (`del nd[k]` on a
/// `_testbuffer.ndarray` reaches `ndarray_ass_subscript(self, key, NULL)`,
/// which raises its own TypeError — test_buffer.test_ndarray_index_invalid).
fn install_delitem(
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
        let func: ObjObjArgProc = unsafe { slot.cast() };
        if args.len() != 2 {
            return Err(type_error(format!(
                "__delitem__() takes 2 args ({} given)",
                args.len()
            )));
        }
        let self_p = crate::object::into_owned(args[0].clone());
        let key_p = crate::object::into_owned(args[1].clone());
        let r =
            crate::interp::ensure_active(|| unsafe { func(self_p, key_p, std::ptr::null_mut()) });
        unsafe {
            crate::object::Py_DecRef(self_p);
            crate::object::Py_DecRef(key_p);
        }
        let _ = tn;
        if r < 0 {
            return Err(take_pending_or_default());
        }
        Ok(Object::None)
    };
    out.push((
        "__delitem__".to_string(),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__delitem__",
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
            // TEMP diagnostic: catch dangling operands before numpy derefs them.
            if std::env::var_os("WEAVEPY_RCMP_DIAG").is_some() {
                wp_rcmp_diag(&args[0], a, "self");
                wp_rcmp_diag(&args[1], b, "other");
                if (a as usize) <= 0x10000 || (b as usize) <= 0x10000 {
                    eprintln!("[RCMP-BAD] skipping func call to avoid crash");
                    unsafe {
                        crate::object::Py_DecRef(a);
                        crate::object::Py_DecRef(b);
                    }
                    return Ok(weavepy_vm::vm_singletons::not_implemented());
                }
            }
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
    let tname_pos: &'static str = Box::leak(type_name.to_string().into_boxed_str());
    let tname_kw = tname_pos;
    let f_pos = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: TernaryFunc = unsafe { slot.cast() };
        if args.is_empty() {
            return Err(type_error(format!("{static_name}() requires self")));
        }
        let self_p = crate::object::into_owned(args[0].clone());
        // RFC 0047 (wave 5): argument scalars mint through the canonical
        // pin cache — the callee may store a pointer borrowed (pandas'
        // khash keys); see `mirror::ScalarPinKey`.
        let arg_tuple = crate::mirror::args_tuple_out(Object::new_tuple(args[1..].to_vec()));
        // No keyword arguments → CPython hands the slot a NULL `kwds`.
        let kw: *mut PyObject = std::ptr::null_mut();
        let raw = crate::interp::ensure_active(|| unsafe { func(self_p, arg_tuple, kw) });
        unsafe {
            crate::object::Py_DecRef(self_p);
            crate::object::Py_DecRef(arg_tuple);
        }
        if raw.is_null() && std::env::var_os("WEAVEPY_TRACE_NULL").is_some() {
            eprintln!("[WEAVEPY_TRACE_NULL] __call__ NULL from type {tname_pos}");
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
            let arg_tuple = crate::mirror::args_tuple_out(Object::new_tuple(args[1..].to_vec()));
            let kw_p = kwds_ptr(kwargs);
            let raw = crate::interp::ensure_active(|| unsafe { func(self_p, arg_tuple, kw_p) });
            unsafe {
                crate::object::Py_DecRef(self_p);
                crate::object::Py_DecRef(arg_tuple);
                crate::object::Py_DecRef(kw_p);
            }
            if raw.is_null() && std::env::var_os("WEAVEPY_TRACE_NULL").is_some() {
                eprintln!("[WEAVEPY_TRACE_NULL] __call__ NULL from type {tname_kw}");
            }
            unwrap_pyobject(raw)
        };
    let shim = Object::Builtin(Rc::new(BuiltinFn {
        name: static_name,
        binds_instance: true,
        call: Box::new(f_pos),
        call_kw: Some(Box::new(f_kw)),
    }));
    // CPython's slot wrapper for `tp_call` publishes the slotdef clinic
    // string, so `inspect.signature(np.ufunc.__call__)` resolves
    // (numpy test_ufunc_method_signatures — RFC 0075 WS8).
    weavepy_vm::descr_registry::register_text_signature(&shim, "($self, /, *args, **kwargs)");
    out.push((mname, shim));
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
    // A type whose `tp_getattro` is WeavePy's own generic getattr resolves
    // attributes through the default `object.__getattribute__` — which the
    // VM's `load_attr_instance` already performs natively. Installing a
    // `__getattribute__` shim that calls `PyObject_GenericGetAttr` would
    // re-enter the full dispatch (`load_attr_instance` → shim →
    // `PyObject_GenericGetAttr` → `PyObject_GetAttr` → `load_attr_instance`)
    // and recurse until the stack overflows. CPython likewise only wraps a
    // *custom* `tp_getattro`; the generic one is never exposed as a dict
    // `__getattribute__`. Mirrors [`crate::abstract_::foreign_getattr_dispatch`].
    let generic_getattro = crate::genericalloc::PyObject_GenericGetAttr
        as unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject
        as usize;
    if slot.as_void() as usize == generic_getattro {
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
    // Symmetric to `install_getattro`: a type using WeavePy's generic
    // `tp_setattro` uses the default `object.__setattr__`; wrapping it
    // would recurse through `PyObject_GenericSetAttr` → `PyObject_SetAttr`.
    let generic_setattro = crate::genericalloc::PyObject_GenericSetAttr
        as unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int
        as usize;
    if slot.as_void() as usize == generic_setattro {
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

/// `tp_setattro` → `__delattr__(self, name)`: the delete half of the
/// slot (CPython's `wrap_delattr` — the slot is invoked with a NULL
/// value). Installed alongside [`install_setattro`] from the same slot.
fn install_delattro(
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
    // Same guard as `install_setattro`: the generic default must not be
    // wrapped (it would recurse through `PyObject_SetAttr`).
    let generic_setattro = crate::genericalloc::PyObject_GenericSetAttr
        as unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int
        as usize;
    if slot.as_void() as usize == generic_setattro {
        return;
    }
    let mname = name.to_owned();
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let _ = type_name;
    let f = move |args: &[Object]| -> Result<Object, RuntimeError> {
        let func: SetAttroFunc = unsafe { slot.cast() };
        if args.len() != 2 {
            return Err(type_error(format!(
                "{static_name}() takes 2 args ({} given)",
                args.len()
            )));
        }
        let self_p = crate::object::into_owned(args[0].clone());
        let name_p = crate::object::into_owned(args[1].clone());
        let r =
            crate::interp::ensure_active(|| unsafe { func(self_p, name_p, std::ptr::null_mut()) });
        unsafe {
            crate::object::Py_DecRef(self_p);
            crate::object::Py_DecRef(name_p);
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
/// Choose the `tp_new` to invoke for a `cls` resolved inside a `__new__`
/// shim, mirroring CPython's `type_call` (which calls `type->tp_new`).
///
/// A `__new__` shim is installed per bridged base type and captures *that
/// base's* `tp_new` (`captured`). Python-level `__new__` resolution walks
/// the MRO and picks the **first** base that carries a `__new__` — which,
/// for a class that multiply-inherits C extension types, need not be the
/// *solid base* (`best_base`, the base owning the widest inline instance
/// layout). CPython instead inherits `type->tp_new` from the solid base,
/// so its `cdef object` fields get initialised (Cython's `__pyx_tp_new`
/// zeroes them to `None`).
///
/// `install_user_type` / `synth_type_for_class` already bake the solid
/// base's faithful `tp_new` into the resolved `cls`'s own type struct. So
/// when `cls` is an inline-body type whose own `tp_new` is a real
/// (non-generic) slot differing from this shim's captured base slot, that
/// is the solid-base constructor — dispatch to it. This is the fix for
/// pandas' `MultiIndexUIntEngine(BaseMultiIndexCodesEngine, UInt64Engine)`,
/// whose MRO-first base leaves the inherited `IndexEngine` fields NULL.
///
/// In every other case (`cls` is the bridged base itself, or a
/// single-solid-base subclass, or a non-inline subclass) `cls->tp_new`
/// equals `captured` or is the generic allocator, so the captured slot is
/// kept and behaviour is unchanged.
fn effective_new_slot(type_ptr: *mut crate::types::PyTypeObject, captured: SlotPtr) -> SlotPtr {
    if type_ptr.is_null() {
        return captured;
    }
    let own = unsafe { (*type_ptr).tp_new };
    let generic: unsafe extern "C" fn(
        *mut crate::types::PyTypeObject,
        *mut PyObject,
        *mut PyObject,
    ) -> *mut PyObject = crate::genericalloc::PyType_GenericNew;
    let own_addr = own as usize;
    if own_addr != 0
        && own_addr != captured.0 as usize
        && own_addr != generic as usize
        && crate::types::is_inline_instance_type(type_ptr)
    {
        SlotPtr(own)
    } else {
        captured
    }
}

fn install_new(
    out: &mut Vec<(String, Object)>,
    table: &SlotTable,
    slot_id: c_int,
    type_name: &Rc<str>,
    new_null_raises: bool,
) {
    let slot = table.get(slot_id);
    if slot.is_null() {
        // CPython inherits `tp_new` from the base only when the base is
        // *not* `object` (`inherit_special` in `Objects/typeobject.c`), so
        // a readied extension type whose slot is still NULL *and* whose
        // base is `object` is genuinely non-instantiable: `type_call`
        // raises "cannot create ... instances". Falling through to
        // `object.__new__` minted a bare VM instance instead — Pillow's
        // `CmsProfile()` / `CmsTransform()` "does not segfault" typesafety
        // tests expect the TypeError (RFC 0075 WS9, Pillow selftest lane).
        // With a non-`object` base the caller passes `new_null_raises =
        // false` and we install nothing, so the VM's MRO lookup reaches
        // the base's usable `__new__` (pybind11's
        // `pybind11_static_property` extends `property` this way).
        if !new_null_raises {
            return;
        }
        let tn = type_name.clone();
        let msg = move || type_error(format!("cannot create '{tn}' instances"));
        let msg_kw = msg.clone();
        out.push((
            "__new__".to_owned(),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__new__",
                binds_instance: false,
                call: Box::new(move |_args: &[Object]| Err(msg())),
                call_kw: Some(Box::new(
                    move |_args: &[Object], _kwargs: &[(String, Object)]| Err(msg_kw()),
                )),
            })),
        ));
        return;
    }
    fn cls_ptr(
        arg: &Object,
        tn: &Rc<str>,
    ) -> Result<*mut crate::types::PyTypeObject, RuntimeError> {
        match arg {
            // Resolve `cls` to its *canonical* C `PyTypeObject*` — the
            // registered static / heap / readied pointer — exactly as
            // `crate::object::into_owned(Object::Type)` does. A bare
            // `install_user_type` would mint a fresh generic mirror that
            // drops the real type's `tp_basicsize`/inline-storage/`tp_alloc`
            // setup, so a faithful `tp_new` (e.g. the datetime shells') would
            // allocate a plain box and write the subclass's inline fields over
            // the box payload (RFC 0029: pandas `_NaT.__new__(NaTType, …)`).
            Object::Type(t) => Ok(crate::types::type_ptr_for_class(t)
                .or_else(|| crate::types::synth_type_for_class(t))
                .unwrap_or_else(|| crate::types::install_user_type(t))),
            _ => Err(type_error(format!("{tn}.__new__(X): X is not a type"))),
        }
    }
    let tn = type_name.clone();
    let f_pos = move |args: &[Object]| -> Result<Object, RuntimeError> {
        if slot.0.is_null() {
            return Err(type_error(format!(
                "{tn}.__new__: tp_new slot is NULL (unbound base constructor)"
            )));
        }
        if std::env::var_os("WEAVEPY_TRACE_NEW").is_some() {
            let a0 = match args.first() {
                Some(Object::Type(t)) => format!("Type({})", t.name),
                Some(other) => format!("{:?}", std::mem::discriminant(other)),
                None => "none".to_owned(),
            };
            eprintln!("[NEW] type={tn:?} slot={:p} args0={a0}", slot.0);
        }
        if args.is_empty() {
            return Err(type_error(
                "__new__() requires the type as its first argument",
            ));
        }
        let type_ptr = cls_ptr(&args[0], &tn)?;
        // Dispatch to `cls->tp_new` when it is the faithful solid-base slot
        // (mirrors CPython's `type_call`); falls back to this shim's captured
        // base slot otherwise (identical for single-solid-base subclasses).
        let func: NewFunc = unsafe { effective_new_slot(type_ptr, slot).cast() };
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
            if slot.0.is_null() {
                return Err(type_error(format!(
                    "{tn_kw}.__new__: tp_new slot is NULL (unbound base constructor)"
                )));
            }
            if std::env::var_os("WEAVEPY_TRACE_NEW").is_some() {
                eprintln!(
                    "[NEW-KW] type={tn_kw:?} slot={:p} nargs={}",
                    slot.0,
                    args.len()
                );
            }
            if args.is_empty() {
                return Err(type_error(
                    "__new__() requires the type as its first argument",
                ));
            }
            let type_ptr = cls_ptr(&args[0], &tn_kw)?;
            let func: NewFunc = unsafe { effective_new_slot(type_ptr, slot).cast() };
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

/// TEMP diagnostic: describe an operand and its marshalled pointer,
/// flagging dangling/near-null pointers before a C slot dereferences them.
fn wp_rcmp_diag(obj: &Object, p: *mut PyObject, role: &str) {
    let kind = match obj {
        Object::Foreign(s) => format!("Foreign({}) ptr=0x{:x}", s.type_name, s.ptr),
        Object::Instance(i) => {
            format!(
                "Instance(cls={} c_body=0x{:x})",
                i.cls().name,
                i.c_body.get()
            )
        }
        Object::None => "None".to_string(),
        Object::Int(_) => "Int".to_string(),
        Object::Str(_) => "Str".to_string(),
        Object::List(_) => "List".to_string(),
        Object::Tuple(_) => "Tuple".to_string(),
        _ => "other".to_string(),
    };
    let pv = p as usize;
    if pv <= 0x10000 {
        eprintln!("[RCMP-BAD] {role} obj={kind} p=0x{pv:x} <near-null ptr>");
        return;
    }
    let rc = unsafe { (*p).ob_refcnt } as i64;
    if rc > 0 && rc < (1i64 << 40) {
        return; // looks healthy
    }
    let ty = unsafe { (*p).ob_type };
    let tyname = if (ty as usize) > 0x10000 {
        let np = unsafe { (*(ty as *mut crate::layout::PyTypeObjectFull)).tp_name };
        if np.is_null() {
            "<null>".to_string()
        } else {
            unsafe { std::ffi::CStr::from_ptr(np) }
                .to_string_lossy()
                .into_owned()
        }
    } else {
        format!("<bad ty 0x{:x}>", ty as usize)
    };
    eprintln!("[RCMP-BAD] {role} obj={kind} p=0x{pv:x} refcnt={rc} ty={tyname}");
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
        if std::env::var_os("WEAVEPY_TRACE_NULL").is_some() {
            eprintln!(
                "[WEAVEPY_TRACE_NULL] C slot returned NULL with no exception set\n{}",
                std::backtrace::Backtrace::force_capture()
            );
        }
        weavepy_vm::error::runtime_error(
            "C extension reported failure without setting an exception",
        )
    }
}

fn build_kw_dict(kwargs: &[(String, Object)]) -> weavepy_vm::object::DictData {
    let mut out = weavepy_vm::object::DictData::default();
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
