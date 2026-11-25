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
use crate::{Interpreter, LeafKind};

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

/// `map(func, it)` over one iterator already made.
#[cfg(feature = "jit")]
pub(crate) fn lazy_map1(func: Object, it: Object) -> Object {
    lazy(LazyIterKind::Map {
        func,
        iters: vec![it],
        strict: false,
    })
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
        // A list or tuple subclass inheriting `__iter__` (a named tuple's
        // `_asdict`, `dict(zip(self._fields, self))`): its value's iterator,
        // as `h_get_iter` makes it.
        Object::Instance(inst) => {
            let native @ (Object::List(_) | Object::Tuple(_)) = inst.native.get()? else {
                return None;
            };
            let cls = inst.cls_raw();
            if cls.native_kind.get() != 0
                || cls.c_ext_ptr.get() != 0
                || !cls.dunder(crate::types::Dunder::Iter).builtin_owner()
            {
                return None;
            }
            Some(Object::Iter(Rc::new(RefCell::new(
                native.make_iter().ok()?,
            ))))
        }
        _ => None,
    }
}

/// Whether `e` is a `StopIteration` (or a subclass's) being raised.
pub(crate) fn is_stop_iteration(e: &RuntimeError) -> bool {
    matches!(e, RuntimeError::PyException(exc) if matches!(&exc.instance,
        Object::Instance(i) if i.cls_raw().is_subclass_of(&builtin_types().stop_iteration)))
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
    /// place, anything else through the iterator protocol. A `StopIteration`
    /// that says something comes back as an error, for the adapter's
    /// consumer (see [`Interpreter::iter_step`]).
    #[inline]
    fn seq_step_source(
        &mut self,
        it: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Option<Object>, RuntimeError> {
        if let Object::Iter(c) = it {
            let mut cur = c.borrow_mut();
            // (A list's or tuple's cursor with an item left: the item, in
            // one view; the end and every other cursor, the full step.)
            if let Some(v) = cur.pure_peek() {
                cur.pure_skip();
                return Ok(Some(v));
            }
            return cur.next_value_checked();
        }
        self.iter_step(it, globals)
    }

    /// `func(v)` for an adapter's callback: a pure callback in place (see
    /// [`Self::pure_callback`]), a plain function or a method over one
    /// bound directly, else a full call.
    #[inline]
    pub(crate) fn seq_call1(
        &mut self,
        func: &Object,
        v: Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        if let Some(r) = self.pure_callback(func, std::slice::from_ref(&v), true) {
            self.release(v);
            return Ok(r);
        }
        // (The arguments' vector comes from the scratch pool, which the
        // bound call returns it to.)
        match func {
            Object::Function(f) => {
                let mut args = self.pooled_scratch();
                args.push(v);
                return self.call_python_owned(f, args, Vec::new());
            }
            Object::BoundMethod(bm) if !bm.redispatch_descriptor => {
                if let Object::Function(f) = &bm.function {
                    let mut args = self.pooled_scratch();
                    args.push(bm.receiver.clone());
                    args.push(v);
                    return self.call_python_owned(f, args, Vec::new());
                }
            }
            // A placeholder-free `functools.partial` with no stored
            // keywords (`map(partial(add, 3), xs)`): its function over the
            // stored positionals and `v`, as its native `__call__` makes
            // the call, without the bound `__call__` and its general call.
            Object::Instance(inst) if Self::is_plain_partial(inst) => {
                let parts = crate::stdlib::functools_mod::with_partial_call(inst, |f, args, kw| {
                    kw.is_empty().then(|| {
                        let mut all = Vec::with_capacity(args.len() + 1);
                        all.extend_from_slice(args);
                        (f.clone(), all)
                    })
                });
                if let Some((f, mut all)) = parts {
                    all.push(v);
                    if let Some(r) = self.pure_callback(&f, &all, true) {
                        return Ok(r);
                    }
                    return self.call(&f, &all, &[], globals);
                }
            }
            _ => {}
        }
        let r = self.call(func, std::slice::from_ref(&v), &[], globals);
        // A dead item's allocation is recycled (a tuple's, as CPython's
        // freelist does: `map(id, ((i, i) for ...))` sees one address).
        self.release(v);
        r
    }

    /// Whether calling `inst` is `functools.partial`'s own native
    /// `__call__` (its class is `partial` or a subclass that keeps it).
    fn is_plain_partial(inst: &crate::types::PyInstance) -> bool {
        matches!(inst.cls_raw().lookup("__call__"),
            Some(Object::Builtin(b)) if crate::stdlib::functools_mod::is_partial_call(&b))
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
        if chain_drain(l, out) {
            return Ok(true);
        }
        if let Some(items) = accumulate_collect(l) {
            out.extend(items);
            return Ok(true);
        }
        if zip_seq_bulk(l, out) {
            return Ok(true);
        }
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
        // (A callback's `StopIteration` ends the drain, as it ends a step:
        // see `Interpreter::lazy_iter_next`.)
        let stop = |e: RuntimeError| {
            if is_stop_iteration(&e) {
                Ok(true)
            } else {
                Err(e)
            }
        };
        if filter {
            if filter_seq_bulk(&func, &source, out) {
                return Ok(true);
            }
            loop {
                match self.filter_next(&func, &source, globals) {
                    Ok(Some(v)) => out.push(v),
                    Ok(None) => return Ok(true),
                    Err(e) => return stop(e),
                }
            }
        }
        // A placeholder-free `partial` with no stored keywords, resolved
        // once (its function and stored arguments are read-only): each
        // item is its function's last argument (see `seq_call1`).
        let partial = match &func {
            Object::Instance(inst) if Self::is_plain_partial(inst) => {
                crate::stdlib::functools_mod::with_partial_call(inst, |f, args, kw| {
                    kw.is_empty().then(|| (f.clone(), args.to_vec()))
                })
            }
            _ => None,
        };
        if let Some((f, stored)) = partial {
            let mut all = stored;
            let at = all.len();
            loop {
                let v = match self.seq_step_source(&source, globals) {
                    Ok(Some(v)) => v,
                    Ok(None) => return Ok(true),
                    Err(e) => return stop(e),
                };
                all.truncate(at);
                all.push(v);
                let r = match self.pure_callback(&f, &all, true) {
                    Some(r) => Ok(r),
                    None => self.call(&f, &all, &[], globals),
                };
                match r {
                    Ok(r) => out.push(r),
                    Err(e) => return stop(e),
                }
            }
        }
        // A leaf builtin callback (`map(str.upper, words)`, `map(abs, xs)`),
        // its kind resolved once: each item through its body in place; one
        // it declines or raises on takes the full call (a leaf runs no
        // code, so nothing is observed twice).
        let leaf = match &func {
            Object::Builtin(b) => self.leaf_call_kind(b).map(|k| (b.clone(), k)),
            _ => None,
        };
        if let Some((b, kind)) = leaf {
            loop {
                let v = match self.seq_step_source(&source, globals) {
                    Ok(Some(v)) => v,
                    Ok(None) => return Ok(true),
                    Err(e) => return stop(e),
                };
                let r = match self.leaf_builtin_call(kind, &b, std::slice::from_ref(&v)) {
                    Some(Ok(r)) => {
                        self.release(v);
                        Ok(r)
                    }
                    _ => self.seq_call1(&func, v, globals),
                };
                match r {
                    Ok(r) => out.push(r),
                    Err(e) => return stop(e),
                }
            }
        }
        // A leaf method's fast half bound to an instance of a built-in
        // subclass (`map(_Quoter(safe).__getitem__, bs)` in
        // `urllib.parse.quote`), resolved once: each item through it, on
        // views of the receiver and the item; one it declines takes the
        // full call.
        let bound_fast = match &func {
            Object::BoundMethod(bm)
                if !bm.redispatch_descriptor && matches!(bm.receiver, Object::Instance(_)) =>
            {
                match &bm.function {
                    Object::Builtin(b) if b.binds_instance => match self.leaf_call_kind(b) {
                        Some(LeafKind::Fast(f)) => Some((f, bm.receiver.clone())),
                        _ => None,
                    },
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some((fast, recv)) = bound_fast {
            loop {
                let v = match self.seq_step_source(&source, globals) {
                    Ok(Some(v)) => v,
                    Ok(None) => return Ok(true),
                    Err(e) => return stop(e),
                };
                // SAFETY: bitwise views of the live receiver and item, never
                // dropped (the fast half clones what it keeps).
                let views = std::mem::ManuallyDrop::new(unsafe {
                    [
                        std::ptr::read(&raw const recv),
                        std::ptr::read(&raw const v),
                    ]
                });
                let r = match fast(&views[..]) {
                    Some(Ok(r)) => {
                        self.release(v);
                        Ok(r)
                    }
                    _ => self.seq_call1(&func, v, globals),
                };
                match r {
                    Ok(r) => out.push(r),
                    Err(e) => return stop(e),
                }
            }
        }
        loop {
            let v = match self.seq_step_source(&source, globals) {
                Ok(Some(v)) => v,
                Ok(None) => return Ok(true),
                Err(e) => return stop(e),
            };
            match self.seq_call1(&func, v, globals) {
                Ok(r) => out.push(r),
                Err(e) => return stop(e),
            }
        }
    }

    /// An exact `map`/`filter`/`zip` built from positional arguments
    /// without the interpreter (the core loop's `CALL`): `None` when an
    /// argument needs `iter()` to run code, or the call would raise.
    pub(crate) fn seq_new_pure(ty: SeqType, args: &[Object]) -> Option<Object> {
        Self::seq_new_pure_strict(ty, args, false)
    }

    /// `zip(*its, strict=True)` and `map(f, *its, strict=True)` of a plain
    /// `bool` flag (compiled `CALL_KW`'s operands: the type, the empty self
    /// slot, the positionals, the flag, the names tuple), built as
    /// [`Self::seq_new_pure`] builds the positional call. `None` for any
    /// other shape.
    #[cfg(feature = "jit")]
    pub(crate) fn seq_new_strict_kw(ops: &[Object], argc: usize) -> Option<Object> {
        let bt = builtin_types();
        let (Object::Type(t), Object::Unbound, Some(Object::Tuple(names))) =
            (ops.first()?, ops.get(1)?, ops.last())
        else {
            return None;
        };
        let ty = if Rc::ptr_eq(t, &bt.zip_) {
            SeqType::Zip
        } else if Rc::ptr_eq(t, &bt.map_) {
            SeqType::Map
        } else {
            return None;
        };
        if ops.len() != argc + 4
            || !matches!(&names[..], [Object::Str(n)] if n.as_ref() == "strict")
        {
            return None;
        }
        let Object::Bool(strict) = ops[2 + argc] else {
            return None;
        };
        if !Self::core_all_droppable(ops) {
            return None;
        }
        Self::seq_new_pure_strict(ty, &ops[2..2 + argc], strict)
    }

    fn seq_new_pure_strict(ty: SeqType, args: &[Object], strict: bool) -> Option<Object> {
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
                    strict,
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
                    strict,
                    result: None,
                    spare: None,
                    held: Vec::new(),
                    distinct: None,
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
                    spare: None,
                    held: Vec::new(),
                    distinct: None,
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
                if iters.len() == 1 && !*strict {
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

    /// The pairs of a non-strict two-source `zip` (`dict(zip(ks, vs))`),
    /// stepped as the zip steps them: the first source, then the second,
    /// until either ends (an item the first gave is dropped when the second
    /// has none, as the zip drops it). `None`, nothing stepped, for any
    /// other iterable.
    pub(crate) fn zip2_pairs(
        &mut self,
        it: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Option<Result<Vec<(Object, Object)>, RuntimeError>> {
        let Object::LazyIter(l) = it else {
            return None;
        };
        let (a, b) = match &*l.state.try_borrow().ok()? {
            LazyIterKind::Zip {
                iters,
                strict: false,
                held,
                ..
            } if held.is_empty() && iters.len() == 2 => (iters[0].clone(), iters[1].clone()),
            _ => return None,
        };
        let mut out = Vec::new();
        Some((|| loop {
            let Some(x) = self.seq_step_source(&a, globals)? else {
                return Ok(out);
            };
            let Some(y) = self.seq_step_source(&b, globals)? else {
                self.release(x);
                return Ok(out);
            };
            out.push((x, y));
        })())
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
        // (What the frame JIT's direct step already took from the first
        // sources of this step, if it stopped partway: see
        // `Self::lazy_gen_pair`. Taken before anything runs, so a raise
        // below drops it, as CPython drops a step's items.)
        let mut items = match &mut *l.state.borrow_mut() {
            LazyIterKind::Zip { held, .. } if !held.is_empty() => std::mem::take(held),
            _ => Vec::with_capacity(n),
        };
        for i in items.len()..n {
            let Some(it) = source(l, i) else {
                return Ok(None);
            };
            // (Strict mode's checks clear a `StopIteration`, as they read on.)
            let step = if strict {
                self.iter_next(&it, globals)?
            } else {
                self.seq_step_source(&it, globals)?
            };
            match step {
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
                        if self.iter_next(&it, globals)?.is_some() {
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
        // A plain sequence cursor's items whose truth is plain data, stepped
        // in place: anything else (an item whose `__bool__` could run code,
        // the end) is the general step's, from where this one stopped.
        if check_true {
            if let Object::Iter(it) = source {
                // SAFETY: nothing below runs code while the view lives (a
                // falsy item dropped here is a scalar or a `str`).
                if let Some(cursor) = unsafe { it.peek_mut() } {
                    while let Some(item) = cursor.pure_peek() {
                        match scalar_truth(&item) {
                            Some(true) => {
                                cursor.pure_skip();
                                return Ok(Some(item));
                            }
                            Some(false) => {
                                cursor.pure_skip();
                            }
                            None => break,
                        }
                    }
                }
            }
        }
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
    // (A `StopIteration` its source raised goes on, value and all.)
    match interp.lazy_iter_step(&l, &globals)? {
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
            // A `chain` whose current source is a plain cursor with an
            // item left (`islice(chain(xs, ys), n)`), for one-source
            // adapters only: a `zip` could hold that cursor as another
            // source, and the chain's step would exhaust it for that one.
            // (A cursor, never another adapter: a chain may come to step
            // itself.)
            LazyIterKind::Chain { source, active } => {
                !leaf && chain_pure_ready(source.as_ref(), active.as_ref())
            }
            _ => false,
        }),
        _ => false,
    }
}

/// Whether [`chain_pure_next`] has an item for `chain`'s state.
fn chain_pure_ready(source: Option<&Object>, active: Option<&Object>) -> bool {
    match active {
        Some(a @ Object::Iter(_)) if src_ready(a, false) => true,
        // (A cursor, never another adapter: a chain may come to step
        // itself.)
        Some(a) if !spent_cursor_droppable(a) => false,
        _ => chain_next_source(source).is_some(),
    }
}

/// `chain`'s next item where no code runs: the current source's, a plain
/// cursor's; or, that one spent (or none begun), the first item of the
/// next source, an exact list or tuple with items that a plain cursor
/// over the sources holds next. `None`, touching nothing, otherwise.
/// (Releasing a spent cursor frees nothing: its sequence is held
/// elsewhere.)
fn chain_pure_next(source: &mut Option<Object>, active: &mut Option<Object>) -> Option<Object> {
    // The common step: the active source a plain cursor with an item.
    if let Some(Object::Iter(c)) = active.as_ref() {
        // SAFETY: a step that runs no code; the view ends with it.
        if let Some(v) = unsafe { c.peek_mut() }.and_then(PyIterator::pure_next) {
            return Some(v);
        }
    }
    if let Some(a) = active.as_ref() {
        if let Object::Iter(c) = a {
            if let Some(v) = cursor_peek(c) {
                cursor_advance(c)?;
                return Some(v);
            }
        }
        if !spent_cursor_droppable(a) {
            return None;
        }
    }
    let (sources, next) = chain_next_source(source.as_ref())?;
    let it = Rc::new(RefCell::new(next.make_iter().ok()?));
    let v = cursor_peek(&it)?;
    cursor_advance(&it)?;
    cursor_advance(sources)?;
    let old = active.replace(Object::Iter(it));
    drop(old);
    Some(v)
}

/// The cursor over `chain`'s sources and its next one, where that is an
/// exact list or tuple with items.
fn chain_next_source(source: Option<&Object>) -> Option<(&Rc<RefCell<PyIterator>>, Object)> {
    let Some(Object::Iter(sources)) = source else {
        return None;
    };
    let next = cursor_peek(sources)?;
    let nonempty = match &next {
        // SAFETY: a read that runs no code.
        Object::List(l) => unsafe { l.peek() }.is_some_and(|v| !v.is_empty()),
        Object::Tuple(t) => !t.is_empty(),
        _ => false,
    };
    nonempty.then_some((sources, next))
}

/// Whether `a`, `chain`'s current source, is a spent plain cursor whose
/// release frees nothing (its sequence, if any, held elsewhere too).
fn spent_cursor_droppable(a: &Object) -> bool {
    let Object::Iter(c) = a else {
        return false;
    };
    // SAFETY: a read that runs no code.
    let Some(cur) = (unsafe { c.peek() }) else {
        return false;
    };
    if cur.pure_ready() {
        return false;
    }
    match cur {
        PyIterator::List {
            items, owner: None, ..
        } => Rc::strong_count(c) > 1 || Rc::strong_count(items) > 1,
        PyIterator::Tuple { items, .. } => {
            Rc::strong_count(c) > 1 || crate::object::SharedTuple::strong_count(items) > 1
        }
        PyIterator::Range { .. } | PyIterator::Str { .. } => true,
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
fn srcs_ready(iters: &[Object], distinct: &mut Option<bool>) -> bool {
    iters.iter().all(|it| src_ready(it, true))
        && *distinct.get_or_insert_with(|| srcs_distinct(iters))
}

/// Whether no two of `iters` are the same iterator. (Sorted addresses for
/// more than a few: `zip(*rows)` has a source per row.)
fn srcs_distinct(iters: &[Object]) -> bool {
    let addr = |it: &Object| match it {
        Object::Iter(c) => Some(Rc::as_ptr(c) as usize),
        Object::LazyIter(l) => Some(Rc::as_ptr(l) as usize),
        _ => None,
    };
    if iters.len() <= 8 {
        return iters.iter().enumerate().all(|(i, a)| {
            let a = addr(a);
            a.is_none() || iters[..i].iter().all(|b| addr(b) != a)
        });
    }
    let mut addrs: Vec<usize> = iters.iter().filter_map(addr).collect();
    addrs.sort_unstable();
    addrs.windows(2).all(|w| w[0] != w[1])
}

/// One step of a native adapter that runs no code (the core loop's
/// `FOR_ITER`): `None` when the step needs the interpreter, nothing
/// having been consumed. Every source must have an item left, so the
/// step neither stops partway through the sources nor releases a
/// source's container (which could queue a finalizer). (So a strict
/// `zip` steps the same way: its end, where the strictness check is, is
/// the general step's.)
pub(crate) fn lazy_pure_next(l: &PyLazyIter) -> Option<Option<Object>> {
    // SAFETY: nothing below runs code; the view ends with this call.
    let st = unsafe { l.state.peek_mut() }?;
    let LazyIterKind::Zip {
        iters,
        strict: _,
        result,
        spare,
        held,
        distinct,
    } = st
    else {
        return itertools_pure_next(l);
    };
    // (A step in progress is the general step's to finish.)
    if !held.is_empty() {
        return None;
    }
    let n = iters.len();
    // Two plain sequence cursors (`zip(xs, ys)`), the commonest shape.
    if let [a, b] = iters.as_slice() {
        if let Some((x, y)) = cursor_pair(a, b) {
            if let Some(slots) = reusable_result(result, spare, 2) {
                let old = (
                    std::mem::replace(&mut slots[0], x),
                    std::mem::replace(&mut slots[1], y),
                );
                drop(old);
                return Some(Some(Object::Tuple(result.clone()?)));
            }
            return Some(Some(fresh_result(
                result,
                spare,
                crate::object::TupleStorage::from_array([x, y]),
            )));
        }
    }
    // Up to eight plain sequence cursors (`zip(xs, ys, zs)`): their items,
    // into the previous result when this adapter holds it alone.
    if (3..=CURSORS_MAX).contains(&n) {
        // (Tuple and list cursors, straight into the result's slots, in
        // one pass over them.)
        if let Some(slots) = reusable_result(result, spare, n) {
            if seq_cursors_fill(iters, slots) {
                return Some(Some(Object::Tuple(result.clone()?)));
            }
        } else {
            let mut items = vec![Object::None; n];
            if seq_cursors_fill(iters, &mut items) {
                let t = crate::object::TupleStorage::from_vec(items);
                return Some(Some(fresh_result(result, spare, t)));
            }
        }
        if let Some(mut items) = cursors_next(iters) {
            if let Some(slots) = reusable_result(result, spare, n) {
                // (The old items leave in `items`, released below.)
                for (slot, x) in slots.iter_mut().zip(items.iter_mut()) {
                    std::mem::swap(slot, x);
                }
                drop(items);
                return Some(Some(Object::Tuple(result.clone()?)));
            }
            let t = crate::object::TupleStorage::from_vec(items[..n].to_vec());
            return Some(Some(fresh_result(result, spare, t)));
        }
    }
    if !srcs_ready(iters, distinct) {
        return None;
    }
    if iters.is_empty() {
        return Some(None);
    }
    // The previous result, refilled when this adapter holds it alone.
    if let Some(slots) = reusable_result(result, spare, n) {
        for (slot, it) in slots.iter_mut().zip(iters.iter()) {
            *slot = src_step(it)?;
        }
        return Some(Some(Object::Tuple(result.clone()?)));
    }
    let t = match iters.as_slice() {
        [a, b] => crate::object::TupleStorage::from_array([src_step(a)?, src_step(b)?]),
        _ => crate::object::TupleStorage::from_vec(
            iters.iter().map(src_step).collect::<Option<Vec<_>>>()?,
        ),
    };
    Some(Some(fresh_result(result, spare, t)))
}

/// The slots of a `zip`'s result tuple of `n` items that only the adapter
/// holds: `result`'s, or else its spare's (which then becomes `result`).
#[inline]
fn reusable_result<'a>(
    result: &'a mut Option<crate::object::SharedTuple>,
    spare: &mut Option<crate::object::SharedTuple>,
    n: usize,
) -> Option<&'a mut [Object]> {
    let alone = |t: &mut Option<crate::object::SharedTuple>| {
        t.as_mut()
            .and_then(crate::shared_value::ThinArc::get_mut)
            .is_some_and(|slots| slots.len() == n)
    };
    if !alone(result) {
        if !alone(spare) {
            return None;
        }
        std::mem::swap(result, spare);
    }
    result
        .as_mut()
        .and_then(crate::shared_value::ThinArc::get_mut)
        .map(|slots| &mut slots[..])
}

/// A `zip`'s new result `t`, kept as `result` (the old one as the spare).
#[inline]
fn fresh_result(
    result: &mut Option<crate::object::SharedTuple>,
    spare: &mut Option<crate::object::SharedTuple>,
    t: crate::object::SharedTuple,
) -> Object {
    *spare = result.replace(t.clone());
    Object::Tuple(t)
}

/// [`lazy_pure_next`] for a `zip` of `n` (at most four) distinct plain
/// sequence cursors whose result is unpacked at once: each one's next item
/// written from `out` as the unpacking leaves them (the last lowest), no
/// tuple built, and `true`; `false` (nothing taken) unless every cursor
/// has one.
///
/// # Safety
///
/// `out` has `n` free slots.
pub(crate) unsafe fn lazy_pure_cursors_to(l: &PyLazyIter, out: *mut Object, n: usize) -> bool {
    // SAFETY: nothing below runs code; the view ends with this call.
    let Some(st) = (unsafe { l.state.peek() }) else {
        return false;
    };
    // (A strict `zip` too: its end is the general step's.)
    let LazyIterKind::Zip { iters, held, .. } = st else {
        return false;
    };
    if !held.is_empty() || iters.len() != n || n > 4 {
        return false;
    }
    let mut runs: [(*const Object, usize, *mut usize); 4] =
        [(std::ptr::null(), 0, std::ptr::null_mut()); 4];
    for (k, it) in iters.iter().enumerate() {
        let Object::Iter(c) = it else {
            return false;
        };
        // (Distinct cells, so the views never alias.)
        if iters[..k]
            .iter()
            .any(|d| matches!(d, Object::Iter(d) if Rc::ptr_eq(c, d)))
        {
            return false;
        }
        // SAFETY: distinct cells, each viewed once; nothing runs code until
        // the views end with this call.
        let Some((xs, i)) = (unsafe { c.peek_mut() }).and_then(seq_cursor) else {
            return false;
        };
        if *i >= xs.len() {
            return false;
        }
        runs[k] = (xs.as_ptr(), *i, std::ptr::from_mut(i));
    }
    for (k, &(xs, i, at)) in runs[..n].iter().enumerate() {
        // SAFETY: `i` is below its sequence's length (checked above), the
        // sequences live while their cursors do, and `out` has `n` slots.
        unsafe {
            out.add(n - 1 - k).write(crate::clone_hot(&*xs.add(i)));
            *at = i + 1;
        }
    }
    true
}

/// [`lazy_pure_next`] for a two-source `zip` whose pair is unpacked at
/// once: the two items, with no tuple built.
pub(crate) fn lazy_pure_pair(l: &PyLazyIter) -> Option<(Object, Object)> {
    // SAFETY: nothing below runs code; the view ends with this call.
    let st = unsafe { l.state.peek() }?;
    // (A strict `zip` too: see `lazy_pure_next`.)
    let LazyIterKind::Zip {
        iters,
        strict: _,
        held,
        ..
    } = st
    else {
        return None;
    };
    if !held.is_empty() {
        return None;
    }
    let [a, b] = iters.as_slice() else {
        return None;
    };
    if let Some(pair) = cursor_pair(a, b) {
        return Some(pair);
    }
    // (Two sources: compared outright, the state only being read here.)
    if !srcs_ready(iters, &mut None) {
        return None;
    }
    Some((src_step(a)?, src_step(b)?))
}

/// `next(it[, default])`'s fast half: the next item of a plain sequence
/// cursor or a native adapter that steps without running code. `None`
/// (nothing consumed) for anything else, an exhausted iterator included,
/// which the full call answers.
pub(crate) fn next_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let it = match args {
        [it] | [it, _] => it,
        _ => return None,
    };
    let v = match it {
        Object::Iter(c) => {
            // SAFETY: a view that runs no code and ends with the step.
            let cursor = unsafe { c.peek_mut() }?;
            match cursor.pure_peek() {
                Some(v) => {
                    cursor.pure_skip();
                    v
                }
                // A dict's or dict view's iterator, stepped as `for`
                // steps it (`next(iter(d))`, the first key): any change of
                // the dict, and the end, are the full call's to report.
                None if matches!(cursor, PyIterator::DictKeys { .. }) => {
                    crate::Interpreter::core_dict_next_item(cursor)?
                }
                None => return None,
            }
        }
        Object::LazyIter(l) => lazy_pure_next(l)??,
        // An `os.scandir` iterator (`os.walk`'s `next(scandir_it)`): its
        // `__next__`'s step, the default or `StopIteration` at the end.
        Object::Instance(inst) => match crate::stdlib::os::scandir_step(inst)? {
            Some(v) => v,
            None => {
                return Some(match args {
                    [_, default] => Ok(default.clone()),
                    _ => Err(crate::error::stop_iteration()),
                })
            }
        },
        _ => return None,
    };
    Some(Ok(v))
}

/// The most sources [`cursors_next`] steps at once.
const CURSORS_MAX: usize = 8;

/// The next items of distinct tuple and list cursors, in one pass: when
/// every one of the distinct plain sequence cursors `iters` has an item left,
/// each steps, its item written over the matching slot of `out` (the old
/// one released), and `true`; otherwise `false`, nothing touched.
fn seq_cursors_fill(iters: &[Object], out: &mut [Object]) -> bool {
    let n = iters.len();
    if n > CURSORS_MAX || out.len() < n {
        return false;
    }
    // (Distinct cells first, so the views below never alias.)
    for (k, it) in iters.iter().enumerate() {
        let Object::Iter(c) = it else {
            return false;
        };
        if iters[..k]
            .iter()
            .any(|d| matches!(d, Object::Iter(d) if Rc::ptr_eq(c, d)))
        {
            return false;
        }
    }
    let mut runs: [(*const Object, usize, *mut usize); CURSORS_MAX] =
        [(std::ptr::null(), 0, std::ptr::null_mut()); CURSORS_MAX];
    for (run, it) in runs.iter_mut().zip(iters) {
        let Object::Iter(c) = it else {
            return false;
        };
        // SAFETY: distinct cells, each viewed once; nothing runs code until
        // the views end with this call.
        let Some((xs, i)) = (unsafe { c.peek_mut() }).and_then(seq_cursor) else {
            return false;
        };
        if *i >= xs.len() {
            return false;
        }
        *run = (xs.as_ptr(), *i, std::ptr::from_mut(i));
    }
    for (slot, &(xs, i, at)) in out.iter_mut().zip(&runs[..n]) {
        // SAFETY: `i` is below its sequence's length (checked above), and
        // the sequences live while their cursors do; an item's release only
        // queues work for the next safe point.
        unsafe {
            *slot = (*xs.add(i)).clone();
            *at = i + 1;
        }
    }
    true
}

/// [`cursor_pair`] for up to [`CURSORS_MAX`] distinct plain sequence
/// cursors: every one's next item, all taken, in the first `iters.len()`
/// slots of the result; `None` (none taken) unless each has one.
fn cursors_next(iters: &[Object]) -> Option<[Object; CURSORS_MAX]> {
    let n = iters.len();
    if n > CURSORS_MAX {
        return None;
    }
    let mut cells: [Option<&Rc<crate::sync::RefCell<crate::object::PyIterator>>>; CURSORS_MAX] =
        [None; CURSORS_MAX];
    for (k, it) in iters.iter().enumerate() {
        let Object::Iter(c) = it else { return None };
        if cells[..k].iter().flatten().any(|d| Rc::ptr_eq(c, d)) {
            return None;
        }
        cells[k] = Some(c);
    }
    let mut items = [const { Object::None }; CURSORS_MAX];
    for (slot, c) in items.iter_mut().zip(cells.iter().flatten()) {
        // SAFETY: distinct cells, one viewed at a time; nothing here runs
        // code, and each view ends with its step.
        *slot = unsafe { c.peek_mut()? }.pure_peek()?;
    }
    for c in cells.iter().flatten() {
        // SAFETY: as above; `pure_skip` steps past exactly what
        // `pure_peek` found.
        unsafe { c.peek_mut()? }.pure_skip();
    }
    Some(items)
}

/// `list(islice(seq, ...))` and its kin: the rest of an `islice` over a
/// tuple or list cursor only it holds, taken at once in CPython's
/// `islice_next` order (the skipped items first, then the stop bound),
/// the adapter left finished with its source released. `None`, nothing
/// consumed, for any other `islice` or iterator.
/// `list(product(...))` and its kin: every row of a combinatoric machine
/// (`product`, `permutations`, `combinations`, `combinations_with_replacement`,
/// a bounded `repeat`) stepped straight to its end, as the items one at a
/// time would leave it (rows are built from the pools it holds, running no
/// code). `None` for any other iterator.
pub(crate) fn machine_collect(l: &PyLazyIter) -> Option<Vec<Object>> {
    // SAFETY: nothing below runs code; the view ends with this call.
    let st = unsafe { l.state.peek_mut() }?;
    if !matches!(
        st,
        LazyIterKind::Product { .. }
            | LazyIterKind::Permutations { .. }
            | LazyIterKind::Combinations { .. }
            | LazyIterKind::Cwr { .. }
            | LazyIterKind::Repeat { times: Some(_), .. }
    ) {
        return None;
    }
    let mut out = Vec::new();
    while let Some(row) = machine_step(st)? {
        out.push(row);
    }
    Some(out)
}

/// The items a list's or tuple's cursor has left, cloned (which runs no
/// code), the cursor moved past them (the step that finds its end still
/// to come). `None`, touching nothing, for any other source.
fn cursor_take_rest(source: &Object) -> Option<Vec<Object>> {
    use crate::object::PyIterator;
    let Object::Iter(c) = source else {
        return None;
    };
    // SAFETY: reads and an index move that run no code; the view ends here.
    match unsafe { c.peek_mut() }? {
        PyIterator::Tuple { items, index } => {
            let rest = items
                .get(*index..)
                .map_or_else(Vec::new, <[Object]>::to_vec);
            *index += rest.len();
            Some(rest)
        }
        PyIterator::List {
            items,
            index,
            owner: None,
        } => {
            let rest = unsafe { items.peek() }?
                .get(*index..)
                .map_or_else(Vec::new, <[Object]>::to_vec);
            *index += rest.len();
            Some(rest)
        }
        _ => None,
    }
}

/// `pairwise(xs)` or a non-strict (or evenly dividing) `batched(xs, n)`
/// over a list's or tuple's cursor, drained at once: every pair or batch
/// left, the adapter left as its last step leaves it (its source stepped
/// to its end and let go). Only where letting go of the source runs
/// nothing; `None`, nothing consumed, for anything else.
pub(crate) fn window_collect(l: &PyLazyIter) -> Option<Vec<Object>> {
    use crate::object::PyIterator;
    // SAFETY: nothing below runs code (clones, tuples, quiet releases);
    // the view ends with this call.
    let st = unsafe { l.state.peek_mut() }?;
    let (source, rest_len) = match st {
        LazyIterKind::Pairwise { source, old } if old.as_ref().is_none_or(plain_value) => {
            (source, None)
        }
        LazyIterKind::Batched { source, n, strict } => (source, Some((*n, *strict))),
        _ => return None,
    };
    if !quiet_source(source.as_ref()?) {
        return None;
    }
    // (A strict batching whose last batch would be short raises: the
    // general step's.)
    if let Some((n, true)) = rest_len {
        let Some(Object::Iter(c)) = source.as_ref() else {
            return None;
        };
        // SAFETY: a read that runs no code.
        let left = match unsafe { c.peek() }? {
            PyIterator::Tuple { items, index } => items.len().saturating_sub(*index),
            PyIterator::List {
                items,
                index,
                owner: None,
            } => unsafe { items.peek() }?.len().saturating_sub(*index),
            _ => return None,
        };
        if left % n != 0 {
            return None;
        }
    }
    let rest = cursor_take_rest(source.as_ref()?)?;
    let src = source.take()?;
    exhaust_cursor(&src);
    let mut out = Vec::new();
    match st {
        LazyIterKind::Pairwise { old, .. } => {
            let mut it = rest.into_iter();
            let mut prev = old.take().or_else(|| it.next());
            out.reserve(it.len());
            for x in it {
                let p = prev.replace(x.clone()).expect("a previous item");
                out.push(Object::new_tuple_array([p, x]));
            }
            drop(prev);
        }
        LazyIterKind::Batched { n, .. } => {
            let n = *n;
            out.reserve(rest.len().div_ceil(n));
            let mut it = rest.into_iter();
            loop {
                let batch: Vec<Object> = it.by_ref().take(n).collect();
                if batch.is_empty() {
                    break;
                }
                out.push(Object::new_tuple(batch));
            }
        }
        _ => unreachable!("matched above"),
    }
    drop(src);
    Some(out)
}

/// `accumulate(xs)` with no function over a plain list or tuple cursor:
/// every remaining running total (the initial value first, when one is
/// still to come), the adapter left as its last step leaves it: its total
/// the last, its cursor at the end (the end itself changes nothing).
/// Totals of machine ints (none overflowing) or floats, the first item
/// any object; `None`, nothing consumed, for anything else.
pub(crate) fn accumulate_collect(l: &PyLazyIter) -> Option<Vec<Object>> {
    // SAFETY: nothing below runs code (clones and native sums); the views
    // end with this call.
    let st = unsafe { l.state.peek_mut() }?;
    let LazyIterKind::Accumulate {
        source: Object::Iter(c),
        func: None,
        total,
        initial,
    } = st
    else {
        return None;
    };
    // SAFETY: the source's own cell, viewed alone.
    let (xs, idx) = unsafe { c.peek_mut() }.and_then(seq_cursor)?;
    let rest = xs.get(*idx..)?;
    let mut out = Vec::with_capacity(rest.len() + 1);
    let mut t = total.clone();
    if let Some(init) = initial.as_ref() {
        out.push(init.clone());
        t = Some(init.clone());
    }
    for item in rest {
        let next = match (&t, item) {
            (None, _) => item.clone(),
            (Some(Object::Int(a)), Object::Int(b)) => Object::Int(a.checked_add(*b)?),
            (Some(Object::Float(a)), Object::Float(b)) => Object::Float(a + b),
            _ => return None,
        };
        out.push(next.clone());
        t = Some(next);
    }
    // (Committed: the general steps would have left exactly this.)
    *initial = None;
    *idx = xs.len();
    let old = std::mem::replace(total, t);
    drop(old);
    Some(out)
}

pub(crate) fn islice_collect(l: &PyLazyIter) -> Option<Vec<Object>> {
    // SAFETY: nothing below runs code until the source's release, after
    // the views end.
    let st = unsafe { l.state.peek_mut() }?;
    let LazyIterKind::Islice {
        source,
        next_idx,
        pos,
        stop,
        step,
        done,
    } = st
    else {
        return None;
    };
    let Object::Iter(c) = source else {
        return None;
    };
    if *done || Rc::strong_count(c) != 1 || *step == 0 {
        return None;
    }
    // SAFETY: the source's own cell, viewed alone.
    let (xs, idx) = unsafe { c.peek_mut() }.and_then(seq_cursor)?;
    let len = xs.len();
    let mut i = *idx;
    let mut out = Vec::new();
    loop {
        if *pos < *next_idx {
            let skip = *next_idx - *pos;
            let left = len.saturating_sub(i) as u64;
            if skip > left {
                *pos += left;
                break;
            }
            i += skip as usize;
            *pos += skip;
        }
        if stop.is_some_and(|s| *pos >= s) || i >= len {
            break;
        }
        out.push(xs[i].clone());
        i += 1;
        *pos += 1;
        *next_idx = next_idx.saturating_add(*step);
        if let Some(s) = *stop {
            *next_idx = (*next_idx).min(s);
        }
    }
    *idx = i;
    *done = true;
    // (CPython clears `lz->it`: the source and what only it holds go.)
    let released = std::mem::replace(source, Object::None);
    drop(released);
    Some(out)
}

/// Whether a native adapter stops now with nothing consumed and its
/// release, when this is the last reference, frees nothing that could
/// hold a finalizer: the core loop's `FOR_ITER` end, in place. A `zip`
/// whose first source, a tuple or list cursor, is at its end, and an
/// `accumulate` without a function over one (its end changes no state,
/// as CPython's doesn't).
pub(crate) fn lazy_stops_quietly(l: &Rc<PyLazyIter>) -> bool {
    // SAFETY: reads that run no code, ending with this call.
    let Some(st) = (unsafe { l.state.peek() }) else {
        return false;
    };
    let last = Rc::strong_count(l) == 1;
    match st {
        LazyIterKind::Zip {
            iters,
            strict: false,
            result,
            spare,
            held,
            distinct: _,
        } => {
            let Some(first) = iters.first() else {
                return false;
            };
            if !held.is_empty() || !cursor_at_end(first) {
                return false;
            }
            !last
                || (iters.iter().all(quiet_source)
                    && quiet_tuple(result.as_ref())
                    && quiet_tuple(spare.as_ref()))
        }
        LazyIterKind::Accumulate {
            source,
            func: None,
            total,
            initial: None,
        } => {
            cursor_at_end(source)
                && (!last || (quiet_source(source) && total.as_ref().is_none_or(plain_value)))
        }
        _ => false,
    }
}

/// Whether `it` is a tuple's or an unowned list's cursor at its end.
pub(crate) fn cursor_at_end(it: &Object) -> bool {
    use crate::object::PyIterator;
    let Object::Iter(c) = it else {
        return false;
    };
    // SAFETY: reads that run no code, ending with this call.
    match unsafe { c.peek() } {
        Some(PyIterator::Tuple { items, index }) => *index >= items.len(),
        Some(PyIterator::List {
            items,
            index,
            owner: None,
        }) => unsafe { items.peek() }.is_some_and(|v| *index >= v.len()),
        _ => false,
    }
}

/// The step that finds the end of a cursor [`cursor_at_end`] found at its
/// end, so one someone else holds is exhausted as a general step leaves
/// it (a list's sees no later appends). Runs no code.
fn exhaust_cursor(it: &Object) {
    if let Object::Iter(c) = it {
        // SAFETY: a step that runs no code at the end; the view ends here.
        if let Some(cur) = unsafe { c.peek_mut() } {
            let end = cur.next_value();
            debug_assert!(end.is_none());
            drop(end);
        }
    }
}

/// A scalar dies quietly.
fn plain_value(v: &Object) -> bool {
    matches!(
        v,
        Object::Int(_)
            | Object::Float(_)
            | Object::Bool(_)
            | Object::None
            | Object::Str(_)
            | Object::Bytes(_)
    )
}

/// A source whose sequence another owner keeps (or that holds only
/// scalars) releases quietly.
fn quiet_source(o: &Object) -> bool {
    use crate::object::PyIterator;
    let Object::Iter(rc) = o else {
        return false;
    };
    if Rc::strong_count(rc) > 1 {
        return true;
    }
    // SAFETY: reads that run no code, ending with this call.
    match unsafe { rc.peek() } {
        Some(PyIterator::Tuple { items, .. }) => {
            crate::shared_value::ThinArc::strong_count(items) > 1 || items.iter().all(plain_value)
        }
        Some(PyIterator::List { items, .. }) => {
            Rc::strong_count(items) > 1
                || unsafe { items.peek() }.is_some_and(|v| v.iter().all(plain_value))
        }
        Some(PyIterator::Range { .. }) => true,
        _ => false,
    }
}

/// A result tuple a `zip` keeps for reuse, released quietly.
fn quiet_tuple(t: Option<&crate::object::SharedTuple>) -> bool {
    t.is_none_or(|t| crate::shared_value::ThinArc::strong_count(t) > 1 || t.iter().all(plain_value))
}

/// A tuple's or an unowned list's cursor: the items, and the position.
#[inline(always)]
fn seq_cursor(it: &mut crate::object::PyIterator) -> Option<(&[Object], &mut usize)> {
    use crate::object::PyIterator;
    match it {
        PyIterator::Tuple { items, index } => Some((&items[..], index)),
        PyIterator::List {
            items,
            index,
            owner: None,
        } => {
            // SAFETY: a read that runs no code, ending before anything does.
            let v = unsafe { items.peek() }?;
            Some((&v[..], index))
        }
        _ => None,
    }
}

/// The next items of two distinct plain sequence cursors, both taken; `None`
/// (neither taken) unless both have one.
#[inline]
fn cursor_pair(a: &Object, b: &Object) -> Option<(Object, Object)> {
    let (Object::Iter(ca), Object::Iter(cb)) = (a, b) else {
        return None;
    };
    if Rc::ptr_eq(ca, cb) {
        return None;
    }
    // SAFETY: two distinct cells; nothing below runs code, and the views
    // end with this call.
    let (ia, ib) = unsafe { (ca.peek_mut()?, cb.peek_mut()?) };
    // Two tuple or list cursors (the commonest), indexed directly.
    if let (Some((xs, i)), Some((ys, j))) = (seq_cursor(ia), seq_cursor(ib)) {
        let pair = (xs.get(*i)?.clone(), ys.get(*j)?.clone());
        *i += 1;
        *j += 1;
        return Some(pair);
    }
    let x = ia.pure_peek()?;
    let y = ib.pure_peek()?;
    // (`pure_skip` steps past exactly what `pure_peek` found.)
    ia.pure_skip();
    ib.pure_skip();
    Some((x, y))
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
                return Some(Some(Object::tuple_from_refs(pools.iter().map(|p| &p[0]))));
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
            Some(Some(Object::tuple_from_refs(
                pools.iter().zip(indices.iter()).map(|(p, &ix)| &p[ix]),
            )))
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
                return Some(Some(Object::tuple_from_refs(
                    indices[..r].iter().map(|&ix| &pool[ix]),
                )));
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
                    return Some(Some(Object::tuple_from_refs(
                        indices[..r].iter().map(|&ix| &pool[ix]),
                    )));
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
                return Some(Some(Object::tuple_from_refs(
                    indices.iter().map(|&ix| &pool[ix]),
                )));
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
            Some(Some(Object::tuple_from_refs(
                indices.iter().map(|&ix| &pool[ix]),
            )))
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
                return Some(Some(Object::tuple_from_refs(
                    indices.iter().map(|&ix| &pool[ix]),
                )));
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
            Some(Some(Object::tuple_from_refs(
                indices.iter().map(|&ix| &pool[ix]),
            )))
        }
        _ => None,
    }
}

/// The next item of a native iterator source when taking it runs no
/// code and releases nothing (see [`PyIterator::pure_ready`]).
fn ready_next(it: &Object) -> Option<Object> {
    // A plain sequence cursor (`chain(xs, ys)`'s, `islice(xs, n)`'s), in
    // one view.
    if let Object::Iter(c) = it {
        // SAFETY: a step that runs no code; the view ends with this call.
        let cur = unsafe { c.peek_mut() }?;
        if let Some(v) = cur.pure_peek() {
            cur.pure_skip();
            return Some(v);
        }
    }
    if !src_ready(it, false) {
        return None;
    }
    src_step(it)
}

/// `zip`'s items over sources that are all list or tuple iterators (not
/// `strict`, nothing held back), drained in one pass: as many tuples as
/// the shortest has items left, then the end step CPython's takes, one
/// more item from each source before the first exhausted one (dropped,
/// as it drops them) and that one stepped to its end. Cloning items runs
/// no code. `false`, touching nothing, for any other `zip`.
fn zip_seq_bulk(l: &Rc<PyLazyIter>, out: &mut Vec<Object>) -> bool {
    use crate::object::PyIterator;
    const WIDTH: usize = 8;
    let Ok(st) = l.state.try_borrow() else {
        return false;
    };
    let LazyIterKind::Zip {
        iters,
        strict: false,
        held,
        ..
    } = &*st
    else {
        return false;
    };
    let width = iters.len();
    if !held.is_empty() || !(2..=WIDTH).contains(&width) {
        return false;
    }
    let mut cells: [Option<Rc<RefCell<PyIterator>>>; WIDTH] = Default::default();
    for (k, src) in iters.iter().enumerate() {
        let Object::Iter(c) = src else {
            return false;
        };
        // (A source given twice steps twice a row: the general steps'.)
        if cells[..k].iter().flatten().any(|d| Rc::ptr_eq(c, d)) {
            return false;
        }
        cells[k] = Some(c.clone());
    }
    drop(st);
    let cells = &cells[..width];
    // Every source's items and position, read in place (no code runs
    // while the views live).
    let mut views: [(&[Object], usize); WIDTH] = [(&[], 0); WIDTH];
    let mut guards = Vec::with_capacity(width);
    for c in cells.iter().flatten() {
        let Ok(cur) = c.try_borrow() else {
            return false;
        };
        guards.push(cur);
    }
    let mut list_guards = Vec::with_capacity(width);
    for g in &guards {
        match &**g {
            PyIterator::List { items, .. } => match items.try_borrow() {
                Ok(v) => list_guards.push(Some(v)),
                Err(_) => return false,
            },
            PyIterator::Tuple { .. } => list_guards.push(None),
            _ => return false,
        }
    }
    let mut n = usize::MAX;
    for (k, g) in guards.iter().enumerate() {
        let (items, index): (&[Object], usize) = match (&**g, &list_guards[k]) {
            (PyIterator::List { index, .. }, Some(v)) => (v, *index),
            (PyIterator::Tuple { items, index }, None) => (items, *index),
            _ => return false,
        };
        let index = index.min(items.len());
        views[k] = (items, index);
        n = n.min(items.len() - index);
    }
    out.reserve(n);
    for row in 0..n {
        let item = |k: usize| views[k].0[views[k].1 + row].clone();
        out.push(match width {
            2 => Object::new_tuple_array([item(0), item(1)]),
            3 => Object::new_tuple_array([item(0), item(1), item(2)]),
            _ => Object::new_tuple((0..width).map(item).collect()),
        });
    }
    // The end step: each source before the first exhausted one gives one
    // more item, which the step drops.
    let ends: [bool; WIDTH] =
        std::array::from_fn(|k| k < width && views[k].1 + n >= views[k].0.len());
    drop(list_guards);
    drop(guards);
    for c in cells.iter().flatten() {
        match &mut *c.borrow_mut() {
            PyIterator::List { index, .. } | PyIterator::Tuple { index, .. } => *index += n,
            _ => unreachable!("checked above"),
        }
    }
    let mut dropped = Vec::new();
    for (k, c) in cells.iter().flatten().enumerate() {
        let mut cur = c.borrow_mut();
        let last = cur.next_value();
        drop(cur);
        if ends[k] {
            debug_assert!(last.is_none());
            break;
        }
        dropped.extend(last);
    }
    drop(dropped);
    true
}

/// One step of an `itertools` adapter that runs no code and releases
/// nothing (the core loop's `FOR_ITER`, and the interpreter's fast path
/// ahead of the general one): `None` when the step needs the general
/// path, nothing having been consumed. Exhaustion of a source is left to
/// that path, which releases it.
/// [`Interpreter::seq_drain`] of an `itertools.chain` whose iterables are
/// lists and tuples, read from a list or tuple of them (`chain(xs, ys)`,
/// `chain.from_iterable([xs, ys])`): every remaining item is copied into
/// `out` at once, and the chain is exhausted (`true`). A chain reaching any
/// other iterable stops before it (`false`), the items copied so far
/// consumed, for the step-by-step drain to go on from. No Python code runs.
fn chain_drain(l: &PyLazyIter, out: &mut Vec<Object>) -> bool {
    // The items of a plain sequence (a list's, a tuple's) from `from`.
    fn copy_from(v: &Object, from: usize, out: &mut Vec<Object>) -> Option<()> {
        match v {
            Object::List(items) => {
                let items = items.try_borrow().ok()?;
                out.extend_from_slice(items.get(from..).unwrap_or(&[]));
            }
            Object::Tuple(items) => out.extend_from_slice(items.get(from..).unwrap_or(&[])),
            // A `range` of machine ints (`chain(range(a), range(b))`).
            Object::Range(r) if from == 0 && r.big.is_none() => {
                let (start, stop, step) = (
                    i64::try_from(r.start).ok()?,
                    i64::try_from(r.stop).ok()?,
                    i64::try_from(r.step).ok()?,
                );
                range_ints(start, stop, step, out)?;
            }
            _ => return None,
        }
        Some(())
    }
    // `start` to `stop` by `step` (non-zero), at most a million of them.
    fn range_ints(start: i64, stop: i64, step: i64, out: &mut Vec<Object>) -> Option<()> {
        let n = if step > 0 && start < stop {
            (i128::from(stop) - i128::from(start) + i128::from(step) - 1) / i128::from(step)
        } else if step < 0 && start > stop {
            (i128::from(start) - i128::from(stop) - i128::from(step) - 1) / -i128::from(step)
        } else {
            0
        };
        if n > 1 << 20 {
            return None;
        }
        out.reserve(n as usize);
        let mut v = start;
        for _ in 0..n {
            out.push(Object::Int(v));
            v = v.wrapping_add(step);
        }
        Some(())
    }
    // SAFETY: nothing below runs code; the view ends with this call.
    let Some(LazyIterKind::Chain { source, active }) = (unsafe { l.state.peek_mut() }) else {
        return false;
    };
    // The iterable in progress, a list or tuple cursor, runs out first.
    if let Some(Object::Iter(c)) = &*active {
        let Ok(mut c) = c.try_borrow_mut() else {
            return false;
        };
        match &mut *c {
            PyIterator::List {
                items,
                index,
                owner: None,
            } => {
                {
                    let Ok(v) = items.try_borrow() else {
                        return false;
                    };
                    out.extend_from_slice(v.get(*index..).unwrap_or(&[]));
                }
                // Exhausted: detached from the list, as its own last step
                // leaves it (see `PyIterator::next_value`).
                let old = std::mem::replace(items, Rc::new(RefCell::new(Vec::new())));
                *index = 0;
                drop(old);
            }
            PyIterator::Tuple { items, index } => {
                out.extend_from_slice(items.get(*index..).unwrap_or(&[]));
                *index = items.len();
            }
            PyIterator::Range {
                current,
                stop,
                step,
            } => {
                if range_ints(*current, *stop, *step, out).is_none() {
                    return false;
                }
                *current = *stop;
            }
            _ => return false,
        }
    } else if active.is_some() {
        return false;
    }
    let old_active = active.take();
    // Then the iterables still to come, each a list or tuple.
    let done = match source.as_ref() {
        None => true,
        Some(Object::Iter(c)) => {
            let Ok(mut c) = c.try_borrow_mut() else {
                *active = old_active;
                return false;
            };
            match &mut *c {
                PyIterator::List { items, index, .. } => match items.try_borrow() {
                    Ok(items) => loop {
                        match items.get(*index) {
                            Some(v) if copy_from(v, 0, out).is_some() => *index += 1,
                            Some(_) => break false,
                            None => break true,
                        }
                    },
                    Err(_) => false,
                },
                PyIterator::Tuple { items, index } => loop {
                    match items.get(*index) {
                        Some(v) if copy_from(v, 0, out).is_some() => *index += 1,
                        Some(_) => break false,
                        None => break true,
                    }
                },
                _ => false,
            }
        }
        Some(_) => false,
    };
    if done {
        // Exhausted, as the step's own end leaves it.
        let old_source = source.take();
        drop((old_source, old_active));
    } else {
        drop(old_active);
    }
    done
}

pub(crate) fn itertools_pure_next(l: &PyLazyIter) -> Option<Option<Object>> {
    // SAFETY: nothing below runs code; the view ends with this call.
    let st = unsafe { l.state.peek_mut() }?;
    if let Some(r) = machine_step(st) {
        return Some(r);
    }
    match st {
        LazyIterKind::Chain { source, active } => chain_pure_next(source, active).map(Some),
        // `compress` over two distinct plain sequence cursors: the data
        // items whose selector is a plain bool, int or `None`, skipped or
        // taken in place (a dropped item is the sequence's too). Either
        // source's end, and any other selector, take the general step.
        LazyIterKind::Compress { data, selectors } => {
            let (Object::Iter(dc), Object::Iter(sc)) = (&*data, &*selectors) else {
                return None;
            };
            if Rc::ptr_eq(dc, sc) {
                return None;
            }
            loop {
                let (d, s) = (cursor_peek(dc)?, cursor_peek(sc)?);
                let keep = match s {
                    Object::Bool(b) => b,
                    Object::Int(i) => i != 0,
                    Object::None => false,
                    _ => return None,
                };
                cursor_advance(dc)?;
                cursor_advance(sc)?;
                if keep {
                    return Some(Some(d));
                }
            }
        }
        // A tee branch: the shared buffer's next item, or one pulled from
        // a source whose step runs no code (its end, and any other
        // source, take the general step).
        LazyIterKind::TeeBranch { shared, index, .. } => {
            // SAFETY: as above (distinct cells).
            let sh = unsafe { shared.peek_mut() }?;
            if sh.busy {
                return None;
            }
            // SAFETY: as above.
            let buf = unsafe { sh.buffer.peek_mut() }?;
            let item = match buf.get(*index) {
                Some(v) => v.clone(),
                None => {
                    let v = ready_next(sh.source.as_ref()?)?;
                    buf.push(v.clone());
                    v
                }
            };
            *index += 1;
            Some(Some(item))
        }
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
            // (A plain cursor's next item, peeked and skipped in one view
            // each. At its end this is the end, as the general step's
            // would be: nothing changes.)
            if cursor_at_end(source) {
                return Some(None);
            }
            let Object::Iter(c) = source else {
                return None;
            };
            let item = cursor_peek(c)?;
            let next = match (&*total, &item) {
                (None, _) => item,
                (Some(Object::Int(a)), Object::Int(b)) => Object::Int(a.checked_add(*b)?),
                (Some(Object::Float(a)), Object::Float(b)) => Object::Float(a + b),
                _ => return None,
            };
            cursor_advance(c)?;
            *total = Some(next.clone());
            Some(Some(next))
        }
        LazyIterKind::Pairwise {
            source: Some(source),
            old: Some(old),
        } => {
            // At its source's end it lets go of both, as the general step
            // does, when releasing them runs nothing.
            if cursor_at_end(source) {
                if !(plain_value(old) && quiet_source(source)) {
                    return None;
                }
                exhaust_cursor(source);
                let (src, prev) = (std::mem::replace(source, Object::None), old.clone());
                *st = LazyIterKind::Pairwise {
                    source: None,
                    old: None,
                };
                drop((src, prev));
                return Some(None);
            }
            let new = ready_next(source)?;
            let prev = std::mem::replace(old, new.clone());
            Some(Some(Object::new_tuple_array([prev, new])))
        }
        // `batched` over a list's or tuple's cursor: the next `n` items (as
        // many as are left, at the end of a non-strict one), with no code
        // run. A short last batch, or the end itself, takes the step that
        // finds the cursor's end and lets go of the source, as the general
        // step does, when releasing it runs nothing; a strict one's short
        // batch is the general step's (it raises).
        LazyIterKind::Batched { source, n, strict } => {
            use crate::object::PyIterator;
            let (n, strict) = (*n, *strict);
            let Some(Object::Iter(c)) = source.as_ref() else {
                return None;
            };
            let left = {
                // SAFETY: a read that runs no code; the view ends here.
                match unsafe { c.peek() }? {
                    PyIterator::Tuple { items, index } => items.len().saturating_sub(*index),
                    PyIterator::List {
                        items,
                        index,
                        owner: None,
                    } => unsafe { items.peek() }?.len().saturating_sub(*index),
                    _ => return None,
                }
            };
            let last = left < n;
            if last && ((strict && left > 0) || !quiet_source(source.as_ref()?)) {
                return None;
            }
            let mut batch = Vec::with_capacity(left.min(n));
            {
                // SAFETY: steps that run no code; the view ends here.
                let cur = unsafe { c.peek_mut() }?;
                while batch.len() < n {
                    let Some(v) = cur.pure_peek() else {
                        break;
                    };
                    cur.pure_skip();
                    batch.push(v);
                }
                if last {
                    let end = cur.next_value();
                    debug_assert!(end.is_none());
                    drop(end);
                }
                // (A full batch that ends the cursor leaves the end for the
                // next step, as the general step does.)
            }
            if last {
                drop(source.take());
                if batch.is_empty() {
                    return Some(None);
                }
            }
            Some(Some(Object::new_tuple(batch)))
        }
        // `zip_longest` over plain sequence cursors: each active source's
        // next item, or the fill value for one finished before or now (a
        // source finishing now is let go, as the general step lets it go,
        // when releasing it runs nothing); the step that finishes the last
        // one ends it, as the general step does.
        LazyIterKind::ZipLongest {
            iters,
            fillvalue,
            numactive,
        } => {
            if iters.is_empty() || *numactive == 0 {
                return Some(None);
            }
            // (A source passed twice, `zip_longest(it, it)`, steps twice:
            // the general step's.)
            for (i, a) in iters.iter().enumerate() {
                if let Some(Object::Iter(a)) = a {
                    if iters[..i]
                        .iter()
                        .any(|b| matches!(b, Some(Object::Iter(b)) if Rc::ptr_eq(a, b)))
                    {
                        return None;
                    }
                }
            }
            let mut items: Vec<Option<Object>> = Vec::with_capacity(iters.len());
            let mut ending = 0usize;
            for slot in iters.iter() {
                items.push(match slot {
                    None => None,
                    Some(src) if cursor_at_end(src) => {
                        if !quiet_source(src) {
                            return None;
                        }
                        exhaust_cursor(src);
                        ending += 1;
                        None
                    }
                    Some(Object::Iter(c)) => Some(cursor_peek(c)?),
                    Some(_) => return None,
                });
            }
            // The end: the general step keeps its sources and counts none
            // active.
            if ending >= *numactive {
                *numactive = 0;
                return Some(None);
            }
            let mut result = Vec::with_capacity(iters.len());
            let mut done = Vec::new();
            for (slot, item) in iters.iter_mut().zip(items) {
                match item {
                    Some(v) => {
                        if let Some(Object::Iter(c)) = slot {
                            cursor_advance(c)?;
                        }
                        result.push(v);
                    }
                    None => {
                        if let Some(src) = slot.take() {
                            done.push(src);
                            *numactive -= 1;
                        }
                        result.push(fillvalue.clone());
                    }
                }
            }
            drop(done);
            Some(Some(Object::new_tuple(result)))
        }
        _ => None,
    }
}

/// A builtin adapter type's call (`map`, `filter`, `zip`, `enumerate`,
/// `reversed`), `type(x)`, a literal's `int(text)`/`float(text)`, or an
/// empty container, built without running any code (the core loop's
/// `CALL`): `None` for every shape that might.
pub(crate) fn builtin_ctor_pure(cls: &Rc<TypeObject>, args: &[Object]) -> Option<Object> {
    // The type's pure constructor (see `TypeObject::pure_new`): one of the
    // arms below for the builtin types they serve, or one of its own
    // (`io.BytesIO()`, `threading.Lock()`, `object()`).
    cls.pure_new.get()?(cls, args)
}

/// `map(...)`, `filter(...)` and `zip(...)` of the exact adapter types
/// (see [`Interpreter::seq_new_pure`]).
pub(crate) fn seq_ctor_pure(cls: &Rc<TypeObject>, args: &[Object]) -> Option<Object> {
    Interpreter::seq_new_pure(SeqType::of_exact(cls)?, args)
}

/// The value of an `int` subclass's instance whose class keeps `int`'s own
/// `__int__`, as `int(x)` returns it: an exact `int`. `None` for any other
/// instance.
fn int_subclass_value(
    inst: &crate::types::PyInstance,
    bt: &crate::builtin_types::BuiltinTypes,
) -> Option<Object> {
    let v @ (Object::Int(_) | Object::Long(_)) = inst.native.get()? else {
        return None;
    };
    let own = bt
        .int_
        .dict
        .try_borrow()
        .ok()?
        .get(&crate::object::StrKey("__int__"))
        .cloned()?;
    match inst.cls_raw().lookup("__int__") {
        Some(g) if g.is_same(&own) => Some(v.clone()),
        _ => None,
    }
}

/// [`builtin_ctor_pure`]'s arms for the builtin types `builtin_types`
/// registers it on.
pub(crate) fn builtin_ctor_arms(cls: &Rc<TypeObject>, args: &[Object]) -> Option<Object> {
    let bt = builtin_types();
    // `float(x)` and `int(x)` of a plain number or a well-formed `str`
    // (a malformed one raises through the full constructor).
    if Rc::ptr_eq(cls, &bt.float_) {
        return match args {
            [] => Some(Object::Float(0.0)),
            [Object::Float(f)] => Some(Object::Float(*f)),
            [Object::Int(i)] => Some(Object::Float(*i as f64)),
            [Object::Bool(b)] => Some(Object::Float(f64::from(*b))),
            [Object::Str(s)] => crate::builtins::parse_float_text(s).map(Object::Float),
            _ => None,
        };
    }
    if Rc::ptr_eq(cls, &bt.int_) {
        return match args {
            [] => Some(Object::Int(0)),
            [Object::Int(i)] => Some(Object::Int(*i)),
            // An `int` subclass's instance whose class keeps `int`'s own
            // `__int__` (`re`'s named opcode constants): its value, exact.
            [Object::Instance(inst)] => int_subclass_value(inst, bt),
            [Object::Bool(b)] => Some(Object::Int(i64::from(*b))),
            // Truncated toward zero, inside the machine range.
            [Object::Float(f)] if f.is_finite() && f.abs() < 9.0e18 => Some(Object::Int(*f as i64)),
            [Object::Str(text)] => decimal_i64(text)
                .map(Object::Int)
                .or_else(|| big_from_digits(text, 10)),
            // `int('ff', 16)`: plain digits of the base, an optional sign
            // and surrounding ASCII whitespace, inside the machine range
            // (a prefix, an underscore or anything malformed is the full
            // constructor's).
            [Object::Str(text), Object::Int(base)] if (2..=36).contains(base) => {
                // (Hex digits and nothing else, of any width: read once.)
                if *base == 16 {
                    if let Some(v) = crate::builtins::int_from_plain_hex(text.as_bytes()) {
                        return Some(v);
                    }
                }
                let t = text.trim_matches(|c: char| c.is_ascii_whitespace());
                let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_alphanumeric()) {
                    return None;
                }
                // (Digits enough to pass 64 bits skip the machine parse,
                // which would read them all to find that out.)
                let radix = *base as u32;
                let machine = if digits.len() as u32 * radix.ilog2() > 64 {
                    None
                } else {
                    i64::from_str_radix(t, radix).ok()
                };
                machine
                    .map(Object::Int)
                    .or_else(|| big_from_digits(text, radix))
            }
            _ => None,
        };
    }
    // `super(C, obj)` whose receiver passes the plain check (a failing
    // one, or a duck-typed `__class__`, goes the full way).
    if Rc::ptr_eq(cls, &bt.super_) {
        let [Object::Type(class), receiver] = args else {
            return None;
        };
        let receiver_class = crate::builtins::supercheck(class, receiver).ok()?;
        return Some(crate::builtins::build_super_proxy(
            cls.clone(),
            class.clone(),
            receiver.clone(),
            receiver_class,
        ));
    }
    // `str(x)` of an instance that renders with no code run (a `str`,
    // `int` or `float` subclass's payload, an enum member's name), as
    // `Interpreter::do_str_call` answers.
    if Rc::ptr_eq(cls, &bt.str_) {
        return match args {
            [Object::Str(_)] => Some(args[0].clone()),
            [Object::Instance(inst)] => Interpreter::quick_str(inst),
            _ => None,
        };
    }
    // `type(x)`: the object's class (`b_type`'s one-argument form).
    if Rc::ptr_eq(cls, &bt.type_) {
        return match args {
            [x] => Some(Object::Type(crate::builtins::class_of(x))),
            _ => None,
        };
    }
    // `weakref.ref(x)` and `weakref.ref(x, callback)`.
    if let [target, callback @ ..] = args {
        if callback.len() <= 1 && crate::stdlib::weakref_real::is_ref_type(cls) {
            return crate::stdlib::weakref_real::ref_pure(target, callback.first());
        }
    }
    // `dict(d)` of an exact dict, and `dict(zip(xs, ys))` of plain
    // sequences with simple keys.
    if let [src] = args {
        if Rc::ptr_eq(cls, &bt.dict_) {
            if crate::stdlib::tracemalloc_real::is_tracking() || crate::gc_trace::auto_collect_due()
            {
                return None;
            }
            let data = match src {
                // SAFETY: a read between two instructions (cloning runs no
                // code).
                Object::Dict(d) => unsafe { d.peek() }?.clone(),
                Object::LazyIter(l) => dict_from_zip_pure(l)?,
                _ => return None,
            };
            let obj = Object::Dict(Rc::new(crate::sync::RefCell::new(data)));
            crate::gc_trace::track(&obj);
            return Some(obj);
        }
    }
    // `bytes(n)`, `bytes(b)`, `bytearray(n)` and `bytearray(b)` of a small
    // count or an exact `bytes` or `bytearray` (a copy that runs no code).
    let bytes = Rc::ptr_eq(cls, &bt.bytes_);
    if bytes || Rc::ptr_eq(cls, &bt.bytearray_) {
        let data = match args {
            [] => Vec::new(),
            [Object::Int(n)] if (0..=1 << 16).contains(n) => vec![0; *n as usize],
            // (An exact `bytes` is its own copy.)
            [Object::Bytes(_)] if bytes => return Some(args[0].clone()),
            // (A `bytearray`'s bytes go straight into the new `bytes`.)
            [Object::ByteArray(b)] if bytes => {
                return Some(Object::new_bytes(&b.try_borrow().ok()?[..]));
            }
            // (Likewise a live contiguous view's.)
            [Object::MemoryView(mv)] if bytes && !mv.released.get() && mv.is_c_contiguous() => {
                return Some(mv.with_bytes(|run| Object::new_bytes(run)));
            }
            [Object::Bytes(b)] => b.to_vec(),
            [Object::ByteArray(b)] => b.try_borrow().ok()?.clone(),
            _ => return None,
        };
        return Some(if bytes {
            Object::new_bytes(data)
        } else {
            Object::new_bytearray(data)
        });
    }
    // `types.MethodType(func, obj)`: `func` bound to `obj`, as the full
    // constructor binds it.
    if Rc::ptr_eq(cls, &bt.method_) {
        return match args {
            [func, obj] => Some(Object::BoundMethod(Rc::new(
                crate::object::BoundMethod::py_method(obj.clone(), func.clone()),
            ))),
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
    // `tuple(xs)` and `list(xs)` of an exact list or tuple: a copy of its
    // items (an exact tuple is its own tuple), with nothing to iterate.
    if let [src @ (Object::List(_) | Object::Tuple(_))] = args {
        if Rc::ptr_eq(cls, &bt.tuple_) {
            return match src {
                Object::Tuple(_) => Some(src.clone()),
                // SAFETY: a read between two instructions (cloning runs no
                // code).
                Object::List(l) => Some(Object::tuple_from_slice(unsafe { l.peek() }?)),
                _ => None,
            };
        }
        if Rc::ptr_eq(cls, &bt.list_) {
            if crate::stdlib::tracemalloc_real::is_tracking() || crate::gc_trace::auto_collect_due()
            {
                return None;
            }
            let items = match src {
                Object::Tuple(t) => crate::object::cloned_vec(t),
                // SAFETY: as above.
                Object::List(l) => crate::object::cloned_vec(unsafe { l.peek() }?),
                _ => return None,
            };
            let obj = Object::new_list(items);
            crate::gc_trace::track(&obj);
            return Some(obj);
        }
    }
    // `float(x)`, `bool(x)` and `str(x)` of a plain scalar (an int's float
    // is correctly rounded, as `int.__float__` is).
    if let [x] = args {
        if Rc::ptr_eq(cls, &bt.float_) {
            return match x {
                Object::Int(i) => Some(Object::Float(*i as f64)),
                Object::Float(f) => Some(Object::Float(*f)),
                Object::Bool(b) => Some(Object::Float(f64::from(u8::from(*b)))),
                _ => None,
            };
        }
        if Rc::ptr_eq(cls, &bt.bool_) {
            return match x {
                Object::Int(i) => Some(Object::Bool(*i != 0)),
                Object::Float(f) => Some(Object::Bool(*f != 0.0)),
                Object::Bool(b) => Some(Object::Bool(*b)),
                Object::None => Some(Object::Bool(false)),
                Object::Str(s) => Some(Object::Bool(!s.is_empty())),
                // An exact container is true when it holds anything
                // (`bool(items)`, as `if items:` reads it).
                Object::Bytes(b) => Some(Object::Bool(!b.is_empty())),
                Object::Tuple(t) => Some(Object::Bool(!t.is_empty())),
                Object::List(l) => Some(Object::Bool(!l.try_borrow().ok()?.is_empty())),
                Object::Dict(d) => Some(Object::Bool(!d.try_borrow().ok()?.is_empty())),
                Object::Set(s) => Some(Object::Bool(!s.try_borrow().ok()?.is_empty())),
                Object::FrozenSet(s) => Some(Object::Bool(s.iter().next().is_some())),
                _ => None,
            };
        }
        if Rc::ptr_eq(cls, &bt.str_) {
            return match x {
                Object::Int(i) => Some(Object::from_str(i.to_string())),
                Object::Str(_) => Some(x.clone()),
                Object::Bool(b) => Some(Object::from_static(if *b { "True" } else { "False" })),
                _ => None,
            };
        }
    }
    // `slice(stop)`, `slice(start, stop)` and `slice(start, stop, step)`
    // of ints and `None`s: the slice object, which holds them as they are.
    if Rc::ptr_eq(cls, &bt.slice_)
        && (1..=3).contains(&args.len())
        && args
            .iter()
            .all(|a| matches!(a, Object::Int(_) | Object::None))
    {
        let part = |i: usize| args.get(i).cloned().unwrap_or(Object::None);
        let (start, stop, step) = match args.len() {
            1 => (Object::None, part(0), Object::None),
            _ => (part(0), part(1), part(2)),
        };
        return Some(Object::Slice(Rc::new(crate::object::PySlice {
            start,
            stop,
            step,
        })));
    }
    // `set(xs)` and `frozenset(xs)` of an exact list or tuple of plain
    // hashable values: no element's hash or comparison runs any code.
    if let [src @ (Object::List(_) | Object::Tuple(_))] = args {
        let frozen = Rc::ptr_eq(cls, &bt.frozenset_);
        if frozen || Rc::ptr_eq(cls, &bt.set_) {
            if crate::stdlib::tracemalloc_real::is_tracking() || crate::gc_trace::auto_collect_due()
            {
                return None;
            }
            let borrowed;
            let items: &[Object] = match src {
                Object::Tuple(t) => t,
                Object::List(l) => {
                    borrowed = l.try_borrow().ok()?;
                    &borrowed
                }
                _ => return None,
            };
            // (What this saves is the general call around the build, which
            // a long input doesn't notice: that one keeps the general
            // constructor's own bulk path.)
            let simple = |x: &Object| {
                matches!(
                    x,
                    Object::Int(_) | Object::Str(_) | Object::Bool(_) | Object::None
                ) || plain_hashable(x, 0)
            };
            if items.len() > 64 || !items.iter().all(simple) {
                return None;
            }
            // (Sized once, as the general constructor's simple-key path
            // sizes it: a set grown an item at a time rehashes as it goes.)
            let mut data = crate::object::SetData::with_capacity(items.len());
            for v in items {
                data.insert(crate::object::DictKey(v.clone()));
            }
            let obj = if !frozen {
                Object::Set(Rc::new(crate::sync::RefCell::new(data)))
            } else if data.is_empty() {
                // (The shared empty frozenset.)
                Object::new_frozenset_from(std::iter::empty())
            } else {
                Object::FrozenSet(Rc::new(crate::object::FrozenSetObj::new(data)))
            };
            crate::gc_trace::track(&obj);
            return Some(obj);
        }
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
            [Object::List(_)
            | Object::Tuple(_)
            | Object::Range(_)
            | Object::Dict(_)
            | Object::Str(_)
            | Object::Bytes(_)] => crate::builtins::b_reversed(args).ok(),
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
        | Object::DictView(_)
        | Object::MappingProxy(_)
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
        [Object::Tuple(t)] => Some(crate::object::cloned_vec(t)),
        [Object::List(l)] => Some(crate::object::cloned_vec(&l.try_borrow().ok()?)),
        [_] => None,
        _ if multi && args.len() > 1 => Some(args.to_vec()),
        _ => None,
    }
}

/// `min(...)`/`max(...)` over plain ints, floats, or strs of one kind
/// (CPython's rich-comparison loop: a strict comparison keeps the first
/// extremum).
fn min_max_fast(args: &[Object], want_max: bool) -> Option<Result<Object, RuntimeError>> {
    // (A non-empty byte string's or buffer's extreme byte; an empty one
    // raises on the full path.)
    let extreme = |b: &[u8]| {
        let x = if want_max {
            b.iter().max()
        } else {
            b.iter().min()
        };
        x.map(|&x| Ok(Object::Int(i64::from(x))))
    };
    match args {
        [Object::Bytes(b)] => return extreme(b),
        [Object::ByteArray(b)] => return extreme(&b.try_borrow().ok()?),
        _ => {}
    }
    let items = scan_items(args, true)?;
    let (first, rest) = items.split_first()?;
    // Ints, big ints and `int` subclasses that keep `int`'s ordering
    // (`re`'s `min(hi, MAXWIDTH)`, its widths against `1 << 64`): by value,
    // the first of equal ones winning.
    if items
        .iter()
        .any(|x| matches!(x, Object::Instance(_) | Object::Long(_)))
    {
        return min_max_integers(&items, want_max).map(Ok);
    }
    let mut best = first;
    match first {
        Object::Int(_) if rest.iter().all(|x| matches!(x, Object::Int(_))) => {
            for x in rest {
                let (Object::Int(a), Object::Int(b)) = (x, best) else {
                    return None;
                };
                if (want_max && a > b) || (!want_max && a < b) {
                    best = x;
                }
            }
        }
        // Ints and floats mixed (see the float arm).
        Object::Int(_) => {
            for x in rest {
                let (Some(a), Some(b)) = (exact_f64(x), exact_f64(best)) else {
                    return None;
                };
                if (want_max && a > b) || (!want_max && a < b) {
                    best = x;
                }
            }
        }
        Object::Float(_) => {
            for x in rest {
                // An int among floats compares by its exact value: as a
                // float only while that's exact.
                let (Some(a), Some(b)) = (exact_f64(x), exact_f64(best)) else {
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

/// [`min_max_fast`] over integers: machine ints, big ints, and instances
/// of `int` subclasses that keep `int`'s ordering (see
/// `object::int_order_value`). `None` if any item is anything else.
fn min_max_integers(items: &[Object], want_max: bool) -> Option<Object> {
    use std::cmp::Ordering;
    enum Key<'a> {
        Small(i64),
        Big(&'a num_bigint::BigInt),
    }
    fn key(o: &Object) -> Option<Key<'_>> {
        match o {
            Object::Long(b) => Some(Key::Big(b)),
            _ => crate::object::int_order_value(o).map(Key::Small),
        }
    }
    let cmp = |a: &Key<'_>, b: &Key<'_>| match (a, b) {
        (Key::Small(x), Key::Small(y)) => x.cmp(y),
        (Key::Small(x), Key::Big(y)) => num_bigint::BigInt::from(*x).cmp(y),
        (Key::Big(x), Key::Small(y)) => (*x).cmp(&num_bigint::BigInt::from(*y)),
        (Key::Big(x), Key::Big(y)) => x.cmp(y),
    };
    let (first, rest) = items.split_first()?;
    let mut best = (first, key(first)?);
    for x in rest {
        let k = key(x)?;
        let ord = cmp(&k, &best.1);
        if (want_max && ord == Ordering::Greater) || (!want_max && ord == Ordering::Less) {
            best = (x, k);
        }
    }
    Some(best.0.clone())
}

/// `int(text, radix)` for plain digits of the radix (an optional sign,
/// surrounding ASCII whitespace) whose value is past a machine int: the
/// big value, built here once. The full constructor, which the arms above
/// otherwise leave such text to, tries the machine parse again at each of
/// its layers before it builds the same value (a UUID's 32 hex digits
/// cost 7,300 instructions that way). `None` for anything malformed, and
/// for decimal-like text past the conversion limit, which the full
/// constructor reports.
fn big_from_digits(text: &str, radix: u32) -> Option<Object> {
    let t = text.trim_matches(|c: char| c.is_ascii_whitespace());
    let (negative, digits) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    // (Digits of the radix, or the text is the full constructor's to
    // reject: `int("x12")` in a `try` is an everyday test, and it is tried
    // at each layer that reaches these arms.)
    if digits.is_empty() || !digits.bytes().all(|b| (b as char).is_digit(radix)) {
        return None;
    }
    if !radix.is_power_of_two() {
        let max = crate::stdlib::sys::int_max_str_digits();
        if max > 0 && digits.len() > max as usize {
            return None;
        }
    }
    if radix == 16 && !negative {
        if let Some(v) = crate::builtins::int_from_plain_hex(digits.as_bytes()) {
            return Some(v);
        }
    }
    let big = num_bigint::BigInt::parse_bytes(digits.as_bytes(), radix)?;
    Some(Object::int_from_bigint(if negative { -big } else { big }))
}

/// `int(text)` for plain decimal text (ASCII digits, an optional sign,
/// surrounding ASCII whitespace) that fits a machine int; `None` for
/// anything else, which the full constructor parses (or rejects).
fn decimal_i64(text: &str) -> Option<i64> {
    let t = text.trim_matches(|c: char| c.is_ascii_whitespace());
    let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
    if digits.is_empty() || digits.len() > 18 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse().ok()
}

/// A float's value, or a machine int's when a float holds it exactly
/// (so comparing the two is comparing the numbers).
fn exact_f64(x: &Object) -> Option<f64> {
    match x {
        Object::Float(f) => Some(*f),
        Object::Int(i) if i.unsigned_abs() <= 1 << 53 => Some(*i as f64),
        _ => None,
    }
}

/// `round(x[, n])` of a plain number with a plain `int` (or no) digit
/// count: the builtin's own arithmetic, which runs no code for these.
pub(crate) fn round_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let num = |o: &Object| matches!(o, Object::Int(_) | Object::Float(_) | Object::Bool(_));
    match args {
        [x] | [x, Object::None] if num(x) => Some(crate::builtins::b_round(args)),
        [x, Object::Int(_)] if num(x) => Some(crate::builtins::b_round(args)),
        _ => None,
    }
}

/// `repr(x)` of a value whose repr runs no code: a native scalar or a
/// `str`.
pub(crate) fn repr_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    match args {
        [x @ (Object::Int(_)
        | Object::Float(_)
        | Object::Bool(_)
        | Object::None
        | Object::Str(_))] => Some(Ok(Object::from_str(x.repr()))),
        _ => None,
    }
}

/// `list.sort()` with no arguments of a list whose comparisons run no
/// code (see [`sort_plain`]): sorted in place.
pub(crate) fn list_sort_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [Object::List(l)] = args else {
        return None;
    };
    let mut v = l.try_borrow_mut().ok()?;
    Some(sort_plain(&mut v, false)?.map(|()| Object::None))
}

/// `list.sort(*, key=None, reverse=...)`'s keyword half (see
/// `leaf_builtins::register_fast_kw`): the receiver, then `key` and
/// `reverse`, each `Unbound` when the call left it out. Only no key, and a
/// `bool` or `int` flag, of a list `list_sort_fast` sorts.
pub(crate) fn list_sort_kw_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [Object::List(l), key, reverse] = args else {
        return None;
    };
    let reverse = plain_sort_flag(reverse)?;
    let mut v = l.try_borrow_mut().ok()?;
    Some(sort_maybe_keyed(&mut v, key, reverse)?.map(|()| Object::None))
}

/// `sorted(iterable, /, *, key=None, reverse=...)`'s keyword half, as
/// [`list_sort_kw_fast`] for a list's or tuple's items: a new list.
pub(crate) fn sorted_kw_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [iterable, key, reverse] = args else {
        return None;
    };
    let reverse = plain_sort_flag(reverse)?;
    let mut v = match iterable {
        Object::List(l) => l.try_borrow().ok()?.clone(),
        Object::Tuple(t) => crate::object::cloned_vec(t),
        // A dict's keys, or a view's keys, values or `(key, value)` items,
        // in the dict's order (cloning them runs no code).
        Object::Dict(d) => d.try_borrow().ok()?.keys().map(|k| k.0.clone()).collect(),
        Object::DictView(view) => {
            let d = view.dict.try_borrow().ok()?;
            match view.kind {
                crate::object::DictViewKind::Keys => d.keys().map(|k| k.0.clone()).collect(),
                crate::object::DictViewKind::Values => d.values().cloned().collect(),
                crate::object::DictViewKind::Items => d
                    .iter()
                    .map(|(k, v)| Object::new_tuple_array([k.0.clone(), v.clone()]))
                    .collect(),
            }
        }
        _ => return None,
    };
    Some(sort_maybe_keyed(&mut v, key, reverse)?.map(|()| {
        let obj = Object::new_list(v);
        crate::gc_trace::track(&obj);
        obj
    }))
}

/// The direction a keyword sort runs in (`None` for a flag whose truth
/// could run code).
fn plain_sort_flag(reverse: &Object) -> Option<bool> {
    match *reverse {
        Object::Unbound => Some(false),
        Object::Bool(b) => Some(b),
        Object::Int(i) => Some(i != 0),
        _ => None,
    }
}

/// [`sort_plain`] of `v`, or with `key` (not `None`) a key function whose
/// keys compute without running code (see [`native_sort_keys`]), the items
/// sorted by those keys as CPython's `list.sort` decorates them. `None`, `v`
/// untouched, for anything else.
fn sort_maybe_keyed(
    v: &mut [Object],
    key: &Object,
    reverse: bool,
) -> Option<Result<(), RuntimeError>> {
    if matches!(key, Object::Unbound | Object::None) {
        return sort_plain(v, reverse);
    }
    let keys = native_sort_keys(v, key)?;
    let kind = plain_kind(&keys)?;
    // Each item moves beside its key (its slot left `None`) and back after.
    let mut pairs: Vec<(Object, Object)> = Vec::with_capacity(v.len());
    for (k, item) in keys.into_iter().zip(v.iter_mut()) {
        pairs.push((k, std::mem::replace(item, Object::None)));
    }
    if reverse {
        pairs.reverse();
    }
    let sorted = sort_by_kind(&mut pairs, kind, |p| &p.0);
    if reverse {
        pairs.reverse();
    }
    // (The keys are plain values, and the slots hold `None`: what drops here
    // runs no code.)
    for (slot, (_, item)) in v.iter_mut().zip(pairs) {
        *slot = item;
    }
    Some(sorted)
}

/// Each item's key under `key` when computing it runs no code: `len` of a
/// string or built-in container, or an instance whose native `__call__`
/// has a leaf half (`itemgetter(1)`, `attrgetter("x")`). `None` for any
/// other key function, or an item it declines.
fn native_sort_keys(items: &[Object], key: &Object) -> Option<Vec<Object>> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()?;
    // SAFETY: published by the enclosing VM frame on this thread; only its
    // tables are read.
    let interp = unsafe { &*ptr };
    // `itemgetter(i)` of tuples or lists: each item's `i`th element.
    if let Some(i) = crate::stdlib::operator_accel::itemgetter_single_index(key) {
        let at = |len: usize| {
            let k = if i < 0 { i + len as i64 } else { i };
            usize::try_from(k).ok().filter(|&k| k < len)
        };
        let mut keys = Vec::with_capacity(items.len());
        for item in items {
            keys.push(match item {
                Object::Tuple(t) => t.get(at(t.len())?)?.clone(),
                // SAFETY: a read that runs no code.
                Object::List(l) => {
                    let xs = unsafe { l.peek() }?;
                    xs.get(at(xs.len())?)?.clone()
                }
                _ => return None,
            });
        }
        return Some(keys);
    }
    let is_len =
        matches!(key, Object::Builtin(b) if Rc::as_ptr(b) as usize == interp.leaf_fns().len_ptr);
    // A builtin the leaf registry vouches runs no code (`abs`), or a bound
    // one over its receiver (`d.get`): its leaf half on each item.
    let leaf = match key {
        _ if is_len => None,
        Object::Builtin(b) => Some((b, None)),
        Object::BoundMethod(bm) if !bm.redispatch_descriptor => match &bm.function {
            Object::Builtin(b) => Some((b, Some(&bm.receiver))),
            _ => None,
        },
        _ => None,
    };
    if let Some((b, recv)) = leaf {
        if let Some(kind) = interp.leaf_call_kind(b) {
            let mut keys = Vec::with_capacity(items.len());
            for item in items {
                let k = match recv {
                    Some(r) => interp.leaf_builtin_call(kind, b, &[r.clone(), item.clone()]),
                    None => interp.leaf_builtin_call(kind, b, std::slice::from_ref(item)),
                };
                keys.push(k?.ok()?);
            }
            return Some(keys);
        }
    }
    let call_fast = if is_len {
        None
    } else {
        Some(interp.instance_call_fast(key)?)
    };
    let mut keys = Vec::with_capacity(items.len());
    for item in items {
        let k = match call_fast {
            None => match item {
                Object::Str(_)
                | Object::List(_)
                | Object::Tuple(_)
                | Object::Dict(_)
                | Object::Set(_)
                | Object::FrozenSet(_)
                | Object::Bytes(_) => Object::Int(i64::try_from(item.len().ok()?).ok()?),
                _ => return None,
            },
            Some(fast) => {
                // SAFETY: bitwise views of the live key function and item,
                // never dropped (the half clones what it keeps).
                let views = std::mem::ManuallyDrop::new(unsafe {
                    [std::ptr::read(key), std::ptr::read(item)]
                });
                fast(&views[..])?.ok()?
            }
        };
        keys.push(k);
    }
    Some(keys)
}

/// Sort `v` in place, as the interpreter's sort does, when its items are
/// strings only, plain numbers only, or tuples of ints and strings
/// (comparisons that run no code);
/// `None`, `v` untouched, for any other items. A reverse sort reverses,
/// sorts and reverses again, as CPython's does, so equal items keep their
/// order.
fn sort_plain(v: &mut [Object], reverse: bool) -> Option<Result<(), RuntimeError>> {
    let kind = plain_kind(v)?;
    if reverse {
        v.reverse();
    }
    let sorted = sort_by_kind(v, kind, |o| o);
    if reverse {
        v.reverse();
    }
    Some(sorted)
}

/// Which plain kind every item of `v` is, for [`sort_by_kind`]: strings only
/// (0), ints only (1), floats only (4), plain numbers only (2), or tuples
/// of ints and strings (3); `None` for anything else.
fn plain_kind(v: &[Object]) -> Option<u8> {
    Some(if v.iter().all(|x| matches!(x, Object::Str(_))) {
        0
    } else if v.iter().all(|x| matches!(x, Object::Int(_))) {
        1
    } else if v.iter().all(|x| matches!(x, Object::Float(_))) {
        4
    } else if v
        .iter()
        .all(|x| matches!(x, Object::Int(_) | Object::Float(_) | Object::Bool(_)))
    {
        2
    } else if v.iter().all(|x| {
        matches!(x, Object::Tuple(t) if t.iter().all(|e| matches!(e, Object::Int(_) | Object::Bool(_) | Object::Str(_))))
    }) {
        3
    } else {
        return None;
    })
}

/// Sort `v` stably by the values `value` gives, all of [`plain_kind`]
/// `kind`, as the interpreter's sort compares them, running no code:
/// code-point order for strings (which their UTF-8 bytes sort in), and
/// CPython's tuple order, the first unequal place deciding (an int against
/// a string raises as the interpreter's comparison does). Each kind's
/// comparison is its own closure.
fn sort_by_kind<T>(
    v: &mut [T],
    kind: u8,
    value: impl Fn(&T) -> &Object,
) -> Result<(), RuntimeError> {
    use weavepy_compiler::CompareKind as C;
    match kind {
        0 => crate::timsort::sort(v, |a, b| {
            Ok(match (value(a), value(b)) {
                (Object::Str(a), Object::Str(b)) => a.as_bytes() < b.as_bytes(),
                _ => false,
            })
        }),
        1 => crate::timsort::sort(v, |a, b| {
            Ok(match (value(a), value(b)) {
                (Object::Int(a), Object::Int(b)) => a < b,
                _ => false,
            })
        }),
        2 => crate::timsort::sort(v, |a, b| crate::compare_op(value(a), value(b), C::Lt)),
        // (`<` of two floats, a NaN on either side false, as the
        // interpreter's comparison gives it.)
        4 => crate::timsort::sort(v, |a, b| {
            Ok(match (value(a), value(b)) {
                (Object::Float(a), Object::Float(b)) => a < b,
                _ => false,
            })
        }),
        _ => crate::timsort::sort(v, |a, b| {
            let (Object::Tuple(a), Object::Tuple(b)) = (value(a), value(b)) else {
                return Ok(false);
            };
            for (x, y) in a.iter().zip(b.iter()) {
                if !x.eq_value(y) {
                    return crate::compare_op(x, y, C::Lt);
                }
            }
            Ok(a.len() < b.len())
        }),
    }
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

/// Whether hashing `x` runs no Python code and can't fail: a native
/// scalar, a `str` or `bytes`, or a tuple of such (nested a few levels at
/// most). A NaN is left out: it hashes by identity.
pub(crate) fn plain_hashable(x: &Object, depth: u32) -> bool {
    match x {
        Object::Int(_) | Object::Bool(_) | Object::None | Object::Str(_) | Object::Bytes(_) => true,
        Object::Float(f) => !f.is_nan(),
        // (An element that's a scalar or a string is settled here: only a
        // nested tuple or a float costs a call.)
        Object::Tuple(t) => {
            depth < 4
                && t.iter().all(|e| {
                    matches!(
                        e,
                        Object::Int(_)
                            | Object::Bool(_)
                            | Object::None
                            | Object::Str(_)
                            | Object::Bytes(_)
                    ) || plain_hashable(e, depth + 1)
                })
        }
        _ => false,
    }
}

/// `dict(zip(xs, ys))` over two plain list or tuple cursors whose keys
/// hash and compare without running code ([`plain_hashable`]): the dict,
/// with both cursors left as the general `zip` step leaves them (the first
/// source one past the pairs when the second ran out first, and the source
/// that ran out finished). `None`, nothing consumed, for anything else, or
/// when finishing a source would free its list (which could run code).
pub(crate) fn dict_from_zip_pure(l: &PyLazyIter) -> Option<crate::object::DictData> {
    use crate::object::PyIterator;
    // (No more pairs than CPython's own loop would take without a look at
    // anything else in a few tens of milliseconds.)
    const MAX_PAIRS: usize = 1 << 20;
    // SAFETY: nothing below runs code; the views end with this call.
    let st = unsafe { l.state.peek_mut() }?;
    let LazyIterKind::Zip {
        iters,
        strict: false,
        held,
        ..
    } = st
    else {
        return None;
    };
    if !held.is_empty() {
        return None;
    }
    let [Object::Iter(ca), Object::Iter(cb)] = iters.as_slice() else {
        return None;
    };
    if Rc::ptr_eq(ca, cb) {
        return None;
    }
    // SAFETY: two distinct cells, as above.
    let (ia, ib) = unsafe { (ca.peek_mut()?, cb.peek_mut()?) };
    // A cursor's items from its position, and whether finishing it frees
    // nothing.
    fn rest(it: &PyIterator) -> Option<(&[Object], bool)> {
        match it {
            PyIterator::List {
                items,
                index,
                owner: None,
            } => {
                // SAFETY: a read that runs no code.
                let v = unsafe { items.peek() }?;
                Some((v.get(*index..).unwrap_or(&[]), Rc::strong_count(items) > 1))
            }
            PyIterator::Tuple { items, index } => Some((items.get(*index..).unwrap_or(&[]), true)),
            _ => None,
        }
    }
    let ((xs, a_free), (ys, b_free)) = (rest(ia)?, rest(ib)?);
    let n = xs.len().min(ys.len());
    let first_ends = xs.len() <= ys.len();
    if n > MAX_PAIRS || !(if first_ends { a_free } else { b_free }) {
        return None;
    }
    let keys = &xs[..n];
    if !keys.iter().all(|k| plain_hashable(k, 0)) {
        return None;
    }
    let mut data = crate::object::DictData::default();
    data.reserve(n);
    for (k, v) in keys.iter().zip(&ys[..n]) {
        data.insert(crate::object::DictKey(k.clone()), v.clone());
    }
    // (Keys that mostly repeated leave most of the room unused.)
    if n > 64 && data.len() * 4 < n {
        data.shrink_to_fit();
    }
    // (The general step takes the first source's next item before it
    // finds the second finished.)
    let advance = |it: &mut PyIterator, by: usize| match it {
        PyIterator::List { index, .. } | PyIterator::Tuple { index, .. } => *index += by,
        _ => {}
    };
    if first_ends {
        advance(ia, n);
        advance(ib, n);
        let _ = ia.next_value();
    } else {
        advance(ia, n + 1);
        advance(ib, n);
        let _ = ib.next_value();
    }
    Some(data)
}

/// `hash(x)` of a value whose hash runs no Python code and can't fail
/// ([`plain_hashable`]), or an instance's where that holds.
pub(crate) fn hash_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [x] = args else {
        return None;
    };
    // A plain instance whose class keeps `object`'s `__hash__`: its identity
    // hash, as the dict probes hash it (`LeafProbe`).
    if let Object::Instance(i) = x {
        if i.native.get().is_none() && i.class_dunder(crate::types::Dunder::Hash).object_owner() {
            return Some(Ok(Object::Int(crate::object::identity_hash(x))));
        }
    }
    // An instance whose Python `__hash__` evaluates as a pure leaf (a frozen
    // dataclass's `hash((self.a, self.b))`).
    if let Object::Instance(_) = x {
        // SAFETY: the running thread's interpreter, under the GIL; the
        // evaluation runs no Python and takes no frame.
        let interp = unsafe { &*crate::vm_singletons::current_interpreter_ptr()? };
        return interp.leaf_instance_hash(x).map(|h| Ok(Object::Int(h)));
    }
    if !plain_hashable(x, 0) {
        return None;
    }
    Some(Ok(Object::Int(crate::object::py_hash_value(x)?)))
}

/// `vars(obj)` of a plain instance whose `__dict__` nothing but the
/// default lookup answers: its attribute dictionary. (A class with a
/// `__dict__` of its own, attribute hooks, or `__slots__` alone takes the
/// attribute protocol, as the full call does.)
pub(crate) fn vars_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [Object::Instance(inst)] = args else {
        return None;
    };
    let cls = inst.cls_raw();
    if cls.forbids_dict
        || cls.flags.is_builtin
        || inst.native.get().is_some()
        || inst.c_body.get() != 0
        || cls.native_kind.get() != 0
        || cls.is_super_proxy_type()
        || !Interpreter::default_getattribute(cls)
        || cls.dunder(crate::types::Dunder::GetAttr).present()
        // (Every class with instance dictionaries holds the `__dict__`
        // member descriptor; anything else under that name is the class's
        // own.)
        || !matches!(cls.lookup("__dict__"), Some(Object::SlotDescriptor(d)) if d.name == "__dict__")
    {
        return None;
    }
    Some(Ok(Object::Dict(inst.dict_shared())))
}

/// `pow` of plain numbers: `pow(base, exp)` of machine ints while the
/// power fits (a negative exponent's is a float), or with a float operand,
/// as `**` computes them; and `pow(base, exp, mod)` of machine ints with a
/// non-negative exponent and a nonzero modulus (modular exponentiation by
/// squaring, the result taking the modulus's sign as `%` does).
pub(crate) fn pow_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    // `pow(base, exp)` of machine ints with a non-negative exponent, where
    // the power fits a machine int too.
    if let [Object::Int(base), Object::Int(exp)] = args {
        if *exp < 0 {
            // A negative exponent's power is the floats' (zero to one
            // raises there).
            return Some(crate::float_pow(*base as f64, *exp as f64));
        }
        let exp = u32::try_from(*exp).ok()?;
        return base.checked_pow(exp).map(|v| Ok(Object::Int(v)));
    }
    // A float operand: `float ** float`, with its errors and its complex
    // results.
    if let [x, y] = args {
        let (x, y) = match (x, y) {
            (Object::Float(x), Object::Float(y)) => (*x, *y),
            (Object::Float(x), Object::Int(y)) => (*x, *y as f64),
            (Object::Int(x), Object::Float(y)) => (*x as f64, *y),
            _ => return None,
        };
        return Some(crate::float_pow(x, y));
    }
    let [Object::Int(base), Object::Int(exp), Object::Int(modulus)] = args else {
        return None;
    };
    if *exp < 0 || *modulus == 0 {
        return None;
    }
    let m = i128::from(*modulus).unsigned_abs();
    let mut b = i128::from(*base).rem_euclid(m as i128) as u128;
    let mut e = *exp as u64;
    let mut r: u128 = 1 % m;
    while e != 0 {
        if e & 1 != 0 {
            r = r * b % m;
        }
        b = b * b % m;
        e >>= 1;
    }
    let r = r as i128;
    let r = if *modulus < 0 && r != 0 {
        r + i128::from(*modulus)
    } else {
        r
    };
    Some(Ok(Object::Int(r as i64)))
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

/// `min(iterable, *, key=None, default=...)`'s keyword half (see
/// `leaf_builtins::register_fast_kw`): the iterable, then `key` and
/// `default`, each `Unbound` when the call left it out. No key: the
/// positional half's answer, or the default for an empty list or tuple.
pub(crate) fn min_kw_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    min_max_kw_fast(args, false)
}

/// [`min_kw_fast`] for `max`.
pub(crate) fn max_kw_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    min_max_kw_fast(args, true)
}

fn min_max_kw_fast(args: &[Object], want_max: bool) -> Option<Result<Object, RuntimeError>> {
    let [iterable, key, default] = args else {
        return None;
    };
    if !matches!(key, Object::Unbound | Object::None) {
        return min_max_keyed(iterable, key, default, want_max);
    }
    let empty = match iterable {
        Object::List(l) => l.try_borrow().ok()?.is_empty(),
        Object::Tuple(t) => t.is_empty(),
        _ => false,
    };
    if empty {
        return match default {
            Object::Unbound => None,
            d => Some(Ok(d.clone())),
        };
    }
    min_max_fast(std::slice::from_ref(iterable), want_max)
}

/// [`min_max_kw_fast`] with a key function [`native_sort_keys`] computes
/// without running code (`max(d, key=d.get)`), over a list's, a tuple's or
/// a dict's items whose keys are all ints, all floats or all strings: the
/// first item with the least (or greatest) key, as CPython keeps it.
fn min_max_keyed(
    iterable: &Object,
    key: &Object,
    default: &Object,
    want_max: bool,
) -> Option<Result<Object, RuntimeError>> {
    let items: Vec<Object> = match iterable {
        Object::List(l) => l.try_borrow().ok()?.clone(),
        Object::Tuple(t) => crate::object::cloned_vec(t),
        Object::Dict(d) => d.try_borrow().ok()?.keys().map(|k| k.0.clone()).collect(),
        _ => return None,
    };
    if items.is_empty() {
        return match default {
            Object::Unbound => None,
            d => Some(Ok(d.clone())),
        };
    }
    let keys = native_sort_keys(&items, key)?;
    // (Whether `a` replaces the best so far `b`: strictly beyond it.)
    let beyond = |a: &Object, b: &Object| -> Option<bool> {
        Some(match (a, b) {
            (Object::Int(x), Object::Int(y)) => {
                if want_max {
                    x > y
                } else {
                    x < y
                }
            }
            (Object::Float(x), Object::Float(y)) => {
                if want_max {
                    x > y
                } else {
                    x < y
                }
            }
            (Object::Str(x), Object::Str(y)) => {
                let (x, y): (&str, &str) = (x, y);
                if want_max {
                    x > y
                } else {
                    x < y
                }
            }
            _ => return None,
        })
    };
    let mut best = 0;
    for i in 1..keys.len() {
        if beyond(&keys[i], &keys[best])? {
            best = i;
        }
    }
    // (A lone item's key still has to be one of the plain kinds.)
    beyond(&keys[0], &keys[0])?;
    Some(Ok(items[best].clone()))
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
    // (A byte string's or buffer's bytes: their total can't overflow.)
    let bytes_total = |b: &[u8]| b.iter().map(|&x| u64::from(x)).sum::<u64>() as i64;
    // (A list or tuple with floats, or past a machine int: the numeric
    // sum's int-then-float loops, which run no code either.)
    let numeric = || crate::builtins::sum_numeric_sequence(&args[0], &Object::Int(0));
    match args {
        [Object::Tuple(t)] => {
            if t.iter().try_for_each(&mut add).is_none() {
                return numeric();
            }
        }
        [Object::List(l)] => {
            if l.try_borrow().ok()?.iter().try_for_each(&mut add).is_none() {
                return numeric();
            }
        }
        [Object::Bytes(b)] => return Some(Ok(Object::Int(bytes_total(b)))),
        [Object::ByteArray(b)] => return Some(Ok(Object::Int(bytes_total(&b.try_borrow().ok()?)))),
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

/// `list(enumerate(xs))` over a list or tuple iterator: the pairs of the
/// rest of the source at once, both cursors advanced past them and the
/// source detached as its end leaves it. `None`, nothing stepped, for any
/// other iterator or a count that would overflow.
pub(crate) fn enumerate_seq_collect(it: &Object) -> Option<Vec<Object>> {
    let Object::Iter(c) = it else {
        return None;
    };
    let mut e = c.try_borrow_mut().ok()?;
    let PyIterator::Enumerate {
        inner,
        count,
        count_big: None,
    } = &mut *e
    else {
        return None;
    };
    let mut src = inner.try_borrow_mut().ok()?;
    let rest: Vec<Object> = match &*src {
        PyIterator::List {
            items,
            index,
            owner: None,
        } => items
            .try_borrow()
            .ok()?
            .get(*index..)
            .unwrap_or(&[])
            .to_vec(),
        PyIterator::Tuple { items, index } => items.get(*index..).unwrap_or(&[]).to_vec(),
        _ => return None,
    };
    let start = *count;
    *count = count.checked_add(i64::try_from(rest.len()).ok()?)?;
    match &mut *src {
        PyIterator::List { index, .. } | PyIterator::Tuple { index, .. } => *index += rest.len(),
        _ => unreachable!("checked above"),
    }
    // (The step past the end detaches the source, as the last one does.)
    let detached = src.next_value();
    drop(src);
    drop(e);
    drop(detached);
    Some(
        rest.into_iter()
            .zip(start..)
            .map(|(x, k)| Object::new_tuple_array([Object::Int(k), x]))
            .collect(),
    )
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
    // A list's or tuple's next item, read in place (their end goes through
    // `next_value`, which detaches the source).
    let item = match src {
        PyIterator::List {
            items,
            index,
            owner: None,
        } => unsafe { items.peek() }
            .and_then(|xs| xs.get(*index))
            .map(crate::clone_hot),
        PyIterator::Tuple { items, index } => items.get(*index).map(crate::clone_hot),
        _ => None,
    };
    if let Some(v) = item {
        if let PyIterator::List { index, .. } | PyIterator::Tuple { index, .. } = src {
            *index += 1;
        }
        let i = std::mem::replace(count, next);
        return Some((Object::Int(i), v));
    }
    if !src.pure_ready() {
        return None;
    }
    let v = src.next_value()?;
    let i = std::mem::replace(count, next);
    Some((Object::Int(i), v))
}

/// The rest of `filter(None, xs)` or `filter(bool, xs)` over a plain list
/// or tuple cursor whose items' truth runs no code, taken at once (see
/// [`filter_seq_bulk`]): `None`, nothing consumed, for anything else.
pub(crate) fn filter_collect(l: &PyLazyIter) -> Option<Vec<Object>> {
    // SAFETY: nothing below runs code; the view ends with this call.
    let st = unsafe { l.state.peek() }?;
    let LazyIterKind::Filter { func, source } = st else {
        return None;
    };
    let mut out = Vec::new();
    filter_seq_bulk(func, source, &mut out).then_some(out)
}

/// The truth of a value whose truth is plain data (`None` for anything
/// whose `__bool__`/`__len__` could run code).
/// `filter(None, xs)` or `filter(bool, xs)` drained over a plain list or
/// tuple cursor whose items left all have a truth that runs no code (see
/// [`scalar_truth`]): the items kept appended to `out` at once, and the
/// cursor stepped to its end and then past it, as stepping it item by item
/// leaves it. `false`, nothing consumed, for anything else.
fn filter_seq_bulk(func: &Object, source: &Object, out: &mut Vec<Object>) -> bool {
    let plain = match func {
        Object::None => true,
        Object::Type(t) => Rc::ptr_eq(t, &builtin_types().bool_),
        _ => false,
    };
    let Object::Iter(c) = source else {
        return false;
    };
    if !plain {
        return false;
    }
    {
        // SAFETY: a view that runs no code (cloning a scalar or a string
        // runs none) and ends in this block.
        let Some((xs, idx)) = (unsafe { c.peek_mut() }).and_then(seq_cursor) else {
            return false;
        };
        let Some(rest) = xs.get(*idx..) else {
            return false;
        };
        if rest.is_empty() {
            return false;
        }
        let mut kept = 0;
        for x in rest {
            match scalar_truth(x) {
                Some(keep) => kept += usize::from(keep),
                None => return false,
            }
        }
        out.reserve(kept);
        for x in rest {
            if scalar_truth(x) == Some(true) {
                out.push(x.clone());
            }
        }
        *idx = xs.len();
    }
    // (The step that finds the end, so the cursor records its exhaustion;
    // what that releases holds only scalars and strings.)
    if let Ok(mut cursor) = c.try_borrow_mut() {
        let end = cursor.next_value();
        drop(cursor);
        drop(end);
    }
    true
}

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

/// `a == b` for two `groupby` keys when it is settled natively (identity,
/// two machine ints, strings or floats, two classes that compare by
/// identity): `None` when it could need Python's `__eq__`.
fn native_key_eq(a: &Object, b: &Object) -> Option<bool> {
    if a.is_same(b) {
        return Some(true);
    }
    Some(match (a, b) {
        (Object::Int(x), Object::Int(y)) => x == y,
        (Object::Str(x), Object::Str(y)) => {
            crate::dictmap::key_bytes_eq(x.as_bytes(), y.as_bytes())
        }
        (Object::Float(x), Object::Float(y)) => x == y,
        (Object::Bool(x), Object::Bool(y)) => x == y,
        (Object::Type(x), Object::Type(y))
            if crate::object::type_hash_is_identity(x)
                && crate::object::type_hash_is_identity(y) =>
        {
            Rc::ptr_eq(x, y)
        }
        _ => return None,
    })
}

/// The next item of a plain sequence cursor, without taking it (see
/// [`PyIterator::pure_peek`]).
#[inline]
fn cursor_peek(c: &Rc<RefCell<PyIterator>>) -> Option<Object> {
    // SAFETY: a read that runs no code; the view ends with this call.
    unsafe { c.peek() }?.pure_peek()
}

/// Step past the item [`cursor_peek`] found.
#[inline]
fn cursor_advance(c: &Rc<RefCell<PyIterator>>) -> Option<()> {
    // SAFETY: as above; the item was found ready.
    unsafe { c.peek_mut() }?.pure_skip().then_some(())
}

/// Whether [`Interpreter::pure_callback`] could run `func` at all: not a
/// Python function (or a method over one) whose code isn't a pure leaf,
/// one load where that verdict is recorded.
#[inline]
pub(crate) fn pure_callback_maybe(func: &Object) -> bool {
    let f = match func {
        Object::Function(f) => f,
        Object::BoundMethod(bm) => match &bm.function {
            Object::Function(f) => f,
            _ => return true,
        },
        _ => return true,
    };
    // SAFETY: a function's code is only replaced under the GIL, and the
    // reference ends here.
    crate::code_is_pure_leaf(unsafe { &*f.code.as_ptr() })
}

impl Interpreter {
    /// `func(*args)` for an adapter's callback when the call runs no Python
    /// code: a warm pure-leaf function or a bound method over one (evaluated
    /// in place, as the core loop calls one), a registered leaf builtin or a
    /// leaf builtin method bound to its receiver, or a builtin type whose
    /// native constructor builds the value (`int`, `str`, `type`, ...).
    /// `None` leaves the call to the full machinery, nothing having
    /// happened (a pure call that would raise declines too, so the full
    /// call raises it). Without `effects`, only callbacks with no effect
    /// beyond their result qualify (a caller that may yet decline after
    /// the call: no `list.append`, `set.pop`, or opaque registered leaf).
    pub(crate) fn pure_callback(
        &self,
        func: &Object,
        args: &[Object],
        effects: bool,
    ) -> Option<Object> {
        let admit = |kind: LeafKind| -> Option<LeafKind> {
            let effect = matches!(
                kind,
                LeafKind::ListAppend
                    | LeafKind::ListPop
                    | LeafKind::ListExtend
                    | LeafKind::ListInsert
                    | LeafKind::ListReverse
                    | LeafKind::ListRemove
                    | LeafKind::SetAdd
                    | LeafKind::SetDiscard
                    | LeafKind::SetRemove
                    | LeafKind::SetPop
                    | LeafKind::Opaque
                    | LeafKind::Scalar
                    | LeafKind::Fast(_)
            );
            (effects || !effect).then_some(kind)
        };
        match func {
            Object::Function(f) => self.call_pure_leaf(f, args),
            // (As the core loop's leaf builtin lane admits them: a function,
            // or a method bound to its exact builtin receiver.)
            // (A method read off its type, `map(str.upper, xs)`, takes its
            // receiver first, as its kind expects.)
            Object::Builtin(b) if !b.binds_instance || !args.is_empty() => {
                let kind = admit(self.leaf_call_kind(b)?)?;
                self.leaf_builtin_call(kind, b, args)?.ok()
            }
            Object::Type(t) => self.leaf_type_call(t, args),
            Object::BoundMethod(bm) if !bm.redispatch_descriptor => match &bm.function {
                Object::Function(f) => self.call_pure_leaf_with(f, Some(&bm.receiver), args),
                Object::Builtin(b)
                    if b.binds_instance
                        && crate::builtins::method_memo_tag(&bm.receiver).is_some() =>
                {
                    let kind = admit(self.leaf_call_kind(b)?)?;
                    let call = match args {
                        [a] => [bm.receiver.clone(), a.clone()],
                        _ => return None,
                    };
                    self.leaf_builtin_call(kind, b, &call)?.ok()
                }
                // A leaf method's fast half bound to an instance of a
                // built-in subclass (`map(_Quoter(dict).__getitem__, bs)` in
                // `urllib.parse.quote`), which settles what it can natively.
                Object::Builtin(b)
                    if b.binds_instance && matches!(bm.receiver, Object::Instance(_)) =>
                {
                    let LeafKind::Fast(f) = admit(self.leaf_call_kind(b)?)? else {
                        return None;
                    };
                    let call = match args {
                        [a] => [bm.receiver.clone(), a.clone()],
                        _ => return None,
                    };
                    f(&call)?.ok()
                }
                _ => None,
            },
            // An instance whose class's `__call__` is a registered leaf
            // half (`key=attrgetter("x")`, `map(itemgetter(0), ...)`).
            // (Its half found once per class version; a half counts as an
            // effect, as `admit` counts one.)
            Object::Instance(_) if effects => {
                let f = self.instance_call_fast(func)?;
                let [a] = args else {
                    return None;
                };
                // SAFETY: bitwise views of the live callable and argument,
                // never dropped (the half clones what it keeps).
                let views = std::mem::ManuallyDrop::new(unsafe {
                    [std::ptr::read(func), std::ptr::read(a)]
                });
                f(&views[..])?.ok()
            }
            _ => None,
        }
    }

    /// The fast half of an instance's class's native `__call__`
    /// (`attrgetter("x")`, `itemgetter(0)`), for a caller that calls it per
    /// item (a sort's key) to resolve once; it takes the instance first.
    pub(crate) fn instance_call_fast(&self, func: &Object) -> Option<crate::leaf_builtins::Fast> {
        use crate::types::LeafAttrKind as K;
        let Object::Instance(i) = func else {
            return None;
        };
        if crate::gil::free_threading_enabled() {
            return None;
        }
        // (Found once per class version: a callback calls one key function
        // for every item.)
        let cls = i.cls_raw();
        let ver = cls.attr_version.get();
        let key = crate::types::AttrKey::vm(crate::types::vm_key::CALL_LEAF);
        match cls.leaf_attrs.get(key, ver) {
            Some(K::LeafFast(f)) => return Some(*f),
            Some(_) => return None,
            None => {}
        }
        if crate::object::exotic_str_keys_possible() {
            return None;
        }
        let fast = match cls.lookup("__call__") {
            Some(Object::Builtin(b)) => match self.leaf_call_kind(&b) {
                Some(LeafKind::Fast(f)) => Some(f),
                _ => None,
            },
            _ => None,
        };
        cls.leaf_attrs
            .set(key, ver, fast.map_or(K::Other, K::LeafFast));
        fast
    }

    /// `groupby_step` when taking the next value and computing its key
    /// run no code: the value and its key, the value taken. `None` (nothing
    /// consumed) otherwise.
    fn groupby_step_pure(&self, source: &Object, keyfunc: &Object) -> Option<(Object, Object)> {
        let Object::Iter(c) = source else {
            return None;
        };
        let value = cursor_peek(c)?;
        let key = match keyfunc {
            Object::None => value.clone(),
            f => self.pure_callback(f, std::slice::from_ref(&value), true)?,
        };
        cursor_advance(c)?;
        Some((value, key))
    }

    /// One item of `source` for [`Self::lazy_gen_pair`]: a generator's
    /// next yield by its native code ([`Self::gen_native_next`]), or a
    /// plain cursor's next item. `None` for anything else, and for a
    /// generator that didn't yield (finished, raised, stopped partway, or
    /// turned away untouched: the general step finds it as it is).
    #[cfg(feature = "jit")]
    #[inline]
    fn lazy_gen_pull(
        &mut self,
        source: &Object,
        snap_gen: u64,
        dead: *const std::cell::Cell<bool>,
        depth_cell: *const std::cell::Cell<usize>,
    ) -> Option<Object> {
        match source {
            Object::Generator(g) => match self.gen_native_next(g, snap_gen, dead, depth_cell, 0) {
                crate::gen_fast::GenNext::Yielded(v) => Some(v),
                _ => None,
            },
            Object::Iter(c) => {
                let v = cursor_peek(c)?;
                cursor_advance(c)?;
                Some(v)
            }
            _ => None,
        }
    }

    /// One step of `enumerate(gen)`, or of a two-source `zip` with a
    /// generator among its sources (the other a generator or a plain
    /// cursor), from a running activation whose recursion depth is at
    /// `depth_cell`: the pair's two items, each generator stepped by its
    /// native code where it stands. The general step of such an adapter
    /// runs in the core loop's lane, with the calling activation published
    /// and the generator resumed through three more layers: about 1,600
    /// instructions an item for `for i, x in enumerate(gen)`, against 550
    /// this way.
    ///
    /// `None` when the step isn't one of these or a source didn't give an
    /// item. Nothing is lost then: a `zip` holds what its first source gave
    /// (`LazyIterKind::Zip::held`) for the general step, which goes on
    /// from the second, and a generator that finished, raised or stopped
    /// partway is found as it is by that step.
    #[cfg(feature = "jit")]
    pub(crate) fn lazy_gen_pair(
        &mut self,
        l: &Rc<PyLazyIter>,
        snap_gen: u64,
        dead: *const std::cell::Cell<bool>,
        depth_cell: *const std::cell::Cell<usize>,
    ) -> Option<(Object, Object)> {
        let direct = |s: &Object| matches!(s, Object::Generator(_)) || matches!(s, Object::Iter(_));
        // The sources are copied out: a generator's body may reach this
        // very adapter, so no view of its state survives a step.
        let (a, b) = {
            // SAFETY: a read that runs no code; the view ends with the block.
            match unsafe { l.state.peek() }? {
                LazyIterKind::Enumerate {
                    source: source @ Object::Generator(_),
                    count,
                    count_big: None,
                } if *count < i64::MAX => (source.clone(), None),
                LazyIterKind::Zip {
                    iters,
                    strict: false,
                    held,
                    ..
                } if held.is_empty() => match iters.as_slice() {
                    [a, b]
                        if direct(a)
                            && direct(b)
                            && (matches!(a, Object::Generator(_))
                                || matches!(b, Object::Generator(_))) =>
                    {
                        (a.clone(), Some(b.clone()))
                    }
                    _ => return None,
                },
                _ => return None,
            }
        };
        let x = self.lazy_gen_pull(&a, snap_gen, dead, depth_cell)?;
        let Some(b) = b else {
            // `enumerate`: the index, counted now that the item is in hand.
            let mut st = l.state.borrow_mut();
            let LazyIterKind::Enumerate {
                count,
                count_big: None,
                ..
            } = &mut *st
            else {
                // (The body replaced the adapter's state under the step:
                // the general pairing's.)
                drop(st);
                let pair = Self::enumerate_advance(l, x);
                let Object::Tuple(t) = &pair else {
                    return None;
                };
                return Some((t[0].clone(), t[1].clone()));
            };
            let i = *count;
            *count += 1;
            return Some((Object::Int(i), x));
        };
        match self.lazy_gen_pull(&b, snap_gen, dead, depth_cell) {
            Some(y) => Some((x, y)),
            None => {
                if let LazyIterKind::Zip { held, .. } = &mut *l.state.borrow_mut() {
                    held.push(x);
                }
                None
            }
        }
    }

    /// One step of `itertools.chain` whose current iterable is a generator,
    /// or of `itertools.islice` over one, from a running activation (see
    /// [`Self::lazy_gen_pair`]): the item, the generator stepped by its
    /// native code where it stands. `None` for any other adapter, and
    /// whenever the generator doesn't yield: the general step finds it, and
    /// the adapter, as they are (an `islice`'s skipped items are counted as
    /// they're taken).
    #[cfg(feature = "jit")]
    pub(crate) fn lazy_gen_one(
        &mut self,
        l: &Rc<PyLazyIter>,
        snap_gen: u64,
        dead: *const std::cell::Cell<bool>,
        depth_cell: *const std::cell::Cell<usize>,
    ) -> Option<Object> {
        // (The source is copied out: its body may reach this adapter.)
        // SAFETY: a read that runs no code; the view ends with the match.
        let (source, skip) = match unsafe { l.state.peek() }? {
            LazyIterKind::Chain {
                active: Some(g @ Object::Generator(_)),
                ..
            } => (g.clone(), None),
            LazyIterKind::Islice {
                source: g @ Object::Generator(_),
                next_idx,
                pos,
                stop,
                done: false,
                ..
            } if *pos <= *next_idx && stop.is_none_or(|s| *next_idx < s) => {
                (g.clone(), Some(*next_idx - *pos))
            }
            _ => return None,
        };
        let Some(skip) = skip else {
            return self.lazy_gen_pull(&source, snap_gen, dead, depth_cell);
        };
        // `islice`: the items before the next one to give are taken first.
        for _ in 0..skip {
            drop(self.lazy_gen_pull(&source, snap_gen, dead, depth_cell)?);
            if let LazyIterKind::Islice { pos, .. } = &mut *l.state.borrow_mut() {
                *pos += 1;
            }
        }
        let item = self.lazy_gen_pull(&source, snap_gen, dead, depth_cell)?;
        if let LazyIterKind::Islice {
            next_idx,
            pos,
            stop,
            step,
            ..
        } = &mut *l.state.borrow_mut()
        {
            *pos += 1;
            *next_idx = next_idx.saturating_add(*step);
            if let Some(s) = stop {
                *next_idx = (*next_idx).min(*s);
            }
        }
        Some(item)
    }

    /// One step of a native adapter that runs no Python code, for every
    /// `FOR_ITER` (the core loop's, the frame JIT's, tier 2's) and ahead of
    /// the interpreter's general step: [`lazy_pure_next`]'s state machines,
    /// and a `map` or `filter` over plain sequence cursors whose callback
    /// is pure (see [`Self::pure_callback`]; the item is taken only once
    /// the callback has succeeded), an `enumerate` over such an adapter,
    /// and a `groupby` or grouper whose key function and key comparisons
    /// are native. `None` (nothing consumed, or only whole steps the
    /// general path would have taken the same way) for every other step.
    pub(crate) fn lazy_core_next(&self, l: &Rc<PyLazyIter>) -> Option<Option<Object>> {
        // SAFETY: nothing below runs code that could reach the adapter
        // (pure callbacks call nothing); the view ends with this call.
        let st = unsafe { l.state.peek_mut() }?;
        match st {
            // (A callback that can't run as a pure leaf, a Python function
            // that isn't one say, skips the item's peek.)
            LazyIterKind::Map { func, .. } | LazyIterKind::Filter { func, .. }
                if !pure_callback_maybe(func) =>
            {
                None
            }
            LazyIterKind::Map { func, iters, .. } => match iters.as_slice() {
                [Object::Iter(c)] => {
                    let v = cursor_peek(c)?;
                    let r = self.pure_callback(func, std::slice::from_ref(&v), true)?;
                    cursor_advance(c)?;
                    Some(Some(r))
                }
                [Object::Iter(a), Object::Iter(b)] if !Rc::ptr_eq(a, b) => {
                    let args = [cursor_peek(a)?, cursor_peek(b)?];
                    let r = self.pure_callback(func, &args, true)?;
                    cursor_advance(a)?;
                    cursor_advance(b)?;
                    Some(Some(r))
                }
                _ => None,
            },
            LazyIterKind::Filter {
                func,
                source: Object::Iter(c),
            } => loop {
                let v = cursor_peek(c)?;
                let keep = match &*func {
                    Object::None => scalar_truth(&v)?,
                    Object::Type(t) if Rc::ptr_eq(t, &builtin_types().bool_) => scalar_truth(&v)?,
                    // (The truth test may yet decline: no effects.)
                    f => scalar_truth(&self.pure_callback(f, std::slice::from_ref(&v), false)?)?,
                };
                cursor_advance(c)?;
                if keep {
                    return Some(Some(v));
                }
            },
            // `starmap(f, pairs)` over a list or tuple of tuples, as `map`
            // over one source steps (the item's fields the arguments).
            LazyIterKind::StarMap {
                func,
                source: Object::Iter(c),
            } if pure_callback_maybe(func) => {
                let v = cursor_peek(c)?;
                let Object::Tuple(t) = &v else {
                    return None;
                };
                let r = self.pure_callback(func, t, true)?;
                cursor_advance(c)?;
                Some(Some(r))
            }
            // `takewhile(pred, xs)` over a plain cursor: the item while the
            // predicate holds, the end (stopped) at the first that fails,
            // as the general step consumes it.
            LazyIterKind::TakeWhile {
                func,
                source: Object::Iter(c),
                stopped: stopped @ false,
            } if pure_callback_maybe(func) => {
                let v = cursor_peek(c)?;
                let keep =
                    scalar_truth(&self.pure_callback(func, std::slice::from_ref(&v), false)?)?;
                cursor_advance(c)?;
                if keep {
                    Some(Some(v))
                } else {
                    *stopped = true;
                    Some(None)
                }
            }
            // `dropwhile(pred, xs)` over a plain cursor: the items the
            // predicate holds for skipped (each a whole step), then the
            // rest as they are.
            LazyIterKind::DropWhile {
                func,
                source: Object::Iter(c),
                started,
            } => loop {
                let v = cursor_peek(c)?;
                if !*started {
                    if !pure_callback_maybe(func) {
                        return None;
                    }
                    let skip = scalar_truth(&self.pure_callback(
                        func,
                        std::slice::from_ref(&v),
                        false,
                    )?)?;
                    cursor_advance(c)?;
                    if skip {
                        continue;
                    }
                    *started = true;
                    return Some(Some(v));
                }
                cursor_advance(c)?;
                return Some(Some(v));
            },
            // `filterfalse(pred, xs)` over a plain cursor, as `filter`'s
            // step with the verdict inverted.
            LazyIterKind::FilterFalse {
                func,
                source: Object::Iter(c),
            } if matches!(func, Object::None) || pure_callback_maybe(func) => loop {
                let v = cursor_peek(c)?;
                let truthy = match &*func {
                    Object::None => scalar_truth(&v)?,
                    f => scalar_truth(&self.pure_callback(f, std::slice::from_ref(&v), false)?)?,
                };
                cursor_advance(c)?;
                if !truthy {
                    return Some(Some(v));
                }
            },
            // `enumerate(map(...))` and the like: the source's own pure step.
            LazyIterKind::Enumerate {
                source: Object::LazyIter(src),
                count,
                count_big: None,
            } if *count < i64::MAX => {
                let Some(v) = self.lazy_core_next(src)? else {
                    return Some(None);
                };
                let i = *count;
                *count += 1;
                Some(Some(Object::new_tuple_array([Object::Int(i), v])))
            }
            LazyIterKind::GroupBy {
                source,
                keyfunc,
                tgtkey,
                currkey,
                currvalue,
                currgrouper,
                grouper_cls,
            } => {
                // `groupby_next`: skip to the next group, a whole step at a
                // time (a decline midway leaves the state a general step
                // would have left).
                *currgrouper = None;
                loop {
                    match (&*currkey, &*tgtkey) {
                        (None, _) => {}
                        (Some(_), None) => break,
                        (Some(curr), Some(tgt)) => {
                            if !native_key_eq(tgt, curr)? {
                                break;
                            }
                        }
                    }
                    let (value, key) = self.groupby_step_pure(source, keyfunc)?;
                    let old = (currkey.replace(key), currvalue.replace(value));
                    drop(old);
                }
                let key = currkey.clone()?;
                let old = tgtkey.replace(key.clone());
                let grouper = Rc::new(PyLazyIter {
                    state: RefCell::new(LazyIterKind::Grouper {
                        parent: l.clone(),
                        tgtkey: key.clone(),
                    }),
                    cls: grouper_cls.clone(),
                });
                *currgrouper = Some(Rc::downgrade(&grouper));
                drop(old);
                Some(Some(Object::new_tuple_array([
                    key,
                    Object::LazyIter(grouper),
                ])))
            }
            LazyIterKind::Grouper { parent, tgtkey } => {
                // `_grouper_next`.
                // SAFETY: as above (the parent's distinct cell).
                let LazyIterKind::GroupBy {
                    source,
                    keyfunc,
                    currkey,
                    currvalue,
                    currgrouper,
                    ..
                } = (unsafe { parent.state.peek_mut() })?
                else {
                    return Some(None);
                };
                if !currgrouper
                    .as_ref()
                    .is_some_and(|w| std::ptr::eq(w.as_ptr(), Rc::as_ptr(l)))
                {
                    return Some(None);
                }
                if currvalue.is_none() {
                    let (value, key) = self.groupby_step_pure(source, keyfunc)?;
                    let old = (currkey.replace(key), currvalue.replace(value));
                    drop(old);
                }
                let Some(curr) = currkey.as_ref() else {
                    return Some(None);
                };
                if !native_key_eq(tgtkey, curr)? {
                    return Some(None);
                }
                let value = currvalue.take()?;
                let old = currkey.take();
                drop(old);
                Some(Some(value))
            }
            // A `filter` or `enumerate` over a source only the
            // interpreter can step.
            LazyIterKind::Filter { .. } | LazyIterKind::Enumerate { .. } => None,
            // `chain(xs, ys)`: its active source's next item, a plain
            // cursor's read and stepped in one.
            LazyIterKind::Chain { source, active } => chain_pure_next(source, active).map(Some),
            _ => lazy_pure_next(l),
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
