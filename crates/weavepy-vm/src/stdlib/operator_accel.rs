//! The `_operator` accelerator module.
//!
//! WeavePy ships a pure-Python `operator` (`stdlib/python/operator_mod.py`)
//! whose function bodies are written in Python, guarded by
//! `from _operator import *`. CPython's real `operator.py` does the same and
//! relies on `_operator` being a **C** module: its functions are
//! `builtin_function_or_method`s, which (unlike a Python `def`) are *not*
//! descriptors and so do **not** bind `self` when stored as a class
//! attribute. The stdlib leans on this — e.g.
//! `glob._StringGlobber.concat_path = operator.add` then calls
//! `self.concat_path(path, text)` expecting `operator.add(path, text)`, not
//! `add(self, path, text)`. If `operator.add` were a plain Python function it
//! would bind `self` and raise "add() takes 2 positional arguments but 3 were
//! given".
//!
//! So we expose the operator surface here as non-binding native builtins that
//! delegate to the interpreter's own operation machinery (identical to what
//! the `BINARY_OP`/`COMPARE_OP`/`UNARY_OP` bytecodes do, dunders and all).
//! `_compare_digest` also lives here (constant-time compare for `hmac`).

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use weavepy_compiler::{BinOpKind, CompareKind, UnaryKind};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_operator"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Operator interface."),
        );

        // Names exported, in declaration order, so `from _operator import *`
        // is deterministic (CPython's C module relies on dir() minus
        // underscores; we publish an explicit `__all__`).
        let mut names: Vec<Object> = Vec::new();

        macro_rules! reg {
            ($name:literal, $f:expr) => {{
                let f = Object::Builtin(Rc::new(BuiltinFn {
                    name: $name,
                    binds_instance: false,
                    call: Box::new($f),
                    call_kw: None,
                }));
                // Attribute `__module__ == "_operator"` (CPython's C module)
                // so `pickle` can round-trip `operator.<op>` — it resolves
                // `getattr(_operator, qualname) is obj`. Without this the
                // builtin would default to `"builtins"` and pickle would find
                // the unrelated builtin of the same name (e.g. `pow`).
                crate::descr_registry::register_module(&f, "_operator");
                d.insert(DictKey(Object::from_static($name)), f);
                names.push(Object::from_static($name));
            }};
        }

        // -- binary arithmetic / bitwise --------------------------------
        reg!("add", |a| binary(a, BinOpKind::Add, "add"));
        reg!("sub", |a| binary(a, BinOpKind::Sub, "sub"));
        reg!("mul", |a| binary(a, BinOpKind::Mult, "mul"));
        reg!("truediv", |a| binary(a, BinOpKind::Div, "truediv"));
        reg!("floordiv", |a| binary(a, BinOpKind::FloorDiv, "floordiv"));
        reg!("mod", |a| binary(a, BinOpKind::Mod, "mod"));
        reg!("pow", |a| binary(a, BinOpKind::Pow, "pow"));
        reg!("lshift", |a| binary(a, BinOpKind::LShift, "lshift"));
        reg!("rshift", |a| binary(a, BinOpKind::RShift, "rshift"));
        reg!("and_", |a| binary(a, BinOpKind::BitAnd, "and_"));
        reg!("or_", |a| binary(a, BinOpKind::BitOr, "or_"));
        reg!("xor", |a| binary(a, BinOpKind::BitXor, "xor"));
        reg!("matmul", |a| binary(a, BinOpKind::MatMult, "matmul"));

        // -- in-place variants ------------------------------------------
        reg!("iadd", |a| inplace(a, BinOpKind::Add, "iadd"));
        reg!("isub", |a| inplace(a, BinOpKind::Sub, "isub"));
        reg!("imul", |a| inplace(a, BinOpKind::Mult, "imul"));
        reg!("itruediv", |a| inplace(a, BinOpKind::Div, "itruediv"));
        reg!("ifloordiv", |a| inplace(
            a,
            BinOpKind::FloorDiv,
            "ifloordiv"
        ));
        reg!("imod", |a| inplace(a, BinOpKind::Mod, "imod"));
        reg!("ipow", |a| inplace(a, BinOpKind::Pow, "ipow"));
        reg!("ilshift", |a| inplace(a, BinOpKind::LShift, "ilshift"));
        reg!("irshift", |a| inplace(a, BinOpKind::RShift, "irshift"));
        reg!("iand", |a| inplace(a, BinOpKind::BitAnd, "iand"));
        reg!("ior", |a| inplace(a, BinOpKind::BitOr, "ior"));
        reg!("ixor", |a| inplace(a, BinOpKind::BitXor, "ixor"));
        reg!("imatmul", |a| inplace(a, BinOpKind::MatMult, "imatmul"));

        // -- rich comparisons -------------------------------------------
        reg!("lt", |a| compare(a, CompareKind::Lt, "lt"));
        reg!("le", |a| compare(a, CompareKind::LtE, "le"));
        reg!("eq", |a| compare(a, CompareKind::Eq, "eq"));
        reg!("ne", |a| compare(a, CompareKind::NotEq, "ne"));
        reg!("gt", |a| compare(a, CompareKind::Gt, "gt"));
        reg!("ge", |a| compare(a, CompareKind::GtE, "ge"));

        // -- unary ------------------------------------------------------
        reg!("neg", |a| unary(a, UnaryKind::Neg, "neg"));
        reg!("pos", |a| unary(a, UnaryKind::Pos, "pos"));
        reg!("invert", |a| unary(a, UnaryKind::Invert, "invert"));
        reg!("inv", |a| unary(a, UnaryKind::Invert, "inv"));
        reg!("not_", op_not);
        reg!("truth", op_truth);
        reg!("is_", op_is);
        reg!("is_not", op_is_not);

        d.insert(
            DictKey(Object::from_static("__all__")),
            Object::new_list(names),
        );

        let compare_digest_fn = Object::Builtin(Rc::new(BuiltinFn {
            name: "_compare_digest",
            binds_instance: false,
            call: Box::new(compare_digest),
            call_kw: None,
        }));
        crate::descr_registry::register_module(&compare_digest_fn, "_operator");
        d.insert(
            DictKey(Object::from_static("_compare_digest")),
            compare_digest_fn,
        );
    }
    Rc::new(PyModule {
        name: "_operator".to_owned(),
        filename: None,
        dict,
    })
}

