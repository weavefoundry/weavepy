//! `_weave_local` — native attribute hooks for `threading.local`.
//!
//! The shim in `python/_threading_local.py` keeps each `local` object's
//! per-thread namespaces in a dict by thread ident, makes one on a
//! thread's first touch (running the subclass's `__init__`), and drops a
//! dying thread's. Its `__getattribute__`, `__setattr__` and `__delattr__`
//! are the builtins here, which find the calling thread's namespace
//! without running that Python: each thread remembers, for every `local`
//! object it has touched, the namespace the shim gave it. Both ends of
//! that memory are weak, so it keeps neither the object nor what a thread
//! parked in it alive, and a namespace the shim dropped is simply asked
//! for again.

use std::cell::RefCell as StdRefCell;

use crate::sync::{Rc, RefCell, Weak};

use crate::error::{attribute_error, type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule, StrKey};
use crate::types::PyInstance;

/// The hooks' names, in storage of their own: a builtin whose name is one
/// of these very strings is one of the hooks (see [`is_hook`]).
static GETATTRIBUTE: [u8; 16] = *b"__getattribute__";
static SETATTR: [u8; 11] = *b"__setattr__";
static DELATTR: [u8; 11] = *b"__delattr__";

fn hook_name(bytes: &'static [u8]) -> &'static str {
    std::str::from_utf8(bytes).expect("ASCII")
}

/// Whether `b` is `local.__getattribute__` (or, with `store`,
/// `local.__setattr__`): compiled code calls those in place, as builtins
/// that take their operands where they are.
pub(crate) fn is_hook(b: &BuiltinFn, store: bool) -> bool {
    let name: &[u8] = if store { &SETATTR } else { &GETATTRIBUTE };
    std::ptr::eq(b.name.as_ptr(), name.as_ptr())
}

/// One `local` object's namespace on this thread.
struct Space {
    owner: Weak<PyInstance>,
    dict: Weak<RefCell<DictData>>,
}

thread_local! {
    /// The namespaces this thread has been given, the latest used first.
    static SPACES: StdRefCell<Vec<Space>> = const { StdRefCell::new(Vec::new()) };
}

/// Forget the calling thread's namespaces: its Python thread is ending
/// (the shim's `_clear_thread` drops them), and whatever runs on this OS
/// thread next starts from none.
pub(crate) fn forget_thread() {
    let _ = SPACES.try_with(|s| {
        if let Ok(mut s) = s.try_borrow_mut() {
            s.clear();
        }
    });
}

/// The namespace this thread remembers for `inst`, if it is still the
/// shim's.
fn remembered(inst: &Rc<PyInstance>) -> Option<Rc<RefCell<DictData>>> {
    // (A thread that is tearing its storage down remembers nothing: the
    // shim answers, as on a first touch.)
    SPACES
        .try_with(|s| {
            let mut s = s.try_borrow_mut().ok()?;
            let at = s
                .iter()
                .position(|sp| std::ptr::eq(sp.owner.as_ptr(), Rc::as_ptr(inst)))?;
            match s[at].dict.upgrade() {
                Some(d) => {
                    if at != 0 {
                        s.swap(0, at);
                    }
                    Some(d)
                }
                None => {
                    s.swap_remove(at);
                    None
                }
            }
        })
        .ok()
        .flatten()
}

/// The calling thread's namespace for the `local` object `recv`: the one
/// this thread remembers, or the shim's (`_local__namespace`, which makes
/// it on a first touch and runs the subclass's `__init__`).
fn namespace(
    interp: &mut crate::Interpreter,
    recv: &Object,
) -> Result<Rc<RefCell<DictData>>, RuntimeError> {
    let Object::Instance(inst) = recv else {
        return Err(type_error(format!(
            "descriptor for '_thread._local' objects doesn't apply to a '{}' object",
            crate::builtins::class_of(recv).name
        )));
    };
    if let Some(d) = remembered(inst) {
        return Ok(d);
    }
    let make = interp.generic_getattr_public(recv, "_local__namespace")?;
    let g = interp.builtins_dict();
    let Object::Dict(d) = interp.call(&make, &[], &[], &g)? else {
        return Err(type_error("a thread-local namespace must be a dict"));
    };
    let _ = SPACES.try_with(|s| {
        if let Ok(mut s) = s.try_borrow_mut() {
            // (Objects that are gone leave with the next one remembered.)
            s.retain(|sp| sp.owner.strong_count() != 0 && sp.dict.strong_count() != 0);
            s.insert(
                0,
                Space {
                    owner: Rc::downgrade(inst),
                    dict: Rc::downgrade(&d),
                },
            );
        }
    });
    Ok(d)
}

/// [`local_getattribute`] where it runs no Python code: the namespace
/// this thread remembers for the receiver, or its entry for `name`.
/// `None`, having touched nothing, for anything else (a first touch, a
/// name the class answers).
pub(crate) fn getattribute_fast(recv: &Object, name: &Object) -> Option<Object> {
    let (Object::Instance(inst), Object::Str(name)) = (recv, name) else {
        return None;
    };
    let space = remembered(inst)?;
    if &**name == "__dict__" {
        return Some(Object::Dict(space));
    }
    let own = space.try_borrow().ok()?.get(&StrKey(name)).cloned();
    own
}

/// [`local_setattr`] where it runs no Python code: the entry stored in
/// the namespace this thread remembers for the receiver (`true`), or
/// nothing touched.
pub(crate) fn setattr_fast(recv: &Object, name: &Object, value: &Object) -> bool {
    let (Object::Instance(inst), Object::Str(n)) = (recv, name) else {
        return false;
    };
    if &**n == "__dict__" {
        return false;
    }
    let Some(space) = remembered(inst) else {
        return false;
    };
    let Ok(mut d) = space.try_borrow_mut() else {
        return false;
    };
    let old = d.insert(DictKey(name.clone()), value.clone());
    drop(d);
    drop(old);
    true
}

/// The attribute name of a hook's call, as a string.
fn attr_name(name: &Object) -> Result<&str, RuntimeError> {
    match name {
        Object::Str(s) => Ok(s),
        other => Err(type_error(format!(
            "attribute name must be string, not '{}'",
            crate::builtins::class_of(other).name
        ))),
    }
}

fn read_only(recv: &Object) -> RuntimeError {
    attribute_error(format!(
        "'{}' object attribute '__dict__' is read-only",
        crate::builtins::class_of(recv).name
    ))
}

/// `local.__getattribute__(self, name)`: the thread's namespace for
/// `__dict__`, its entry for `name` (which shadows a class attribute of
/// that name, as the C type's instance dict does), or what the default
/// lookup finds on the class.
fn local_getattribute(args: &[Object]) -> Result<Object, RuntimeError> {
    let [recv, name_obj] = args else {
        return Err(type_error("__getattribute__() takes exactly one argument"));
    };
    let name = attr_name(name_obj)?;
    let interp = crate::builtins::reentrant_interp()?;
    let space = namespace(interp, recv)?;
    if name == "__dict__" {
        return Ok(Object::Dict(space));
    }
    let own = space.borrow().get(&StrKey(name)).cloned();
    match own {
        Some(v) => Ok(v),
        None => interp.generic_getattr_public(recv, name),
    }
}

/// `local.__setattr__(self, name, value)`: an entry of the thread's
/// namespace.
fn local_setattr(args: &[Object]) -> Result<Object, RuntimeError> {
    let [recv, name_obj, value] = args else {
        return Err(type_error("__setattr__() takes exactly two arguments"));
    };
    if attr_name(name_obj)? == "__dict__" {
        return Err(read_only(recv));
    }
    let interp = crate::builtins::reentrant_interp()?;
    let space = namespace(interp, recv)?;
    // (The value it replaces is released once the dict is free again.)
    let old = space
        .borrow_mut()
        .insert(DictKey(name_obj.clone()), value.clone());
    drop(old);
    Ok(Object::None)
}

/// `local.__delattr__(self, name)`: the thread's namespace's entry, an
/// `AttributeError` without one.
fn local_delattr(args: &[Object]) -> Result<Object, RuntimeError> {
    let [recv, name_obj] = args else {
        return Err(type_error("__delattr__() takes exactly one argument"));
    };
    let name = attr_name(name_obj)?;
    if name == "__dict__" {
        return Err(read_only(recv));
    }
    let interp = crate::builtins::reentrant_interp()?;
    let space = namespace(interp, recv)?;
    let old = space.borrow_mut().shift_remove(&DictKey(name_obj.clone()));
    match old {
        Some(_) => Ok(Object::None),
        None => Err(attribute_error(format!(
            "'{}' object has no attribute '{name}'",
            crate::builtins::class_of(recv).name
        ))),
    }
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_weave_local"),
        );
        type Hook = fn(&[Object]) -> Result<Object, RuntimeError>;
        let hooks: [(&'static str, &'static str, Hook); 3] = [
            (
                "local_getattribute",
                hook_name(&GETATTRIBUTE),
                local_getattribute,
            ),
            ("local_setattr", hook_name(&SETATTR), local_setattr),
            ("local_delattr", hook_name(&DELATTR), local_delattr),
        ];
        for (key, name, body) in hooks {
            d.insert(
                DictKey(Object::from_static(key)),
                Object::Builtin(Rc::new(BuiltinFn {
                    name,
                    binds_instance: true,
                    call: Box::new(body),
                    call_kw: None,
                })),
            );
        }
    }
    Rc::new(PyModule {
        name: "_weave_local".to_owned(),
        filename: None,
        dict,
    })
}
