//! RFC 0060 — native backing for the frozen `_weave_frame_locals`
//! module's PEP 667 `FrameLocalsProxy`.
//!
//! CPython's proxy is a C type reading `f_localsplus` directly. WeavePy
//! implements the mapping surface in Python (`_weave_frame_locals.py`)
//! over these primitives, which read/write the frame's *live* fast
//! locals — the mirror shares the executing frame's storage (RFC 0047),
//! so proxy writes are immediately visible to running code and vice
//! versa.

use crate::sync::{Cell, Rc, RefCell};

use crate::error::RuntimeError;
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, CoroutineKind, DictData, DictKey, Object, PyFrame};

fn frame_arg<'a>(args: &'a [Object], who: &str) -> Result<&'a Rc<PyFrame>, RuntimeError> {
    match args.first() {
        Some(Object::Frame(f)) => Ok(f),
        Some(other) => Err(crate::error::type_error(format!(
            "{who} expected a frame, not {}",
            other.type_name()
        ))),
        None => Err(crate::error::type_error(format!(
            "{who} expected a frame argument"
        ))),
    }
}

fn str_arg<'a>(args: &'a [Object], idx: usize, who: &str) -> Result<&'a str, RuntimeError> {
    match args.get(idx) {
        Some(Object::Str(s)) => Ok(s.as_ref()),
        Some(other) => Err(crate::error::type_error(format!(
            "{who}: name must be str, not {}",
            other.type_name()
        ))),
        None => Err(crate::error::type_error(format!("{who}: missing name"))),
    }
}

/// `check(obj)` — raise `TypeError` unless `obj` is a frame object
/// (the `FrameLocalsProxy(frame)` constructor contract).
fn check(args: &[Object]) -> Result<Object, RuntimeError> {
    match args.first() {
        Some(Object::Frame(_)) => Ok(Object::None),
        Some(other) => Err(crate::error::type_error(format!(
            "argument must be a frame, not {}",
            other.type_name()
        ))),
        None => Err(crate::error::type_error(
            "FrameLocalsProxy expected 1 argument",
        )),
    }
}

/// `is_comp(frame)` — WeavePy lowers PEP 709-inlined comprehensions to
/// their own frames; the proxy treats such a frame's variables as
/// CPython's *hidden* fast locals of the enclosing frame.
fn is_comp(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = frame_arg(args, "is_comp")?;
    Ok(Object::Bool(matches!(
        f.code.name.as_str(),
        "<listcomp>" | "<setcomp>" | "<dictcomp>"
    )))
}

/// `is_module_scope(frame)` — module/class/exec frames are not
/// "optimized" scopes; their `f_locals` is the namespace itself.
fn is_module_scope(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = frame_arg(args, "is_module_scope")?;
    Ok(Object::Bool(f.is_unoptimized_scope()))
}

/// `namespace(frame)` — the live namespace mapping of a non-optimized
/// frame (module globals, class body dict, or an exec locals mapping).
/// The proxy built for such a frame (only when hidden comprehension
/// locals are live, PEP 709) delegates its visible surface here.
fn namespace(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = frame_arg(args, "namespace")?;
    Ok(f.locals())
}

/// `fast_names(frame)` — every user-visible fast-local name, in
/// `co_localsplusnames` order, bound or not.
fn fast_names(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = frame_arg(args, "fast_names")?;
    Ok(Object::new_list(
        f.fast_names().into_iter().map(Object::from_str).collect(),
    ))
}

/// `bound_names(frame)` — the subset of `fast_names` currently bound,
/// in order. The enumeration base for `iter`/`len`/`keys`.
fn bound_names(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = frame_arg(args, "bound_names")?;
    Ok(Object::new_list(
        f.fast_names()
            .into_iter()
            .filter(|n| f.fast_get(n).is_some())
            .map(Object::from_str)
            .collect(),
    ))
}

/// `getvar(frame, name)` — the bound value, or `KeyError` when `name`
/// is not a fast local or is unbound.
fn getvar(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = frame_arg(args, "getvar")?;
    let name = str_arg(args, 1, "getvar")?;
    f.fast_get(name)
        .ok_or_else(|| crate::error::key_error_object(Object::from_str(name)))
}

/// `setvar(frame, name, value)` — write a fast local through to the
/// live frame. `False` when `name` is not a writable fast local (the
/// proxy stores it in the extras dict instead).
fn setvar(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = frame_arg(args, "setvar")?;
    let name = str_arg(args, 1, "setvar")?;
    let value = args
        .get(2)
        .cloned()
        .ok_or_else(|| crate::error::type_error("setvar: missing value"))?;
    Ok(Object::Bool(f.fast_set(name, value)))
}

