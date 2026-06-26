//! The `_bisect` accelerator module — a faithful port of CPython's
//! `Modules/_bisectmodule.c`.
//!
//! The verbatim pure-Python `bisect` (`stdlib/python/bisect_mod.py`) ends with
//! `from _bisect import *`, so `test_bisect`'s `import_fresh_module` C/Py pair
//! exercises this accelerator and the Python fallback side by side. The
//! functions accept `a, x, lo=0, hi=None, *, key=None` (positional or keyword
//! for the first four, keyword-only `key`), drive the search over the
//! `__getitem__` protocol (`PySequence_GetItem`), and — for `insort_*` —
//! insert via `list.insert` (exact lists) or the object's `.insert` method.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::builtins::coerce_index_i64;
use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use weavepy_compiler::CompareKind;

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_bisect"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Bisection algorithms."),
        );
        let mut all: Vec<Object> = Vec::new();
        macro_rules! reg {
            ($name:literal, $f:expr) => {{
                let f = Object::Builtin(Rc::new(BuiltinFn {
                    name: $name,
                    binds_instance: false,
                    call: Box::new(move |a: &[Object]| $f(a, &[])),
                    call_kw: Some(Box::new($f)),
                }));
                crate::descr_registry::register_module(&f, "_bisect");
                d.insert(DictKey(Object::from_static($name)), f);
                all.push(Object::from_static($name));
            }};
        }
        reg!("bisect_right", bisect_right);
        reg!("bisect_left", bisect_left);
        reg!("insort_right", insort_right);
        reg!("insort_left", insort_left);
        // CPython aliases `bisect`/`insort` to the `*_right` forms.
        reg!("bisect", bisect_right);
        reg!("insort", insort_right);
        d.insert(
            DictKey(Object::from_static("__all__")),
            Object::new_list(all),
        );
    }
    Rc::new(PyModule {
        name: "_bisect".to_owned(),
        filename: None,
        dict,
    })
}

fn with_interp<F, R>(f: F) -> Result<R, RuntimeError>
where
    F: FnOnce(&mut crate::Interpreter) -> Result<R, RuntimeError>,
{
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| type_error("_bisect: no active interpreter"))?;
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    f(interp)
}

struct Args {
    a: Object,
    x: Object,
    lo: i64,
    hi: Option<i64>,
    key: Option<Object>,
}

