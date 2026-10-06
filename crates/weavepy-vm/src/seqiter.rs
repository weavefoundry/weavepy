//! The builtin `map`, `filter`, and `zip` types, and `enumerate` over a
//! source only the interpreter can step.
//!
//! CPython implements these in C: an exact instance is a small native
//! object whose `__next__` pushes no Python frame. WeavePy's exact
//! instances are [`Object::LazyIter`]s of the matching
//! [`LazyIterKind`], built and stepped here without running any Python
//! code of their own, so `for x in map(f, xs)` costs one call of `f` per
//! item. A subclass instance (`class M(map)`) is a `PyInstance` whose
//! native payload is such an adapter; the type's methods below serve
//! both forms, and a subclass's own `__next__` overrides them as usual.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::builtin_types::{builtin_types, BuiltinTypes};
use crate::error::{type_error, value_error, RuntimeError};
use crate::object::{BuiltinFn, DictData, DictKey, LazyIterKind, Object, PyIterator, PyLazyIter};
use crate::types::{PyInstance, TypeObject};
use crate::Interpreter;

/// Which builtin adapter type a class is (or derives from).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeqType {
    Map,
    Filter,
    Zip,
}

impl SeqType {
    fn name(self) -> &'static str {
        match self {
            SeqType::Map => "map",
            SeqType::Filter => "filter",
            SeqType::Zip => "zip",
        }
    }

    fn type_object(self) -> Rc<TypeObject> {
        let bt = builtin_types();
        match self {
            SeqType::Map => bt.map_.clone(),
            SeqType::Filter => bt.filter_.clone(),
            SeqType::Zip => bt.zip_.clone(),
        }
    }

    /// The exact type `ty` is, if it is one of these.
    pub(crate) fn of_exact(ty: &Rc<TypeObject>) -> Option<Self> {
        let bt = builtin_types();
        if Rc::ptr_eq(ty, &bt.map_) {
            Some(SeqType::Map)
        } else if Rc::ptr_eq(ty, &bt.filter_) {
            Some(SeqType::Filter)
        } else if Rc::ptr_eq(ty, &bt.zip_) {
            Some(SeqType::Zip)
        } else {
            None
        }
    }

    /// The adapter type `ty` is or derives from, if any.
    fn of_class(ty: &Rc<TypeObject>) -> Option<Self> {
        let bt = builtin_types();
        if ty.is_subclass_of(&bt.map_) {
            Some(SeqType::Map)
        } else if ty.is_subclass_of(&bt.filter_) {
            Some(SeqType::Filter)
        } else if ty.is_subclass_of(&bt.zip_) {
            Some(SeqType::Zip)
        } else {
            None
        }
    }
}

fn lazy(kind: LazyIterKind) -> Object {
    Object::LazyIter(Rc::new(PyLazyIter::new(kind)))
}

/// `iter(obj)` for a constructor argument when it needs no interpreter:
/// an iterator is its own iterator, and the native containers build
/// theirs directly. `None` for anything that might run Python code.
fn pure_iter(obj: &Object) -> Option<Object> {
    match obj {
        Object::Iter(_) | Object::Generator(_) | Object::LazyIter(_) => Some(obj.clone()),
        Object::List(_)
        | Object::Tuple(_)
        | Object::Str(_)
        | Object::Range(_)
        | Object::Dict(_)
        | Object::Set(_)
        | Object::FrozenSet(_)
        | Object::Bytes(_) => Some(Object::Iter(Rc::new(RefCell::new(obj.make_iter().ok()?)))),
        _ => None,
    }
}

/// `"argument 1"` / `"arguments 1-N"`: the strict-mode mismatch wording.
fn arg_range(count: usize) -> String {
    if count == 1 {
        "argument 1".to_owned()
    } else {
        format!("arguments 1-{count}")
    }
}

