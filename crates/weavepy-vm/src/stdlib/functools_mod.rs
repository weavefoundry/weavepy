//! The `_functools` built-in module — Rust core for `functools`.
//!
//! CPython implements `functools.partial` in C, so calling a partial
//! pushes no Python frame and leaves no traceback entry. The frozen
//! Python `partial` class delegates `__call__` here to match that:
//! `test_traceback` asserts that a `partial(exec, …)` call site shows
//! only the caller's frame.

mod lru_order;

use indexmap::map::RawEntryApiV1;

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule, StrKey};

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
                name: "__call__",
                binds_instance: false,
                call: Box::new(|args| partial_call(args, &[])),
                call_kw: Some(Box::new(partial_call)),
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
    }
    Rc::new(PyModule {
        name: "_functools".to_owned(),
        filename: None,
        dict,
    })
}

/// `partial.__call__(self, /, *args, **keywords)` without a Python
/// frame: merge stored args/keywords with the call's and tail-call
/// `self.func` through the interpreter.
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
    // 3.14 `functools.Placeholder`: a partial with placeholders merges
    // call-site positionals into the stored tuple. That path stays in
    // the frozen `functools.py` (`_partial_call_py`); the native fast
    // path covers the common placeholder-free case.
    if let Ok(phcount) = interp.load_attr_public(slf, "_phcount") {
        if !matches!(phcount, Object::Int(0)) {
            let py_call = interp.load_attr_public(slf, "_partial_call_py")?;
            let globals = interp.builtins_dict();
            return interp.call(&py_call, &args[1..], kwargs, &globals);
        }
    }
    let func = interp.load_attr_public(slf, "func")?;
    let stored_args = interp.load_attr_public(slf, "args")?;
    let stored_kw = interp.load_attr_public(slf, "keywords")?;

    let mut call_args: Vec<Object> = match &stored_args {
        Object::Tuple(xs) => xs.to_vec(),
        _ => return Err(type_error("partial 'args' must be a tuple")),
    };
    call_args.extend_from_slice(&args[1..]);

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

    let globals = interp.builtins_dict();
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
// ---------------------------------------------------------------------------

/// Sentinel string separating positional from keyword parts of a cache key
/// (CPython uses a fresh `object()`; the embedded NULs keep any real user
/// string from colliding).
const LRU_KWD_MARK: &str = "\0\0__weavepy_lru_kwd_mark__\0\0";

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

fn lru_get(inst: &crate::types::PyInstance, name: &'static str) -> Option<Object> {
    inst.dict_cell().borrow().get(&StrKey(name)).cloned()
}

fn lru_set(inst: &crate::types::PyInstance, name: &'static str, v: Object) {
    inst.dict_cell()
        .borrow_mut()
        .insert(DictKey(Object::from_static(name)), v);
}