/// Parse `(a, x, lo=0, hi=None, *, key=None)`. The first four are
/// positional-or-keyword; `key` is keyword-only.
fn parse_args(
    name: &str,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Args, RuntimeError> {
    if args.len() > 4 {
        return Err(type_error(format!(
            "{name} takes at most 4 positional arguments ({} given)",
            args.len()
        )));
    }
    let mut a = args.first().cloned();
    let mut x = args.get(1).cloned();
    let mut lo_obj = args.get(2).cloned();
    let mut hi_obj = args.get(3).cloned();
    let mut key: Option<Object> = None;
    for (k, v) in kwargs {
        match k.as_str() {
            "a" => {
                if a.is_some() {
                    return Err(type_error(format!(
                        "argument for {name}() given by name ('a') and position (1)"
                    )));
                }
                a = Some(v.clone());
            }
            "x" => {
                if x.is_some() {
                    return Err(type_error(format!(
                        "argument for {name}() given by name ('x') and position (2)"
                    )));
                }
                x = Some(v.clone());
            }
            "lo" => {
                if lo_obj.is_some() {
                    return Err(type_error(format!(
                        "argument for {name}() given by name ('lo') and position (3)"
                    )));
                }
                lo_obj = Some(v.clone());
            }
            "hi" => {
                if hi_obj.is_some() {
                    return Err(type_error(format!(
                        "argument for {name}() given by name ('hi') and position (4)"
                    )));
                }
                hi_obj = Some(v.clone());
            }
            "key" => key = Some(v.clone()),
            other => {
                return Err(type_error(format!(
                    "{name}() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    let a =
        a.ok_or_else(|| type_error(format!("{name}() missing required argument 'a' (pos 1)")))?;
    let x =
        x.ok_or_else(|| type_error(format!("{name}() missing required argument 'x' (pos 2)")))?;
    let lo = match lo_obj {
        Some(Object::None) | None => 0,
        Some(o) => coerce_index_i64(&o)?,
    };
    let hi = match hi_obj {
        Some(Object::None) | None => None,
        Some(o) => Some(coerce_index_i64(&o)?),
    };
    if let Some(k) = &key {
        if matches!(k, Object::None) {
            key = None;
        }
    }
    Ok(Args { a, x, lo, hi, key })
}

/// Resolve the `hi=None` default to `len(a)` and reject a negative `lo`.
fn resolve_bounds(
    interp: &mut crate::Interpreter,
    args: &Args,
) -> Result<(i64, i64), RuntimeError> {
    if args.lo < 0 {
        return Err(value_error("lo must be non-negative"));
    }
    let hi = match args.hi {
        Some(h) => h,
        None => interp.accel_len(&args.a)?,
    };
    Ok((args.lo, hi))
}

/// `internal_bisect_right`: leftmost index where `item` could be inserted to
/// keep `a` sorted, with everything `<= item` to its left.
fn internal_bisect_right(
    interp: &mut crate::Interpreter,
    a: &Object,
    item: &Object,
    mut lo: i64,
    mut hi: i64,
    key: Option<&Object>,
) -> Result<i64, RuntimeError> {
    while lo < hi {
        // `lo + (hi - lo) / 2` rather than `(lo + hi) / 2`: identical result
        // for `0 <= lo <= hi`, but can't overflow when `hi` is near
        // `sys.maxsize` (CPython's C code uses `(size_t)lo + hi`; see
        // `test_bisect.test_large_range`).
        let mid = lo + (hi - lo) / 2;
        let mut litem = interp.accel_subscript(a, &Object::Int(mid))?;
        if let Some(k) = key {
            litem = interp.call_object(k.clone(), &[litem], &[])?;
        }
        // `item < litem` → search left half.
        if interp.op_compare(item, &litem, CompareKind::Lt)? {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Ok(lo)
}

/// `internal_bisect_left`: leftmost index keeping everything `< item` to its
/// left (so an equal element sorts to the right of the insertion point).
fn internal_bisect_left(
    interp: &mut crate::Interpreter,
    a: &Object,
    item: &Object,
    mut lo: i64,
    mut hi: i64,
    key: Option<&Object>,
) -> Result<i64, RuntimeError> {
    while lo < hi {
        // See `internal_bisect_right`: overflow-safe midpoint for huge `hi`.
        let mid = lo + (hi - lo) / 2;
        let mut litem = interp.accel_subscript(a, &Object::Int(mid))?;
        if let Some(k) = key {
            litem = interp.call_object(k.clone(), &[litem], &[])?;
        }
        // `litem < item` → search right half.
        if interp.op_compare(&litem, item, CompareKind::Lt)? {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    Ok(lo)
}

fn bisect_right(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let parsed = parse_args("bisect_right", args, kwargs)?;
    with_interp(|interp| {
        let (lo, hi) = resolve_bounds(interp, &parsed)?;
        let idx = internal_bisect_right(interp, &parsed.a, &parsed.x, lo, hi, parsed.key.as_ref())?;
        Ok(Object::Int(idx))
    })
}

fn bisect_left(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let parsed = parse_args("bisect_left", args, kwargs)?;
    with_interp(|interp| {
        let (lo, hi) = resolve_bounds(interp, &parsed)?;
        let idx = internal_bisect_left(interp, &parsed.a, &parsed.x, lo, hi, parsed.key.as_ref())?;
        Ok(Object::Int(idx))
    })
}

/// Insert `x` into `a` at `index` — direct for an exact list, else via the
/// object's `.insert(index, x)` method (so `UserList` and friends work).
fn do_insert(
    interp: &mut crate::Interpreter,
    a: &Object,
    index: i64,
    x: Object,
) -> Result<(), RuntimeError> {
    if let Object::List(l) = a {
        let mut v = l.borrow_mut();
        let idx = (index.max(0) as usize).min(v.len());
        v.insert(idx, x);
        return Ok(());
    }
    let insert = interp.load_attr_public(a, "insert")?;
    interp.call_object(insert, &[Object::Int(index), x], &[])?;
    Ok(())
}

fn insort_right(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let parsed = parse_args("insort_right", args, kwargs)?;
    with_interp(|interp| {
        let (lo, hi) = resolve_bounds(interp, &parsed)?;
        // With a key, the search value is `key(x)`; the *original* x is stored.
        let search = match &parsed.key {
            Some(k) => interp.call_object(k.clone(), &[parsed.x.clone()], &[])?,
            None => parsed.x.clone(),
        };
        let idx = internal_bisect_right(interp, &parsed.a, &search, lo, hi, parsed.key.as_ref())?;
        do_insert(interp, &parsed.a, idx, parsed.x.clone())?;
        Ok(Object::None)
    })
}

fn insort_left(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let parsed = parse_args("insort_left", args, kwargs)?;
    with_interp(|interp| {
        let (lo, hi) = resolve_bounds(interp, &parsed)?;
        let search = match &parsed.key {
            Some(k) => interp.call_object(k.clone(), &[parsed.x.clone()], &[])?,
            None => parsed.x.clone(),
        };
        let idx = internal_bisect_left(interp, &parsed.a, &search, lo, hi, parsed.key.as_ref())?;
        do_insert(interp, &parsed.a, idx, parsed.x.clone())?;
        Ok(Object::None)
    })
}