impl Interpreter {
    /// `next(it)` for an adapter's source: a native iterator steps in
    /// place, anything else through the iterator protocol.
    #[inline]
    fn seq_step_source(
        &mut self,
        it: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Option<Object>, RuntimeError> {
        if let Object::Iter(c) = it {
            return c.borrow_mut().next_value_checked();
        }
        self.iter_next(it, globals)
    }

    /// `func(v)` for an adapter's callback: a plain function through
    /// the pure-leaf evaluator when it qualifies, else a full call.
    #[inline]
    pub(crate) fn seq_call1(
        &mut self,
        func: &Object,
        v: Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        if let Object::Function(f) = func {
            if let Some(r) = self.call_pure_leaf(f, std::slice::from_ref(&v)) {
                return Ok(r);
            }
            return self.call_python_owned(f, vec![v], Vec::new());
        }
        self.call(func, std::slice::from_ref(&v), &[], globals)
    }

    /// Drain a builtin adapter into `out` (`list(map(f, xs))`, `sorted`,
    /// `sum`, ...): `false` when `l` is another kind, nothing consumed.
    /// A `map` or `filter` over one source can't change its callback or
    /// source while it runs, so both are read once for the whole drain.
    pub(crate) fn seq_drain(
        &mut self,
        l: &Rc<PyLazyIter>,
        out: &mut Vec<Object>,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<bool, RuntimeError> {
        let (func, source, filter) = match &*l.state.borrow() {
            LazyIterKind::Map { func, iters, .. } if iters.len() == 1 => {
                (func.clone(), iters[0].clone(), false)
            }
            LazyIterKind::Filter { func, source } => (func.clone(), source.clone(), true),
            LazyIterKind::Repeat { times: Some(n), .. } => {
                out.reserve(usize::try_from(*n).unwrap_or(0).min(1 << 16));
                return Ok(false);
            }
            _ => return Ok(false),
        };
        if let Object::Iter(c) = &source {
            if let Some(n) = c.try_borrow().ok().and_then(|c| c.remaining()) {
                out.reserve(if filter { n.min(64) } else { n });
            }
        }
        if filter {
            while let Some(v) = self.filter_next(&func, &source, globals)? {
                out.push(v);
            }
            return Ok(true);
        }
        while let Some(v) = self.seq_step_source(&source, globals)? {
            let r = self.seq_call1(&func, v, globals)?;
            out.push(r);
        }
        Ok(true)
    }

    /// An exact `map`/`filter`/`zip` built from positional arguments
    /// without the interpreter (the core loop's `CALL`): `None` when an
    /// argument needs `iter()` to run code, or the call would raise.
    pub(crate) fn seq_new_pure(ty: SeqType, args: &[Object]) -> Option<Object> {
        Some(match ty {
            SeqType::Map => {
                let (func, sources) = args.split_first()?;
                if sources.is_empty() {
                    return None;
                }
                let iters = sources.iter().map(pure_iter).collect::<Option<Vec<_>>>()?;
                lazy(LazyIterKind::Map {
                    func: func.clone(),
                    iters,
                    strict: false,
                })
            }
            SeqType::Filter => {
                let [func, source] = args else {
                    return None;
                };
                lazy(LazyIterKind::Filter {
                    func: func.clone(),
                    source: pure_iter(source)?,
                })
            }
            SeqType::Zip => {
                let iters = args.iter().map(pure_iter).collect::<Option<Vec<_>>>()?;
                lazy(LazyIterKind::Zip {
                    iters,
                    strict: false,
                    result: None,
                })
            }
        })
    }

    /// The `strict=` keyword of `map`/`zip` (`|$p` in CPython: truth of
    /// the value); any other keyword is rejected.
    fn seq_strict_kw(
        &mut self,
        ty: SeqType,
        kwargs: &[(String, Object)],
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<bool, RuntimeError> {
        let mut strict = false;
        for (k, v) in kwargs {
            if k != "strict" {
                return Err(type_error(format!(
                    "{}() got an unexpected keyword argument '{k}'",
                    ty.name()
                )));
            }
            strict = self.obj_truthy(v, globals)?;
        }
        Ok(strict)
    }

    /// `map(...)`, `filter(...)`, or `zip(...)`: the adapter state for
    /// `cls` (the exact type, or a subclass whose payload this becomes),
    /// with CPython's argument checks and `iter()` of every source at
    /// construction.
    fn seq_build(
        &mut self,
        ty: SeqType,
        cls: &Rc<TypeObject>,
        args: &[Object],
        kwargs: &[(String, Object)],
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        match ty {
            SeqType::Map => {
                let strict = self.seq_strict_kw(ty, kwargs, globals)?;
                if args.len() < 2 {
                    return Err(type_error("map() must have at least two arguments."));
                }
                let mut iters = Vec::with_capacity(args.len() - 1);
                for a in &args[1..] {
                    iters.push(self.make_iter(a, globals)?);
                }
                Ok(lazy(LazyIterKind::Map {
                    func: args[0].clone(),
                    iters,
                    strict,
                }))
            }
            SeqType::Filter => {
                // CPython's `filter_new`: keywords are an error unless a
                // subclass overrides `__init__` (which then owns them).
                if !kwargs.is_empty() {
                    let own_init = !Rc::ptr_eq(cls, &builtin_types().filter_)
                        && cls
                            .lookup("__init__")
                            .is_some_and(|i| !matches!(i, Object::Builtin(_)));
                    if !own_init {
                        return Err(type_error("filter() takes no keyword arguments"));
                    }
                }
                if args.len() != 2 {
                    return Err(type_error(format!(
                        "filter expected 2 arguments, got {}",
                        args.len()
                    )));
                }
                let source = self.make_iter(&args[1], globals)?;
                Ok(lazy(LazyIterKind::Filter {
                    func: args[0].clone(),
                    source,
                }))
            }
            SeqType::Zip => {
                let strict = self.seq_strict_kw(ty, kwargs, globals)?;
                let mut iters = Vec::with_capacity(args.len());
                for a in args {
                    iters.push(self.make_iter(a, globals)?);
                }
                Ok(lazy(LazyIterKind::Zip {
                    iters,
                    strict,
                    result: None,
                }))
            }
        }
    }

    /// Call `map`, `filter`, or `zip` (the exact type).
    pub(crate) fn seq_new(
        &mut self,
        ty: SeqType,
        args: &[Object],
        kwargs: &[(String, Object)],
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        if kwargs.is_empty() {
            if let Some(obj) = Self::seq_new_pure(ty, args) {
                return Ok(obj);
            }
        }
        self.seq_build(ty, &ty.type_object(), args, kwargs, globals)
    }

    /// `enumerate(source, start)` over a source only the interpreter can
    /// iterate; `start` is already an index (`Int` or `Long`).
    pub(crate) fn enumerate_lazy(
        &mut self,
        iterable: &Object,
        start: Option<&Object>,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        let source = self.make_iter(iterable, globals)?;
        let (count, count_big) = match start {
            None => (0, None),
            Some(s) => match crate::builtins::coerce_index_object(s)? {
                Object::Int(i) => (i, None),
                Object::Long(b) => match i64::try_from(&*b) {
                    Ok(i) => (i, None),
                    Err(_) => (0, Some(Object::Long(b))),
                },
                other => (0, Some(other)),
            },
        };
        Ok(lazy(LazyIterKind::Enumerate {
            source,
            count,
            count_big,
        }))
    }

    /// Step a `map`, `filter`, `zip`, or `enumerate` adapter: `None`
    /// when `l` is another kind.
    pub(crate) fn seq_lazy_next(
        &mut self,
        l: &Rc<PyLazyIter>,
        globals: &Rc<RefCell<DictData>>,
    ) -> Option<Result<Option<Object>, RuntimeError>> {
        enum Step {
            Map1(Object, Object),
            MapN(Object, usize, bool),
            Filter(Object, Object),
            Zip(usize, bool),
            Enumerate(Object),
        }
        // Copy out what the step needs: the callbacks and sources may
        // re-enter this very adapter, so no borrow survives a call.
        let step = match &*l.state.borrow() {
            LazyIterKind::Map {
                func,
                iters,
                strict,
            } => {
                if iters.len() == 1 {
                    Step::Map1(func.clone(), iters[0].clone())
                } else {
                    Step::MapN(func.clone(), iters.len(), *strict)
                }
            }
            LazyIterKind::Filter { func, source } => Step::Filter(func.clone(), source.clone()),
            LazyIterKind::Zip { iters, strict, .. } => Step::Zip(iters.len(), *strict),
            LazyIterKind::Enumerate { source, .. } => Step::Enumerate(source.clone()),
            _ => return None,
        };
        Some(match step {
            Step::Map1(func, it) => match self.seq_step_source(&it, globals) {
                Ok(Some(v)) => self.seq_call1(&func, v, globals).map(Some),
                other => other,
            },
            Step::MapN(func, n, strict) => match self.seq_pull_all(l, n, strict, "map", globals) {
                Ok(Some(args)) => self.call(&func, &args, &[], globals).map(Some),
                Ok(None) => Ok(None),
                Err(e) => Err(e),
            },
            Step::Filter(func, source) => self.filter_next(&func, &source, globals),
            Step::Zip(n, strict) => {
                if n == 0 {
                    Ok(None)
                } else {
                    self.seq_pull_all(l, n, strict, "zip", globals)
                        .map(|r| r.map(Object::new_tuple))
                }
            }
            Step::Enumerate(source) => match self.seq_step_source(&source, globals) {
                Ok(Some(v)) => Ok(Some(Self::enumerate_advance(l, v))),
                other => other,
            },
        })
    }

    /// The `(index, value)` pair for `v`, advancing a lazy enumerate's
    /// counter (CPython counts only produced items).
    fn enumerate_advance(l: &PyLazyIter, v: Object) -> Object {
        let mut st = l.state.borrow_mut();
        let LazyIterKind::Enumerate {
            count, count_big, ..
        } = &mut *st
        else {
            return Object::new_tuple_array([Object::None, v]);
        };
        let idx = match count_big {
            Some(big) => {
                let cur = big.clone();
                let next = match &cur {
                    Object::Long(b) => Object::int_from_bigint((**b).clone() + 1),
                    Object::Int(i) => Object::int_from_bigint(num_bigint::BigInt::from(*i) + 1),
                    other => other.clone(),
                };
                *big = next;
                cur
            }
            None => {
                let i = *count;
                match count.checked_add(1) {
                    Some(n) => *count = n,
                    None => {
                        *count_big = Some(Object::int_from_bigint(num_bigint::BigInt::from(i) + 1))
                    }
                }
                Object::Int(i)
            }
        };
        Object::new_tuple_array([idx, v])
    }

    /// One item from each of the `n` sources of a `map`/`zip`, or `None`
    /// at the shortest's end, with the strict-mode length checks.
    fn seq_pull_all(
        &mut self,
        l: &PyLazyIter,
        n: usize,
        strict: bool,
        name: &str,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Option<Vec<Object>>, RuntimeError> {
        let source = |l: &PyLazyIter, i: usize| -> Option<Object> {
            match &*l.state.borrow() {
                LazyIterKind::Map { iters, .. } | LazyIterKind::Zip { iters, .. } => {
                    iters.get(i).cloned()
                }
                _ => None,
            }
        };
        let mut items = Vec::with_capacity(n);
        for i in 0..n {
            let Some(it) = source(l, i) else {
                return Ok(None);
            };
            match self.seq_step_source(&it, globals)? {
                Some(v) => items.push(v),
                None if !strict => return Ok(None),
                None => {
                    if i > 0 {
                        return Err(value_error(format!(
                            "{name}() argument {} is shorter than {}",
                            i + 1,
                            arg_range(i)
                        )));
                    }
                    // The first source is exhausted: strict mode requires
                    // every other to be exhausted too.
                    for j in 1..n {
                        let Some(it) = source(l, j) else {
                            break;
                        };
                        if self.seq_step_source(&it, globals)?.is_some() {
                            return Err(value_error(format!(
                                "{name}() argument {} is longer than {}",
                                j + 1,
                                arg_range(j)
                            )));
                        }
                    }
                    return Ok(None);
                }
            }
        }
        Ok(Some(items))
    }

    /// CPython's `filter_next`: `None` and `bool` test items directly.
    fn filter_next(
        &mut self,
        func: &Object,
        source: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Option<Object>, RuntimeError> {
        let check_true = match func {
            Object::None => true,
            Object::Type(t) => Rc::ptr_eq(t, &builtin_types().bool_),
            _ => false,
        };
        loop {
            let Some(item) = self.seq_step_source(source, globals)? else {
                return Ok(None);
            };
            let keep = if check_true {
                self.obj_truthy(&item, globals)?
            } else {
                let r = self.seq_call1(func, item.clone(), globals)?;
                self.obj_truthy(&r, globals)?
            };
            if keep {
                return Ok(Some(item));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The types' methods. Each serves an exact instance (the adapter itself)
// and a subclass instance (whose native payload is the adapter).
// ---------------------------------------------------------------------------

/// The adapter behind `obj`, an exact instance or a subclass instance.
fn adapter_of(obj: &Object) -> Option<Rc<PyLazyIter>> {
    match obj {
        Object::LazyIter(l) => Some(l.clone()),
        Object::Instance(inst) => match inst.native.get() {
            Some(Object::LazyIter(l)) => Some(l.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn interp() -> Result<&'static mut Interpreter, RuntimeError> {
    crate::builtins::reentrant_interp()
}

fn descriptor_error(name: &str, ty: SeqType) -> RuntimeError {
    type_error(format!(
        "descriptor '{name}' requires a '{}' object",
        ty.name()
    ))
}

/// `type.__new__(cls, *args, **kwargs)`.
fn seq_type_new(
    ty: SeqType,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let Some(Object::Type(cls)) = args.first() else {
        return Err(type_error(format!(
            "{}.__new__(X): X is not a type object",
            ty.name()
        )));
    };
    if SeqType::of_class(cls) != Some(ty) {
        return Err(type_error(format!(
            "{}.__new__({}): {} is not a subtype of {}",
            ty.name(),
            cls.name,
            cls.name,
            ty.name()
        )));
    }
    let interp = interp()?;
    let globals = interp.builtins_dict();
    let adapter = interp.seq_build(ty, cls, &args[1..], kwargs, &globals)?;
    if Rc::ptr_eq(cls, &ty.type_object()) {
        return Ok(adapter);
    }
    let inst = Object::Instance(Rc::new(PyInstance::with_native(cls.clone(), adapter)));
    crate::gc_trace::track(&inst);
    Ok(inst)
}

/// `__iter__`: an adapter is its own iterator.
fn seq_iter(args: &[Object]) -> Result<Object, RuntimeError> {
    args.first()
        .cloned()
        .ok_or_else(|| type_error("__iter__() missing self"))
}

/// `__next__`.
pub(crate) fn seq_next(args: &[Object]) -> Result<Object, RuntimeError> {
    let Some(l) = args.first().and_then(adapter_of) else {
        return Err(type_error("descriptor '__next__' requires an iterator"));
    };
    let interp = interp()?;
    let globals = interp.builtins_dict();
    match interp.lazy_iter_next(&l, &globals)? {
        Some(v) => Ok(v),
        None => Err(crate::error::stop_iteration()),
    }
}

/// The class `__reduce__` names: the instance's own type.
fn reduce_class(obj: &Object) -> Object {
    Object::Type(crate::builtins::class_of(obj))
}

/// `__reduce__` of `map`, `filter`, and `zip` (CPython's `map_reduce`,
/// `filter_reduce`, `zip_reduce`).
fn seq_reduce(ty: SeqType, args: &[Object]) -> Result<Object, RuntimeError> {
    let Some(slf) = args.first() else {
        return Err(descriptor_error("__reduce__", ty));
    };
    let Some(l) = adapter_of(slf) else {
        return Err(descriptor_error("__reduce__", ty));
    };
    let cls = reduce_class(slf);
    let st = l.state.borrow();
    Ok(match (&*st, ty) {
        (
            LazyIterKind::Map {
                func,
                iters,
                strict,
            },
            SeqType::Map,
        ) => {
            let mut a = Vec::with_capacity(iters.len() + 1);
            a.push(func.clone());
            a.extend(iters.iter().cloned());
            if *strict {
                Object::new_tuple_array([cls, Object::new_tuple(a), Object::Bool(true)])
            } else {
                Object::new_tuple_array([cls, Object::new_tuple(a)])
            }
        }
        (LazyIterKind::Filter { func, source }, SeqType::Filter) => {
            Object::new_tuple_array([cls, Object::new_tuple_array([func.clone(), source.clone()])])
        }
        (LazyIterKind::Zip { iters, strict, .. }, SeqType::Zip) => {
            let a = Object::new_tuple(iters.clone());
            if *strict {
                Object::new_tuple_array([cls, a, Object::Bool(true)])
            } else {
                Object::new_tuple_array([cls, a])
            }
        }
        _ => return Err(descriptor_error("__reduce__", ty)),
    })
}

/// `__setstate__` of `map` and `zip`: the strict flag.
fn seq_setstate(ty: SeqType, args: &[Object]) -> Result<Object, RuntimeError> {
    let [slf, state] = args else {
        return Err(type_error(format!(
            "__setstate__() takes exactly one argument ({} given)",
            args.len().saturating_sub(1)
        )));
    };
    let Some(l) = adapter_of(slf) else {
        return Err(descriptor_error("__setstate__", ty));
    };
    let interp = interp()?;
    let flag = interp.op_truth(state)?;
    match &mut *l.state.borrow_mut() {
        LazyIterKind::Map { strict, .. } | LazyIterKind::Zip { strict, .. } => *strict = flag,
        _ => return Err(descriptor_error("__setstate__", ty)),
    }
    Ok(Object::None)
}

/// `enumerate.__reduce__` of a lazy enumerate: `(enumerate, (it, index))`.
pub(crate) fn enumerate_reduce(l: &PyLazyIter, cls: Object) -> Option<Object> {
    let st = l.state.borrow();
    let LazyIterKind::Enumerate {
        source,
        count,
        count_big,
    } = &*st
    else {
        return None;
    };
    let index = count_big.clone().unwrap_or(Object::Int(*count));
    Some(Object::new_tuple_array([
        cls,
        Object::new_tuple_array([source.clone(), index]),
    ]))
}

fn method(name: &'static str, f: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(f),
        call_kw: None,
    }))
}

fn put(ty: &TypeObject, name: &'static str, value: Object) {
    ty.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static(name)), value);
}

/// Fill in the `map`, `filter`, and `zip` type dicts.
pub(crate) fn install(bt: &BuiltinTypes) {
    for (ty, which) in [
        (&bt.map_, SeqType::Map),
        (&bt.filter_, SeqType::Filter),
        (&bt.zip_, SeqType::Zip),
    ] {
        put(
            ty,
            "__new__",
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__new__",
                binds_instance: false,
                call: Box::new(move |args| seq_type_new(which, args, &[])),
                call_kw: Some(Box::new(move |args, kwargs| {
                    seq_type_new(which, args, kwargs)
                })),
            })),
        );
        put(ty, "__iter__", method("__iter__", seq_iter));
        put(ty, "__next__", method("__next__", seq_next));
        put(
            ty,
            "__reduce__",
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__reduce__",
                binds_instance: true,
                call: Box::new(move |args| seq_reduce(which, args)),
                call_kw: None,
            })),
        );
        if which != SeqType::Filter {
            put(
                ty,
                "__setstate__",
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "__setstate__",
                    binds_instance: true,
                    call: Box::new(move |args| seq_setstate(which, args)),
                    call_kw: None,
                })),
            );
        }
    }
}

