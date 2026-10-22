//! The `_functools` built-in module — Rust core for `functools`.
//!
//! CPython implements `functools.partial` in C, so calling a partial
//! pushes no Python frame and leaves no traceback entry. The frozen
//! Python `partial` class delegates `__call__` here to match that:
//! `test_traceback` asserts that a `partial(exec, …)` call site shows
//! only the caller's frame.

mod lru_state;

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_functools"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Tools that operate on functions — native core."),
        );
        d.insert(
            DictKey(Object::from_static("_partial_call")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: PARTIAL_CALL_NAME,
                binds_instance: false,
                call: Box::new(|args| partial_call(args, &[])),
                call_kw: Some(Box::new(partial_call)),
            })),
        );
        d.insert(
            DictKey(Object::from_static("_partial_new_for")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "_partial_new_for",
                binds_instance: false,
                call: Box::new(partial_new_for),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("cmp_to_key")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "cmp_to_key",
                binds_instance: false,
                call: Box::new(|args| cmp_to_key(args, &[])),
                call_kw: Some(Box::new(cmp_to_key)),
            })),
        );
        d.insert(
            DictKey(Object::from_static("reduce")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "reduce",
                binds_instance: false,
                call: Box::new(|args| reduce(args, &[])),
                call_kw: Some(Box::new(reduce)),
            })),
        );
        // The *class* itself, as in CPython's C module — torch._dynamo
        // classifies wrapped callables with
        // `isinstance(value, functools._lru_cache_wrapper)` (RFC 0076
        // WS5), so a bare constructor function is not enough. The class
        // carries a `__new__` shim that runs the native constructor.
        d.insert(
            DictKey(Object::from_static("_lru_cache_wrapper")),
            Object::Type(lru_type()),
        );
        d.insert(
            DictKey(Object::from_static("_lru_private")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "_lru_private",
                binds_instance: false,
                call: Box::new(lru_private),
                call_kw: None,
            })),
        );
    }
    Rc::new(PyModule {
        name: "_functools".to_owned(),
        filename: None,
        dict,
    })
}

/// What a `partial` class's native `__new__` knows about it (see
/// [`partial_new_for`]): the class, the `Placeholder` singleton, and the
/// pure-Python `__new__` that serves every shape [`partial_new`] doesn't.
struct PartialReg {
    cls: crate::sync::Weak<crate::types::TypeObject>,
    placeholder: Object,
    py_new: Object,
}

/// `_partial_new_for(partial, Placeholder, _partial_new)`: the native
/// `__new__` for this `functools` module's `partial` class (a fresh
/// import of `functools` makes its own).
fn partial_new_for(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Type(cls), placeholder, py_new] = args else {
        return Err(type_error(
            "_partial_new_for expected (partial, Placeholder, __new__)",
        ));
    };
    let reg = Rc::new(PartialReg {
        cls: Rc::downgrade(cls),
        placeholder: placeholder.clone(),
        py_new: py_new.clone(),
    });
    let reg_kw = reg.clone();
    Ok(Object::Builtin(Rc::new(BuiltinFn {
        name: "__new__",
        binds_instance: false,
        call: Box::new(move |args| partial_new(&reg, args, &[])),
        call_kw: Some(Box::new(move |args, kwargs| {
            partial_new(&reg_kw, args, kwargs)
        })),
    })))
}

/// The native `partial.__call__`'s name (the dispatch loop recognizes
/// it; the visible name is `__call__`).
pub(crate) const PARTIAL_CALL_NAME: &str = ".partial.__call__";

/// The call a `partial` instance stands for, as the dispatch loop can make
/// it directly: its function, and the one stored positional argument to
/// pass first (`Unbound` for none). `None` when the partial has keywords,
/// placeholders, or more than one stored argument.
pub(crate) fn partial_inline(inst: &crate::types::PyInstance) -> Option<(Object, Object)> {
    with_partial_fields(inst, |[func, args, kw, phcount]| {
        if !matches!(phcount, Object::Int(0)) {
            return None;
        }
        let Object::Dict(kw) = kw else {
            return None;
        };
        // SAFETY: a read that runs no code.
        if !unsafe { kw.peek() }?.is_empty() {
            return None;
        }
        let Object::Tuple(args) = args else {
            return None;
        };
        let first = match &args[..] {
            [] => Object::Unbound,
            [a] => a.clone(),
            _ => return None,
        };
        Some((func.clone(), first))
    })
    .flatten()
}

thread_local! {
    /// The interned names of a partial's first four slots.
    static PARTIAL_KEYS: [crate::shared_value::SharedStr; 4] = PARTIAL_SLOTS[..4]
        .iter()
        .map(|name| match crate::stdlib::sys::intern_name(name) {
            Object::Str(s) => s,
            _ => unreachable!("interned names are strs"),
        })
        .collect::<Vec<_>>()
        .try_into()
        .unwrap_or_else(|_| unreachable!("four names"));
}

/// `f` over a partial instance's `_func`, `_args`, `_keywords`, and
/// `_phcount` slots, read in place when they lead its slot store (as
/// both constructors lay them out): `None` otherwise.
fn with_partial_fields<R>(
    inst: &crate::types::PyInstance,
    f: impl FnOnce([&Object; 4]) -> R,
) -> Option<R> {
    PARTIAL_KEYS.with(|keys| {
        // SAFETY: a read; `f` runs no code that could borrow the slots.
        let slots = unsafe { inst.slots.peek() }?;
        let fields = slots.leading_interned([&keys[0], &keys[1], &keys[2], &keys[3]])?;
        Some(f(fields))
    })
}

/// The slots of a `partial` instance, in the order [`partial_new`]
/// stores them.
const PARTIAL_SLOTS: [&str; 5] = ["_func", "_args", "_keywords", "_phcount", "_merger"];

/// `partial.__new__(cls, func, /, *args, **keywords)` for the common
/// shape: a callable that isn't itself a partial, and no `Placeholder`.
/// Everything else (merging a partial's arguments, placeholders, and
/// every error) runs the pure-Python `__new__`.
fn partial_new(
    reg: &PartialReg,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let interp = crate::builtins::reentrant_interp()?;
    let (placeholder, py_new) = (&reg.placeholder, &reg.py_new);
    let Some(cls) = reg.cls.upgrade() else {
        let globals = interp.builtins_dict();
        return interp.call(py_new, args, kwargs, &globals);
    };
    if let [Object::Type(sub)] = args {
        return Err(type_error(format!(
            "type '{}' takes at least one argument",
            sub.name
        )));
    }
    let simple = match args {
        [Object::Type(sub), func, rest @ ..] => {
            sub.is_subclass_of(&cls)
                && crate::builtins::object_is_callable(func)
                && !matches!(func, Object::Instance(i) if i.cls().is_subclass_of(&cls))
                && !rest.iter().any(|a| a.is_same(placeholder))
                && !kwargs.iter().any(|(_, v)| v.is_same(placeholder))
        }
        _ => false,
    };
    if !simple {
        let globals = interp.builtins_dict();
        return interp.call(py_new, args, kwargs, &globals);
    }
    let Object::Type(sub) = &args[0] else {
        unreachable!("checked above");
    };
    let mut kw = DictData::default();
    for (k, v) in kwargs {
        kw.insert(DictKey(Object::from_str(k.clone())), v.clone());
    }
    let keywords = Object::Dict(Rc::new(RefCell::new(kw)));
    crate::gc_trace::track(&keywords);
    let values = [
        args[1].clone(),
        Object::new_tuple(args[2..].to_vec()),
        keywords,
        Object::Int(0),
        Object::None,
    ];
    let inst = crate::types::PyInstance::new(sub.clone());
    for (name, v) in PARTIAL_SLOTS.iter().zip(values) {
        inst.slot_set(name, v);
    }
    let obj = Object::Instance(Rc::new(inst));
    crate::gc_trace::track(&obj);
    Ok(obj)
}