/// Borrow the active interpreter published on this thread by the dispatch
/// loop. Always present while a builtin runs.
fn with_interp<F, R>(f: F) -> Result<R, RuntimeError>
where
    F: FnOnce(&mut crate::Interpreter) -> Result<R, RuntimeError>,
{
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| type_error("_operator: no active interpreter"))?;
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    f(interp)
}

fn two_args<'a>(args: &'a [Object], name: &str) -> Result<(&'a Object, &'a Object), RuntimeError> {
    match (args.first(), args.get(1)) {
        (Some(a), Some(b)) if args.len() == 2 => Ok((a, b)),
        _ => Err(type_error(format!(
            "{name} expected 2 arguments, got {}",
            args.len()
        ))),
    }
}

fn one_arg<'a>(args: &'a [Object], name: &str) -> Result<&'a Object, RuntimeError> {
    match args.first() {
        Some(a) if args.len() == 1 => Ok(a),
        _ => Err(type_error(format!(
            "{name} expected 1 argument, got {}",
            args.len()
        ))),
    }
}

fn binary(args: &[Object], op: BinOpKind, name: &str) -> Result<Object, RuntimeError> {
    let (a, b) = two_args(args, name)?;
    with_interp(|interp| interp.op_binary(a, b, op))
}

fn inplace(args: &[Object], op: BinOpKind, name: &str) -> Result<Object, RuntimeError> {
    let (a, b) = two_args(args, name)?;
    with_interp(|interp| interp.op_inplace(a, b, op))
}