/// `enumerate`'s `(iterable, start=0)` from a call's arguments, with
/// CPython's argument-clinic errors.
pub(crate) fn enumerate_args(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<(Object, Option<Object>), RuntimeError> {
    if args.len() > 2 {
        return Err(type_error(format!(
            "enumerate() takes at most 2 arguments ({} given)",
            args.len()
        )));
    }
    let mut iterable = args.first().cloned();
    let mut start = args.get(1).cloned();
    for (k, v) in kwargs {
        let (slot, pos) = match k.as_str() {
            "iterable" => (&mut iterable, 1),
            "start" => (&mut start, 2),
            other => {
                return Err(type_error(format!(
                    "enumerate() got an unexpected keyword argument '{other}'"
                )))
            }
        };
        if slot.is_some() {
            return Err(type_error(format!(
                "argument for enumerate() given by name ('{k}') and position ({pos})"
            )));
        }
        *slot = Some(v.clone());
    }
    let Some(iterable) = iterable else {
        return Err(type_error(
            "enumerate() missing required argument 'iterable' (pos 1)",
        ));
    };
    Ok((iterable, start))
}

/// `__reduce__` of a lazy `enumerate`.
fn lazy_enumerate_reduce(args: &[Object]) -> Result<Object, RuntimeError> {
    let Some(slf) = args.first() else {
        return Err(type_error(
            "descriptor '__reduce__' requires an 'enumerate' object",
        ));
    };
    adapter_of(slf)
        .and_then(|l| enumerate_reduce(&l, reduce_class(slf)))
        .ok_or_else(|| type_error("descriptor '__reduce__' requires an 'enumerate' object"))
}

/// The methods a native adapter answers itself, ahead of its type's
/// dict: the iterator protocol of a lazy `enumerate` (whose type's
/// methods step `Object::Iter`s) and of a classless `itertools` core.
pub(crate) fn lazy_special_method(l: &PyLazyIter, name: &str) -> Option<Object> {
    if l.cls.is_some() {
        return None;
    }
    let is_enumerate = matches!(
        l.state.try_borrow().as_deref(),
        Ok(LazyIterKind::Enumerate { .. })
    );
    if !is_enumerate && l.is_builtin_kind() {
        return None;
    }
    match name {
        "__next__" => Some(method("__next__", seq_next)),
        "__iter__" => Some(method("__iter__", seq_iter)),
        "__reduce__" if is_enumerate => Some(method("__reduce__", lazy_enumerate_reduce)),
        _ => None,
    }
}

/// Whether `it`'s next step yields an item without running code or
/// releasing anything (see [`PyIterator::pure_ready`]); `leaf` also
/// excludes iterators that step another (`enumerate`, shared handles),
/// for adapters that step several sources which could alias.
fn src_ready(it: &Object, leaf: bool) -> bool {
    match it {
        // SAFETY: a read that runs no code.
        Object::Iter(c) => unsafe { c.peek() }.is_some_and(|c| {
            c.pure_ready()
                && !(leaf && matches!(c, PyIterator::Enumerate { .. } | PyIterator::Shared(_)))
        }),
        // An unbounded counter or repeater (`zip(count(), xs)`).
        // SAFETY: as above.
        Object::LazyIter(l) => unsafe { l.state.peek() }.is_some_and(|st| match st {
            LazyIterKind::Count {
                current: Object::Int(a),
                step: Object::Int(b),
            } => a.checked_add(*b).is_some(),
            LazyIterKind::Repeat { times, .. } => times.is_none_or(|t| t > 0),
            _ => false,
        }),
        _ => false,
    }
}

/// The next item of a source [`src_ready`] admitted.
fn src_step(it: &Object) -> Option<Object> {
    match it {
        // SAFETY: the step runs no code (the source was found ready).
        Object::Iter(c) => unsafe { c.peek_mut() }?.next_value(),
        Object::LazyIter(l) => itertools_pure_next(l)?,
        _ => None,
    }
}

/// Whether every source of a several-source adapter can step purely and
/// none is another's alias (which one step could exhaust for the next).
fn srcs_ready(iters: &[Object]) -> bool {
    iters.iter().all(|it| src_ready(it, true))
        && iters.iter().enumerate().all(|(i, a)| {
            iters[..i].iter().all(|b| match (a, b) {
                (Object::Iter(a), Object::Iter(b)) => !Rc::ptr_eq(a, b),
                (Object::LazyIter(a), Object::LazyIter(b)) => !Rc::ptr_eq(a, b),
                _ => true,
            })
        })
}

/// One step of a native adapter that runs no code (the core loop's
/// `FOR_ITER`): `None` when the step needs the interpreter, nothing
/// having been consumed. Every source must have an item left, so the
/// step neither stops partway through the sources nor releases a
/// source's container (which could queue a finalizer).
pub(crate) fn lazy_pure_next(l: &PyLazyIter) -> Option<Option<Object>> {
    // SAFETY: nothing below runs code; the view ends with this call.
    let st = unsafe { l.state.peek_mut() }?;
    let LazyIterKind::Zip {
        iters,
        strict: false,
        result,
    } = st
    else {
        return itertools_pure_next(l);
    };
    if !srcs_ready(iters) {
        return None;
    }
    if iters.is_empty() {
        return Some(None);
    }
    // The previous result, refilled when this adapter holds it alone.
    if let Some(slots) = result
        .as_mut()
        .and_then(crate::shared_value::ThinArc::get_mut)
    {
        if slots.len() == iters.len() {
            for (slot, it) in slots.iter_mut().zip(iters.iter()) {
                *slot = src_step(it)?;
            }
            return Some(Some(Object::Tuple(result.clone()?)));
        }
    }
    let t = match iters.as_slice() {
        [a, b] => crate::object::TupleStorage::from_array([src_step(a)?, src_step(b)?]),
        _ => crate::object::TupleStorage::from_vec(
            iters.iter().map(src_step).collect::<Option<Vec<_>>>()?,
        ),
    };
    *result = Some(t.clone());
    Some(Some(Object::Tuple(t)))
}

/// [`lazy_pure_next`] for a two-source `zip` whose pair is unpacked at
/// once: the two items, with no tuple built.
pub(crate) fn lazy_pure_pair(l: &PyLazyIter) -> Option<(Object, Object)> {
    // SAFETY: nothing below runs code; the view ends with this call.
    let st = unsafe { l.state.peek() }?;
    let LazyIterKind::Zip {
        iters,
        strict: false,
        ..
    } = st
    else {
        return None;
    };
    let [a, b] = iters.as_slice() else {
        return None;
    };
    if !srcs_ready(iters) {
        return None;
    }
    Some((src_step(a)?, src_step(b)?))
}

/// The `itertools` state machines that touch no other object (`repeat`,
/// `product`, `permutations`, `combinations`,
/// `combinations_with_replacement`), stepped under one borrow: `None`
/// for any other kind.
pub(crate) fn machine_step(st: &mut LazyIterKind) -> Option<Option<Object>> {
    match st {
        LazyIterKind::Repeat { obj, times } => match times {
            None => Some(Some(obj.clone())),
            Some(t) if *t <= 0 => Some(None),
            Some(t) => {
                *t -= 1;
                Some(Some(obj.clone()))
            }
        },
        LazyIterKind::Product {
            pools,
            indices,
            started,
            stopped,
        } => {
            if *stopped {
                return Some(None);
            }
            if !*started {
                if pools.iter().any(|p| p.is_empty()) {
                    *stopped = true;
                    return Some(None);
                }
                *started = true;
                indices.clear();
                indices.resize(pools.len(), 0);
                let t: Vec<Object> = pools.iter().map(|p| p[0].clone()).collect();
                return Some(Some(Object::new_tuple(t)));
            }
            let n = pools.len();
            let mut i = n as i64 - 1;
            while i >= 0 {
                let k = i as usize;
                indices[k] += 1;
                if indices[k] < pools[k].len() {
                    break;
                }
                indices[k] = 0;
                i -= 1;
            }
            if i < 0 {
                *stopped = true;
                return Some(None);
            }
            let t: Vec<Object> = pools
                .iter()
                .zip(indices.iter())
                .map(|(p, &ix)| p[ix].clone())
                .collect();
            Some(Some(Object::new_tuple(t)))
        }
        LazyIterKind::Permutations {
            pool,
            r,
            indices,
            cycles,
            started,
            stopped,
        } => {
            if *stopped {
                return Some(None);
            }
            let n = pool.len();
            let r = *r;
            if !*started {
                *started = true;
                let t: Vec<Object> = indices[..r].iter().map(|&ix| pool[ix].clone()).collect();
                return Some(Some(Object::new_tuple(t)));
            }
            if n == 0 {
                *stopped = true;
                return Some(None);
            }
            let mut i = r as i64 - 1;
            while i >= 0 {
                let k = i as usize;
                cycles[k] -= 1;
                if cycles[k] == 0 {
                    indices[k..].rotate_left(1);
                    cycles[k] = n - k;
                } else {
                    let j = n - cycles[k];
                    indices.swap(k, j);
                    let t: Vec<Object> = indices[..r].iter().map(|&ix| pool[ix].clone()).collect();
                    return Some(Some(Object::new_tuple(t)));
                }
                i -= 1;
            }
            *stopped = true;
            Some(None)
        }
        LazyIterKind::Combinations {
            pool,
            r,
            indices,
            started,
            stopped,
        } => {
            if *stopped {
                return Some(None);
            }
            let n = pool.len();
            let r = *r;
            if !*started {
                *started = true;
                let t: Vec<Object> = indices.iter().map(|&ix| pool[ix].clone()).collect();
                return Some(Some(Object::new_tuple(t)));
            }
            let mut i = r as i64 - 1;
            while i >= 0 && indices[i as usize] == i as usize + n - r {
                i -= 1;
            }
            if i < 0 {
                *stopped = true;
                return Some(None);
            }
            let k = i as usize;
            indices[k] += 1;
            for j in k + 1..r {
                indices[j] = indices[j - 1] + 1;
            }
            let t: Vec<Object> = indices.iter().map(|&ix| pool[ix].clone()).collect();
            Some(Some(Object::new_tuple(t)))
        }
        LazyIterKind::Cwr {
            pool,
            r,
            indices,
            started,
            stopped,
        } => {
            if *stopped {
                return Some(None);
            }
            let n = pool.len();
            let r = *r;
            if !*started {
                *started = true;
                let t: Vec<Object> = indices.iter().map(|&ix| pool[ix].clone()).collect();
                return Some(Some(Object::new_tuple(t)));
            }
            let mut i = r as i64 - 1;
            while i >= 0 && indices[i as usize] == n - 1 {
                i -= 1;
            }
            if i < 0 {
                *stopped = true;
                return Some(None);
            }
            let k = i as usize;
            let v = indices[k] + 1;
            for slot in indices[k..].iter_mut() {
                *slot = v;
            }
            let t: Vec<Object> = indices.iter().map(|&ix| pool[ix].clone()).collect();
            Some(Some(Object::new_tuple(t)))
        }
        _ => None,
    }
}

/// The next item of a native iterator source when taking it runs no
/// code and releases nothing (see [`PyIterator::pure_ready`]).
fn ready_next(it: &Object) -> Option<Object> {
    if !src_ready(it, false) {
        return None;
    }
    src_step(it)
}

/// One step of an `itertools` adapter that runs no code and releases
/// nothing (the core loop's `FOR_ITER`, and the interpreter's fast path
/// ahead of the general one): `None` when the step needs the general
/// path, nothing having been consumed. Exhaustion of a source is left to
/// that path, which releases it.
pub(crate) fn itertools_pure_next(l: &PyLazyIter) -> Option<Option<Object>> {
    // SAFETY: nothing below runs code; the view ends with this call.
    let st = unsafe { l.state.peek_mut() }?;
    if let Some(r) = machine_step(st) {
        return Some(r);
    }
    match st {
        LazyIterKind::Chain {
            active: Some(active),
            ..
        } => ready_next(active).map(Some),
        LazyIterKind::Count { current, step } => {
            let (Object::Int(a), Object::Int(b)) = (&*current, &*step) else {
                return None;
            };
            let next = a.checked_add(*b)?;
            Some(Some(std::mem::replace(current, Object::Int(next))))
        }
        LazyIterKind::Islice {
            source,
            next_idx,
            pos,
            stop,
            step,
            done: false,
        } => {
            // The next item is the source's next one (no elements to
            // skip) and within the stop bound.
            if *pos != *next_idx || stop.is_some_and(|s| *pos >= s) {
                return None;
            }
            let item = ready_next(source)?;
            *pos += 1;
            *next_idx = next_idx.saturating_add(*step);
            if let Some(s) = *stop {
                if *next_idx > s {
                    *next_idx = s;
                }
            }
            Some(Some(item))
        }
        LazyIterKind::Cycle {
            source,
            saved,
            index,
            firstpass,
        } => match source {
            Some(src) => {
                // SAFETY: as above (a distinct cell).
                let buf = unsafe { saved.peek_mut() }?;
                let item = ready_next(src)?;
                if !*firstpass {
                    buf.push(item.clone());
                }
                Some(Some(item))
            }
            None => {
                // SAFETY: as above (a distinct cell).
                let buf = unsafe { saved.peek() }?;
                if buf.is_empty() {
                    return None;
                }
                let item = buf[(*index).min(buf.len() - 1)].clone();
                let next = *index + 1;
                *index = if next >= buf.len() { 0 } else { next };
                Some(Some(item))
            }
        },
        // `accumulate` without a function over machine ints or floats.
        LazyIterKind::Accumulate {
            source,
            func: None,
            total,
            initial,
        } => {
            if let Some(init) = initial.take() {
                *total = Some(init.clone());
                return Some(Some(init));
            }
            if !src_ready(source, false) {
                return None;
            }
            let Object::Iter(c) = source else {
                return None;
            };
            // SAFETY: a read that runs no code (a distinct cell).
            let item = unsafe { c.peek() }?.pure_peek()?;
            let next = match (&*total, &item) {
                (None, _) => item,
                (Some(Object::Int(a)), Object::Int(b)) => Object::Int(a.checked_add(*b)?),
                (Some(Object::Float(a)), Object::Float(b)) => Object::Float(a + b),
                _ => return None,
            };
            src_step(source)?;
            *total = Some(next.clone());
            Some(Some(next))
        }
        LazyIterKind::Pairwise {
            source: Some(source),
            old: Some(old),
        } => {
            let new = ready_next(source)?;
            let prev = std::mem::replace(old, new.clone());
            Some(Some(Object::new_tuple_array([prev, new])))
        }
        _ => None,
    }
}

/// A builtin adapter type's call (`map`, `filter`, `zip`, `enumerate`,
/// `reversed`), `type(x)`, a literal's `int(text)`/`float(text)`, or an
/// empty container, built without running any code (the core loop's
/// `CALL`): `None` for every shape that might.
pub(crate) fn builtin_ctor_pure(cls: &Rc<TypeObject>, args: &[Object]) -> Option<Object> {
    if let Some(ty) = SeqType::of_exact(cls) {
        return Interpreter::seq_new_pure(ty, args);
    }
    let bt = builtin_types();
    // `type(x)`: the object's class (`b_type`'s one-argument form).
    if Rc::ptr_eq(cls, &bt.type_) {
        return match args {
            [x] => Some(Object::Type(crate::builtins::class_of(x))),
            _ => None,
        };
    }
    // `set()`, `dict()`, `list()`, `tuple()`: an empty one (a container
    // tracked like the builtins' own, unless that would start a collection).
    if args.is_empty() {
        if Rc::ptr_eq(cls, &bt.tuple_) {
            return Some(Object::new_tuple(Vec::new()));
        }
        let obj = if Rc::ptr_eq(cls, &bt.set_) {
            Object::new_set()
        } else if Rc::ptr_eq(cls, &bt.dict_) {
            Object::new_dict()
        } else if Rc::ptr_eq(cls, &bt.list_) {
            Object::new_list(Vec::new())
        } else {
            return None;
        };
        if crate::stdlib::tracemalloc_real::is_tracking() || crate::gc_trace::auto_collect_due() {
            return None;
        }
        crate::gc_trace::track(&obj);
        return Some(obj);
    }
    // `int(x)` of an exact int, bool, or a finite float whose truncation
    // fits a machine word.
    if let [x @ (Object::Int(_) | Object::Bool(_) | Object::Float(_))] = args {
        if !Rc::ptr_eq(cls, &bt.int_) {
            return None;
        }
        return match x {
            Object::Int(i) => Some(Object::Int(*i)),
            Object::Bool(b) => Some(Object::Int(i64::from(*b))),
            Object::Float(f) => {
                let t = f.trunc();
                // (Both bounds are exact powers of two as floats.)
                (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0)
                    .contains(&t)
                    .then(|| Object::Int(t as i64))
            }
            _ => None,
        };
    }
    // `int(text, base)` of a plain ASCII literal: an optional sign, and
    // digits of an explicit base, or of the base a `0x`/`0o`/`0b` prefix
    // names for base 0 (where a decimal literal can't have a leading
    // zero). Underscores, spaces, and anything that would raise or not fit
    // a machine word take the full path.
    if let [Object::Str(text), Object::Int(base)] = args {
        if !Rc::ptr_eq(cls, &bt.int_) {
            return None;
        }
        let t: &str = text.as_ref();
        let (neg, body) = match t.as_bytes().first() {
            Some(b'-') => (true, &t[1..]),
            Some(b'+') => (false, &t[1..]),
            _ => (false, t),
        };
        let (radix, digits) = match *base {
            0 => match body.as_bytes() {
                [b'0', b'x' | b'X', ..] => (16, &body[2..]),
                [b'0', b'o' | b'O', ..] => (8, &body[2..]),
                [b'0', b'b' | b'B', ..] => (2, &body[2..]),
                [b'0', rest @ ..] if rest.iter().all(|&c| c == b'0') => (10, body),
                [b'0', ..] => return None,
                _ => (10, body),
            },
            b @ 2..=36 => (b as u32, body),
            _ => return None,
        };
        if digits.is_empty()
            || digits.len() > 15
            || !digits.bytes().all(|c| char::from(c).is_digit(radix))
        {
            return None;
        }
        let v = i64::from_str_radix(digits, radix).ok()?;
        return Some(Object::Int(if neg { -v } else { v }));
    }
    // `int(text)` and `float(text)` of a plain ASCII literal (an optional
    // sign and digits; for a float, also a point and an exponent). Any
    // other text, including one that would raise, takes the full path.
    if let [Object::Str(text)] = args {
        let t = text.as_ref();
        if Rc::ptr_eq(cls, &bt.int_) {
            let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
            if !digits.is_empty()
                && digits.len() <= 18
                && digits.bytes().all(|c| c.is_ascii_digit())
            {
                return t.parse::<i64>().ok().map(Object::Int);
            }
            return None;
        }
        if Rc::ptr_eq(cls, &bt.float_) {
            let ok = !t.is_empty()
                && t.len() <= 64
                && t.bytes().any(|c| c.is_ascii_digit())
                && t.bytes()
                    .all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-'));
            return if ok {
                t.parse::<f64>().ok().map(Object::Float)
            } else {
                None
            };
        }
    }
    let native = |o: &Object| {
        matches!(
            o,
            Object::List(_)
                | Object::Tuple(_)
                | Object::Str(_)
                | Object::Range(_)
                | Object::Iter(_)
                | Object::Dict(_)
                | Object::Set(_)
                | Object::FrozenSet(_)
                | Object::Bytes(_)
        )
    };
    if Rc::ptr_eq(cls, &bt.enumerate_) {
        return match args {
            [x] | [x, Object::Int(_)] if native(x) => crate::builtins::b_enumerate(args).ok(),
            _ => None,
        };
    }
    if Rc::ptr_eq(cls, &bt.reversed_) {
        return match args {
            [Object::List(_) | Object::Tuple(_) | Object::Range(_) | Object::Dict(_)] => {
                crate::builtins::b_reversed(args).ok()
            }
            _ => None,
        };
    }
    None
}

// ---------------------------------------------------------------------------
// Pure fast halves of the iteration builtins (the dispatch loop's leaf
// calls): each serves the argument shapes whose result needs no Python
// code and no error, and declines (`None`) everything else, which then
// takes the builtin's full path untouched.
// ---------------------------------------------------------------------------

/// `iter(x)` of a native container or an iterator.
pub(crate) fn iter_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [x] = args else {
        return None;
    };
    Some(Ok(match x {
        Object::Iter(_) | Object::LazyIter(_) | Object::Generator(_) => x.clone(),
        Object::List(_)
        | Object::Tuple(_)
        | Object::Str(_)
        | Object::Range(_)
        | Object::Dict(_)
        | Object::Set(_)
        | Object::FrozenSet(_)
        | Object::Bytes(_) => Object::Iter(Rc::new(RefCell::new(x.make_iter().ok()?))),
        _ => return None,
    }))
}