/// `partial.__call__(self, /, *args, **keywords)` without a Python
/// frame: merge stored args/keywords with the call's and tail-call
/// `self.func` through the interpreter. Like CPython's `partial_call`, it
/// reads the instance's own fields, never the (overridable) attributes.
fn partial_call(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() else {
        return Err(type_error(
            "partial.__call__ requires a running interpreter",
        ));
    };
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    let slf = args.first().ok_or_else(|| {
        type_error("descriptor '__call__' of 'functools.partial' object needs an argument")
    })?;
    let fields = match slf {
        Object::Instance(inst) => with_partial_fields(inst, |[f, a, k, p]| {
            (f.clone(), a.clone(), k.clone(), p.clone())
        })
        .or_else(|| {
            let slots = inst.slots.borrow();
            let mut out: [Option<Object>; 4] = Default::default();
            for (slot, name) in out.iter_mut().zip(PARTIAL_SLOTS) {
                *slot = slots.get(name).cloned();
            }
            match out {
                [Some(f), Some(a), Some(k), Some(p)] => Some((f, a, k, p)),
                _ => None,
            }
        }),
        _ => None,
    };
    let (func, stored_args, stored_kw, phcount) = match fields {
        Some(f) => f,
        None => (
            interp.load_attr_public(slf, "func")?,
            interp.load_attr_public(slf, "args")?,
            interp.load_attr_public(slf, "keywords")?,
            interp.load_attr_public(slf, "_phcount")?,
        ),
    };
    // 3.14 `functools.Placeholder`: a partial with placeholders merges
    // call-site positionals into the stored tuple. That path stays in
    // the frozen `functools.py` (`_partial_call_py`); the native fast
    // path covers the common placeholder-free case.
    if !matches!(phcount, Object::Int(0)) {
        let py_call = interp.load_attr_public(slf, "_partial_call_py")?;
        let globals = interp.builtins_dict();
        return interp.call(&py_call, &args[1..], kwargs, &globals);
    }

    let Object::Tuple(stored) = &stored_args else {
        return Err(type_error("partial 'args' must be a tuple"));
    };
    let mut call_args: Vec<Object> = Vec::with_capacity(stored.len() + args.len() - 1);
    call_args.extend(stored.iter().cloned());
    call_args.extend_from_slice(&args[1..]);

    let globals = interp.builtins_dict();
    let stored_empty = match &stored_kw {
        Object::Dict(d) => d.borrow().is_empty(),
        _ => false,
    };
    if stored_empty {
        return interp.call(&func, &call_args, kwargs, &globals);
    }
    let mut call_kwargs: Vec<(String, Object)> = Vec::new();
    if let Object::Dict(d) = &stored_kw {
        for (k, v) in d.borrow().iter() {
            if let Object::Str(s) = &k.0 {
                call_kwargs.push((s.to_string(), v.clone()));
            } else {
                // CPython rejects non-string keys at call time
                // (`PyObject_Call` kwargs validation).
                return Err(type_error("keywords must be strings"));
            }
        }
    }
    // Call-site keywords override stored ones (`{**self.keywords, **keywords}`).
    for (k, v) in kwargs {
        if let Some(slot) = call_kwargs.iter_mut().find(|(name, _)| name == k) {
            slot.1 = v.clone();
        } else {
            call_kwargs.push((k.clone(), v.clone()));
        }
    }
    interp.call(&func, &call_args, &call_kwargs, &globals)
}

/// `cmp_to_key(mycmp)` — CPython implements this in C so the module
/// attribute is a non-binding builtin (a test class stores it as a
/// class attribute and calls it through `self.`). The K-class factory
/// itself stays in the frozen `functools.py` (`_cmp_to_key_py`).
fn cmp_to_key(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let mycmp = match (args, kwargs) {
        ([m], []) => m.clone(),
        ([], [(name, m)]) if name == "mycmp" => m.clone(),
        ([], []) => {
            return Err(type_error(
                "cmp_to_key() missing required argument: 'mycmp' (pos 1)",
            ))
        }
        _ => {
            return Err(type_error(format!(
                "cmp_to_key() takes at most 1 argument ({} given)",
                args.len() + kwargs.len()
            )))
        }
    };
    let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() else {
        return Err(type_error("cmp_to_key() requires a running interpreter"));
    };
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    let Some(functools) = interp.module_cache().get("functools") else {
        return Err(type_error("cmp_to_key() requires functools to be imported"));
    };
    let factory = interp.load_attr_public(&functools, "_cmp_to_key_py")?;
    let globals = interp.builtins_dict();
    interp.call(&factory, &[mycmp], &[], &globals)
}

// ---------------------------------------------------------------------------
// `_lru_cache_wrapper` — native `functools.lru_cache` core.
//
// CPython implements the LRU wrapper in C: a cache *hit or miss pushes no
// Python frame*, which is observable — pandas' `find_stack_level()` walks
// `f_back` filenames to compute a warning `stacklevel`, and a Python-level
// wrapper frame (`<frozen functools>`) in the chain mis-attributes the
// warning (`tests/io/formats/test_to_excel.py::test_css_to_excel_bad_colors`
// asserts the warning points at the *caller*). The frozen `functools.py`
// falls back to its pure-Python wrapper only if this import fails.
//
// The wrapper's state lives in its instance slots, outside its `__dict__`
// (CPython keeps it in the C struct): `update_wrapper` copies a cached
// function's `__dict__` onto the functions that wrap it, so state there
// would travel with it, and replacing `__wrapped__` mustn't change what
// the wrapper calls. A bounded cache whose keys all compare natively (an
// exact `int` or `str`, or a tuple of them) keeps the recency of its
// entries in links by entry position (see [`lru_state`]): a hit is one
// probe and one relink, and a full cache's miss replaces its oldest entry
// in place. The first key whose comparison can run Python moves the cache
// to the ordered-dictionary fallback for good, where a hit moves its
// entry to the end with no borrow held across the comparisons.
// ---------------------------------------------------------------------------

/// Sentinel string separating positional from keyword parts of a cache key
/// (CPython uses a fresh `object()`; the embedded NULs keep any real user
/// string from colliding).
const LRU_KWD_MARK: &str = "\0\0__weavepy_lru_kwd_mark__\0\0";

/// The [`LRU_KWD_MARK`] string every key shares (its hash computed once).
fn lru_kwd_mark() -> &'static Object {
    static MARK: std::sync::OnceLock<Object> = std::sync::OnceLock::new();
    MARK.get_or_init(|| Object::from_static(LRU_KWD_MARK))
}

/// The address of the `_lru_cache_wrapper` class, for the call sites'
/// quick check (see [`lru_hit`]); zero until the class exists.
static LRU_TYPE_ADDR: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn lru_type() -> Rc<crate::types::TypeObject> {
    thread_local! {
        static LRU_TYPE: RefCell<Option<Rc<crate::types::TypeObject>>> =
            const { RefCell::new(None) };
    }
    LRU_TYPE.with(|slot| {
        if let Some(t) = slot.borrow().as_ref() {
            return t.clone();
        }
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::default();
        dict.insert(
            DictKey(Object::from_static("__module__")),
            Object::from_static("functools"),
        );
        // `attr` is the Python-visible dict key; `name` is the builtin's
        // internal (dotted) name, which `builtin_display_name` folds back
        // to the last component and `builtin_text_signature` keys on —
        // `inspect.signature(lru.cache_info)` must report `()`.
        let mut method = |attr: &'static str,
                          name: &'static str,
                          call_kw: fn(
            &[Object],
            &[(String, Object)],
        ) -> Result<Object, RuntimeError>| {
            dict.insert(
                DictKey(Object::from_static(attr)),
                Object::Builtin(Rc::new(BuiltinFn {
                    name,
                    binds_instance: true,
                    call: Box::new(move |args| call_kw(args, &[])),
                    call_kw: Some(Box::new(call_kw)),
                })),
            );
        };
        method("__call__", "__call__", lru_call);
        method(
            "cache_info",
            ".lru_cache_wrapper.cache_info",
            lru_cache_info,
        );
        method(
            "cache_clear",
            ".lru_cache_wrapper.cache_clear",
            lru_cache_clear,
        );
        method("__get__", "__get__", lru_descr_get);
        // CPython's C wrapper: `copy`/`deepcopy` hand back the wrapper
        // itself, and pickling reduces to the qualname (pickle-by-
        // reference, like a plain function).
        method("__copy__", "__copy__", lru_identity);
        method("__deepcopy__", "__deepcopy__", lru_identity);
        method("__reduce__", "__reduce__", lru_reduce);
        // `_lru_cache_wrapper(user_function, maxsize, typed, _CacheInfo)`
        // instantiation — the class is what the module exports (CPython
        // parity), so construction runs through `__new__`, which drops
        // the implicit `cls` and defers to the native constructor.
        dict.insert(
            DictKey(Object::from_static("__new__")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__new__",
                binds_instance: false,
                call: Box::new(|args| lru_cache_wrapper_new(args.get(1..).unwrap_or(&[]))),
                call_kw: None,
            })),
        );
        let cls = crate::types::TypeObject::new_user(
            "_lru_cache_wrapper",
            vec![bt.object_.clone()],
            dict,
        )
        .expect("_lru_cache_wrapper class must linearise");
        // An immutable type, as CPython's static one: its `__call__` is
        // the native core the call sites settle wrappers' calls by.
        cls.immutable.set(true);
        let _ = LRU_TYPE_ADDR.compare_exchange(
            0,
            Rc::as_ptr(&cls) as usize,
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
        );
        *slot.borrow_mut() = Some(cls.clone());
        cls
    })
}