/// `is_fast(frame, name)` — does `name` name a fast local slot
/// (bound or not)? Drives the `del proxy[fast]` → `ValueError` rule.
fn is_fast(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = frame_arg(args, "is_fast")?;
    let name = str_arg(args, 1, "is_fast")?;
    Ok(Object::Bool(f.has_fast(name)))
}

/// `extra(frame, create)` — the frame's `f_extra_locals` dict, or
/// `None` when it doesn't exist and `create` is false.
fn extra(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = frame_arg(args, "extra")?;
    let create = matches!(args.get(1), Some(Object::Bool(true)));
    Ok(match f.extra_locals_dict(create) {
        Some(d) => Object::Dict(d),
        None => Object::None,
    })
}

/// `generator(frame)` — the generator/coroutine/async-generator that
/// owns this frame, or `None` (CPython `PyFrame_GetGenerator`).
fn generator(args: &[Object]) -> Result<Object, RuntimeError> {
    let f = frame_arg(args, "generator")?;
    let owner = f.gen_owner.borrow().as_ref().and_then(|w| w.upgrade());
    Ok(match owner {
        Some(g) => match g.kind {
            CoroutineKind::Generator => Object::Generator(g),
            CoroutineKind::Coroutine => Object::Coroutine(g),
            CoroutineKind::AsyncGenerator => Object::AsyncGenerator(g),
        },
        None => Object::None,
    })
}

/// `frame_new(code, globals, locals)` — a detached frame object that
/// was never executed (CPython `PyFrame_New`): `f_back` is `None`,
/// all fast locals unbound. `test_frame.test_frame_fback_api`.
fn frame_new(args: &[Object]) -> Result<Object, RuntimeError> {
    let code = match args.first() {
        Some(Object::Code(c)) => c.clone(),
        _ => {
            return Err(crate::error::type_error(
                "frame_new: first argument must be a code object",
            ))
        }
    };
    let globals = match args.get(1) {
        Some(Object::Dict(d)) => d.clone(),
        _ => {
            return Err(crate::error::type_error(
                "frame_new: globals must be a dict",
            ))
        }
    };
    let builtins = Rc::new(RefCell::new(DictData::default()));
    let nlocals = code.varnames.len();
    let ncells = code.cellvars.len() + code.freevars.len();
    let frame = Rc::new(PyFrame {
        code,
        globals,
        builtins,
        lasti: Cell::new(0),
        back: RefCell::new(None),
        locals_cache: RefCell::new(None),
        cells: Rc::new(
            (0..ncells)
                .map(|_| Rc::new(RefCell::new(Object::Unbound)))
                .collect(),
        ),
        class_namespace: None,
        class_namespace_obj: None,
        is_module_scope: false,
        locals_mirror: RefCell::new(Some(Rc::new(RefCell::new(vec![Object::Unbound; nlocals])))),
        trace: RefCell::new(Object::None),
        gen_owner: RefCell::new(None),
        override_lineno: Cell::new(None),
        trace_event: Cell::new(crate::linejump::TraceEvent::None),
        pending_jump: Cell::new(None),
        last_line: Cell::new(None),
        trace_lines: Cell::new(true),
        trace_opcodes: Cell::new(false),
        on_stack: Cell::new(0),
        extra_locals: RefCell::new(None),
        cleared: Cell::new(false),
    });
    Ok(Object::Frame(frame))
}

fn builtin(name: &'static str, f: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(f),
        call_kw: None,
    }))
}

pub fn build(_cache: &ModuleCache) -> Rc<crate::object::PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_weave_frame"),
        );
        for (name, f) in [
            (
                "check",
                check as fn(&[Object]) -> Result<Object, RuntimeError>,
            ),
            ("is_comp", is_comp),
            ("is_module_scope", is_module_scope),
            ("namespace", namespace),
            ("fast_names", fast_names),
            ("bound_names", bound_names),
            ("getvar", getvar),
            ("setvar", setvar),
            ("is_fast", is_fast),
            ("extra", extra),
            ("generator", generator),
            ("frame_new", frame_new),
        ] {
            d.insert(DictKey(Object::from_static(name)), builtin(name, f));
        }
    }
    Rc::new(crate::object::PyModule {
        name: "_weave_frame".to_owned(),
        filename: None,
        dict,
    })
}