/// The items `min`/`max`/`sum`/`any`/`all` scan: the positional
/// arguments, or a single list or tuple argument's items.
fn scan_items(args: &[Object], multi: bool) -> Option<Vec<Object>> {
    match args {
        [Object::Tuple(t)] => Some(t.to_vec()),
        [Object::List(l)] => Some(l.try_borrow().ok()?.clone()),
        [_] => None,
        _ if multi && args.len() > 1 => Some(args.to_vec()),
        _ => None,
    }
}

/// `min(...)`/`max(...)` over plain ints, floats, or strs of one kind
/// (CPython's rich-comparison loop: a strict comparison keeps the first
/// extremum).
fn min_max_fast(args: &[Object], want_max: bool) -> Option<Result<Object, RuntimeError>> {
    let items = scan_items(args, true)?;
    let (first, rest) = items.split_first()?;
    let mut best = first;
    match first {
        Object::Int(_) => {
            for x in rest {
                let (Object::Int(a), Object::Int(b)) = (x, best) else {
                    return None;
                };
                if (want_max && a > b) || (!want_max && a < b) {
                    best = x;
                }
            }
        }
        Object::Float(_) => {
            for x in rest {
                let (Object::Float(a), Object::Float(b)) = (x, best) else {
                    return None;
                };
                if (want_max && a > b) || (!want_max && a < b) {
                    best = x;
                }
            }
        }
        Object::Str(_) => {
            for x in rest {
                let (Object::Str(a), Object::Str(b)) = (x, best) else {
                    return None;
                };
                let (a, b): (&str, &str) = (a, b);
                if (want_max && a > b) || (!want_max && a < b) {
                    best = x;
                }
            }
        }
        _ => return None,
    }
    Some(Ok(best.clone()))
}