fn lru_self(args: &[Object]) -> Result<Rc<crate::types::PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(inst)) => Ok(inst.clone()),
        _ => Err(type_error("expected _lru_cache_wrapper instance")),
    }
}

/// The wrapper's private slots, in the order [`lru_cache_wrapper_new`]
/// stores them: the cache, the state words (see [`lru_state`]), the
/// wrapped function, and the `CacheInfo` class.
const LRU_SLOTS: [&str; 4] = ["_lru_cache", "_lru_state", "_lru_func", "_lru_info"];

/// The interned names of [`LRU_SLOTS`], which every wrapper's slots are
/// keyed by (so a read compares pointers).
fn lru_keys() -> &'static [crate::shared_value::SharedStr; 4] {
    static KEYS: std::sync::OnceLock<[crate::shared_value::SharedStr; 4]> =
        std::sync::OnceLock::new();
    KEYS.get_or_init(|| {
        LRU_SLOTS.map(|name| match crate::stdlib::sys::intern_name(name) {
            Object::Str(s) => s,
            _ => unreachable!("interned names are strs"),
        })
    })
}

/// A wrapper's cache, state words, and wrapped function, held for a call.
struct LruParts {
    cache: Rc<RefCell<DictData>>,
    state: Rc<RefCell<Vec<u8>>>,
    func: Object,
}

fn lru_lost() -> RuntimeError {
    type_error("lru_cache wrapper lost its state")
}

fn lru_parts(inst: &crate::types::PyInstance) -> Result<LruParts, RuntimeError> {
    let keys = lru_keys();
    let slots = inst.slots.borrow();
    match slots.leading_interned([&keys[0], &keys[1], &keys[2]]) {
        Some([Object::Dict(cache), Object::ByteArray(state), func]) => Ok(LruParts {
            cache: cache.clone(),
            state: state.clone(),
            func: func.clone(),
        }),
        _ => Err(lru_lost()),
    }
}

/// `_lru_cache_wrapper(user_function, maxsize, typed, _CacheInfo)`.
fn lru_cache_wrapper_new(args: &[Object]) -> Result<Object, RuntimeError> {
    let [user_function, maxsize, typed, cache_info_cls] = args else {
        return Err(type_error("_lru_cache_wrapper() takes exactly 4 arguments"));
    };
    if !crate::builtins::object_is_callable(user_function) {
        return Err(type_error("the first argument must be callable"));
    }
    // CPython's `lru_cache_new`: `None` is unbounded, and a negative size
    // caches nothing.
    let limit = match maxsize {
        Object::None => lru_state::UNBOUNDED,
        Object::Int(n) => u64::try_from(*n).unwrap_or(0),
        Object::Bool(b) => u64::from(*b),
        Object::Long(_) => {
            return Err(RuntimeError::PyException(
                crate::error::PyException::from_builtin(
                    "OverflowError",
                    "Python int too large to convert to C ssize_t",
                ),
            ))
        }
        _ => return Err(type_error("maxsize should be integer or None")),
    };
    let flags = if typed.is_truthy() { lru_state::TYPED } else { 0 };
    let inst = Rc::new(crate::types::PyInstance::new(lru_type()));
    let values = [
        Object::Dict(Rc::new(RefCell::new(DictData::default()))),
        Object::ByteArray(Rc::new(RefCell::new(lru_state::new(limit, flags)))),
        user_function.clone(),
        cache_info_cls.clone(),
    ];
    {
        let mut slots = inst.slots.borrow_mut();
        for (key, value) in lru_keys().iter().zip(values) {
            slots.insert_shared(key, value);
        }
    }
    // CPython's `lru_cache_new` GC-tracks the wrapper: its function edge
    // closes the `module dict -> wrapper -> function -> __globals__`
    // cycle of every `@lru_cache` at module level, and an untracked
    // wrapper counted as an external root that made that namespace
    // immortal.
    let wrapper = Object::Instance(inst);
    crate::gc_trace::track(&wrapper);
    Ok(wrapper)
}

/// `_lru_private(wrapper)`: the cache, the state words, and whether the
/// cache keeps native recency links (see [`lru_state`]). For WeavePy's
/// own regression tests, which check the mode the native core is in.
fn lru_private(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = lru_self(args)?;
    lru_flush_deferred()?;
    let parts = lru_parts(&inst)?;
    let (limit, flags) =
        lru_state::mode(&parts.state.borrow()).ok_or_else(invalid_lru_order)?;
    let native = limit != 0 && limit != lru_state::UNBOUNDED && flags & lru_state::FALLBACK == 0;
    Ok(Object::new_tuple_array([
        Object::Dict(parts.cache),
        Object::ByteArray(parts.state),
        Object::Bool(native),
    ]))
}

/// Build the cache key for one call (CPython's `lru_cache_make_key`): a
/// single exactly-`int`/`str` positional short-circuits to itself; anything
/// else keys on the flattened `(args…, MARK, k, v, …)` tuple, with argument
/// *types* appended in `typed` mode.
fn lru_make_key(call_args: &[Object], kwargs: &[(String, Object)], typed: bool) -> Object {
    if kwargs.is_empty() && call_args.len() == 1 && !typed {
        if let Object::Int(_) | Object::Long(_) | Object::Str(_) = &call_args[0] {
            return call_args[0].clone();
        }
    }
    let mut parts: Vec<Object> = call_args.to_vec();
    if !kwargs.is_empty() {
        parts.push(lru_kwd_mark().clone());
        for (k, v) in kwargs {
            parts.push(Object::from_str(k.clone()));
            parts.push(v.clone());
        }
    }
    if typed {
        for a in call_args {
            parts.push(Object::Type(crate::builtins::class_of(a)));
        }
        for (_, v) in kwargs {
            parts.push(Object::Type(crate::builtins::class_of(v)));
        }
    }
    Object::new_tuple(parts)
}

/// Count one more hit (or miss) in `state`, under its lock.
fn lru_count(state: &RefCell<Vec<u8>>, hit: bool) -> Result<(), RuntimeError> {
    let mut bytes = state.borrow_mut();
    if hit {
        lru_state::hit(&mut bytes)
    } else {
        lru_state::miss(&mut bytes)
    }
    .ok_or_else(invalid_lru_order)
}

/// Per-thread nesting depth of the native wrapper, standing in for
/// CPython's C-stack consumption. Each nesting level is charged two
/// units against `C_RECURSION_LIMIT`: CPython's C wrapper eats several
/// C frames per call, which is why `fib(10000)` under `lru_cache` must
/// raise `RecursionError` even though `Py_C_RECURSION_LIMIT` is 10 000
/// and `sys.setrecursionlimit` was raised past 20 000
/// (test_functools `test_lru_recursion`).
const LRU_C_STACK_UNITS: usize = 2;