/// `_lru_cache_wrapper(user_function, maxsize, typed, _CacheInfo)`.
fn lru_cache_wrapper_new(args: &[Object]) -> Result<Object, RuntimeError> {
    let [user_function, maxsize, typed, cache_info_cls] = args else {
        return Err(type_error("_lru_cache_wrapper() takes exactly 4 arguments"));
    };
    match maxsize {
        Object::None | Object::Int(_) => {}
        _ => return Err(type_error("maxsize should be integer or None")),
    }
    let inst = Rc::new(crate::types::PyInstance::new(lru_type()));
    lru_set(&inst, "__wrapped__", user_function.clone());
    lru_set(&inst, "_lru_maxsize", maxsize.clone());
    // Untyped caches reuse this field for recency state once called. Adding
    // another field would grow a normally decorated wrapper's dictionary.
    lru_set(&inst, "_lru_state", Object::Bool(typed.is_truthy()));
    lru_set(
        &inst,
        "_lru_cache",
        Object::Dict(Rc::new(RefCell::new(DictData::default()))),
    );
    lru_set(&inst, "_lru_hits", Object::Int(0));
    lru_set(&inst, "_lru_misses", Object::Int(0));
    lru_set(&inst, "_lru_cache_info_cls", cache_info_cls.clone());
    // CPython's `lru_cache_new` GC-tracks the wrapper: its `__wrapped__`
    // edge closes the `module dict -> wrapper -> function -> __globals__`
    // cycle of every `@lru_cache` at module level, and an untracked
    // wrapper counted as an external root that made that namespace
    // immortal.
    let wrapper = Object::Instance(inst);
    crate::gc_trace::track(wrapper.clone());
    Ok(wrapper)
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
        parts.push(Object::from_static(LRU_KWD_MARK));
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

fn lru_counter_bump(inst: &crate::types::PyInstance, name: &'static str) {
    // Keep the read and increment under one lock. Separate get/set borrows
    // lose updates when multiple threads hit a shared cache under gil=0.
    let mut dict = inst.dict_cell().borrow_mut();
    let (_, count) = dict
        .raw_entry_mut_v1()
        .from_key(&StrKey(name))
        .or_insert_with(|| (DictKey(Object::from_static(name)), Object::Int(0)));
    match count {
        Object::Int(n) => *n += 1,
        _ => *count = Object::Int(1),
    }
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

fn scalar_lru_key(key: &Object) -> bool {
    matches!(key, Object::Int(_) | Object::Long(_) | Object::Str(_))
}

fn invalid_lru_order() -> RuntimeError {
    type_error("lru_cache has inconsistent private recency data")
}

/// Once a cache admits a key that can invoke Python, retain its existing
/// ordered-map implementation. The scalar representation holds no Python
/// owners outside that map and never changes the layout of ordinary objects.
fn scalar_lru_links(
    inst: &crate::types::PyInstance,
    scalar: bool,
) -> Result<Option<Rc<RefCell<Vec<u8>>>>, RuntimeError> {
    let mut dict = inst.dict_cell().borrow_mut();
    let (_, state) = dict
        .raw_entry_mut_v1()
        .from_key(&StrKey("_lru_state"))
        .or_insert_with(|| {
            (
                DictKey(Object::from_static("_lru_state")),
                Object::Bool(false),
            )
        });
    // True remains a typed cache; false is an uninitialized untyped cache.
    // None permanently disables scalar mode, and a bytearray owns the links.
    if matches!(state, Object::Bool(false)) {
        *state = if scalar {
            Object::ByteArray(Rc::new(RefCell::new(lru_order::empty())))
        } else {
            Object::None
        };
    }
    match state {
        Object::ByteArray(links) => Ok(Some(links.clone())),
        Object::Bool(true) | Object::None => Ok(None),
        _ => Err(invalid_lru_order()),
    }
}

fn scalar_lru_operation(
    inst: &crate::types::PyInstance,
    cache: &Rc<RefCell<DictData>>,
    key: &Object,
    insert: Option<(&Object, usize)>,
) -> Result<ScalarLru, RuntimeError> {
    let scalar = scalar_lru_key(key);
    let Some(links) = scalar_lru_links(inst, scalar)? else {
        return Ok(ScalarLru::Slow);
    };
    let removed;
    {
        // The shared cache lock also serializes its recency metadata under
        // gil=0. Neither key comparison nor an owner release may run Python
        // while these two cells are borrowed.
        let mut data = cache.borrow_mut();
        let mut bytes = links.borrow_mut();
        if bytes.is_empty() {
            return Ok(ScalarLru::Slow);
        }
        let mut order =
            lru_order::Order::new(&mut bytes, data.len()).ok_or_else(invalid_lru_order)?;
        if !scalar || insert.is_some_and(|(_, limit)| data.len() > limit) {
            crate::object::bytearray_check_resizable(&links)?;
            let indices = order.indices().ok_or_else(invalid_lru_order)?;
            if !data.keys().all(|stored| scalar_lru_key(&stored.0)) {
                return Err(invalid_lru_order());
            }
            // Restore logical oldest-to-newest order once, before any
            // callback-capable comparison reaches the existing slow path.
            let old = std::mem::take(&mut *data);
            let mut entries: Vec<_> = old.into_iter().map(Some).collect();
            for index in indices {
                let (key, value) = entries
                    .get_mut(index)
                    .and_then(Option::take)
                    .ok_or_else(invalid_lru_order)?;
                data.insert(key, value);
            }
            bytes.clear();
            // Keep the empty buffer for in-flight users of this snapshot,
            // but let future calls decline from their existing state read.
            lru_set(inst, "_lru_state", Object::None);
            #[cfg(test)]
            note_scalar_lru_event(2);
            return Ok(ScalarLru::Slow);
        }
        // Deferral also protects against private-cache tampering: a foreign
        // key must not invoke Python inside the paired borrow.
        let (found, deferred) = crate::object::with_key_eq_deferred(|| {
            crate::object::key_cmp_scope(|| data.get_index_of(&DictKey(key.clone())))
        });
        let found = found?;
        if deferred {
            return Err(invalid_lru_order());
        }
        if let Some(index) = found {
            let (stored, value) = data.get_index(index).ok_or_else(invalid_lru_order)?;
            if !scalar_lru_key(&stored.0) {
                return Err(invalid_lru_order());
            }
            // A recursive call may have populated a miss. Its entry and
            // recency win; only an actual hit refreshes the order.
            return if insert.is_some() {
                Ok(ScalarLru::Complete(None))
            } else {
                order.touch(index).ok_or_else(invalid_lru_order)?;
                #[cfg(test)]
                note_scalar_lru_event(0);
                Ok(ScalarLru::Complete(Some(value.clone())))
            };
        }
        let Some((value, limit)) = insert else {
            return Ok(ScalarLru::Complete(None));
        };
        crate::object::bytearray_check_resizable(&links)?;
        removed = if data.len() == limit {
            let index = order.head().ok_or_else(invalid_lru_order)?;
            order.swap_remove(index).ok_or_else(invalid_lru_order)?;
            #[cfg(test)]
            note_scalar_lru_event(1);
            Some(
                data.swap_remove_index(index)
                    .ok_or_else(invalid_lru_order)?,
            )
        } else {
            None
        };
        data.insert(DictKey(key.clone()), value.clone());
        order.push(limit).ok_or_else(invalid_lru_order)?;
    }
    // Publish a consistent cache before releasing an evicted result. The
    // prompt-reap queue preserves its finalizer's real calling frame.
    if let Some((_, value)) = removed {
        crate::vm_singletons::queue_container_removed(&value);
    }
    Ok(ScalarLru::Complete(None))
}

fn lru_call(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let _depth_guard = lru_enter_native()?;
    let inst = lru_self(args)?;
    let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() else {
        return Err(type_error("lru_cache requires a running interpreter"));
    };
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    let func = lru_get(&inst, "__wrapped__")
        .ok_or_else(|| type_error("lru_cache wrapper lost its function"))?;
    let maxsize = lru_get(&inst, "_lru_maxsize").unwrap_or(Object::None);
    let call_args = &args[1..];
    let globals = interp.builtins_dict();

    // maxsize == 0: no caching, statistics only.
    if matches!(maxsize, Object::Int(0)) {
        lru_counter_bump(&inst, "_lru_misses");
        return interp.call(&func, call_args, kwargs, &globals);
    }

    let (typed, scalar_enabled) = match lru_get(&inst, "_lru_state") {
        Some(Object::Bool(true)) => (true, false),
        Some(Object::None) => (false, false),
        _ => (false, true),
    };
    let key = lru_make_key(call_args, kwargs, typed);
    // Compute a tuple key's hash before touching the cache. Successful
    // hashes are retained by the tuple, so lookup, recency updates, and
    // insertion invoke each argument's hash only once per call. Errors
    // propagate before the wrapped function or cache statistics change.
    // The scalar shortcut only produces exact integers and strings.
    if matches!(key, Object::Tuple(_)) {
        interp.do_hash_call(&key, &globals)?;
    }
    let Some(Object::Dict(cache)) = lru_get(&inst, "_lru_cache") else {
        return Err(type_error("lru_cache wrapper lost its cache"));
    };
    let bounded = matches!(maxsize, Object::Int(m) if m > 0);

    let scalar_hit = if bounded && scalar_enabled {
        scalar_lru_operation(&inst, &cache, &key, None)?
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
        lru_counter_bump(&inst, "_lru_hits");
        return Ok(v);
    }
    lru_counter_bump(&inst, "_lru_misses");
    let result = interp.call(&func, call_args, kwargs, &globals)?;
    if scalar {
        let Object::Int(limit) = maxsize else {
            unreachable!()
        };
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        if matches!(
            scalar_lru_operation(&inst, &cache, &key, Some((&result, limit)))?,
            ScalarLru::Complete(_)
        ) {
            return Ok(result);
        }
    }
    crate::object::key_cmp_scope(|| {
        with_stolen_cache(&cache, |c| {
            // A reentrant call may have populated the key while `func` ran;
            // keep the existing entry (CPython does the same).
            if !c.contains_key(&DictKey(key.clone())) {
                c.insert(DictKey(key.clone()), result.clone());
                if let Object::Int(m) = maxsize {
                    while c.len() > m.max(0) as usize {
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
    let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() else {
        return Err(type_error("cache_info requires a running interpreter"));
    };
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    let cls = lru_get(&inst, "_lru_cache_info_cls")
        .ok_or_else(|| type_error("lru_cache wrapper lost its CacheInfo class"))?;
    let hits = lru_get(&inst, "_lru_hits").unwrap_or(Object::Int(0));
    let misses = lru_get(&inst, "_lru_misses").unwrap_or(Object::Int(0));
    let maxsize = lru_get(&inst, "_lru_maxsize").unwrap_or(Object::None);
    let currsize = match lru_get(&inst, "_lru_cache") {
        Some(Object::Dict(c)) => Object::Int(c.borrow().len() as i64),
        _ => Object::Int(0),
    };
    let globals = interp.builtins_dict();
    interp.call(&cls, &[hits, misses, maxsize, currsize], &[], &globals)
}

fn lru_cache_clear(args: &[Object], _kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = lru_self(args)?;
    if let Some(Object::Dict(c)) = lru_get(&inst, "_lru_cache") {
        let retired = {
            // Read the mode after locking the cache: a concurrent first call
            // may initialize recency storage before we acquire this lock.
            let mut cache = c.borrow_mut();
            if let Some(Object::ByteArray(links)) = lru_get(&inst, "_lru_state") {
                let mut bytes = links.borrow_mut();
                if bytes.is_empty() {
                    cache.clear();
                    None
                } else {
                    crate::object::bytearray_check_resizable(&links)?;
                    *bytes = lru_order::empty();
                    Some(std::mem::take(&mut *cache))
                }
            } else {
                cache.clear();
                None
            }
        };
        if let Some(retired) = retired {
            for (_, value) in retired {
                crate::vm_singletons::queue_container_removed(&value);
            }
        }
    }
    lru_set(&inst, "_lru_hits", Object::Int(0));
    lru_set(&inst, "_lru_misses", Object::Int(0));
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
    let function = args[0].clone();
    let it = interp.iter_object(args[1].clone())?;
    let mut acc = match args.get(2) {
        Some(initial) => initial.clone(),
        None => match interp.iter_next_object(it.clone())? {
            Some(first) => first,
            None => {
                return Err(type_error(
                    "reduce() of empty iterable with no initial value",
                ))
            }
        },
    };
    let globals = interp.builtins_dict();
    while let Some(x) = interp.iter_next_object(it.clone())? {
        acc = interp.call(&function, &[acc, x], &[], &globals)?;
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