/// `abs(x)` of a plain number (an int's magnitude must fit).
pub(crate) fn abs_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    Some(Ok(match args {
        [Object::Int(i)] => Object::Int(i.checked_abs()?),
        [Object::Float(f)] => Object::Float(f.abs()),
        [Object::Bool(b)] => Object::Int(i64::from(*b)),
        _ => return None,
    }))
}

/// `ord(c)` of a one-character `str`.
pub(crate) fn ord_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [Object::Str(s)] = args else {
        return None;
    };
    let mut chars = s.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some(Ok(Object::Int(i64::from(u32::from(c)))))
}

/// `chr(i)` of a code point a `str` holds (a surrogate takes the full
/// path).
pub(crate) fn chr_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [Object::Int(i)] = args else {
        return None;
    };
    let c = char::from_u32(u32::try_from(*i).ok()?)?;
    Some(Ok(Object::from_char(c)))
}

/// `divmod(a, b)` of plain ints, floored as Python's (a zero divisor
/// raises on the full path).
pub(crate) fn divmod_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [Object::Int(a), Object::Int(b)] = args else {
        return None;
    };
    if *b == 0 {
        return None;
    }
    let (mut q, mut r) = (a.checked_div(*b)?, a.checked_rem(*b)?);
    if r != 0 && ((r < 0) != (*b < 0)) {
        q -= 1;
        r += b;
    }
    Some(Ok(Object::new_tuple_array([
        Object::Int(q),
        Object::Int(r),
    ])))
}