thread_local! {
    static LRU_NATIVE_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

struct LruDepthGuard;

impl Drop for LruDepthGuard {
    fn drop(&mut self) {
        LRU_NATIVE_DEPTH.with(|d| d.set(d.get().saturating_sub(LRU_C_STACK_UNITS)));
    }
}

fn lru_enter_native() -> Result<LruDepthGuard, RuntimeError> {
    let depth = LRU_NATIVE_DEPTH.with(|d| {
        let n = d.get() + LRU_C_STACK_UNITS;
        d.set(n);
        n
    });
    if depth > crate::recursion::C_RECURSION_LIMIT {
        // Balance eagerly: the guard is never constructed on this path.
        LRU_NATIVE_DEPTH.with(|d| d.set(d.get().saturating_sub(LRU_C_STACK_UNITS)));
        return Err(RuntimeError::PyException(
            crate::error::PyException::from_builtin(
                "RecursionError",
                "maximum recursion depth exceeded",
            ),
        ));
    }
    Ok(LruDepthGuard)
}

#[cfg(test)]
thread_local! {
    static SCALAR_LRU_EVENTS: std::cell::Cell<[usize; 3]> = const { std::cell::Cell::new([0; 3]) };
}

#[cfg(test)]
fn note_scalar_lru_event(index: usize) {
    SCALAR_LRU_EVENTS.with(|events| {
        let mut counts = events.get();
        counts[index] += 1;
        events.set(counts);
    });
}

enum ScalarLru {
    Slow,
    Complete(Option<Object>),
}

#[inline]
fn native_lru_key(key: &Object) -> bool {
    match key {
        Object::Int(_) | Object::Long(_) | Object::Str(_) => true,
        Object::Tuple(_) => native_lru_tuple_key(key, 8, &mut 64),
        _ => false,
    }
}

/// Bound admission work and stack use independently of user key size.
/// Exact tuples of native leaves cannot call Python or own finalizers;
/// subclasses and larger/deeper keys keep the callback-capable path.
fn native_lru_tuple_key(key: &Object, depth: usize, remaining: &mut usize) -> bool {
    if *remaining == 0 {
        return false;
    }
    *remaining -= 1;
    match key {
        Object::Int(_) | Object::Long(_) | Object::Str(_) => true,
        Object::Type(cls) => {
            // Typed keys append argument classes. Only these exact builtin
            // identities have immutable metatype behavior and no finalizers.
            let builtins = crate::builtin_types::builtin_types();
            Rc::ptr_eq(cls, &builtins.int_)
                || Rc::ptr_eq(cls, &builtins.str_)
                || Rc::ptr_eq(cls, &builtins.tuple_)
        }
        Object::Tuple(items) if depth > 0 && items.len() <= *remaining => items
            .iter()
            .all(|item| native_lru_tuple_key(item, depth - 1, remaining)),
        _ => false,
    }
}

fn invalid_lru_order() -> RuntimeError {
    type_error("lru_cache has inconsistent private recency data")
}

/// Leave native mode for good: put the entries in recency order (oldest
/// first, the order the fallback keeps) and drop the links. `data` and
/// `bytes` are `state`'s cache and words, both borrowed.
fn lru_demote(
    state: &Rc<RefCell<Vec<u8>>>,
    data: &mut DictData,
    bytes: &mut Vec<u8>,
) -> Result<(), RuntimeError> {
    crate::object::bytearray_check_resizable(state)?;
    let indices = lru_state::Links::new(bytes, data.len())
        .and_then(|links| links.indices())
        .ok_or_else(invalid_lru_order)?;
    if !data.keys().all(|stored| native_lru_key(&stored.0)) {
        return Err(invalid_lru_order());
    }
    // Restore logical oldest-to-newest order once, before any
    // callback-capable comparison reaches the fallback.
    let old = std::mem::take(data);
    let mut entries: Vec<_> = old.into_iter().map(Some).collect();
    for index in indices {
        let (key, value) = entries
            .get_mut(index)
            .and_then(Option::take)
            .ok_or_else(invalid_lru_order)?;
        data.insert(key, value);
    }
    lru_state::clear_links(bytes).ok_or_else(invalid_lru_order)?;
    lru_state::add_flag(bytes, lru_state::FALLBACK).ok_or_else(invalid_lru_order)?;
    #[cfg(test)]
    note_scalar_lru_event(2);
    Ok(())
}

/// The lookup of a native `key` in a bounded cache in native mode (or,
/// with `insert`, the store of a miss's result with `limit` entries at
/// most), under one hold of the cache and its words: `Slow` when the
/// fallback must settle it, after leaving native mode if `key` needs it.
/// Covers the keys [`lru_native_lookup`] can't probe (big `int`s, nested
/// or typed tuples): their comparisons run no Python either, and one that
/// would (a tampered cache) is an error.
fn scalar_lru_operation(
    cache: &RefCell<DictData>,
    state: &Rc<RefCell<Vec<u8>>>,
    key: &Object,
    insert: Option<(&Object, usize)>,
) -> Result<ScalarLru, RuntimeError> {
    let scalar = native_lru_key(key);
    let removed;
    {
        // The shared cache lock also serializes its recency words under
        // gil=0. Neither key comparison nor an owner release may run Python
        // while these two cells are borrowed.
        let mut data = cache.borrow_mut();
        let mut bytes = state.borrow_mut();
        let (_, flags) = lru_state::mode(&bytes).ok_or_else(invalid_lru_order)?;
        if flags & lru_state::FALLBACK != 0 {
            return Ok(ScalarLru::Slow);
        }
        let len = data.len();
        if !scalar || insert.is_some_and(|(_, limit)| len > limit) {
            lru_demote(state, &mut data, &mut bytes)?;
            return Ok(ScalarLru::Slow);
        }
        let mut links = lru_state::Links::new(&mut bytes, len).ok_or_else(invalid_lru_order)?;
        // A `str` or `int` key settles by native equality in one probe;
        // its hash is kept for the insert below.
        let probe = crate::object::LeafProbe::new(key);
        let exact = match &probe {
            Some(p) => match data.get_index_of(p) {
                Some(i) => Some(Some(i)),
                None if p.miss_is_exact() => Some(None),
                None => None,
            },
            None => None,
        };
        let found = match exact {
            Some(found) => found,
            None => {
                // Deferral also protects against private-cache tampering: a
                // foreign key must not invoke Python inside the paired borrow.
                let (found, deferred) =
                    crate::object::key_cmp_deferred(|| data.get_index_of(&DictKey(key.clone())));
                let found = found?;
                if deferred {
                    return Err(invalid_lru_order());
                }
                found
            }
        };
        if let Some(index) = found {
            let (stored, value) = data.get_index(index).ok_or_else(invalid_lru_order)?;
            if !native_lru_key(&stored.0) {
                return Err(invalid_lru_order());
            }
            // A recursive call may have populated a miss. Its entry and
            // recency win; only an actual hit refreshes the order.
            return if insert.is_some() {
                Ok(ScalarLru::Complete(None))
            } else {
                let value = value.clone();
                links.touch(index).ok_or_else(invalid_lru_order)?;
                #[cfg(test)]
                note_scalar_lru_event(0);
                Ok(ScalarLru::Complete(Some(value)))
            };
        }
        let Some((value, limit)) = insert else {
            return Ok(ScalarLru::Complete(None));
        };
        // Proven absent above (an eviction keeps it so): no second probe.
        let hash = probe
            .as_ref()
            .map_or_else(|| crate::object::dict_key_hash(key), |p| p.hash);
        removed = if len == limit {
            // The least recent entry's place takes the new one, which
            // becomes the most recent.
            let head = links.head().ok_or_else(invalid_lru_order)?;
            links.touch(head).ok_or_else(invalid_lru_order)?;
            #[cfg(test)]
            note_scalar_lru_event(1);
            data.replace_index_unique(head, hash, DictKey(key.clone()), value.clone())
        } else {
            crate::object::bytearray_check_resizable(state)?;
            links.push(limit).ok_or_else(invalid_lru_order)?;
            data.insert_unique_hashed(hash, DictKey(key.clone()), value.clone());
            None
        };
    }
    // Publish a consistent cache before releasing an evicted result.
    drop(removed);
    Ok(ScalarLru::Complete(None))
}

/// What [`lru_native_lookup`] settled.
enum NativeLookup {
    /// The cached value, its hit counted and its recency refreshed.
    Hit(Object),
    /// An exact miss, counted.
    Miss,
}

/// The lookup of `probe` under one hold of the cache and its words,
/// settled natively: the hit or the counted miss, or `None` (nothing
/// touched) for the general path. A bounded cache must be in native mode;
/// an unbounded one needs only that no stored key the probe met could
/// equal it through Python.
#[inline]
fn lru_native_lookup(
    cache: &RefCell<DictData>,
    state: &RefCell<Vec<u8>>,
    probe: &crate::object::LeafProbe<'_>,
    bounded: bool,
) -> Option<NativeLookup> {
    // The cache lock also serializes the words under gil=0 (taken in the
    // order `lru_cache_clear` takes them). Nothing here runs Python.
    let data = cache.borrow_mut();
    let mut bytes = state.borrow_mut();
    let (_, flags) = lru_state::mode(&bytes)?;
    if bounded && flags & lru_state::FALLBACK != 0 {
        return None;
    }
    let found = data.get_index_of(probe);
    if !probe.miss_is_exact() {
        return None;
    }
    match found {
        Some(index) => {
            let value = data.get_index(index)?.1.clone();
            if bounded {
                lru_state::Links::new(&mut bytes, data.len())?.touch(index)?;
            }
            lru_state::hit(&mut bytes)?;
            Some(NativeLookup::Hit(value))
        }
        None => {
            lru_state::miss(&mut bytes)?;
            Some(NativeLookup::Miss)
        }
    }
}

/// Store `result` under the native `key` after [`lru_native_lookup`]'s
/// miss, evicting the least recent entry of a full bounded cache in place:
/// `Some` with the evicted entry (to release once the cache is consistent
/// and unborrowed) when stored or already present, `None` (nothing
/// touched) for the general path.
fn lru_native_store(
    cache: &RefCell<DictData>,
    state: &Rc<RefCell<Vec<u8>>>,
    key: &Object,
    result: &Object,
    limit: u64,
) -> Result<Option<Option<(DictKey, Object)>>, RuntimeError> {
    native_store_in(
        &mut cache.borrow_mut(),
        &mut state.borrow_mut(),
        state,
        key,
        None,
        result,
        limit,
    )
}

/// [`lru_native_store`] with the cache and its words (`state`'s) in hand,
/// and `key`'s hash if known.
#[inline]
fn native_store_in(
    data: &mut DictData,
    bytes: &mut Vec<u8>,
    state: &Rc<RefCell<Vec<u8>>>,
    key: &Object,
    hash: Option<i64>,
    result: &Object,
    limit: u64,
) -> Result<Option<Option<(DictKey, Object)>>, RuntimeError> {
    let probe = match hash {
        Some(hash) => crate::object::LeafProbe::with_hash(key, hash),
        None => match crate::object::LeafProbe::new(key) {
            Some(probe) => probe,
            None => return Ok(None),
        },
    };
    let Some((_, flags)) = lru_state::mode(bytes) else {
        return Ok(None);
    };
    match data.get_index_of(&probe) {
        // A recursive call populated the key: its entry and recency win.
        Some(_) if probe.miss_is_exact() => return Ok(Some(None)),
        None if probe.miss_is_exact() => {}
        _ => return Ok(None),
    }
    if limit == lru_state::UNBOUNDED {
        data.insert_unique_hashed(probe.hash, DictKey(key.clone()), result.clone());
        return Ok(Some(None));
    }
    let len = data.len();
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    if flags & lru_state::FALLBACK != 0 || len > limit {
        return Ok(None);
    }
    let Some(mut links) = lru_state::Links::new(bytes, len) else {
        return Ok(None);
    };
    if len == limit {
        // The least recent entry's place takes the new one, which becomes
        // the most recent.
        let Some(head) = links.head() else {
            return Ok(None);
        };
        links.touch(head).ok_or_else(invalid_lru_order)?;
        let evicted =
            data.replace_index_unique(head, probe.hash, DictKey(key.clone()), result.clone());
        return Ok(Some(evicted));
    }
    crate::object::bytearray_check_resizable(state)?;
    links.push(limit).ok_or_else(invalid_lru_order)?;
    data.insert_unique_hashed(probe.hash, DictKey(key.clone()), result.clone());
    Ok(Some(None))
}

/// What [`lru_probe`] settled for a `CALL` of a wrapper.
pub(crate) enum LruProbe {
    /// The cached value, its hit counted and its recency refreshed.
    Hit(Object),
    /// An exact miss, counted: the wrapped (plain Python) function and the
    /// store its result awaits. The caller runs the function in the
    /// wrapper's place and stores the result when it returns
    /// ([`lru_store_returned`]), or uncounts the miss
    /// ([`LruPending::uncount_miss`]) if it doesn't make the call.
    Miss(Object, LruPending),
}

/// A `functools.lru_cache` miss whose wrapped function runs as an inline
/// activation in the wrapper's place: the store of its result. It holds
/// the wrapper (as the call's operands held it) and the wrapper's native
/// nesting level until the activation finishes.
pub(crate) struct LruPending {
    wrapper: Object,
    key: Object,
    /// The key's Python hash.
    hash: i64,
    _depth: LruDepthGuard,
}

/// A wrapper's cache and state words, read in place.
///
/// # Safety
///
/// Nothing that runs Python code may run while the references live (the
/// slots are set once by the constructor; only the collector's clear,
/// which needs a collection, replaces them).
#[inline(always)]
unsafe fn lru_cells(
    wrapper: &Object,
) -> Option<(&Rc<RefCell<DictData>>, &Rc<RefCell<Vec<u8>>>)> {
    let Object::Instance(inst) = wrapper else {
        return None;
    };
    let keys = lru_keys();
    // SAFETY: the caller's contract.
    let slots = unsafe { inst.slots.peek() }?;
    match slots.leading_interned([&keys[0], &keys[1]])? {
        [Object::Dict(cache), Object::ByteArray(state)] => Some((cache, state)),
        _ => None,
    }
}

/// Take back the miss [`lru_probe`] counted for a call of `wrapper` that
/// wasn't made (nothing ran in between).
pub(crate) fn lru_uncount_miss(wrapper: &Object) {
    // SAFETY: nothing below runs code.
    if let Some((_, state)) = unsafe { lru_cells(wrapper) } {
        // (Only a tampered buffer could fail.)
        let _ = lru_state::unmiss(&mut state.borrow_mut());
    }
}

/// Whether `obj` is a `functools.lru_cache` wrapper (by its class).
#[inline(always)]
pub(crate) fn is_lru_wrapper(obj: &Object) -> bool {
    matches!(obj, Object::Instance(inst)
        if std::ptr::from_ref(inst.cls_raw()) as usize
            == LRU_TYPE_ADDR.load(std::sync::atomic::Ordering::Relaxed))
}

/// Set while a returned result waits in [`LRU_DEFERRED`].
static LRU_DEFERRED_ANY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A returned result an inline miss couldn't store natively (an equal-hash
/// stored key whose comparison needs Python): its cache, words, key, and
/// value, stored by the next call into the native core that can run
/// Python ([`lru_flush_deferred`]), before anything reads that cache.
type DeferredStore = (Rc<RefCell<DictData>>, Rc<RefCell<Vec<u8>>>, Object, Object);

static LRU_DEFERRED: std::sync::Mutex<Vec<DeferredStore>> = std::sync::Mutex::new(Vec::new());

/// Store every deferred result (see [`LRU_DEFERRED`]), in order.
fn lru_flush_deferred() -> Result<(), RuntimeError> {
    while LRU_DEFERRED_ANY.load(std::sync::atomic::Ordering::Acquire) {
        let items = {
            let mut list = LRU_DEFERRED
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            LRU_DEFERRED_ANY.store(false, std::sync::atomic::Ordering::Release);
            std::mem::take(&mut *list)
        };
        for (cache, state, key, value) in items {
            let (limit, _) = lru_state::mode(&state.borrow()).ok_or_else(invalid_lru_order)?;
            lru_store(&cache, &state, &key, value, limit, false)?;
        }
    }
    Ok(())
}

/// A call of `inst`, a `functools.lru_cache` wrapper, with the positional
/// `args`, settled natively without running Python code: the hit, or the
/// exact miss the caller may run the wrapped function for in place (see
/// [`LruProbe`]), or `None`, nothing touched, for the full call. The core
/// loop and the frame JIT try it at a `CALL` of an instance: a sole exact
/// `int` or `str` argument of an untyped wrapper, while nothing else holds
/// its state.
#[inline]
pub(crate) fn lru_probe(inst: &crate::types::PyInstance, args: &[Object]) -> Option<LruProbe> {
    if std::ptr::from_ref(inst.cls_raw()) as usize
        != LRU_TYPE_ADDR.load(std::sync::atomic::Ordering::Relaxed)
        || LRU_DEFERRED_ANY.load(std::sync::atomic::Ordering::Relaxed)
    {
        return None;
    }
    match args {
        // A sole exact `int` or `str` is its own key.
        [key @ (Object::Int(_) | Object::Str(_))] => lru_probe_with::<_, true>(
            inst,
            &crate::object::LeafProbe::new(key)?,
            || key.clone(),
        ),
        // Positional `int`s and `str`s key on their tuple, probed without
        // building it unless the call misses.
        [_, _, ..] if args.len() <= LRU_ARGS_MAX => lru_probe_with::<_, true>(
            inst,
            &ArgsProbe::new(args, &[], &[])?,
            || Object::new_tuple(args.to_vec()),
        ),
        _ => None,
    }
}

/// [`lru_probe`]'s hit for a native caller (`Interpreter::call`), which
/// can't run a miss in place: `None`, nothing touched, for the full call.
fn lru_hit(inst: &crate::types::PyInstance, args: &[Object]) -> Option<Object> {
    if LRU_DEFERRED_ANY.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    let probe = match args {
        [key @ (Object::Int(_) | Object::Str(_))] => lru_probe_with::<_, false>(
            inst,
            &crate::object::LeafProbe::new(key)?,
            || unreachable!("no miss is run"),
        ),
        [_, _, ..] if args.len() <= LRU_ARGS_MAX => lru_probe_with::<_, false>(
            inst,
            &ArgsProbe::new(args, &[], &[])?,
            || unreachable!("no miss is run"),
        ),
        _ => None,
    };
    match probe? {
        LruProbe::Hit(value) => Some(value),
        LruProbe::Miss(..) => None,
    }
}

/// A native caller's call (`Interpreter::call`) of `wrapper` (`inst`), a
/// `functools.lru_cache` wrapper: a hit settled natively, else its
/// `__call__` (the class is immutable), without binding a method.
pub(crate) fn lru_call_native(
    inst: &crate::types::PyInstance,
    wrapper: &Object,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    if kwargs.is_empty() {
        if let Some(value) = lru_hit(inst, args) {
            return Ok(value);
        }
    }
    let mut call_args = Vec::with_capacity(args.len() + 1);
    call_args.push(wrapper.clone());
    call_args.extend_from_slice(args);
    lru_call(&call_args, kwargs)
}

/// A keyword call of `inst`, a `functools.lru_cache` wrapper, with the
/// positional `args` and the keyword arguments `names` and `values`, that
/// hits its cache: [`lru_probe`]'s hit (a miss, or any other shape, is
/// `None`, nothing touched, for the full call). The key is the tuple of the
/// arguments, the keyword mark, and each name and value, probed in place.
pub(crate) fn lru_hit_kw(
    inst: &crate::types::PyInstance,
    args: &[Object],
    names: &[Object],
    values: &[Object],
) -> Option<Object> {
    if std::ptr::from_ref(inst.cls_raw()) as usize
        != LRU_TYPE_ADDR.load(std::sync::atomic::Ordering::Relaxed)
        || LRU_DEFERRED_ANY.load(std::sync::atomic::Ordering::Relaxed)
        || names.is_empty()
        || names.len() != values.len()
        || args.len() + 1 + 2 * names.len() > LRU_KEY_MAX
    {
        return None;
    }
    let probe = ArgsProbe::new(args, names, values)?;
    match lru_probe_with::<_, false>(inst, &probe, || unreachable!("no miss is run"))? {
        LruProbe::Hit(value) => Some(value),
        LruProbe::Miss(..) => None,
    }
}

/// The most positional arguments [`lru_probe`] keys natively.
const LRU_ARGS_MAX: usize = 8;

/// The most items a key [`ArgsProbe`] probes may have.
const LRU_KEY_MAX: usize = 16;

/// A probe that settles by native equality, and knows afterwards whether
/// it met a stored key whose comparison could need Python.
trait ExactProbe: crate::dictmap::Probe {
    /// No stored key the lookup compared could equal the probe through
    /// Python (see [`crate::object::LeafProbe::miss_is_exact`]).
    fn exact(&self) -> bool;
}

impl ExactProbe for crate::object::LeafProbe<'_> {
    #[inline(always)]
    fn exact(&self) -> bool {
        self.miss_is_exact()
    }
}