fn compare(args: &[Object], op: CompareKind, name: &str) -> Result<Object, RuntimeError> {
    let (a, b) = two_args(args, name)?;
    // `operator.gt(a, b)` is the *expression* `a > b`, not `bool(a > b)`: it
    // must return the raw rich-comparison result, so a foreign object (a numpy
    // `ndarray`) yields its element-wise bool array rather than a coerced
    // scalar. pandas' `comparison_op` dispatches `Series > x` through
    // `operator.gt`; a scalar result there makes pandas treat the whole
    // comparison as invalid (`invalid_comparison`) and boolean masks break.
    with_interp(|interp| interp.rich_compare_public(a, b, op))
}

fn unary(args: &[Object], op: UnaryKind, name: &str) -> Result<Object, RuntimeError> {
    let a = one_arg(args, name)?;
    with_interp(|interp| interp.op_unary(a, op))
}

fn op_not(args: &[Object]) -> Result<Object, RuntimeError> {
    let a = one_arg(args, "not_")?;
    Ok(Object::Bool(!with_interp(|interp| interp.op_truth(a))?))
}

fn op_truth(args: &[Object]) -> Result<Object, RuntimeError> {
    let a = one_arg(args, "truth")?;
    Ok(Object::Bool(with_interp(|interp| interp.op_truth(a))?))
}

fn op_is(args: &[Object]) -> Result<Object, RuntimeError> {
    let (a, b) = two_args(args, "is_")?;
    Ok(Object::Bool(a.is_same(b)))
}

fn op_is_not(args: &[Object]) -> Result<Object, RuntimeError> {
    let (a, b) = two_args(args, "is_not")?;
    Ok(Object::Bool(!a.is_same(b)))
}

/// Constant-time comparison, ported from CPython's `_tscmp`. Returns `true`
/// when the inputs are equal. Branch-free over the byte contents so the
/// running time leaks only the (already public) length.
fn tscmp(a: &[u8], b: &[u8]) -> bool {
    // On a length mismatch CPython compares `b` against itself (so the loop
    // body always XORs to zero) but seeds `result` with 1 → never equal.
    let (left, len, mut result) = if a.len() != b.len() {
        (b, b.len(), 1u8)
    } else {
        (a, a.len(), 0u8)
    };
    for i in 0..len {
        result |= left[i] ^ b[i];
    }
    result == 0
}

/// `_operator._compare_digest(a, b)` — constant-time equality for two
/// ASCII `str`s or two bytes-like objects (mixing the two, or passing
/// anything else, raises `TypeError`), matching `Modules/_operator.c`.
fn compare_digest(args: &[Object]) -> Result<Object, RuntimeError> {
    let a = args
        .first()
        .ok_or_else(|| type_error("_compare_digest expected 2 arguments, got 0"))?;
    let b = args
        .get(1)
        .ok_or_else(|| type_error("_compare_digest expected 2 arguments, got 1"))?;
    // `str`/`bytes` subclasses (which may override `__eq__`) compare by their
    // underlying value, so unwrap to the native primitive first.
    let an = a.native_value();
    let bn = b.native_value();
    let a = an.as_ref().unwrap_or(a);
    let b = bn.as_ref().unwrap_or(b);
    let equal = match (a, b) {
        (Object::Str(sa), Object::Str(sb)) => {
            if !sa.is_ascii() || !sb.is_ascii() {
                return Err(type_error(
                    "comparing strings with non-ASCII characters is not supported",
                ));
            }
            tscmp(sa.as_bytes(), sb.as_bytes())
        }
        _ => {
            // Reject a str paired with a non-str (and any non-buffer type)
            // before touching the buffer protocol, exactly as CPython does.
            if matches!(a, Object::Str(_)) || matches!(b, Object::Str(_)) {
                return Err(compare_type_error(a, b));
            }
            match (a.as_bytes_view(), b.as_bytes_view()) {
                (Some(ba), Some(bb)) => tscmp(&ba, &bb),
                _ => return Err(compare_type_error(a, b)),
            }
        }
    };
    Ok(Object::Bool(equal))
}

fn compare_type_error(a: &Object, b: &Object) -> RuntimeError {
    type_error(format!(
        "unsupported operand types(s) or combination of types: '{}' and '{}'",
        a.type_name_owned(),
        b.type_name_owned()
    ))
}