pub(crate) fn min_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    min_max_fast(args, false)
}

pub(crate) fn max_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    min_max_fast(args, true)
}

/// `sum(xs)` of plain ints that stays a machine int.
pub(crate) fn sum_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let mut total: i64 = 0;
    let mut add = |x: &Object| -> Option<()> {
        let Object::Int(i) = x else {
            return None;
        };
        total = total.checked_add(*i)?;
        Some(())
    };
    match args {
        [Object::Tuple(t)] => t.iter().try_for_each(&mut add)?,
        [Object::List(l)] => l.try_borrow().ok()?.iter().try_for_each(&mut add)?,
        _ => return None,
    }
    Some(Ok(Object::Int(total)))
}

/// `any(xs)` / `all(xs)` over values whose truth is plain data.
fn any_all_fast(args: &[Object], want_any: bool) -> Option<Result<Object, RuntimeError>> {
    let scan = |items: &[Object]| -> Option<bool> {
        for x in items {
            let truth = match x {
                Object::Bool(b) => *b,
                Object::Int(i) => *i != 0,
                Object::None => false,
                Object::Float(f) => *f != 0.0,
                Object::Str(s) => !s.is_empty(),
                _ => return None,
            };
            if truth == want_any {
                return Some(want_any);
            }
        }
        Some(!want_any)
    };
    let r = match args {
        [Object::Tuple(t)] => scan(t)?,
        [Object::List(l)] => scan(&l.try_borrow().ok()?)?,
        _ => return None,
    };
    Some(Ok(Object::Bool(r)))
}