/// The key of a call whose arguments are exact `int`s and `str`s (the
/// tuple `lru_cache_make_key` makes of them: the positional arguments,
/// then, for a keyword call, the keyword mark and each name and value),
/// probed in place: its hash is the tuple's, and it equals a stored tuple
/// of the same items.
struct ArgsProbe<'a> {
    args: &'a [Object],
    /// A keyword call's names and values (empty for none).
    names: &'a [Object],
    values: &'a [Object],
    /// The key's item count.
    len: usize,
    hash: i64,
    /// Set when a stored tuple's item paired with one of the key's might
    /// equal it through Python (`1.0`, `True`, an instance).
    foreign: std::cell::Cell<bool>,
}

impl<'a> ArgsProbe<'a> {
    #[inline]
    fn new(args: &'a [Object], names: &'a [Object], values: &'a [Object]) -> Option<Self> {
        let len = args.len() + if names.is_empty() { 0 } else { 1 + 2 * names.len() };
        let mut probe = Self {
            args,
            names,
            values,
            len,
            hash: 0,
            foreign: std::cell::Cell::new(false),
        };
        let mut hash = crate::object::TupleHasher::new();
        for i in 0..len {
            hash.push(match probe.item(i) {
                Object::Int(i) => crate::object::py_hash_long_i64(*i),
                Object::Str(s) => crate::shared_value::SharedStr::hash_cached(s),
                _ => return None,
            });
        }
        probe.hash = hash.finish(len);
        Some(probe)
    }