pub(crate) fn any_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    any_all_fast(args, true)
}

pub(crate) fn all_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    any_all_fast(args, false)
}

/// The `(index, item)` step of an `enumerate` over a native iterator
/// whose step runs no code, without the tuple (a loop that unpacks it):
/// `None` for any other iterator or step.
pub(crate) fn enumerate_pure_pair(it: &Object) -> Option<(Object, Object)> {
    let Object::Iter(c) = it else {
        return None;
    };
    // SAFETY: the step runs no code; the view ends with this call.
    let PyIterator::Enumerate {
        inner,
        count,
        count_big: None,
    } = (unsafe { c.peek_mut() })?
    else {
        return None;
    };
    let next = count.checked_add(1)?;
    // SAFETY: as above; `inner` is a cell of its own.
    let src = unsafe { inner.peek_mut() }?;
    if !src.pure_ready() {
        return None;
    }
    let v = src.next_value()?;
    let i = std::mem::replace(count, next);
    Some((Object::Int(i), v))
}

/// The truth of a value whose truth is plain data (`None` for anything
/// whose `__bool__`/`__len__` could run code).
fn scalar_truth(v: &Object) -> Option<bool> {
    Some(match v {
        Object::Bool(b) => *b,
        Object::Int(i) => *i != 0,
        Object::None => false,
        Object::Float(f) => *f != 0.0,
        Object::Str(s) => !s.is_empty(),
        _ => return None,
    })
}