    /// The key's item `i` (below `len`).
    #[inline(always)]
    fn item(&self, i: usize) -> &Object {
        if i < self.args.len() {
            return &self.args[i];
        }
        match i - self.args.len() {
            0 => lru_kwd_mark(),
            j if j % 2 == 1 => &self.names[j / 2],
            j => &self.values[j / 2 - 1],
        }
    }
}

impl crate::dictmap::Probe for ArgsProbe<'_> {
    #[inline(always)]
    fn probe_hash(&self) -> i64 {
        self.hash
    }

    fn probe_eq(&self, stored: &DictKey) -> bool {
        match &stored.0 {
            // Item by item, as the tuple's own comparison goes: the first
            // unequal pair settles it.
            Object::Tuple(items) if items.len() == self.len => {
                for (i, item) in items.iter().enumerate() {
                    let equal = match (self.item(i), item) {
                        (Object::Int(a), Object::Int(b)) => a == b,
                        (Object::Str(a), Object::Str(b)) => {
                            crate::shared_value::SharedStr::ptr_eq(a, b)
                                || crate::dictmap::key_bytes_eq(a.as_bytes(), b.as_bytes())
                        }
                        // Builtin values of kinds that are never equal.
                        (Object::Int(_), Object::Str(_) | Object::Long(_) | Object::Tuple(_))
                        | (
                            Object::Str(_),
                            Object::Int(_) | Object::Long(_) | Object::Tuple(_) | Object::WStr(_),
                        ) => false,
                        _ => {
                            self.foreign.set(true);
                            return false;
                        }
                    };
                    if !equal {
                        return false;
                    }
                }
                true
            }
            // Builtin keys a tuple of these never equals.
            Object::Int(_) | Object::Long(_) | Object::Str(_) | Object::WStr(_) | Object::Tuple(_) => {
                false
            }
            _ => {
                self.foreign.set(true);
                false
            }
        }
    }
}

impl ExactProbe for ArgsProbe<'_> {
    #[inline(always)]
    fn exact(&self) -> bool {
        !self.foreign.get()
    }
}

/// [`lru_probe`] of the key `probe` stands for (`key` makes it), with a
/// miss to run in place when `MISS` (else `None`, nothing touched).
#[inline(always)]
fn lru_probe_with<P: ExactProbe, const MISS: bool>(
    inst: &crate::types::PyInstance,
    probe: &P,
    key: impl FnOnce() -> Object,
) -> Option<LruProbe> {
    let keys = lru_keys();
    // SAFETY: reads that run no code. The slots of a wrapper are set once
    // by its constructor.
    let slots = unsafe { inst.slots.peek() }?;
    let [Object::Dict(cache), Object::ByteArray(state), func] =
        slots.leading_interned([&keys[0], &keys[1], &keys[2]])?
    else {
        return None;
    };
    // SAFETY: as above: nothing below runs code that could borrow either
    // cell, and `peek_mut` declines while any borrow is live (another
    // thread's included: it declines once cells are shared).
    let (data, bytes) = unsafe { (cache.peek()?, state.peek_mut()?) };
    let (limit, flags) = lru_state::mode(bytes)?;
    if limit == 0 || flags & (lru_state::TYPED | lru_state::FALLBACK) != 0 {
        return None;
    }
    let found = data.get_index_of(probe);
    if !probe.exact() {
        return None;
    }
    let Some(index) = found else {
        // A miss the caller can run in place: a plain function, and words
        // the store may resize if it must add links.
        if !MISS
            || !matches!(func, Object::Function(_))
            || (limit != lru_state::UNBOUNDED
                && (data.len() as u64) < limit
                && crate::object::bytearray_is_exported(state))
        {
            return None;
        }
        let depth = lru_enter_native().ok()?;
        lru_state::miss(bytes)?;
        return Some(LruProbe::Miss(
            func.clone(),
            LruPending {
                wrapper: Object::Unbound,
                key: key(),
                hash: probe.probe_hash(),
                _depth: depth,
            },
        ));
    };
    let value = data.get_index(index)?.1.clone();
    if limit != lru_state::UNBOUNDED {
        lru_state::Links::new(bytes, data.len())?.touch(index)?;
    }
    lru_state::hit(bytes)?;
    Some(LruProbe::Hit(value))
}

impl LruPending {
    /// Hold the wrapper (moved out of the call's operands) until the call
    /// finishes.
    #[inline(always)]
    pub(crate) fn hold(&mut self, wrapper: Object) {
        self.wrapper = wrapper;
    }

    /// Take the held wrapper back, keeping the store (a call that goes on
    /// another way).
    pub(crate) fn take_wrapper(&mut self) -> Object {
        std::mem::replace(&mut self.wrapper, Object::Unbound)
    }

    /// Take the held wrapper back (a declined call restores its operands,
    /// the miss uncounted).
    pub(crate) fn release_wrapper(mut self) -> Object {
        let wrapper = std::mem::replace(&mut self.wrapper, Object::Unbound);
        lru_uncount_miss(&wrapper);
        wrapper
    }
}

/// Store an inline miss's returned `value` (see [`LruProbe::Miss`]) as the
/// wrapper's own miss path would, without running Python code: the entry a
/// full cache evicted, for the caller to release once nothing is borrowed.
/// A store that needs Python's equality waits for the next call into the
/// native core ([`LRU_DEFERRED`]).
pub(crate) fn lru_store_returned(p: &LruPending, value: &Object) -> Option<(DictKey, Object)> {
    // SAFETY: nothing below runs code (an evicted entry is returned, not
    // released).
    let (cache, state) = unsafe { lru_cells(&p.wrapper) }?;
    // The cells in place while nothing holds them (always, but while
    // another thread shares them), else under their locks.
    // SAFETY: as above.
    let stored = match unsafe { (cache.peek_mut(), state.peek_mut()) } {
        (Some(data), Some(bytes)) => {
            let (limit, _) = lru_state::mode(bytes)?;
            native_store_in(data, bytes, state, &p.key, Some(p.hash), value, limit)
                .map(|r| (r, limit))
        }
        _ => {
            let (limit, _) = lru_state::mode(&state.borrow())?;
            lru_native_store(cache, state, &p.key, value, limit).map(|r| (r, limit))
        }
    };
    let limit = match stored {
        Ok((Some(evicted), _)) => return evicted,
        Ok((None, limit)) => limit,
        // A tampered buffer: nothing stored (the full path would raise).
        Err(_) => return None,
    };
    // The ordered-dictionary fallback, when its probe settles natively.
    {
        let probe = crate::object::LeafProbe::with_hash(&p.key, p.hash);
        let mut data = cache.borrow_mut();
        let found = data.get_index_of(&probe);
        if probe.miss_is_exact() {
            if found.is_some() {
                // A recursive call populated the key: its entry wins.
                return None;
            }
            data.insert_unique_hashed(probe.hash, DictKey(p.key.clone()), value.clone());
            let limit = usize::try_from(limit).unwrap_or(usize::MAX);
            let mut evicted = None;
            while data.len() > limit {
                // (More than one only if the cache was tampered with.)
                drop(evicted.take());
                evicted = data.shift_remove_index(0);
            }
            return evicted;
        }
    }
    LRU_DEFERRED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push((cache.clone(), state.clone(), p.key.clone(), value.clone()));
    LRU_DEFERRED_ANY.store(true, std::sync::atomic::Ordering::Release);
    None
}

fn lru_call(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let _depth_guard = lru_enter_native()?;
    let Some(Object::Instance(inst)) = args.first() else {
        return Err(type_error("expected _lru_cache_wrapper instance"));
    };
    lru_flush_deferred()?;
    let call_args = &args[1..];
    let LruParts { cache, state, func } = lru_parts(inst)?;
    let (limit, flags) = lru_state::mode(&state.borrow()).ok_or_else(invalid_lru_order)?;
    let interp = crate::builtins::reentrant_interp()?;
    let globals = interp.builtins_dict();

    // maxsize == 0: no caching, statistics only.
    if limit == 0 {
        lru_count(&state, false)?;
        return interp.call(&func, call_args, kwargs, &globals);
    }

    let bounded = limit != lru_state::UNBOUNDED;
    let key = lru_make_key(call_args, kwargs, flags & lru_state::TYPED != 0);
    // The warm shape: an `int`, `str`, or tuple of them, settled by one
    // native probe under the cache's lock.
    if let Some(probe) = crate::object::LeafProbe::new(&key).filter(|_| native_lru_key(&key)) {
        match lru_native_lookup(&cache, &state, &probe, bounded) {
            Some(NativeLookup::Hit(v)) => return Ok(v),
            Some(NativeLookup::Miss) => {
                let result = interp.call(&func, call_args, kwargs, &globals)?;
                if let Some(evicted) = lru_native_store(&cache, &state, &key, &result, limit)? {
                    // Released with the cache consistent and unborrowed.
                    drop(evicted);
                    return Ok(result);
                }
                return lru_store(&cache, &state, &key, result, limit, bounded);
            }
            None => {}
        }
    }
    // Compute a tuple key's hash before touching the cache. Successful
    // hashes are retained by the tuple, so lookup, recency updates, and
    // insertion invoke each argument's hash only once per call. Errors
    // propagate before the wrapped function or cache statistics change.
    // The scalar shortcut only produces exact integers and strings.
    if matches!(key, Object::Tuple(_)) {
        interp.do_hash_call(&key, &globals)?;
    }
    let scalar_hit = if bounded {
        scalar_lru_operation(&cache, &state, &key, None)?
    } else {
        ScalarLru::Slow
    };
    let (hit, scalar) = match scalar_hit {
        ScalarLru::Complete(hit) => (hit, true),
        ScalarLru::Slow => (
            crate::object::key_cmp_scope(|| {
                with_stolen_cache(&cache, |c| {
                    if bounded {
                        // Hit: refresh recency by moving the entry to the back.
                        match c.shift_remove(&DictKey(key.clone())) {
                            Some(v) => {
                                c.insert(DictKey(key.clone()), v.clone());
                                Some(v)
                            }
                            None => None,
                        }
                    } else {
                        c.get(&DictKey(key.clone())).cloned()
                    }
                })
            })?,
            false,
        ),
    };
    if let Some(v) = hit {
        lru_count(&state, true)?;
        return Ok(v);
    }
    lru_count(&state, false)?;
    let result = interp.call(&func, call_args, kwargs, &globals)?;
    lru_store(&cache, &state, &key, result, limit, scalar)
}

/// Store a miss's `result` under `key` (through the native links when
/// `scalar`), unless a reentrant call stored the key first.
fn lru_store(
    cache: &Rc<RefCell<DictData>>,
    state: &Rc<RefCell<Vec<u8>>>,
    key: &Object,
    result: Object,
    limit: u64,
    scalar: bool,
) -> Result<Object, RuntimeError> {
    if scalar && limit != lru_state::UNBOUNDED {
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        if matches!(
            scalar_lru_operation(cache, state, key, Some((&result, limit)))?,
            ScalarLru::Complete(_)
        ) {
            return Ok(result);
        }
    }
    crate::object::key_cmp_scope(|| {
        with_stolen_cache(cache, |c| {
            // A reentrant call may have populated the key while `func` ran;
            // keep the existing entry (CPython does the same).
            if !c.contains_key(&DictKey(key.clone())) {
                c.insert(DictKey(key.clone()), result.clone());
                if limit != lru_state::UNBOUNDED {
                    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
                    while c.len() > limit {
                        c.shift_remove_index(0);
                    }
                }
            }
        })
    })?;
    Ok(result)
}