impl Interpreter {
    /// [`lazy_pure_next`], and also a `map` or `filter` over a plain
    /// sequence cursor whose callback is a warm pure-leaf function
    /// (evaluated in place, as the core loop calls one): the item is
    /// taken only once the callback has succeeded. `None` (nothing
    /// consumed) for every other step.
    pub(crate) fn lazy_core_next(&self, l: &PyLazyIter) -> Option<Option<Object>> {
        if let Some(r) = lazy_pure_next(l) {
            return Some(r);
        }
        // SAFETY: nothing below runs code that could reach the adapter
        // (pure leaves call nothing); the view ends with this call.
        let st = unsafe { l.state.peek() }?;
        let peek = |c: &Rc<RefCell<PyIterator>>| -> Option<Object> {
            // SAFETY: as above (a distinct cell).
            unsafe { c.peek() }?.pure_peek()
        };
        let advance = |c: &Rc<RefCell<PyIterator>>| -> Option<()> {
            // SAFETY: as above; the item was found ready.
            unsafe { c.peek_mut() }?.next_value().map(drop)
        };
        match st {
            LazyIterKind::Map {
                func: Object::Function(f),
                iters,
                ..
            } => {
                let [Object::Iter(c)] = iters.as_slice() else {
                    return None;
                };
                let v = peek(c)?;
                let r = self.call_pure_leaf(f, std::slice::from_ref(&v))?;
                advance(c)?;
                Some(Some(r))
            }
            LazyIterKind::Filter {
                func,
                source: Object::Iter(c),
            } => loop {
                let v = peek(c)?;
                let keep = match func {
                    Object::None => scalar_truth(&v)?,
                    Object::Type(t) if Rc::ptr_eq(t, &builtin_types().bool_) => scalar_truth(&v)?,
                    Object::Function(f) => {
                        scalar_truth(&self.call_pure_leaf(f, std::slice::from_ref(&v))?)?
                    }
                    _ => return None,
                };
                advance(c)?;
                if keep {
                    return Some(Some(v));
                }
            },
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// `itertools.groupby` and its groupers (CPython's `groupby_next`,
// `groupby_step`, and `_grouper_next`). No borrow of an adapter's state
// survives a call into the key function or a key comparison: both may
// re-enter the groupby (gh-143543, gh-146613).
// ---------------------------------------------------------------------------

/// Whether `obj` is `l` (`currgrouper is self`).
fn is_current_grouper(parent: &PyLazyIter, g: &PyLazyIter) -> bool {
    match &*parent.state.borrow() {
        LazyIterKind::GroupBy {
            currgrouper: Some(w),
            ..
        } => std::ptr::eq(w.as_ptr(), g),
        _ => false,
    }
}

impl Interpreter {
    /// Step a `groupby` or one of its groupers: `None` when `l` is
    /// another kind.
    pub(crate) fn groupby_lazy_next(
        &mut self,
        l: &Rc<PyLazyIter>,
        globals: &Rc<RefCell<DictData>>,
    ) -> Option<Result<Option<Object>, RuntimeError>> {
        let parent = match &*l.state.borrow() {
            LazyIterKind::GroupBy { .. } => None,
            LazyIterKind::Grouper { parent, tgtkey } => Some((parent.clone(), tgtkey.clone())),
            _ => return None,
        };
        Some(match parent {
            None => self.groupby_next(l, globals),
            Some((parent, tgtkey)) => self.grouper_next(l, &parent, &tgtkey, globals),
        })
    }

    /// `groupby_step`: pull the next value and its key, storing both only
    /// once both are in hand. `Ok(false)` at the source's end.
    fn groupby_step(
        &mut self,
        l: &PyLazyIter,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<bool, RuntimeError> {
        let (source, keyfunc) = match &*l.state.borrow() {
            LazyIterKind::GroupBy {
                source, keyfunc, ..
            } => (source.clone(), keyfunc.clone()),
            _ => return Ok(false),
        };
        let Some(value) = self.iter_next(&source, globals)? else {
            return Ok(false);
        };
        let key = match keyfunc {
            Object::None => value.clone(),
            f => self.seq_call1(&f, value.clone(), globals)?,
        };
        if let LazyIterKind::GroupBy {
            currkey, currvalue, ..
        } = &mut *l.state.borrow_mut()
        {
            let old = (currkey.replace(key), currvalue.replace(value));
            drop(old);
        }
        Ok(true)
    }

    /// `tgtkey is currkey or tgtkey == currkey`.
    fn group_keys_equal(&mut self, a: &Object, b: &Object) -> Result<bool, RuntimeError> {
        if a.is_same(b) {
            return Ok(true);
        }
        self.op_compare(a, b, weavepy_compiler::CompareKind::Eq)
    }

    fn groupby_next(
        &mut self,
        l: &Rc<PyLazyIter>,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Option<Object>, RuntimeError> {
        if let LazyIterKind::GroupBy { currgrouper, .. } = &mut *l.state.borrow_mut() {
            *currgrouper = None;
        }
        // Skip to the next group.
        loop {
            let keys = match &*l.state.borrow() {
                LazyIterKind::GroupBy {
                    currkey, tgtkey, ..
                } => (currkey.clone(), tgtkey.clone()),
                _ => return Ok(None),
            };
            match keys {
                (None, _) => {}
                (Some(_), None) => break,
                (Some(curr), Some(tgt)) => {
                    if !self.group_keys_equal(&tgt, &curr)? {
                        break;
                    }
                }
            }
            if !self.groupby_step(l, globals)? {
                return Ok(None);
            }
        }
        let mut st = l.state.borrow_mut();
        let LazyIterKind::GroupBy {
            currkey: Some(key),
            tgtkey,
            currgrouper,
            grouper_cls,
            ..
        } = &mut *st
        else {
            return Ok(None);
        };
        let key = key.clone();
        let old = tgtkey.replace(key.clone());
        let grouper = Rc::new(PyLazyIter {
            state: RefCell::new(LazyIterKind::Grouper {
                parent: l.clone(),
                tgtkey: key.clone(),
            }),
            cls: grouper_cls.clone(),
        });
        *currgrouper = Some(Rc::downgrade(&grouper));
        drop(st);
        drop(old);
        Ok(Some(Object::new_tuple_array([
            key,
            Object::LazyIter(grouper),
        ])))
    }

    fn grouper_next(
        &mut self,
        g: &Rc<PyLazyIter>,
        parent: &Rc<PyLazyIter>,
        tgtkey: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Option<Object>, RuntimeError> {
        if !is_current_grouper(parent, g) {
            return Ok(None);
        }
        let need_step = matches!(
            &*parent.state.borrow(),
            LazyIterKind::GroupBy {
                currvalue: None,
                ..
            }
        );
        if need_step && !self.groupby_step(parent, globals)? {
            return Ok(None);
        }
        let curr = match &*parent.state.borrow() {
            LazyIterKind::GroupBy {
                currkey: Some(k), ..
            } => k.clone(),
            _ => return Ok(None),
        };
        if !self.group_keys_equal(tgtkey, &curr)? {
            return Ok(None);
        }
        // The comparison may have re-entered and advanced the groupby.
        if !is_current_grouper(parent, g) {
            return Ok(None);
        }
        let mut st = parent.state.borrow_mut();
        let LazyIterKind::GroupBy {
            currkey, currvalue, ..
        } = &mut *st
        else {
            return Ok(None);
        };
        let Some(value) = currvalue.take() else {
            return Ok(None);
        };
        let old = currkey.take();
        drop(st);
        drop(old);
        Ok(Some(value))
    }
}