/// Run `op` against the cache's map with **no `RefCell` borrow held**, so a
/// user `__hash__`/`__eq__` invoked by a probe can re-enter this very cache
/// without tripping a `BorrowMutError` — CPython's C `lru_cache` explicitly
/// supports single-thread reentrancy (`test_functools.test_need_for_rlock`:
/// a key's `__eq__` calls the cached function again mid-lookup).
///
/// The map is *moved out* of the `RefCell` for the duration, so a reentrant
/// call observes an empty cache: it misses, computes its value, and stores
/// it into the (temporarily empty) shared map. Those entries are merged back
/// afterwards, existing entries winning — the same "another call already
/// populated the key" rule the miss path applies.
fn with_stolen_cache<T>(cache: &Rc<RefCell<DictData>>, op: impl FnOnce(&mut DictData) -> T) -> T {
    let mut map = std::mem::take(&mut *cache.borrow_mut());
    let out = op(&mut map);
    loop {
        let pending = std::mem::take(&mut *cache.borrow_mut());
        if pending.is_empty() {
            // No Python code runs between this take and the install, so
            // no reentrant write can slip in and be lost.
            *cache.borrow_mut() = map;
            return out;
        }
        // The merge's own probes run user `__eq__` against the *owned*
        // map (still no borrow held); should one re-enter the cache yet
        // again, the next loop iteration picks its entries up too.
        for (k, v) in pending {
            map.entry(k).or_insert(v);
        }
    }
}

fn lru_cache_info(args: &[Object], _kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = lru_self(args)?;
    lru_flush_deferred()?;
    let interp = crate::builtins::reentrant_interp()?;
    let parts = lru_parts(&inst)?;
    let cls = {
        let keys = lru_keys();
        let slots = inst.slots.borrow();
        match slots.leading_interned([&keys[0], &keys[1], &keys[2], &keys[3]]) {
            Some([_, _, _, cls]) => cls.clone(),
            None => return Err(lru_lost()),
        }
    };
    let (hits, misses, limit, currsize) = {
        let data = parts.cache.borrow();
        let bytes = parts.state.borrow();
        let (hits, misses) = lru_state::counts(&bytes).ok_or_else(invalid_lru_order)?;
        let (limit, _) = lru_state::mode(&bytes).ok_or_else(invalid_lru_order)?;
        (hits, misses, limit, data.len())
    };
    let count = |n: u64| Object::Int(i64::try_from(n).unwrap_or(i64::MAX));
    let maxsize = if limit == lru_state::UNBOUNDED {
        Object::None
    } else {
        count(limit)
    };
    let currsize = Object::Int(i64::try_from(currsize).unwrap_or(i64::MAX));
    let globals = interp.builtins_dict();
    interp.call(
        &cls,
        &[count(hits), count(misses), maxsize, currsize],
        &[],
        &globals,
    )
}

fn lru_cache_clear(args: &[Object], _kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = lru_self(args)?;
    lru_flush_deferred()?;
    let LruParts { cache, state, .. } = lru_parts(&inst)?;
    let retired = {
        // The cache's lock first, then its words (as every caller takes
        // them): a concurrent call can't see the one cleared without the
        // other.
        let mut data = cache.borrow_mut();
        let mut bytes = state.borrow_mut();
        let (limit, flags) = lru_state::mode(&bytes).ok_or_else(invalid_lru_order)?;
        if limit != 0
            && limit != lru_state::UNBOUNDED
            && flags & lru_state::FALLBACK == 0
            && !data.is_empty()
        {
            crate::object::bytearray_check_resizable(&state)?;
            lru_state::clear_links(&mut bytes).ok_or_else(invalid_lru_order)?;
        }
        lru_state::reset_counts(&mut bytes).ok_or_else(invalid_lru_order)?;
        std::mem::take(&mut *data)
    };
    // The entries die with the cache empty and unborrowed (their
    // finalizers may call the wrapper).
    drop(retired);
    Ok(Object::None)
}

/// `__copy__` / `__deepcopy__`: CPython's `lru_cache_copy`/`_deepcopy`
/// return the wrapper itself (`copy.copy(cached_f) is cached_f`).
fn lru_identity(args: &[Object], _kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    Ok(Object::Instance(lru_self(args)?))
}

/// `__reduce__`: CPython's `lru_cache_reduce` returns `self.__qualname__`,
/// making the wrapper pickle by reference (looked up in its module on
/// unpickling), exactly like a plain function.
fn lru_reduce(args: &[Object], _kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = lru_self(args)?;
    let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() else {
        return Err(type_error("__reduce__ requires a running interpreter"));
    };
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    interp.load_attr_public(&Object::Instance(inst), "__qualname__")
}

/// Descriptor protocol: a cached function stored on a class binds like a
/// plain function (CPython's `lru_cache_descr_get`), so `@lru_cache` on a
/// method receives `self` as the first cached argument.
fn lru_descr_get(args: &[Object], _kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = lru_self(args)?;
    let obj = args.get(1).cloned().unwrap_or(Object::None);
    if matches!(obj, Object::None) {
        return Ok(Object::Instance(inst));
    }
    Ok(Object::BoundMethod(Rc::new(
        crate::object::BoundMethod::new(obj, Object::Instance(inst)),
    )))
}

/// `reduce(function, iterable[, initial])` — native loop, no Python
/// frame per step (CPython's `_functools.reduce`).
fn reduce(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    // 3.14 (gh-125916): `initial` may be passed by keyword; `function`
    // and `iterable` remain positional-only.
    if args.len() < 2 {
        return Err(type_error(format!(
            "reduce() takes at least 2 positional arguments ({} given)",
            args.len()
        )));
    }
    let mut args: Vec<Object> = args.to_vec();
    for (k, v) in kwargs {
        if k == "initial" {
            args.push(v.clone());
        } else {
            return Err(type_error(format!(
                "reduce() got an unexpected keyword argument '{k}'"
            )));
        }
    }
    if args.len() > 3 {
        return Err(type_error(format!(
            "reduce() takes at most 3 arguments ({} given)",
            args.len()
        )));
    }
    let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() else {
        return Err(type_error("reduce() requires a running interpreter"));
    };
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    let function = &args[0];
    let globals = interp.builtins_dict();
    let it = match interp.make_iter(&args[1], &globals) {
        Ok(it) => it,
        Err(e) if crate::is_type_error(&e) => {
            return Err(type_error("reduce() arg 2 must support iteration"))
        }
        Err(e) => return Err(e),
    };
    let mut acc = match args.get(2) {
        Some(initial) => initial.clone(),
        None => match interp.iter_next(&it, &globals)? {
            Some(first) => first,
            None => {
                return Err(type_error(
                    "reduce() of empty iterable with no initial value",
                ))
            }
        },
    };
    while let Some(x) = interp.iter_next(&it, &globals)? {
        acc = interp.call(function, &[acc, x], &[], &globals)?;
    }
    Ok(acc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_lru_preserves_order_and_owners() {
        const CHILD: &str = "WEAVEPY_TEST_SCALAR_LRU_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "stdlib::functools_mod::tests::scalar_lru_preserves_order_and_owners",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("WEAVEPY_JIT", "0")
                .status()
                .expect("spawn scalar LRU test");
            assert!(status.success(), "scalar LRU child: {status}");
            return;
        }
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let mut interp = crate::Interpreter::new();
                SCALAR_LRU_EVENTS.with(|events| events.set([0; 3]));
                for (name, source) in [
                    (
                        "test_lru_scalar_order.py",
                        include_str!("../../../../tests/regrtest/test_lru_scalar_order.py"),
                    ),
                    (
                        "test_lru_scalar_owners.py",
                        include_str!("../../../../tests/regrtest/test_lru_scalar_owners.py"),
                    ),
                    (
                        "test_lru_native_tuples.py",
                        include_str!("../../../../tests/regrtest/test_lru_native_tuples.py"),
                    ),
                    (
                        "test_lru_typed_native.py",
                        include_str!("../../../../tests/regrtest/test_lru_typed_native.py"),
                    ),
                ] {
                    let module = weavepy_parser::parse_module(source).unwrap();
                    let code = weavepy_compiler::compile_module_with_source(&module, source, name)
                        .unwrap();
                    interp.run_module(&code).unwrap();
                }
                let counts = SCALAR_LRU_EVENTS.with(std::cell::Cell::get);
                assert!(
                    counts[0] > 3000 && counts[1] > 100 && counts[2] >= 3,
                    "{counts:?}"
                );
                eprintln!("Scalar LRU hits, evictions, fallbacks: {counts:?}");
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
