//! `_weave_elementtree`: native bodies for `xml.etree.ElementTree`.
//!
//! WeavePy ships no `_elementtree` accelerator, so `ElementTree` runs the
//! pure-Python `Element`, `TreeBuilder` and `XMLParser` classes, its
//! serializer and `ElementPath`. CPython runs most of that in C; here a
//! parse, a tree walk or a `tostring` is thousands of interpreted calls.
//!
//! [`install`] (called at the end of the `ElementTree` module body) keeps
//! the Python classes and gives their hot paths native bodies, as
//! `stdlib::datetime_native` does for `datetime`:
//!
//! - `Element.__init__`, `makeelement`, `append`, `extend`, `insert`,
//!   `__len__`, `__getitem__`, `get` and `set`, and `SubElement`;
//! - `Element.iter` and `itertext`, as a native iterator that walks the
//!   children lists the way the Python generators do (live lists, read
//!   lazily, so mutation during iteration behaves the same);
//! - `Element.find`, `findall` and `findtext`, which evaluate the
//!   selector list `ElementPath` compiled and cached for the path (the
//!   steps it produces for tags, `*`, `.`, `//` and attribute predicates);
//! - `_namespaces` and `_serialize_xml`, which build the output in one
//!   buffer and hand it to `write` in large pieces;
//! - `TreeBuilder.start`, `end` and `data`, and `XMLParser._start` and
//!   `_end`, so a parse builds the tree without interpreted calls.
//!
//! A native serves only the exact classes, while their dictionaries still
//! hold what [`install`] left there, and elements whose instance
//! dictionaries hold only the fields `Element` itself sets (`tag`,
//! `attrib`, `_children`, `text`, `tail`). Anything else (subclasses,
//! shadowed methods, unusual argument types, every error the Python code
//! raises) calls the Python code the native replaced, so behavior is
//! unchanged. The instances keep their ordinary `__dict__` layout, so
//! pickling, `copy` and Python code reading `elem.tag` see what they
//! always did.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::error::{stop_iteration, type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule, StrKey};
use crate::shared_value::{SharedSlice, SharedStr};
use crate::sync::{Rc, RefCell, Weak};
use crate::types::{PyInstance, SlotStorage, TypeFlags, TypeObject};

type Kw<'a> = &'a [(String, Object)];
type List = Rc<RefCell<Vec<Object>>>;
type Dict = Rc<RefCell<DictData>>;

// ---------------------------------------------------------------------
// Names.

fn interned(s: &str) -> SharedStr {
    match crate::stdlib::sys::intern_name(s) {
        Object::Str(s) => s,
        _ => SharedStr::from(s),
    }
}

/// The attribute names the natives read and write, interned (a name
/// stored through the attribute machinery shares the storage, so a
/// lookup settles on a pointer compare).
struct Names {
    tag: SharedStr,
    attrib: SharedStr,
    children: SharedStr,
    text: SharedStr,
    tail: SharedStr,
    // TreeBuilder.
    data: SharedStr,
    last: SharedStr,
    root: SharedStr,
    tail_flag: SharedStr,
    /// The attributes `Element.__init__`, `TreeBuilder.__init__` and
    /// `XMLParser.__init__` set, in the order they usually appear.
    element_fields: Vec<SharedStr>,
    builder_fields: Vec<SharedStr>,
    parser_fields: Vec<SharedStr>,
}

impl Names {
    fn new() -> Self {
        Self {
            tag: interned("tag"),
            attrib: interned("attrib"),
            children: interned("_children"),
            text: interned("text"),
            tail: interned("tail"),
            data: interned("_data"),
            last: interned("_last"),
            root: interned("_root"),
            tail_flag: interned("_tail"),
            element_fields: ["tag", "attrib", "_children", "text", "tail"]
                .iter()
                .map(|n| interned(n))
                .collect(),
            builder_fields: BUILDER_FIELDS.iter().map(|n| interned(n)).collect(),
            parser_fields: PARSER_FIELDS.iter().map(|n| interned(n)).collect(),
        }
    }
}

// ---------------------------------------------------------------------
// State.

const ELEMENT: usize = 0;
const BUILDER: usize = 1;
const PARSER: usize = 2;

/// One `ElementTree` module's classes (held weakly: the natives live in
/// their dictionaries), its namespace, and the Python code the natives
/// replaced. A fresh import of the module gets a state of its own.
pub(crate) struct State {
    classes: [Weak<TypeObject>; 3],
    class_ptrs: [usize; 3],
    /// The module's globals and `ElementPath`'s.
    ns: Weak<RefCell<DictData>>,
    path_ns: Weak<RefCell<DictData>>,
    names: Names,
    /// `"Element.append"` and so on: the replaced Python function.
    orig: HashMap<&'static str, Object>,
    /// The `attr_version` at which each class was last verified to hold
    /// what [`install`] left in its dictionary; `0` = never.
    sealed: [AtomicU64; 3],
    /// Each class's dictionary after [`install`].
    snapshot: std::sync::OnceLock<[Vec<(DictKey, Object)>; 3]>,
    /// `ElementPath.find`, `findall`, `findtext` and `iterfind` as
    /// [`install`] found them.
    path_funcs: [Object; 4],
    /// The addresses of the natives for `XMLParser._start` and `_end` and
    /// `TreeBuilder.data` (see [`expat_dispatch`]).
    handlers: std::sync::OnceLock<[usize; 3]>,
    /// Each class's [`KeyMap`].
    keymaps: [KeyMap; 3],
}

impl State {
    #[inline]
    fn verified(&self, k: usize, cls: &TypeObject) -> bool {
        let ver = cls.attr_version.get();
        if self.sealed[k].load(Ordering::Relaxed) == ver {
            return true;
        }
        self.verify_slow(k, cls, ver)
    }

    #[cold]
    #[inline(never)]
    fn verify_slow(&self, k: usize, cls: &TypeObject, ver: u64) -> bool {
        let Some(snapshot) = self.snapshot.get() else {
            return false;
        };
        let ok = {
            let Ok(d) = cls.dict.try_borrow() else {
                return false;
            };
            d.len() == snapshot[k].len()
                && snapshot[k]
                    .iter()
                    .all(|(key, want)| d.get(key).is_some_and(|got| got.is_same(want)))
                && cls.mro.borrow().len() == 2
        };
        if ok {
            self.sealed[k].store(ver, Ordering::Relaxed);
        }
        ok
    }

    /// `o` when it is an instance of class `k` (exact), and the class is
    /// as [`install`] left it.
    #[inline]
    fn exact<'a>(&self, k: usize, o: &'a Object) -> Option<&'a Rc<PyInstance>> {
        match o {
            Object::Instance(i) => {
                let cls = class_of(i);
                (std::ptr::from_ref(cls) as usize == self.class_ptrs[k] && self.verified(k, cls))
                    .then_some(i)
            }
            _ => None,
        }
    }

    #[inline]
    fn element_class(&self) -> Option<Rc<TypeObject>> {
        self.classes[ELEMENT].upgrade()
    }

    fn global(&self, name: &str) -> Option<Object> {
        self.ns.upgrade()?.borrow().get(&StrKey(name)).cloned()
    }

    fn call_orig(&self, key: &str, args: &[Object], kw: Kw) -> Result<Object, RuntimeError> {
        let f = self
            .orig
            .get(key)
            .cloned()
            .ok_or_else(|| type_error(format!("_weave_elementtree: {key} missing")))?;
        with_interp(|i| i.call_object(f, args, kw))
    }
}

/// The instance's class, read without the cell's borrow bookkeeping. The
/// natives are installed only while the GIL serializes Python code, and
/// the reference never outlives a call that runs no Python code.
#[inline]
fn class_of(i: &PyInstance) -> &TypeObject {
    // SAFETY: see above; `__class__` assignment, the cell's only writer,
    // can't run while a native reads it.
    unsafe { &*i.class.as_ptr() }
}

fn with_interp<R>(
    f: impl FnOnce(&mut crate::Interpreter) -> Result<R, RuntimeError>,
) -> Result<R, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| type_error("ElementTree: no active interpreter"))?;
    // SAFETY: published by the enclosing VM frame on this thread.
    f(unsafe { &mut *ptr })
}

/// Drop a value a native displaced, flagging the next safe point when
/// that may have released an object with a finalizer.
#[inline]
fn release(o: Object) {
    if !o.is_gc_atomic() {
        crate::gc_trace::mark_maybe_dead();
    }
    drop(o);
}

// ---------------------------------------------------------------------
// Cells.

/// `f` of the cell's value: read in place while cells are unshared (the
/// common case, with no borrow bookkeeping), else through a borrow;
/// `None` when the cell is mutably borrowed. `f` must run no Python code
/// and must not reach the cell again.
#[inline]
fn read<T, R>(c: &RefCell<T>, f: impl FnOnce(&T) -> R) -> Option<R> {
    // SAFETY: `f` runs no code that could borrow the cell (see above).
    if let Some(v) = unsafe { c.peek() } {
        return Some(f(v));
    }
    let v = c.try_borrow().ok()?;
    Some(f(&v))
}

/// [`read`] for a write.
#[inline]
fn write<T, R>(c: &RefCell<T>, f: impl FnOnce(&mut T) -> R) -> Option<R> {
    // SAFETY: as in `read`.
    if let Some(v) = unsafe { c.peek_mut() } {
        return Some(f(v));
    }
    let mut v = c.try_borrow_mut().ok()?;
    Some(f(&mut v))
}

/// `d[key]` for a `str` key, compared natively: the outer `None` when the
/// table met a key whose equality needs Python code (the caller then
/// takes the Python path).
#[inline]
fn str_get(d: &DictData, key: &SharedStr) -> Option<Option<Object>> {
    let probe = crate::object::LeafNameProbe::new(key, SharedStr::hash_cached(key));
    let v = d.get(&probe).cloned();
    (!probe.saw_exotic()).then_some(v)
}

/// [`str_get`] on a dict cell.
#[inline]
fn dict_str_get(d: &Dict, key: &SharedStr) -> Option<Option<Object>> {
    read(d, |d| str_get(d, key))?
}

// ---------------------------------------------------------------------
// Instance fields.

/// Visit each instance attribute (either dictionary layout); `false` when
/// the storage is borrowed.
#[inline]
fn for_each_attr(i: &PyInstance, mut f: impl FnMut(&DictKey, &Object)) -> bool {
    match i.dict.published() {
        Some(d) => read(d, |d| {
            for (k, v) in d.iter() {
                f(k, v);
            }
        })
        .is_some(),
        None => read(i.dict.split_cell(), |s| {
            for (k, v) in s.iter() {
                f(k, v);
            }
        })
        .is_some(),
    }
}

/// Instance attribute `name`, without materializing a split layout.
#[inline]
fn get_field(i: &PyInstance, name: &SharedStr) -> Option<Object> {
    match i.dict.published() {
        Some(d) => read(d, |d| d.get(&StrKey(name)).cloned())?,
        None => read(i.dict.split_cell(), |s| s.get(name).cloned())?,
    }
}

/// `list[i]`, `None` past the end.
#[inline]
fn item(l: &List, i: usize) -> Option<Object> {
    read(l, |v| v.get(i).cloned())?
}

/// `list.append(o)`.
#[inline]
fn append(l: &List, o: Object) {
    if write(l, |v| v.push(o.clone())).is_none() {
        l.borrow_mut().push(o);
    }
}

/// Store instance attribute `name` (a plain `__dict__` store: the caller
/// established that no descriptor intervenes).
fn set_field(i: &PyInstance, name: &SharedStr, value: Object) {
    let old = match i.split_store(name, value) {
        Ok(old) => old,
        Err(value) => {
            let mut d = i.dict_cell().borrow_mut();
            d.insert(DictKey(Object::Str(name.clone())), value)
        }
    };
    if let Some(old) = old {
        release(old);
    }
}

// Element field indices in `Names::element_fields`.
const E_TAG: usize = 0;
const E_ATTRIB: usize = 1;
const E_CHILDREN: usize = 2;
const E_TEXT: usize = 3;

/// Which of `fields` the attribute name `s` is: the name at position
/// `hint` first, then by identity (interned names), then by contents.
#[inline]
fn field_of(s: &SharedStr, fields: &[SharedStr], hint: usize) -> Option<usize> {
    if fields.get(hint).is_some_and(|f| SharedStr::ptr_eq(s, f)) {
        return Some(hint);
    }
    if let Some(j) = fields.iter().position(|f| SharedStr::ptr_eq(s, f)) {
        return Some(j);
    }
    fields.iter().position(|f| **f == **s)
}

/// Which field each of a class's shared attribute names is (see
/// `crate::inst_dict`), for instances whose values are laid out over
/// them: names never move, so the map stays valid as the table grows.
#[derive(Default)]
struct KeyMap {
    /// The `SharedKeys` described (0 = none yet).
    keys: AtomicUsize,
    /// How many of its names are described.
    len: AtomicUsize,
    /// Four bits per name: the field index plus one (0 = not a field).
    map: [AtomicU64; 2],
}

impl KeyMap {
    #[inline]
    fn field_at(&self, pos: usize) -> usize {
        let w = self.map[pos / 16].load(Ordering::Relaxed);
        ((w >> ((pos % 16) * 4)) & 15) as usize
    }

    #[cold]
    #[inline(never)]
    fn refresh(&self, keys: &crate::inst_dict::SharedKeys, fields: &[SharedStr]) {
        let n = keys.len().min(32);
        let mut m = [0u64; 2];
        for pos in 0..n {
            if let Some(DictKey(Object::Str(s))) = keys.get(pos) {
                if let Some(j) = field_of(s, fields, pos) {
                    m[pos / 16] |= ((j as u64 + 1) & 15) << ((pos % 16) * 4);
                }
            }
        }
        self.keys.store(0, Ordering::Relaxed);
        self.map[0].store(m[0], Ordering::Relaxed);
        self.map[1].store(m[1], Ordering::Relaxed);
        self.len.store(n, Ordering::Relaxed);
        self.keys
            .store(std::ptr::from_ref(keys) as usize, Ordering::Relaxed);
    }
}

/// [`scan`] for an instance whose values are split over its class's
/// shared names: the values by position, through the class's
/// [`KeyMap`]. `None` for any other layout.
#[inline]
fn split_scan(
    km: &KeyMap,
    i: &PyInstance,
    fields: &[SharedStr],
    f: &mut impl FnMut(usize, usize, &Object),
) -> Option<bool> {
    let keys = class_of(i).shared_keys.get()?;
    let kp: *const crate::inst_dict::SharedKeys = keys;
    // SAFETY: `f` runs no code (see `scan`), so nothing stores an
    // attribute while the view is held.
    let split = unsafe { i.dict.split_peek() }?;
    let vals = split.values();
    if vals.is_empty() {
        return Some(true);
    }
    split.get_over(kp, 0)?;
    if km.keys.load(Ordering::Relaxed) != kp as usize || km.len.load(Ordering::Relaxed) < vals.len()
    {
        km.refresh(keys, fields);
        if km.len.load(Ordering::Relaxed) < vals.len() {
            return None;
        }
    }
    for (pos, v) in vals.iter().enumerate() {
        match km.field_at(pos) {
            0 => return Some(false),
            j => f(j - 1, pos, v),
        }
    }
    Some(true)
}

/// Visit the attributes of `i`, an instance of class `k`, as `f(field
/// index, position, value)`; `false` when one is not among the class's
/// fields (it is skipped) or the storage is borrowed. `f` must run no
/// Python code.
#[inline]
fn scan(st: &State, k: usize, i: &PyInstance, mut f: impl FnMut(usize, usize, &Object)) -> bool {
    let fields = match k {
        ELEMENT => &st.names.element_fields,
        BUILDER => &st.names.builder_fields,
        _ => &st.names.parser_fields,
    };
    if let Some(r) = split_scan(&st.keymaps[k], i, fields, &mut f) {
        return r;
    }
    let mut ok = true;
    let mut at = 0;
    let read = for_each_attr(i, |k, v| {
        match &k.0 {
            Object::Str(s) => match field_of(s, fields, at) {
                Some(j) => f(j, at, v),
                None => ok = false,
            },
            _ => ok = false,
        }
        at += 1;
    });
    read && ok
}

/// An `Element`'s fields, read in one pass over its attributes.
struct View {
    tag: Object,
    attrib: Option<Object>,
    children: Option<List>,
    text: Object,
    tail: Object,
}

/// The fields of `i`, an exact `Element`, when its instance dictionary
/// holds only fields `Element` sets (so no attribute shadows a method the
/// Python code would call) and `_children`, when set, is a list. Absent
/// fields read as the class attributes (`None`).
fn view(st: &State, i: &PyInstance) -> Option<View> {
    let mut v = View {
        tag: Object::None,
        attrib: None,
        children: None,
        text: Object::None,
        tail: Object::None,
    };
    let mut ok = true;
    let plain = scan(st, ELEMENT, i, |field, _, val| match field {
        E_TAG => v.tag = val.clone(),
        E_ATTRIB => v.attrib = Some(val.clone()),
        E_CHILDREN => match val {
            Object::List(l) => v.children = Some(l.clone()),
            _ => ok = false,
        },
        E_TEXT => v.text = val.clone(),
        _ => v.tail = val.clone(),
    });
    (plain && ok).then_some(v)
}

/// [`view`] of `o` when it is an exact, plain `Element`.
#[inline]
fn element_view(st: &State, o: &Object) -> Option<View> {
    view(st, st.exact(ELEMENT, o)?)
}

/// The children list of `o`, an exact, plain `Element`.
#[inline]
fn children_of(st: &State, o: &Object) -> Option<List> {
    let i = st.exact(ELEMENT, o)?;
    let mut children = None;
    let mut list = true;
    let plain = scan(st, ELEMENT, i, |field, _, v| {
        if field == E_CHILDREN {
            match v {
                Object::List(l) => children = Some(l.clone()),
                _ => list = false,
            }
        }
    });
    if plain && list {
        children
    } else {
        None
    }
}

/// Whether `o` is an instance of `Element` or a subclass (the Python
/// `_assert_is_element` check).
fn is_element(st: &State, o: &Object) -> bool {
    match o {
        Object::Instance(i) => {
            let ptr = st.class_ptrs[ELEMENT] as *const TypeObject;
            class_of(i)
                .mro
                .try_borrow()
                .is_ok_and(|mro| mro.iter().any(|t| std::ptr::eq(Rc::as_ptr(t), ptr)))
        }
        _ => false,
    }
}

fn new_list(items: Vec<Object>) -> Object {
    let l = Object::new_list(items);
    crate::gc_trace::track(&l);
    l
}

fn new_dict(d: DictData) -> Object {
    let o = Object::Dict(Rc::new(RefCell::new(d)));
    crate::gc_trace::track(&o);
    o
}

/// A copy of `attrib` (`{**attrib}`) for an exact `dict`, `None` for
/// anything else.
fn copy_attrib(attrib: &Object) -> Option<Object> {
    merged_attrib(Some(attrib), &[])
}

/// `{**attrib, **extra}` for an exact `dict` (`None` for anything else, or
/// a key only Python code can compare).
fn merged_attrib(attrib: Option<&Object>, extra: Kw) -> Option<Object> {
    let mut d = match attrib {
        None => DictData::default(),
        Some(Object::Dict(d)) => DictData::from(read(d, |d| (**d).clone())?),
        Some(_) => return None,
    };
    for (k, v) in extra {
        let key = SharedStr::from(k.as_str());
        str_get(&d, &key)?;
        d.insert(DictKey(Object::Str(key)), v.clone());
    }
    Some(new_dict(d))
}

/// A new exact `Element` with fields set as `__init__` sets them;
/// `attrib` must be a fresh dict.
fn new_element(st: &State, cls: Rc<TypeObject>, tag: Object, attrib: Object) -> Object {
    let n = &st.names;
    let inst = PyInstance::new_deferred(cls);
    set_field(&inst, &n.tag, tag);
    set_field(&inst, &n.attrib, attrib);
    set_field(&inst, &n.children, new_list(Vec::new()));
    Object::Instance(inst)
}

// ---------------------------------------------------------------------
// Element methods.

/// `Element.__init__(self, tag, attrib={}, **extra)`.
fn el_init(st: &State, a: &[Object], kw: Kw) -> Option<Result<Object, RuntimeError>> {
    let [this, tag, rest @ ..] = a else {
        return None;
    };
    if rest.len() > 1 || kw.iter().any(|(k, _)| k == "tag" || k == "attrib") {
        return None;
    }
    let inst = st.exact(ELEMENT, this)?;
    let attrib = merged_attrib(rest.first(), kw)?;
    let n = &st.names;
    set_field(inst, &n.tag, tag.clone());
    set_field(inst, &n.attrib, attrib);
    set_field(inst, &n.children, new_list(Vec::new()));
    Some(Ok(Object::None))
}

/// `SubElement(parent, tag, attrib={}, **extra)`.
fn sub_element(st: &State, a: &[Object], kw: Kw) -> Option<Result<Object, RuntimeError>> {
    let [parent, tag, rest @ ..] = a else {
        return None;
    };
    if rest.len() > 1
        || kw
            .iter()
            .any(|(k, _)| k == "parent" || k == "tag" || k == "attrib")
    {
        return None;
    }
    let children = children_of(st, parent)?;
    let attrib = merged_attrib(rest.first(), kw)?;
    let elem = new_element(st, st.element_class()?, tag.clone(), attrib);
    append(&children, elem.clone());
    Some(Ok(elem))
}

/// `Element.makeelement(self, tag, attrib)`.
fn el_makeelement(st: &State, a: &[Object], kw: Kw) -> Option<Result<Object, RuntimeError>> {
    let [this, tag, attrib] = a else {
        return None;
    };
    if !kw.is_empty() {
        return None;
    }
    st.exact(ELEMENT, this)?;
    let attrib = copy_attrib(attrib)?;
    Some(Ok(new_element(
        st,
        st.element_class()?,
        tag.clone(),
        attrib,
    )))
}

/// `Element.append(self, subelement)`.
fn el_append(st: &State, a: &[Object], kw: Kw) -> Option<Result<Object, RuntimeError>> {
    let [this, sub] = a else {
        return None;
    };
    if !kw.is_empty() || !is_element(st, sub) {
        return None;
    }
    append(&children_of(st, this)?, sub.clone());
    Some(Ok(Object::None))
}

/// `Element.extend(self, elements)` for a list or tuple of elements.
fn el_extend(st: &State, a: &[Object], kw: Kw) -> Option<Result<Object, RuntimeError>> {
    let [this, elements] = a else {
        return None;
    };
    if !kw.is_empty() {
        return None;
    }
    let items: Vec<Object> = match elements {
        Object::List(l) => l.try_borrow().ok()?.clone(),
        Object::Tuple(t) => t.to_vec(),
        _ => return None,
    };
    if !items.iter().all(|e| is_element(st, e)) {
        return None;
    }
    children_of(st, this)?.borrow_mut().extend(items);
    Some(Ok(Object::None))
}

/// `Element.insert(self, index, subelement)` for an `int` index.
fn el_insert(st: &State, a: &[Object], kw: Kw) -> Option<Result<Object, RuntimeError>> {
    let [this, Object::Int(index), sub] = a else {
        return None;
    };
    if !kw.is_empty() || !is_element(st, sub) {
        return None;
    }
    let children = children_of(st, this)?;
    let mut c = children.borrow_mut();
    let len = c.len() as i64;
    let at = if *index < 0 {
        (index + len).max(0)
    } else {
        (*index).min(len)
    };
    c.insert(at as usize, sub.clone());
    Some(Ok(Object::None))
}

/// `Element.__len__(self)`.
fn el_len(st: &State, a: &[Object], _kw: Kw) -> Option<Result<Object, RuntimeError>> {
    let [this] = a else {
        return None;
    };
    let children = children_of(st, this)?;
    let n = read(&children, Vec::len)?;
    Some(Ok(Object::Int(n as i64)))
}

/// `Element.__getitem__(self, index)` for an in-range `int` index.
fn el_getitem(st: &State, a: &[Object], _kw: Kw) -> Option<Result<Object, RuntimeError>> {
    let [this, Object::Int(index)] = a else {
        return None;
    };
    let children = children_of(st, this)?;
    read(&children, |c| {
        let len = c.len() as i64;
        let at = if *index < 0 { index + len } else { *index };
        (0..len).contains(&at).then(|| Ok(c[at as usize].clone()))
    })?
}

/// The exact `dict` an exact, plain element's `attrib` holds.
fn attrib_of(st: &State, o: &Object) -> Option<Dict> {
    match element_view(st, o)?.attrib? {
        Object::Dict(d) => Some(d),
        _ => None,
    }
}

/// `Element.get(self, key, default=None)` for a `str` key.
fn el_get(st: &State, a: &[Object], kw: Kw) -> Option<Result<Object, RuntimeError>> {
    let (this, key, default) = match (a, kw) {
        ([this, key], []) => (this, key, Object::None),
        ([this, key, default], []) => (this, key, default.clone()),
        ([this, key], [(k, default)]) if k == "default" => (this, key, default.clone()),
        _ => return None,
    };
    let Object::Str(key) = key else {
        return None;
    };
    let attrib = attrib_of(st, this)?;
    let got = dict_str_get(&attrib, key)?;
    Some(Ok(got.unwrap_or(default)))
}

/// `Element.set(self, key, value)` for a `str` key.
fn el_set(st: &State, a: &[Object], kw: Kw) -> Option<Result<Object, RuntimeError>> {
    let [this, Object::Str(key), value] = a else {
        return None;
    };
    if !kw.is_empty() {
        return None;
    }
    let attrib = attrib_of(st, this)?;
    // Probe first: a key only Python code can compare takes that path.
    dict_str_get(&attrib, key)?;
    let old = write(&attrib, |d| {
        d.insert(DictKey(Object::Str(key.clone())), value.clone())
    })?;
    if let Some(old) = old {
        release(old);
    }
    Some(Ok(Object::None))
}

/// Whether `i`'s instance dictionary holds only fields `Element` sets.
#[inline]
fn known_keys_only(st: &State, i: &PyInstance) -> bool {
    scan(st, ELEMENT, i, |_, _, _| {})
}

/// `o` when it is an exact `Element` whose methods no instance attribute
/// shadows.
#[inline]
fn plain<'a>(st: &State, o: &'a Object) -> Option<&'a Rc<PyInstance>> {
    let i = st.exact(ELEMENT, o)?;
    known_keys_only(st, i).then_some(i)
}

/// `a == b` when native code can tell without running Python code: two
/// strings, or `str`/`None`/function operands (which compare by
/// identity unless both are strings).
fn native_eq(a: &Object, b: &Object) -> Option<bool> {
    let simple = |o: &Object| {
        matches!(
            o,
            Object::None | Object::Str(_) | Object::Function(_) | Object::Builtin(_)
        )
    };
    match (a, b) {
        (Object::Str(x), Object::Str(y)) => Some(SharedStr::ptr_eq(x, y) || **x == **y),
        (x, y) if simple(x) && simple(y) => Some(x.is_same(y)),
        _ => None,
    }
}

// ---------------------------------------------------------------------
// Element.iter and Element.itertext.

const IT_STACK: usize = 0;
const IT_TAG: usize = 1;
const IT_MODE: usize = 2;
const IT_RUNNING: usize = 3;
const IT_CLASS: usize = 4;

const MODE_ITER: i64 = 0;
const MODE_TEXT: i64 = 1;
const MODE_SEQ: i64 = 2;

// Frame markers. The stack holds `(object, marker)` pairs:
// - `(element, VISIT)`: an element the walk just reached;
// - `(element, VISITED)`: `iter` yielded the element and reads its
//   children list on the next step;
// - `(children list, i)` for `iter`, `(element, i)` for `itertext` and
//   plain iteration (which index the element, as `for e in self` does):
//   the next child;
// - `(element, TAIL)`: `itertext` yields the element's tail next;
// - `(iterator, DELEGATE)`: a `yield from` an element's own `iter` or
//   `itertext` (an element the natives don't serve).
const F_DELEGATE: i64 = -1;
const F_VISIT: i64 = -2;
const F_VISITED: i64 = -3;
const F_TAIL: i64 = -4;

struct IterType {
    cls: Rc<TypeObject>,
    layout: SharedSlice<DictKey>,
}

fn iter_type() -> &'static IterType {
    static T: std::sync::OnceLock<IterType> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut d = DictData::default();
        d.insert(
            DictKey(Object::from_static("__module__")),
            Object::from_static("xml.etree.ElementTree"),
        );
        let method = |d: &mut DictData,
                      name: &'static str,
                      f: fn(&[Object]) -> Result<Object, RuntimeError>| {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Builtin(Rc::new(BuiltinFn {
                    name,
                    binds_instance: true,
                    call: Box::new(f),
                    call_kw: None,
                })),
            );
        };
        let next = Rc::new(BuiltinFn {
            name: "__next__",
            binds_instance: true,
            call: Box::new(iter_next),
            call_kw: None,
        });
        crate::leaf_builtins::register_fast(&next, iter_next_fast);
        d.insert(
            DictKey(Object::from_static("__next__")),
            Object::Builtin(next),
        );
        method(&mut d, "__iter__", iter_self);
        method(&mut d, "__reduce__", iter_reduce);
        method(&mut d, "__reduce_ex__", iter_reduce);
        let cls = TypeObject::new_with_flags(
            "_element_iterator",
            vec![crate::builtin_types::builtin_types().object_.clone()],
            d,
            TypeFlags {
                is_exception: false,
                is_builtin: true,
            },
        )
        .expect("_element_iterator type");
        let layout: Vec<DictKey> = ["_stack", "_tag", "_mode", "_running", "_class"]
            .iter()
            .map(|n| DictKey(Object::Str(interned(n))))
            .collect();
        IterType {
            cls,
            layout: SharedSlice::from(layout.into_boxed_slice()),
        }
    })
}

fn new_iter(cls: Rc<TypeObject>, root: Object, tag: Object, mode: i64) -> Object {
    let t = iter_type();
    let first = if mode == MODE_SEQ { 0 } else { F_VISIT_ROOT };
    let stack = new_list(vec![root, Object::Int(first)]);
    let mut i = PyInstance::new(t.cls.clone());
    i.slots = RefCell::new(SlotStorage::from_layout(
        t.layout.clone(),
        vec![
            stack,
            tag,
            Object::Int(mode),
            Object::Bool(false),
            Object::Type(cls),
        ],
    ));
    let o = Object::Instance(Rc::new(i));
    crate::gc_trace::track(&o);
    o
}

fn iter_self(a: &[Object]) -> Result<Object, RuntimeError> {
    a.first()
        .cloned()
        .ok_or_else(|| type_error("__iter__ expects an argument"))
}

fn iter_reduce(_a: &[Object]) -> Result<Object, RuntimeError> {
    Err(type_error("cannot pickle '_element_iterator' object"))
}

fn set_running(it: &PyInstance, on: bool) {
    let t = iter_type();
    if let Ok(mut s) = it.slots.try_borrow_mut() {
        if let Some(v) = s.values_for_layout_mut(&t.layout) {
            v[IT_RUNNING] = Object::Bool(on);
        }
    }
}

/// Run Python code on behalf of iterator `it`, marked as running so a
/// reentrant `next` raises as a running generator's does.
fn py<R>(
    it: &PyInstance,
    f: impl FnOnce(&mut crate::Interpreter) -> Result<R, RuntimeError>,
) -> Result<R, RuntimeError> {
    set_running(it, true);
    let r = with_interp(f);
    set_running(it, false);
    r
}

/// What one iterator step produced.
enum Step {
    Yield(Object),
    Done,
    /// (Leaf mode.) The step needs Python code; nothing was consumed, and
    /// the full body continues from here.
    Decline,
}

/// The truth of a text or tail value, when native code can tell.
#[inline]
fn native_truth(o: &Object) -> Option<bool> {
    match o {
        Object::None => Some(false),
        Object::Str(s) => Some(!s.is_empty()),
        _ => None,
    }
}

/// An iterator's frame stack, taken out of its list for one step: plain
/// vector operations instead of a cell borrow each.
struct Frames(Vec<Object>);

impl Frames {
    #[inline]
    fn top(&self) -> Option<(&Object, i64)> {
        let n = self.0.len();
        if n < 2 {
            return None;
        }
        match &self.0[n - 1] {
            Object::Int(m) => Some((&self.0[n - 2], *m)),
            _ => None,
        }
    }

    #[inline]
    fn set_marker(&mut self, marker: i64) {
        let n = self.0.len();
        self.0[n - 1] = Object::Int(marker);
    }

    #[inline]
    fn set_top(&mut self, obj: Object, marker: i64) {
        let n = self.0.len();
        self.0[n - 1] = Object::Int(marker);
        release(std::mem::replace(&mut self.0[n - 2], obj));
    }

    #[inline]
    fn push(&mut self, obj: Object, marker: i64) {
        self.0.push(obj);
        self.0.push(Object::Int(marker));
    }

    #[inline]
    fn pop(&mut self) {
        self.0.pop();
        if let Some(o) = self.0.pop() {
            release(o);
        }
    }
}

/// One step of iterator `a[0]`; in leaf mode it runs no Python code and
/// declines a step that would.
fn iter_step(a: &[Object], leaf: bool) -> Result<Step, RuntimeError> {
    let t = iter_type();
    let Some(Object::Instance(it)) = a.first() else {
        return Err(type_error("descriptor '__next__' needs an argument"));
    };
    let fields = read(&it.slots, |s| {
        s.values_for_layout(&t.layout).map(|v| {
            (
                v[IT_STACK].clone(),
                v[IT_TAG].clone(),
                v[IT_MODE].clone(),
                v[IT_RUNNING].clone(),
                v[IT_CLASS].clone(),
            )
        })
    });
    let (stack, tag, mode, running, cls) = match fields {
        Some(Some(f)) => f,
        Some(None) => {
            return Err(type_error(
                "descriptor '__next__' requires a '_element_iterator' object",
            ))
        }
        None => return Err(value_error("generator already executing")),
    };
    if matches!(running, Object::Bool(true)) {
        if leaf {
            return Ok(Step::Decline);
        }
        return Err(value_error("generator already executing"));
    }
    let (Object::List(stack), Object::Type(cls)) = (stack, cls) else {
        return Ok(Step::Done);
    };
    let Some(st) = state_of_cls(&cls) else {
        return Ok(Step::Done);
    };
    // The frames stay reachable from this call while Python code runs on
    // the iterator's behalf (a reentrant `next` raises first).
    let Some(taken) = write(&stack, std::mem::take) else {
        return Err(value_error("generator already executing"));
    };
    let mut frames = Frames(taken);
    let r = match mode {
        Object::Int(MODE_TEXT) => step_text(st, &mut frames, it, leaf),
        Object::Int(MODE_SEQ) => step_seq(st, &mut frames, it, leaf),
        _ => step_iter(st, &mut frames, &tag, it, leaf),
    };
    if r.is_ok() {
        let mut items = frames.0;
        write(&stack, |v| std::mem::swap(v, &mut items));
        drop(items);
    } else {
        // An exception finishes a generator.
        drop(frames);
        crate::gc_trace::mark_maybe_dead();
    }
    r
}

fn iter_next(a: &[Object]) -> Result<Object, RuntimeError> {
    match iter_step(a, false)? {
        Step::Yield(v) => Ok(v),
        Step::Done | Step::Decline => Err(stop_iteration()),
    }
}

/// `__next__`'s leaf half (see `leaf_builtins::register_fast`).
fn iter_next_fast(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    match iter_step(a, true) {
        Ok(Step::Yield(v)) => Some(Ok(v)),
        Ok(Step::Done) => Some(Err(stop_iteration())),
        Ok(Step::Decline) => None,
        Err(e) => Some(Err(e)),
    }
}

#[inline]
fn state_of_cls(cls: &TypeObject) -> Option<&State> {
    cls.native_ext.get()?.downcast_ref::<State>()
}

/// The iterator for a `yield from obj.<method>(*args)`.
fn delegate(
    it: &PyInstance,
    obj: &Object,
    method: &str,
    args: &[Object],
) -> Result<Object, RuntimeError> {
    py(it, |i| {
        let f = i.load_attr_public(obj, method)?;
        let r = i.call_object(f, args, &[])?;
        i.iter_object(r)
    })
}

/// An element's `tag` and `_children` (absent: `None`), and whether its
/// instance dictionary holds only `Element`'s fields.
fn tag_and_children(st: &State, e: &PyInstance) -> (bool, Object, Option<Object>) {
    let mut tag = Object::None;
    let mut children = None;
    let plain = scan(st, ELEMENT, e, |field, _, v| match field {
        E_TAG => tag = v.clone(),
        E_CHILDREN => children = Some(v.clone()),
        _ => {}
    });
    (plain, tag, children)
}

/// Frame marker for the element an iterator starts from: its method is
/// already running, so no instance attribute can redirect it.
const F_VISIT_ROOT: i64 = -5;

fn step_iter(
    st: &State,
    frames: &mut Frames,
    tag: &Object,
    it: &PyInstance,
    leaf: bool,
) -> Result<Step, RuntimeError> {
    let n = &st.names;
    loop {
        let Some((obj, marker)) = frames.top() else {
            return Ok(Step::Done);
        };
        match marker {
            F_VISIT | F_VISIT_ROOT => {
                let obj = obj.clone();
                let Object::Instance(e) = &obj else {
                    frames.pop();
                    continue;
                };
                let (plain, etag, children) = tag_and_children(st, e);
                if !plain && marker == F_VISIT {
                    if leaf {
                        return Ok(Step::Decline);
                    }
                    // `yield from e.iter(tag)`: the element's own method.
                    let sub = delegate(it, &obj, "iter", std::slice::from_ref(tag))?;
                    frames.set_top(sub, F_DELEGATE);
                    continue;
                }
                let hit = match tag {
                    Object::None => true,
                    _ => match native_eq(&etag, tag) {
                        Some(b) => b,
                        None if leaf => return Ok(Step::Decline),
                        None => py(it, |i| {
                            i.op_compare(&etag, tag, weavepy_compiler::CompareKind::Eq)
                        })?,
                    },
                };
                if hit {
                    frames.set_marker(F_VISITED);
                    return Ok(Step::Yield(obj));
                }
                match children {
                    Some(l @ Object::List(_)) => frames.set_top(l, 0),
                    _ => frames.set_marker(F_VISITED),
                }
            }
            F_VISITED => {
                let obj = obj.clone();
                let Object::Instance(e) = &obj else {
                    frames.pop();
                    continue;
                };
                match get_field(e, &n.children) {
                    Some(l @ Object::List(_)) => frames.set_top(l, 0),
                    _ if leaf => return Ok(Step::Decline),
                    other => {
                        let src = match other {
                            Some(o) => o,
                            None => py(it, |i| i.load_attr_public(&obj, "_children"))?,
                        };
                        let sub = py(it, |i| i.iter_object(src))?;
                        frames.set_top(sub, F_DELEGATE);
                    }
                }
            }
            F_DELEGATE => {
                if leaf {
                    return Ok(Step::Decline);
                }
                let sub = obj.clone();
                match py(it, |i| i.iter_next_object(sub))? {
                    Some(v) => return Ok(Step::Yield(v)),
                    None => frames.pop(),
                }
            }
            i if i >= 0 => {
                let Object::List(l) = obj else {
                    frames.pop();
                    continue;
                };
                match item(l, i as usize) {
                    None => frames.pop(),
                    Some(c) => {
                        if st.exact(ELEMENT, &c).is_some() {
                            frames.set_marker(i + 1);
                            frames.push(c, F_VISIT);
                        } else {
                            if leaf {
                                return Ok(Step::Decline);
                            }
                            frames.set_marker(i + 1);
                            let sub = delegate(it, &c, "iter", std::slice::from_ref(tag))?;
                            frames.push(sub, F_DELEGATE);
                        }
                    }
                }
            }
            _ => frames.pop(),
        }
    }
}

/// `isinstance(tag, str) or tag is None`.
fn text_bearing(tag: &Object) -> bool {
    match tag {
        Object::None | Object::Str(_) | Object::WStr(_) => true,
        Object::Instance(i) => {
            class_of(i).is_subclass_of(&crate::builtin_types::builtin_types().str_)
        }
        _ => false,
    }
}

/// `elem[i]` for `for e in elem` when native code can read it (an exact
/// element whose children are a list): `Some(None)` past the end.
fn child_native(st: &State, e: &Object, i: i64) -> Option<Option<Object>> {
    let inst = st.exact(ELEMENT, e)?;
    match get_field(inst, &st.names.children)? {
        Object::List(l) => Some(item(&l, i as usize)),
        _ => None,
    }
}

/// `elem[i]` through the sequence protocol, `None` past the end.
fn child_py(it: &PyInstance, e: &Object, i: i64) -> Result<Option<Object>, RuntimeError> {
    match py(it, |interp| interp.accel_subscript(e, &Object::Int(i))) {
        Ok(v) => Ok(Some(v)),
        Err(RuntimeError::PyException(pe)) if is_index_error(&pe.instance) => Ok(None),
        Err(e) => Err(e),
    }
}

fn is_index_error(o: &Object) -> bool {
    matches!(o, Object::Instance(x)
        if class_of(x).is_subclass_of(&crate::builtin_types::builtin_types().index_error))
}

fn step_text(
    st: &State,
    frames: &mut Frames,
    it: &PyInstance,
    leaf: bool,
) -> Result<Step, RuntimeError> {
    let n = &st.names;
    loop {
        let Some((obj, marker)) = frames.top() else {
            return Ok(Step::Done);
        };
        let obj = obj.clone();
        match marker {
            F_VISIT | F_VISIT_ROOT => {
                let Object::Instance(e) = &obj else {
                    frames.pop();
                    continue;
                };
                let mut tag = Object::None;
                let mut text = Object::None;
                let plain = scan(st, ELEMENT, e, |field, _, v| match field {
                    E_TAG => tag = v.clone(),
                    E_TEXT => text = v.clone(),
                    _ => {}
                });
                if !plain && marker == F_VISIT {
                    if leaf {
                        return Ok(Step::Decline);
                    }
                    let sub = delegate(it, &obj, "itertext", &[])?;
                    frames.set_top(sub, F_DELEGATE);
                    continue;
                }
                if !text_bearing(&tag) {
                    frames.pop();
                    continue;
                }
                let truth = match native_truth(&text) {
                    Some(b) => b,
                    None if leaf => return Ok(Step::Decline),
                    None => py(it, |i| i.op_truth(&text))?,
                };
                frames.set_marker(0);
                if truth {
                    return Ok(Step::Yield(text));
                }
            }
            F_TAIL => {
                // `t = e.tail; if t: yield t`, after `e`'s own text.
                let tail = match st.exact(ELEMENT, &obj) {
                    Some(e) => get_field(e, &n.tail).unwrap_or(Object::None),
                    None if leaf => return Ok(Step::Decline),
                    None => py(it, |i| i.load_attr_public(&obj, "tail"))?,
                };
                let truth = match native_truth(&tail) {
                    Some(b) => b,
                    None if leaf => return Ok(Step::Decline),
                    None => py(it, |i| i.op_truth(&tail))?,
                };
                frames.pop();
                if truth {
                    return Ok(Step::Yield(tail));
                }
            }
            F_DELEGATE => {
                if leaf {
                    return Ok(Step::Decline);
                }
                match py(it, |i| i.iter_next_object(obj))? {
                    Some(v) => return Ok(Step::Yield(v)),
                    None => frames.pop(),
                }
            }
            i if i >= 0 => {
                let child = match child_native(st, &obj, i) {
                    Some(c) => c,
                    None if leaf => return Ok(Step::Decline),
                    None => child_py(it, &obj, i)?,
                };
                match child {
                    None => frames.pop(),
                    Some(c) => {
                        let exact = st.exact(ELEMENT, &c).is_some();
                        if !exact && leaf {
                            return Ok(Step::Decline);
                        }
                        frames.set_marker(i + 1);
                        frames.push(c.clone(), F_TAIL);
                        if exact {
                            frames.push(c, F_VISIT);
                        } else {
                            let sub = delegate(it, &c, "itertext", &[])?;
                            frames.push(sub, F_DELEGATE);
                        }
                    }
                }
            }
            _ => frames.pop(),
        }
    }
}

/// `iter(elem)`: the sequence protocol over `elem[0]`, `elem[1]`, ...
fn step_seq(
    st: &State,
    frames: &mut Frames,
    it: &PyInstance,
    leaf: bool,
) -> Result<Step, RuntimeError> {
    let Some((obj, i)) = frames.top() else {
        return Ok(Step::Done);
    };
    if i < 0 {
        return Ok(Step::Done);
    }
    let obj = obj.clone();
    let child = match child_native(st, &obj, i) {
        Some(c) => c,
        None if leaf => return Ok(Step::Decline),
        None => child_py(it, &obj, i)?,
    };
    match child {
        None => {
            frames.pop();
            Ok(Step::Done)
        }
        Some(c) => {
            frames.set_marker(i + 1);
            Ok(Step::Yield(c))
        }
    }
}

/// `iter(elem)` for an exact `Element` (which has no `__iter__`: CPython
/// iterates it through `__getitem__`), as a native iterator. `None` for
/// anything else.
pub(crate) fn sequence_iter(o: &Object) -> Option<Object> {
    let Object::Instance(i) = o else {
        return None;
    };
    if crate::gil::free_threading_enabled() {
        return None;
    }
    let st = state_of_cls(class_of(i))?;
    st.exact(ELEMENT, o)?;
    Some(new_iter(
        st.element_class()?,
        o.clone(),
        Object::None,
        MODE_SEQ,
    ))
}

/// `Element.iter(self, tag=None)`.
fn el_iter(st: &State, a: &[Object], kw: Kw) -> Option<Result<Object, RuntimeError>> {
    let (this, tag) = match (a, kw) {
        ([this], []) => (this, Object::None),
        ([this, tag], []) => (this, tag.clone()),
        ([this], [(k, tag)]) if k == "tag" => (this, tag.clone()),
        _ => return None,
    };
    plain(st, this)?;
    let tag = match tag {
        Object::Str(s) if &*s == "*" => Object::None,
        Object::None | Object::Str(_) | Object::Function(_) | Object::Builtin(_) => tag,
        _ => return None,
    };
    Some(Ok(new_iter(
        st.element_class()?,
        this.clone(),
        tag,
        MODE_ITER,
    )))
}

/// `Element.itertext(self)`.
fn el_itertext(st: &State, a: &[Object], kw: Kw) -> Option<Result<Object, RuntimeError>> {
    let [this] = a else {
        return None;
    };
    if !kw.is_empty() {
        return None;
    }
    plain(st, this)?;
    Some(Ok(new_iter(
        st.element_class()?,
        this.clone(),
        Object::None,
        MODE_TEXT,
    )))
}

// ---------------------------------------------------------------------
// Element.find, findall and findtext.

/// One step of a selector `ElementPath` compiled: which of its `select`
/// closures it is, with the closure's tag, key or value.
enum PathStep {
    /// `prepare_child`: the children whose tag equals the name.
    Child(SharedStr),
    /// `prepare_star`: every child.
    Star,
    /// `prepare_self`: the context itself.
    Current,
    /// `prepare_descendant`: the descendants (`elem.iter(tag)` without
    /// `elem`), all of them for `*`.
    Descendant(Option<SharedStr>),
    /// `[@key]`.
    Has(SharedStr),
    /// `[@key='value']`.
    AttrEq(SharedStr, SharedStr),
    /// `[@key!='value']`.
    AttrNe(SharedStr, SharedStr),
}

const EP_FIND: usize = 0;
const EP_FINDALL: usize = 1;
const EP_FINDTEXT: usize = 2;
const EP_ITERFIND: usize = 3;

/// The closure `f` holds for free variable `name`, when it is a `str`.
fn closure_str(
    f: &crate::object::PyFunction,
    freevars: &[String],
    name: &str,
) -> Option<SharedStr> {
    let i = freevars.iter().position(|v| v == name)?;
    match f.closure.get(i)? {
        Object::Cell(c) => match &*c.try_borrow().ok()? {
            Object::Str(s) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// The steps of `selector` (a list of `ElementPath` select closures),
/// when native code serves each of them.
fn compile_selector(path_ns: &Dict, selector: &Object) -> Option<Vec<PathStep>> {
    let Object::List(l) = selector else {
        return None;
    };
    let l = l.try_borrow().ok()?;
    let mut steps = Vec::with_capacity(l.len());
    for f in l.iter() {
        let Object::Function(f) = f else {
            return None;
        };
        if !Rc::ptr_eq(&f.globals, path_ns) {
            return None;
        }
        let code = f.code.try_borrow().ok()?.clone();
        let fv = &code.freevars;
        let step = match (code.qualname.as_str(), fv.len()) {
            ("prepare_child.<locals>.select", 1) => PathStep::Child(closure_str(f, fv, "tag")?),
            ("prepare_star.<locals>.select", 0) => PathStep::Star,
            ("prepare_self.<locals>.select", 0) => PathStep::Current,
            ("prepare_descendant.<locals>.select", 1) => {
                let tag = closure_str(f, fv, "tag")?;
                PathStep::Descendant((&*tag != "*").then_some(tag))
            }
            ("prepare_predicate.<locals>.select", 1) => PathStep::Has(closure_str(f, fv, "key")?),
            ("prepare_predicate.<locals>.select", 2) => {
                PathStep::AttrEq(closure_str(f, fv, "key")?, closure_str(f, fv, "value")?)
            }
            ("prepare_predicate.<locals>.select_negated", 2) => {
                PathStep::AttrNe(closure_str(f, fv, "key")?, closure_str(f, fv, "value")?)
            }
            _ => return None,
        };
        steps.push(step);
    }
    Some(steps)
}

/// The compiled steps for `elem.<method>(path, namespaces)`, when
/// `ElementPath` has the path's selector cached (a path seen for the
/// first time takes the Python code, which compiles and caches it) and
/// native code serves every step.
fn path_steps(
    st: &State,
    which: usize,
    path: &Object,
    namespaces: &Object,
) -> Option<Vec<PathStep>> {
    let Object::Str(p) = path else {
        return None;
    };
    if !matches!(namespaces, Object::None) || p.ends_with('/') {
        return None;
    }
    let path_ns = st.path_ns.upgrade()?;
    match st.global("ElementPath")? {
        Object::Module(m) if Rc::ptr_eq(&m.dict, &path_ns) => {}
        _ => return None,
    }
    let (cache, ok) = {
        let d = path_ns.try_borrow().ok()?;
        let entry = |name: &str, i: usize| {
            d.get(&StrKey(name))
                .is_some_and(|f| f.is_same(&st.path_funcs[i]))
        };
        let ok = entry("iterfind", EP_ITERFIND)
            && match which {
                EP_FIND => entry("find", EP_FIND),
                EP_FINDALL => entry("findall", EP_FINDALL),
                _ => entry("findtext", EP_FINDTEXT),
            };
        (d.get(&StrKey("_cache")).cloned(), ok)
    };
    let (Some(Object::Dict(cache)), true) = (cache, ok) else {
        return None;
    };
    let hash = crate::object::py_hash_value(&Object::new_tuple(vec![path.clone()]))?;
    let probe = PathKey {
        path: p,
        hash,
        exotic: std::cell::Cell::new(false),
    };
    let selector = read(&cache, |c| c.get(&probe).cloned())??;
    if probe.exotic.get() {
        return None;
    }
    compile_selector(&path_ns, &selector)
}

/// An `ElementPath._cache` probe for the key `(path,)`, compared natively:
/// `exotic` is set when the table met a key only Python code can compare.
struct PathKey<'a> {
    path: &'a str,
    hash: i64,
    exotic: std::cell::Cell<bool>,
}

impl std::hash::Hash for PathKey<'_> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.hash.hash(state);
    }
}

impl indexmap::Equivalent<DictKey> for PathKey<'_> {
    fn equivalent(&self, key: &DictKey) -> bool {
        match &key.0 {
            Object::Tuple(t) => {
                let mut items = t.iter();
                let first = items.next();
                let plain = t.iter().all(|x| matches!(x, Object::Str(_)));
                if !plain {
                    self.exotic.set(true);
                    return false;
                }
                matches!(first, Some(Object::Str(s)) if **s == *self.path) && t.len() == 1
            }
            Object::Str(_) | Object::Int(_) | Object::None => false,
            _ => {
                self.exotic.set(true);
                false
            }
        }
    }
}

/// `e.tag == tag` for an exact element `e` and a `str` tag.
fn tag_is(st: &State, e: &Object, tag: &SharedStr) -> Option<bool> {
    let inst = st.exact(ELEMENT, e)?;
    match get_field(inst, &st.names.tag).unwrap_or(Object::None) {
        Object::Str(s) => Some(SharedStr::ptr_eq(&s, tag) || *s == **tag),
        Object::None | Object::Function(_) | Object::Builtin(_) => Some(false),
        _ => None,
    }
}

/// `elem.get(key)` for an exact, plain element: `Some(None)` when the
/// key is absent.
fn attr_value(st: &State, e: &Object, key: &SharedStr) -> Option<Option<Object>> {
    let attrib = attrib_of(st, e)?;
    dict_str_get(&attrib, key)
}

/// Run `steps` from `root`, as `ElementPath.iterfind` would; `None` when
/// an element or value along the way needs the Python code.
fn eval_path(st: &State, root: &Object, steps: &[PathStep]) -> Option<Vec<Object>> {
    let mut result = vec![root.clone()];
    for step in steps {
        let mut out = Vec::new();
        match step {
            PathStep::Child(tag) => {
                for e in &result {
                    let children = children_of(st, e)?;
                    let n = read(&children, Vec::len)?;
                    for i in 0..n {
                        let Some(child) = item(&children, i) else {
                            break;
                        };
                        if tag_is(st, &child, tag)? {
                            out.push(child);
                        }
                    }
                }
            }
            PathStep::Star => {
                for e in &result {
                    let children = children_of(st, e)?;
                    out.extend(read(&children, |c| c.clone())?);
                }
            }
            PathStep::Current => out = result.clone(),
            PathStep::Descendant(tag) => {
                for e in &result {
                    let mut stack: Vec<(List, usize)> = vec![(children_of(st, e)?, 0)];
                    while let Some((list, i)) = stack.pop() {
                        let Some(child) = item(&list, i) else {
                            continue;
                        };
                        stack.push((list, i + 1));
                        let children = children_of(st, &child)?;
                        if tag.as_ref().map_or(Some(true), |t| tag_is(st, &child, t))? {
                            out.push(child);
                        }
                        stack.push((children, 0));
                    }
                }
            }
            PathStep::Has(key) => {
                for e in &result {
                    if !matches!(attr_value(st, e, key)?, None | Some(Object::None)) {
                        out.push(e.clone());
                    }
                }
            }
            PathStep::AttrEq(key, value) | PathStep::AttrNe(key, value) => {
                let negated = matches!(step, PathStep::AttrNe(..));
                for e in &result {
                    let hit = match attr_value(st, e, key)? {
                        None | Some(Object::None) => false,
                        Some(Object::Str(s)) => (*s == **value) != negated,
                        Some(_) => return None,
                    };
                    if hit {
                        out.push(e.clone());
                    }
                }
            }
        }
        result = out;
    }
    Some(result)
}

/// The `(path, namespaces)` arguments of `find`/`findall`.
fn path_args<'a>(
    a: &'a [Object],
    kw: &'a [(String, Object)],
) -> Option<(&'a Object, &'a Object, &'a Object)> {
    match (a, kw) {
        ([this, path], []) => Some((this, path, &Object::None)),
        ([this, path, ns], []) => Some((this, path, ns)),
        ([this, path], [(k, ns)]) if k == "namespaces" => Some((this, path, ns)),
        _ => None,
    }
}

/// `Element.findall(self, path, namespaces=None)`.
fn el_findall(st: &State, a: &[Object], kw: Kw) -> Option<Result<Object, RuntimeError>> {
    let (this, path, ns) = path_args(a, kw)?;
    plain(st, this)?;
    let steps = path_steps(st, EP_FINDALL, path, ns)?;
    let found = eval_path(st, this, &steps)?;
    Some(Ok(new_list(found)))
}

/// `Element.find(self, path, namespaces=None)`.
fn el_find(st: &State, a: &[Object], kw: Kw) -> Option<Result<Object, RuntimeError>> {
    let (this, path, ns) = path_args(a, kw)?;
    plain(st, this)?;
    let steps = path_steps(st, EP_FIND, path, ns)?;
    let found = eval_path(st, this, &steps)?;
    Some(Ok(found.into_iter().next().unwrap_or(Object::None)))
}

/// `Element.findtext(self, path, default=None, namespaces=None)`.
fn el_findtext(st: &State, a: &[Object], kw: Kw) -> Option<Result<Object, RuntimeError>> {
    let mut default = Object::None;
    let mut ns = Object::None;
    let (this, path) = match a {
        [this, path] => (this, path),
        [this, path, d] => {
            default = d.clone();
            (this, path)
        }
        [this, path, d, n] => {
            default = d.clone();
            ns = n.clone();
            (this, path)
        }
        _ => return None,
    };
    for (k, v) in kw {
        match k.as_str() {
            "default" if a.len() < 3 => default = v.clone(),
            "namespaces" if a.len() < 4 => ns = v.clone(),
            _ => return None,
        }
    }
    plain(st, this)?;
    let steps = path_steps(st, EP_FINDTEXT, path, &ns)?;
    let found = eval_path(st, this, &steps)?;
    let Some(first) = found.into_iter().next() else {
        return Some(Ok(default));
    };
    let inst = st.exact(ELEMENT, &first)?;
    Some(Ok(match get_field(inst, &st.names.text) {
        None | Some(Object::None) => Object::from_static(""),
        Some(t) => t,
    }))
}

// ---------------------------------------------------------------------
// Serialization.

fn escape_cdata_into(out: &mut String, s: &str) {
    let mut last = 0;
    for (i, b) in s.bytes().enumerate() {
        let rep = match b {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            _ => continue,
        };
        out.push_str(&s[last..i]);
        out.push_str(rep);
        last = i + 1;
    }
    out.push_str(&s[last..]);
}

fn escape_attrib_into(out: &mut String, s: &str) {
    let mut last = 0;
    for (i, b) in s.bytes().enumerate() {
        let rep = match b {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' => "&quot;",
            b'\r' => "&#13;",
            b'\n' => "&#10;",
            b'\t' => "&#09;",
            _ => continue,
        };
        out.push_str(&s[last..i]);
        out.push_str(rep);
        last = i + 1;
    }
    out.push_str(&s[last..]);
}

/// `_namespaces(elem, default_namespace=None)`.
fn ser_namespaces(st: &State, a: &[Object], kw: Kw) -> Option<Result<Object, RuntimeError>> {
    let (root, default_ns) = match (a, kw) {
        ([e], []) => (e, Object::None),
        ([e, d], []) => (e, d.clone()),
        ([e], [(k, d)]) if k == "default_namespace" => (e, d.clone()),
        _ => return None,
    };
    let default_ns = match default_ns {
        Object::None => None,
        Object::Str(s) => (!s.is_empty()).then_some(s),
        _ => return None,
    };
    let Some(Object::Dict(ns_map)) = st.global("_namespace_map") else {
        return None;
    };
    let comment = st.global("Comment")?;
    let pi = st.global("PI")?;
    let mut qnames = DictData::default();
    qnames.insert(DictKey(Object::None), Object::None);
    let mut namespaces = DictData::default();
    if let Some(d) = &default_ns {
        namespaces.insert(DictKey(Object::Str(d.clone())), Object::from_static(""));
    }
    let ns_map = ns_map.try_borrow().ok()?;
    let add_qname =
        |q: &SharedStr, qnames: &mut DictData, namespaces: &mut DictData| -> Option<()> {
            if let Some(rest) = q.strip_prefix('{') {
                let (uri, tag) = rest.rsplit_once('}')?;
                let prefix = match namespaces.get(&StrKey(uri)) {
                    Some(Object::Str(p)) => p.clone(),
                    Some(_) => return None,
                    None => {
                        let p = match ns_map.get(&StrKey(uri)) {
                            Some(Object::Str(p)) => p.clone(),
                            Some(_) => return None,
                            None => SharedStr::from(format!("ns{}", namespaces.len())),
                        };
                        if &*p != "xml" {
                            namespaces
                                .insert(DictKey(Object::from_str(uri)), Object::Str(p.clone()));
                        }
                        p
                    }
                };
                let value = if prefix.is_empty() {
                    Object::from_str(tag)
                } else {
                    Object::from_str(format!("{}:{tag}", &*prefix))
                };
                qnames.insert(DictKey(Object::Str(q.clone())), value);
            } else {
                if default_ns.is_some() {
                    return None;
                }
                qnames.insert(DictKey(Object::Str(q.clone())), Object::Str(q.clone()));
            }
            Some(())
        };
    // `elem.iter()`: preorder.
    let mut stack: Vec<(List, usize)> = Vec::new();
    let mut next = Some(root.clone());
    loop {
        let e = match next.take() {
            Some(e) => e,
            None => {
                let Some((list, i)) = stack.pop() else {
                    break;
                };
                let Some(child) = item(&list, i) else {
                    continue;
                };
                stack.push((list, i + 1));
                child
            }
        };
        let v = element_view(st, &e)?;
        match &v.tag {
            Object::Str(t) => {
                if qnames.get(&StrKey(t)).is_none() {
                    add_qname(t, &mut qnames, &mut namespaces)?;
                }
            }
            Object::None => {}
            t if t.is_same(&comment) || t.is_same(&pi) => {}
            _ => return None,
        }
        let Some(Object::Dict(attrib)) = &v.attrib else {
            return None;
        };
        for (k, val) in attrib.try_borrow().ok()?.iter() {
            let Object::Str(k) = &k.0 else {
                return None;
            };
            if matches!(val, Object::Instance(_)) {
                return None;
            }
            if qnames.get(&StrKey(k)).is_none() {
                add_qname(k, &mut qnames, &mut namespaces)?;
            }
        }
        if matches!(v.text, Object::Instance(_)) {
            return None;
        }
        stack.push((v.children?, 0));
    }
    Some(Ok(Object::new_tuple(vec![
        new_dict(qnames),
        new_dict(namespaces),
    ])))
}

/// Output for `_serialize_xml`, handed to `write` in large pieces.
struct Out {
    buf: String,
    write: Object,
}

const FLUSH_AT: usize = 1 << 16;

impl Out {
    fn flush(&mut self) -> Result<(), RuntimeError> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let s = Object::from_str(std::mem::take(&mut self.buf));
        let w = self.write.clone();
        with_interp(|i| i.call_object(w, &[s], &[]))?;
        Ok(())
    }

    #[inline]
    fn maybe_flush(&mut self) -> Result<(), RuntimeError> {
        if self.buf.len() >= FLUSH_AT {
            self.flush()?;
        }
        Ok(())
    }
}

/// What `_serialize_xml` writes for one element before its children, as
/// prepared (and checked) before anything is written.
struct Opening {
    /// `None` when the element's tag maps to `None` (only its text and
    /// children are written).
    qtag: Option<SharedStr>,
    text: Option<SharedStr>,
    attrs: Vec<(SharedStr, SharedStr)>,
    children: List,
}

enum Prepared {
    Comment(String),
    Element(Opening),
}

/// Check and prepare an element for the native serializer: `None` when
/// any part of it needs the Python code (the element is then serialized
/// by `_serialize_xml` itself, which raises whatever error it should).
fn prepare(
    st: &State,
    e: &Object,
    qnames: &DictData,
    comment: &Object,
    pi: &Object,
) -> Option<Prepared> {
    let v = element_view(st, e)?;
    let text = match &v.text {
        Object::None => None,
        Object::Str(s) => Some(s.clone()),
        _ => return None,
    };
    if v.tag.is_same(comment) || v.tag.is_same(pi) {
        let t = text.as_deref().unwrap_or("None");
        let s = if v.tag.is_same(comment) {
            format!("<!--{t}-->")
        } else {
            format!("<?{t}?>")
        };
        return Some(Prepared::Comment(s));
    }
    if !matches!(v.tag, Object::Str(_) | Object::None) {
        return None;
    }
    let qtag = match qnames.get(&DictKey(v.tag.clone())) {
        Some(Object::None) => None,
        Some(Object::Str(s)) => Some(s.clone()),
        _ => return None,
    };
    let mut attrs = Vec::new();
    if qtag.is_some() {
        let Some(Object::Dict(attrib)) = &v.attrib else {
            return None;
        };
        for (k, val) in attrib.try_borrow().ok()?.iter() {
            let (Object::Str(_), Object::Str(val)) = (&k.0, val) else {
                return None;
            };
            let Some(Object::Str(qk)) = qnames.get(k) else {
                return None;
            };
            attrs.push((qk.clone(), val.clone()));
        }
    }
    Some(Prepared::Element(Opening {
        qtag,
        text,
        attrs,
        children: v.children?,
    }))
}

/// The namespace declarations for the root, sorted on prefix: `None`
/// when one is not a `str` pair.
fn ns_decls(namespaces: &Object) -> Option<Vec<(SharedStr, SharedStr)>> {
    let Object::Dict(d) = namespaces else {
        return match namespaces {
            Object::None => Some(Vec::new()),
            _ => None,
        };
    };
    let mut v = Vec::new();
    for (uri, prefix) in d.try_borrow().ok()?.iter() {
        let (Object::Str(uri), Object::Str(prefix)) = (&uri.0, prefix) else {
            return None;
        };
        v.push((prefix.clone(), uri.clone()));
    }
    v.sort_by(|a, b| a.0.cmp(&b.0));
    Some(v)
}

enum Frame {
    Elem(Object),
    Children {
        elem: Object,
        list: List,
        index: usize,
        end: Option<SharedStr>,
    },
}

/// `_serialize_xml(write, elem, qnames, namespaces, short_empty_elements,
/// **kwargs)`.
fn ser_xml(st: &State, a: &[Object], kw: Kw) -> Option<Result<Object, RuntimeError>> {
    let (write, root, qnames, namespaces) = match a {
        [w, e, q, n, ..] if a.len() <= 5 => (w, e, q, n),
        _ => return None,
    };
    let mut short = a.get(4).cloned();
    for (k, v) in kw {
        match k.as_str() {
            "short_empty_elements" if short.is_none() => short = Some(v.clone()),
            "write" | "elem" | "qnames" | "namespaces" | "short_empty_elements" => return None,
            _ => {}
        }
    }
    let short = match short? {
        Object::Bool(b) => b,
        Object::Int(i) => i != 0,
        Object::None => false,
        _ => return None,
    };
    let Object::Dict(qnames_d) = qnames else {
        return None;
    };
    let comment = st.global("Comment")?;
    let pi = st.global("ProcessingInstruction")?;
    let decls = ns_decls(namespaces)?;
    // The root must be served natively (the Python code would otherwise
    // run for it anyway).
    read(qnames_d, |q| prepare(st, root, q, &comment, &pi))??;
    let mut out = Out {
        buf: String::new(),
        write: write.clone(),
    };
    let r = ser_run(st, &mut out, root, qnames, &decls, short, &comment, &pi);
    Some(match r {
        Ok(()) => out.flush().map(|()| Object::None),
        Err(e) => {
            // Python would have written everything before the error.
            let _ = out.flush();
            Err(e)
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn ser_run(
    st: &State,
    out: &mut Out,
    root: &Object,
    qnames: &Object,
    decls: &[(SharedStr, SharedStr)],
    short: bool,
    comment: &Object,
    pi: &Object,
) -> Result<(), RuntimeError> {
    let Object::Dict(qd) = qnames else {
        return Ok(());
    };
    let mut stack = vec![Frame::Elem(root.clone())];
    let mut at_root = true;
    while let Some(frame) = stack.pop() {
        out.maybe_flush()?;
        let e = match frame {
            Frame::Elem(e) => e,
            Frame::Children {
                elem,
                list,
                index,
                end,
            } => {
                if let Some(child) = item(&list, index) {
                    stack.push(Frame::Children {
                        elem,
                        list,
                        index: index + 1,
                        end,
                    });
                    stack.push(Frame::Elem(child));
                    continue;
                }
                if let Some(end) = end {
                    out.buf.push_str("</");
                    out.buf.push_str(&end);
                    out.buf.push('>');
                }
                write_tail(st, out, &elem)?;
                continue;
            }
        };
        let prepared = read(qd, |q| prepare(st, &e, q, comment, pi)).flatten();
        let Some(prepared) = prepared else {
            // The Python code serializes this element (and its tail).
            out.flush()?;
            let w = out.write.clone();
            st.call_orig(
                "_serialize_xml",
                &[w, e, qnames.clone(), Object::None],
                &[("short_empty_elements".to_owned(), Object::Bool(short))],
            )?;
            at_root = false;
            continue;
        };
        let decls = if std::mem::take(&mut at_root) {
            decls
        } else {
            &[]
        };
        let o = match prepared {
            Prepared::Comment(s) => {
                out.buf.push_str(&s);
                write_tail(st, out, &e)?;
                continue;
            }
            Prepared::Element(o) => o,
        };
        let Some(qtag) = o.qtag else {
            if let Some(t) = &o.text {
                escape_cdata_into(&mut out.buf, t);
            }
            stack.push(Frame::Children {
                elem: e,
                list: o.children,
                index: 0,
                end: None,
            });
            continue;
        };
        out.buf.push('<');
        out.buf.push_str(&qtag);
        for (prefix, uri) in decls {
            out.buf.push_str(" xmlns");
            if !prefix.is_empty() {
                out.buf.push(':');
                out.buf.push_str(prefix);
            }
            out.buf.push_str("=\"");
            escape_attrib_into(&mut out.buf, uri);
            out.buf.push('"');
        }
        for (k, v) in &o.attrs {
            out.buf.push(' ');
            out.buf.push_str(k);
            out.buf.push_str("=\"");
            escape_attrib_into(&mut out.buf, v);
            out.buf.push('"');
        }
        let has_text = o.text.as_ref().is_some_and(|t| !t.is_empty());
        let has_children = read(&o.children, |c| !c.is_empty()).unwrap_or(true);
        if has_text || has_children || !short {
            out.buf.push('>');
            if let Some(t) = &o.text {
                escape_cdata_into(&mut out.buf, t);
            }
            stack.push(Frame::Children {
                elem: e,
                list: o.children,
                index: 0,
                end: Some(qtag),
            });
        } else {
            out.buf.push_str(" />");
            write_tail(st, out, &e)?;
        }
    }
    Ok(())
}

/// `if elem.tail: write(_escape_cdata(elem.tail))`.
fn write_tail(st: &State, out: &mut Out, e: &Object) -> Result<(), RuntimeError> {
    let tail = match e {
        Object::Instance(i) => get_field(i, &st.names.tail).unwrap_or(Object::None),
        _ => Object::None,
    };
    match &tail {
        Object::None => Ok(()),
        Object::Str(s) => {
            escape_cdata_into(&mut out.buf, s);
            Ok(())
        }
        _ => {
            if !with_interp(|i| i.op_truth(&tail))? {
                return Ok(());
            }
            let escape = st
                .global("_escape_cdata")
                .ok_or_else(|| type_error("_escape_cdata missing"))?;
            let s = with_interp(|i| i.call_object(escape, &[tail.clone()], &[]))?;
            match &s {
                Object::Str(s) => out.buf.push_str(s),
                _ => {
                    out.flush()?;
                    let w = out.write.clone();
                    with_interp(|i| i.call_object(w, &[s.clone()], &[]))?;
                }
            }
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------
// TreeBuilder and XMLParser.

const BUILDER_FIELDS: [&str; 10] = [
    "_data",
    "_elem",
    "_last",
    "_root",
    "_tail",
    "_comment_factory",
    "insert_comments",
    "_pi_factory",
    "insert_pis",
    "_factory",
];
const TB_DATA: usize = 0;
const TB_LAST: usize = 2;
const TB_ROOT: usize = 3;
const TB_TAIL: usize = 4;
const TB_FACTORY: usize = 9;

const PARSER_FIELDS: [&str; 9] = [
    "parser", "_parser", "target", "_target", "_error", "_names", "_doctype", "entity", "version",
];
const XP_TARGET: usize = 2;
const XP_NAMES: usize = 5;

/// Store `value` as the attribute at position `at` of `i`'s instance
/// dictionary (where [`scan`] found `name`), else by name.
fn store_at(i: &PyInstance, at: usize, name: &SharedStr, value: Object) {
    if at != usize::MAX {
        // SAFETY: no code runs while the view is held.
        if let Some((_, slot)) = unsafe { i.attr_peek_index_mut(at, value.is_gc_atomic()) } {
            release(std::mem::replace(slot, value));
            return;
        }
    }
    set_field(i, name, value);
}

/// A `TreeBuilder`'s fields that its methods use, read in one pass, with
/// their positions (for [`store_at`]).
struct TbFields {
    at: [usize; 5],
    data: List,
    elems: List,
    last: Object,
    root: Object,
    tail: Object,
    factory: Object,
}

/// The fields of an exact `TreeBuilder` whose methods no instance
/// attribute shadows, all present (`_data` and `_elem` lists).
fn builder(st: &State, o: &Object) -> Option<(Rc<PyInstance>, TbFields)> {
    let i = st.exact(BUILDER, o)?;
    let mut at = [usize::MAX; 5];
    let mut v: [Option<Object>; 5] = Default::default();
    let mut factory = None;
    let plain = scan(st, BUILDER, i, |j, pos, val| {
        if j < 5 {
            at[j] = pos;
            v[j] = Some(val.clone());
        } else if j == TB_FACTORY {
            factory = Some(val.clone());
        }
    });
    if !plain {
        return None;
    }
    let [Some(Object::List(data)), Some(Object::List(elems)), Some(last), Some(root), Some(tail)] =
        v
    else {
        return None;
    };
    Some((
        i.clone(),
        TbFields {
            at,
            data,
            elems,
            last,
            root,
            tail,
            factory: factory?,
        },
    ))
}

/// Truth of `TreeBuilder._tail` (`None`, `0` or `1`).
fn flag(o: &Object) -> Option<bool> {
    match o {
        Object::None => Some(false),
        Object::Int(i) => Some(*i != 0),
        Object::Bool(b) => Some(*b),
        _ => None,
    }
}

/// `TreeBuilder._flush(self)`. Takes the Python method for anything
/// unusual (a non-`str` piece of data, a last element that is not an
/// exact `Element`, a failing assertion).
fn tb_flush(st: &State, tb: &Rc<PyInstance>, f: &mut TbFields) -> Result<(), RuntimeError> {
    let n = &st.names;
    if read(&f.data, Vec::is_empty) != Some(false) {
        return Ok(());
    }
    let fallback = |f: &mut TbFields| -> Result<(), RuntimeError> {
        st.call_orig("TreeBuilder._flush", &[Object::Instance(tb.clone())], &[])?;
        // The Python method rebinds `_data`.
        if let Some(Object::List(l)) = get_field(tb, &n.data) {
            f.data = l;
        }
        Ok(())
    };
    if !matches!(f.last, Object::None) {
        let Some(last_i) = st.exact(ELEMENT, &f.last) else {
            return fallback(f);
        };
        let Some(tail) = flag(&f.tail) else {
            return fallback(f);
        };
        let text = read(&f.data, |d| {
            if d.len() == 1 {
                match &d[0] {
                    s @ Object::Str(_) => Some(s.clone()),
                    _ => None,
                }
            } else {
                let mut s = String::new();
                for piece in d.iter() {
                    match piece {
                        Object::Str(p) => s.push_str(p),
                        _ => return None,
                    }
                }
                Some(Object::from_str(s))
            }
        })
        .flatten();
        let Some(text) = text else {
            return fallback(f);
        };
        let field = if tail { &n.tail } else { &n.text };
        if !matches!(get_field(last_i, field), None | Some(Object::None)) {
            return fallback(f);
        }
        set_field(last_i, field, text);
    }
    // `self._data = []`: a list nothing else holds (the dictionary and
    // `f`) is emptied in place.
    if Rc::strong_count(&f.data) == 2 {
        let items = write(&f.data, std::mem::take);
        drop(items);
    } else {
        let Object::List(l) = new_list(Vec::new()) else {
            unreachable!("a new list");
        };
        f.data = l.clone();
        store_at(tb, f.at[TB_DATA], &n.data, Object::List(l));
    }
    Ok(())
}

/// `TreeBuilder.start(self, tag, attrs)` with `attrs` already a fresh
/// copy; `None` (having done nothing) when the Python method must run.
fn tb_start_owned(
    st: &State,
    this: &Object,
    tag: &Object,
    attrib: Object,
) -> Option<Result<Object, RuntimeError>> {
    let n = &st.names;
    let (tb, mut f) = builder(st, this)?;
    match &f.factory {
        Object::Type(t) if Rc::as_ptr(t) as usize == st.class_ptrs[ELEMENT] => {}
        _ => return None,
    }
    let cls = st.element_class()?;
    st.verified(ELEMENT, &cls).then_some(())?;
    Some((|| {
        tb_flush(st, &tb, &mut f)?;
        let elem = new_element(st, cls, tag.clone(), attrib);
        store_at(&tb, f.at[TB_LAST], &n.last, elem.clone());
        let parent = read(&f.elems, |v| v.last().cloned()).flatten();
        match parent {
            Some(p) => match children_of(st, &p) {
                Some(c) => append(&c, elem.clone()),
                None => {
                    let e = elem.clone();
                    with_interp(|i| {
                        let m = i.load_attr_public(&p, "append")?;
                        i.call_object(m, &[e], &[])
                    })?;
                }
            },
            None => {
                if matches!(f.root, Object::None) {
                    store_at(&tb, f.at[TB_ROOT], &n.root, elem.clone());
                }
            }
        }
        append(&f.elems, elem.clone());
        store_at(&tb, f.at[TB_TAIL], &n.tail_flag, Object::Int(0));
        Ok(elem)
    })())
}

/// `TreeBuilder.start(self, tag, attrs)`.
fn tb_start(st: &State, a: &[Object], kw: Kw) -> Option<Result<Object, RuntimeError>> {
    let [this, tag, attrs] = a else {
        return None;
    };
    if !kw.is_empty() {
        return None;
    }
    let attrib = copy_attrib(attrs)?;
    tb_start_owned(st, this, tag, attrib)
}

/// `TreeBuilder.end(self, tag)`.
fn tb_end(st: &State, a: &[Object], kw: Kw) -> Option<Result<Object, RuntimeError>> {
    let [this, tag] = a else {
        return None;
    };
    if !kw.is_empty() {
        return None;
    }
    let n = &st.names;
    let (tb, mut f) = builder(st, this)?;
    let last = read(&f.elems, |v| v.last().cloned())??;
    let last_i = st.exact(ELEMENT, &last)?;
    let last_tag = get_field(last_i, &n.tag).unwrap_or(Object::None);
    if native_eq(&last_tag, tag) != Some(true) {
        return None;
    }
    Some((|| {
        tb_flush(st, &tb, &mut f)?;
        // A flush that ran Python code may have moved the stack: the
        // Python method then finishes the job (its own flush is a no-op).
        let still = read(&f.elems, |v| v.last().is_some_and(|e| e.is_same(&last))) == Some(true);
        if !still {
            return st.call_orig("TreeBuilder.end", a, &[]);
        }
        let popped = write(&f.elems, Vec::pop);
        drop(popped);
        store_at(&tb, f.at[TB_LAST], &n.last, last.clone());
        store_at(&tb, f.at[TB_TAIL], &n.tail_flag, Object::Int(1));
        Ok(last)
    })())
}

/// `TreeBuilder.data(self, data)`.
fn tb_data(st: &State, a: &[Object], kw: Kw) -> Option<Result<Object, RuntimeError>> {
    let [this, data] = a else {
        return None;
    };
    if !kw.is_empty() {
        return None;
    }
    let tb = st.exact(BUILDER, this)?;
    let mut list = None;
    let plain = scan(st, BUILDER, tb, |j, _, v| {
        if j == TB_DATA {
            list = Some(v.clone());
        }
    });
    let (true, Some(Object::List(l))) = (plain, list) else {
        return None;
    };
    append(&l, data.clone());
    Some(Ok(Object::None))
}

/// An exact `XMLParser` whose methods no instance attribute shadows: its
/// `_names` memo and `target`.
fn parser_fields(st: &State, o: &Object) -> Option<(Dict, Object)> {
    let p = st.exact(PARSER, o)?;
    let mut memo = None;
    let mut target = None;
    let plain = scan(st, PARSER, p, |j, _, v| match j {
        XP_NAMES => memo = Some(v.clone()),
        XP_TARGET => target = Some(v.clone()),
        _ => {}
    });
    match (plain, memo, target) {
        (true, Some(Object::Dict(d)), Some(t)) => Some((d, t)),
        _ => None,
    }
}

/// `XMLParser._fixname(self, key)` for a `str` key; `None` when the memo
/// holds a key only Python code can compare.
fn fixname(memo: &Dict, key: &SharedStr) -> Option<Object> {
    if let Some(v) = dict_str_get(memo, key)? {
        return Some(v);
    }
    let name = if key.contains('}') {
        Object::from_str(format!("{{{}", &**key))
    } else {
        Object::Str(key.clone())
    };
    write(memo, |m| {
        m.insert(DictKey(Object::Str(key.clone())), name.clone())
    })?;
    Some(name)
}

/// `self.target.<method>(*args)`, natively for an exact `TreeBuilder`.
fn call_target(
    st: &State,
    target: &Object,
    method: &str,
    args: Vec<Object>,
) -> Result<Object, RuntimeError> {
    if st.exact(BUILDER, target).is_some() {
        let r = match (method, args.as_slice()) {
            ("start", [tag, attrib]) => tb_start_owned(st, target, tag, attrib.clone()),
            ("end", [tag]) => tb_end(st, &[target.clone(), tag.clone()], &[]),
            _ => None,
        };
        if let Some(r) = r {
            return r;
        }
    }
    with_interp(|i| {
        let f = i.load_attr_public(target, method)?;
        i.call_object(f, &args, &[])
    })
}

/// `XMLParser._start(self, tag, attr_list)`.
fn xp_start(st: &State, a: &[Object], kw: Kw) -> Option<Result<Object, RuntimeError>> {
    let [this, Object::Str(tag), Object::List(attr_list)] = a else {
        return None;
    };
    if !kw.is_empty() {
        return None;
    }
    let (memo, target) = parser_fields(st, this)?;
    let tag = fixname(&memo, tag)?;
    let attrib = read(attr_list, |attrs| {
        if attrs.len() % 2 != 0 {
            return None;
        }
        let mut attrib = DictData::default();
        for pair in attrs.chunks_exact(2) {
            let Object::Str(k) = &pair[0] else {
                return None;
            };
            attrib.insert(DictKey(fixname(&memo, k)?), pair[1].clone());
        }
        Some(attrib)
    })??;
    Some(call_target(
        st,
        &target,
        "start",
        vec![tag, new_dict(attrib)],
    ))
}

/// `XMLParser._end(self, tag)`.
fn xp_end(st: &State, a: &[Object], kw: Kw) -> Option<Result<Object, RuntimeError>> {
    let [this, Object::Str(tag)] = a else {
        return None;
    };
    if !kw.is_empty() {
        return None;
    }
    let (memo, target) = parser_fields(st, this)?;
    let tag = fixname(&memo, tag)?;
    Some(call_target(st, &target, "end", vec![tag]))
}

/// A `pyexpat` handler call (`handler(*args)`) when `handler` is one of
/// the natives `XMLParser.__init__` installs as handlers (`self._start`,
/// `self._end`, `target.data`): the native runs directly, without the
/// interpreter's call machinery. `None` for any other handler.
pub(crate) fn expat_dispatch(
    handler: &Object,
    args: &[Object],
) -> Option<Result<Object, RuntimeError>> {
    let Object::BoundMethod(bm) = handler else {
        return None;
    };
    let (Object::Builtin(b), Object::Instance(recv)) = (&bm.function, &bm.receiver) else {
        return None;
    };
    if crate::gil::free_threading_enabled() {
        return None;
    }
    let st = state_of_cls(class_of(recv))?;
    let ptr = Rc::as_ptr(b) as usize;
    let k = st.handlers.get()?.iter().position(|&h| h == ptr)?;
    let recv = bm.receiver.clone();
    let (two, three);
    let full: &[Object] = match args {
        [a] => {
            two = [recv, a.clone()];
            &two
        }
        [a, b] => {
            three = [recv, a.clone(), b.clone()];
            &three
        }
        _ => return None,
    };
    let native: Native = [xp_start, xp_end, tb_data][k];
    let key = ["XMLParser._start", "XMLParser._end", "TreeBuilder.data"][k];
    Some(match native(st, full, &[]) {
        Some(r) => r,
        None => st.call_orig(key, full, &[]),
    })
}

// ---------------------------------------------------------------------
// Parse sessions.

/// A parse whose expat events build the tree natively, as the C
/// accelerator's `XMLParser` does with a `TreeBuilder` target: `pyexpat`
/// begins one when the parser's handlers are the natives
/// `XMLParser.__init__` installs (`self._start`, `self._end`,
/// `target.data`) over an exact `XMLParser` and `TreeBuilder`, and feeds it
/// the events directly, without building handler arguments or calling
/// through the interpreter.
///
/// The `TreeBuilder`'s `_data` and `_elem` lists are updated in place; its
/// `_last`, `_tail` and `_root` live here until [`Session::finish`], which
/// `pyexpat` calls before any other handler (that is, any Python code)
/// runs and when the parse call returns. Names are resolved through the
/// parser's intern dictionary and `_names` memo, as the Python code
/// resolves them, then cached by their bytes. An event a session can't
/// serve exactly (say, text arriving for an element that already has
/// some) is declined before anything changes: `pyexpat` then ends the
/// session and dispatches the event as usual.
pub(crate) struct Session {
    ext: Ext,
    tb: Rc<PyInstance>,
    at: [usize; 5],
    memo: Dict,
    intern: Option<Dict>,
    names: crate::fasthash::FxHashMap<Box<[u8]>, Object>,
    data: List,
    elems: List,
    /// The children lists and tags of the elements in `_elem`, innermost
    /// last.
    stack: Vec<(List, Object)>,
    last: Object,
    tail: Object,
    root: Object,
    cls: Rc<TypeObject>,
}

type Ext = Rc<dyn std::any::Any + Send + Sync>;

/// The receiver of `handler` when it is the native handler `k` (see
/// `State::handlers`), with its class's state.
fn native_handler(handler: &Object, k: usize) -> Option<(Rc<PyInstance>, Ext)> {
    let Object::BoundMethod(bm) = handler else {
        return None;
    };
    let (Object::Builtin(b), Object::Instance(recv)) = (&bm.function, &bm.receiver) else {
        return None;
    };
    let ext = class_of(recv).native_ext.get()?.clone();
    let st = ext.downcast_ref::<State>()?;
    (st.handlers.get()?[k] == Rc::as_ptr(b) as usize).then(|| (recv.clone(), ext.clone()))
}

impl Session {
    /// A session for a parser whose handlers are `start`, `end` and
    /// `data` and whose intern dictionary is `intern`, when they are the
    /// natives over an exact `XMLParser` and `TreeBuilder` in the state
    /// their constructors leave.
    pub(crate) fn begin(
        start: &Object,
        end: &Object,
        data: &Object,
        intern: &Object,
    ) -> Option<Box<Session>> {
        if crate::gil::free_threading_enabled() {
            return None;
        }
        let (parser, ext) = native_handler(start, 0)?;
        let (end_parser, _) = native_handler(end, 1)?;
        if !Rc::ptr_eq(&parser, &end_parser) {
            return None;
        }
        let st = ext.downcast_ref::<State>()?;
        let (memo, target) = parser_fields(st, &Object::Instance(parser))?;
        let (tb, f) = builder(st, &target)?;
        match data {
            Object::None => {}
            _ => {
                let (data_tb, _) = native_handler(data, 2)?;
                if !Rc::ptr_eq(&data_tb, &tb) {
                    return None;
                }
            }
        }
        match &f.factory {
            Object::Type(t) if Rc::as_ptr(t) as usize == st.class_ptrs[ELEMENT] => {}
            _ => return None,
        }
        let cls = st.element_class()?;
        if !st.verified(ELEMENT, &cls) || flag(&f.tail).is_none() {
            return None;
        }
        let intern = match intern {
            Object::Dict(d) => Some(d.clone()),
            Object::None => None,
            _ => return None,
        };
        let elems = read(&f.elems, Clone::clone)?;
        let mut stack = Vec::with_capacity(elems.len() + 8);
        for e in &elems {
            let v = element_view(st, e)?;
            stack.push((v.children?, v.tag));
        }
        Some(Box::new(Session {
            ext,
            tb,
            at: f.at,
            memo,
            intern,
            names: Default::default(),
            data: f.data,
            elems: f.elems,
            stack,
            last: f.last,
            tail: f.tail,
            root: f.root,
            cls,
        }))
    }

    #[inline]
    fn st(&self) -> &State {
        self.ext
            .downcast_ref::<State>()
            .expect("a session holds its module's state")
    }

    /// The `_fixname` result for expat's name `raw` (interned through the
    /// parser's intern dictionary first, as `pyexpat` does); `None` when
    /// a dictionary holds a key only Python code can compare.
    fn name(&mut self, raw: &[u8]) -> Option<Object> {
        if let Some(v) = self.names.get(raw) {
            return Some(v.clone());
        }
        let text = String::from_utf8_lossy(raw);
        let interned = match &self.intern {
            Some(d) => {
                let key = SharedStr::from(&*text);
                match dict_str_get(d, &key)? {
                    Some(Object::Str(s)) => s,
                    Some(_) => return None,
                    None => {
                        let o = Object::Str(key.clone());
                        write(d, |m| m.insert(DictKey(o.clone()), o))?;
                        key
                    }
                }
            }
            None => SharedStr::from(&*text),
        };
        let fixed = fixname(&self.memo, &interned)?;
        self.names.insert(raw.into(), fixed.clone());
        Some(fixed)
    }

    /// Whether [`Self::flush`] can run natively.
    fn can_flush(&self) -> bool {
        let st = self.st();
        let Some(pending) = read(&self.data, |d| {
            d.iter()
                .all(|p| matches!(p, Object::Str(_)))
                .then_some(!d.is_empty())
        })
        .flatten() else {
            return false;
        };
        if !pending || matches!(self.last, Object::None) {
            return true;
        }
        let Some(last) = st.exact(ELEMENT, &self.last) else {
            return false;
        };
        let field = if flag(&self.tail) == Some(true) {
            &st.names.tail
        } else {
            &st.names.text
        };
        matches!(get_field(last, field), None | Some(Object::None))
    }

    /// `TreeBuilder._flush`, once [`Self::can_flush`] said it can.
    fn flush(&mut self) {
        let Some(text) = read(&self.data, |d| match d.len() {
            0 => None,
            1 => Some(d[0].clone()),
            _ => {
                let mut s = String::new();
                for piece in d {
                    if let Object::Str(p) = piece {
                        s.push_str(p);
                    }
                }
                Some(Object::from_str(s))
            }
        })
        .flatten() else {
            return;
        };
        let st = self.st();
        if let Some(last) = st.exact(ELEMENT, &self.last) {
            let field = if flag(&self.tail) == Some(true) {
                &st.names.tail
            } else {
                &st.names.text
            };
            set_field(last, field, text);
        }
        if Rc::strong_count(&self.data) == 2 {
            let items = write(&self.data, std::mem::take);
            drop(items);
        } else {
            let Object::List(l) = new_list(Vec::new()) else {
                unreachable!("a new list");
            };
            store_at(
                &self.tb,
                self.at[TB_DATA],
                &st.names.data,
                Object::List(l.clone()),
            );
            self.data = l;
        }
    }

    /// A start tag: `XMLParser._start` and `TreeBuilder.start`. `None`
    /// declines (nothing changed).
    pub(crate) fn start(
        &mut self,
        name: &[u8],
        atts: &mut dyn Iterator<Item = (&[u8], Object)>,
    ) -> Option<()> {
        let tag = self.name(name)?;
        let mut attrib = DictData::default();
        for (k, v) in atts {
            attrib.insert(DictKey(self.name(k)?), v);
        }
        if !self.can_flush() {
            return None;
        }
        self.flush();
        let st = self.st();
        let n = &st.names;
        let inst = PyInstance::new_deferred(self.cls.clone());
        let Object::List(children) = new_list(Vec::new()) else {
            unreachable!("a new list");
        };
        set_field(&inst, &n.tag, tag.clone());
        set_field(&inst, &n.attrib, new_dict(attrib));
        set_field(&inst, &n.children, Object::List(children.clone()));
        let elem = Object::Instance(inst);
        match self.stack.last() {
            Some((parent, _)) => append(parent, elem.clone()),
            None => {
                if matches!(self.root, Object::None) {
                    self.root = elem.clone();
                }
            }
        }
        append(&self.elems, elem.clone());
        self.stack.push((children, tag));
        self.last = elem;
        self.tail = Object::Int(0);
        Some(())
    }

    /// An end tag: `XMLParser._end` and `TreeBuilder.end`. `None`
    /// declines (nothing changed but name resolution).
    pub(crate) fn end(&mut self, name: &[u8]) -> Option<()> {
        let tag = self.name(name)?;
        let (_, top) = self.stack.last()?;
        if native_eq(top, &tag) != Some(true) || !self.can_flush() {
            return None;
        }
        self.flush();
        let elem = write(&self.elems, Vec::pop)??;
        self.stack.pop();
        self.last = elem;
        self.tail = Object::Int(1);
        Some(())
    }

    /// Character data: `TreeBuilder.data`.
    pub(crate) fn data(&mut self, text: Object) {
        append(&self.data, text);
    }

    /// Store the builder fields the session kept.
    pub(crate) fn finish(self) {
        let st = self.st();
        let n = &st.names;
        store_at(&self.tb, self.at[TB_LAST], &n.last, self.last.clone());
        store_at(&self.tb, self.at[TB_TAIL], &n.tail_flag, self.tail.clone());
        store_at(&self.tb, self.at[TB_ROOT], &n.root, self.root.clone());
    }
}

// ---------------------------------------------------------------------
// Installation.

type Native = fn(&State, &[Object], Kw) -> Option<Result<Object, RuntimeError>>;

/// One native method: the class, its name there, the native, and
/// whether it takes keyword arguments.
struct Spec {
    cls: usize,
    name: &'static str,
    key: &'static str,
    native: Native,
    keywords: bool,
}

/// The natives that never run Python code when they serve a call (they
/// decline instead), registered as leaf fast halves (see
/// `leaf_builtins::register_fast`) under their index here.
const LEAF: [Native; 15] = [
    el_makeelement,
    el_append,
    el_extend,
    el_insert,
    el_len,
    el_getitem,
    el_get,
    el_set,
    el_iter,
    el_itertext,
    el_find,
    el_findall,
    el_findtext,
    ser_namespaces,
    sub_element,
];

/// A leaf half: [`LEAF`]`[K]` with the state of its first argument's
/// class (the receiver, `SubElement`'s parent, or the element
/// `_namespaces` walks).
fn leaf_call<const K: usize>(a: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let Some(Object::Instance(i)) = a.first() else {
        return None;
    };
    let st = state_of_cls(class_of(i))?;
    LEAF[K](st, a, &[])
}

/// The leaf half for native `f`, if it has one.
fn leaf_for(f: Native) -> Option<crate::leaf_builtins::Fast> {
    const HALVES: [crate::leaf_builtins::Fast; 15] = [
        leaf_call::<0>,
        leaf_call::<1>,
        leaf_call::<2>,
        leaf_call::<3>,
        leaf_call::<4>,
        leaf_call::<5>,
        leaf_call::<6>,
        leaf_call::<7>,
        leaf_call::<8>,
        leaf_call::<9>,
        leaf_call::<10>,
        leaf_call::<11>,
        leaf_call::<12>,
        leaf_call::<13>,
        leaf_call::<14>,
    ];
    let k = LEAF.iter().position(|g| std::ptr::fn_addr_eq(*g, f))?;
    Some(HALVES[k])
}

const SPECS: &[Spec] = &[
    Spec {
        cls: ELEMENT,
        name: "__init__",
        key: "Element.__init__",
        native: el_init,
        keywords: true,
    },
    Spec {
        cls: ELEMENT,
        name: "makeelement",
        key: "Element.makeelement",
        native: el_makeelement,
        keywords: true,
    },
    Spec {
        cls: ELEMENT,
        name: "append",
        key: "Element.append",
        native: el_append,
        keywords: true,
    },
    Spec {
        cls: ELEMENT,
        name: "extend",
        key: "Element.extend",
        native: el_extend,
        keywords: true,
    },
    Spec {
        cls: ELEMENT,
        name: "insert",
        key: "Element.insert",
        native: el_insert,
        keywords: true,
    },
    Spec {
        cls: ELEMENT,
        name: "__len__",
        key: "Element.__len__",
        native: el_len,
        keywords: false,
    },
    Spec {
        cls: ELEMENT,
        name: "__getitem__",
        key: "Element.__getitem__",
        native: el_getitem,
        keywords: false,
    },
    Spec {
        cls: ELEMENT,
        name: "get",
        key: "Element.get",
        native: el_get,
        keywords: true,
    },
    Spec {
        cls: ELEMENT,
        name: "set",
        key: "Element.set",
        native: el_set,
        keywords: true,
    },
    Spec {
        cls: ELEMENT,
        name: "iter",
        key: "Element.iter",
        native: el_iter,
        keywords: true,
    },
    Spec {
        cls: ELEMENT,
        name: "itertext",
        key: "Element.itertext",
        native: el_itertext,
        keywords: true,
    },
    Spec {
        cls: ELEMENT,
        name: "find",
        key: "Element.find",
        native: el_find,
        keywords: true,
    },
    Spec {
        cls: ELEMENT,
        name: "findall",
        key: "Element.findall",
        native: el_findall,
        keywords: true,
    },
    Spec {
        cls: ELEMENT,
        name: "findtext",
        key: "Element.findtext",
        native: el_findtext,
        keywords: true,
    },
    Spec {
        cls: BUILDER,
        name: "start",
        key: "TreeBuilder.start",
        native: tb_start,
        keywords: true,
    },
    Spec {
        cls: BUILDER,
        name: "end",
        key: "TreeBuilder.end",
        native: tb_end,
        keywords: true,
    },
    Spec {
        cls: BUILDER,
        name: "data",
        key: "TreeBuilder.data",
        native: tb_data,
        keywords: true,
    },
    Spec {
        cls: PARSER,
        name: "_start",
        key: "XMLParser._start",
        native: xp_start,
        keywords: true,
    },
    Spec {
        cls: PARSER,
        name: "_end",
        key: "XMLParser._end",
        native: xp_end,
        keywords: true,
    },
];

/// Module-level functions: the global name and the native.
const FUNCS: &[(&str, Native)] = &[
    ("SubElement", sub_element),
    ("_namespaces", ser_namespaces),
    ("_serialize_xml", ser_xml),
];

fn native_fn(
    st: &Rc<State>,
    name: &'static str,
    key: &'static str,
    native: Native,
    binds: bool,
    keywords: bool,
) -> Object {
    let st1 = st.clone();
    let call = Box::new(move |a: &[Object]| match native(&st1, a, &[]) {
        Some(r) => r,
        None => st1.call_orig(key, a, &[]),
    });
    let call_kw: Option<Box<dyn Fn(&[Object], Kw) -> Result<Object, RuntimeError> + Send + Sync>> =
        if keywords {
            let st2 = st.clone();
            Some(Box::new(move |a: &[Object], kw: Kw| {
                match native(&st2, a, kw) {
                    Some(r) => r,
                    None => st2.call_orig(key, a, kw),
                }
            }))
        } else {
            None
        };
    let b = Rc::new(BuiltinFn {
        name,
        binds_instance: binds,
        call,
        call_kw,
    });
    if let Some(fast) = leaf_for(native) {
        crate::leaf_builtins::register_fast(&b, fast);
    }
    Object::Builtin(b)
}

/// `install(namespace)`: give the `ElementTree` module whose globals are
/// `namespace` its native bodies (see the module docs). Idempotent per
/// module; returns `None`.
fn install(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Dict(ns)] = args else {
        return Err(type_error("install() expects the module namespace"));
    };
    // The natives read classes and fields without borrow bookkeeping,
    // which needs the GIL.
    if crate::gil::free_threading_enabled() {
        return Ok(Object::None);
    }
    let get = |name: &str| ns.borrow().get(&StrKey(name)).cloned();
    let class = |name: &str| match get(name) {
        Some(Object::Type(t)) => Ok(t),
        _ => Err(type_error(format!("install(): {name} is not a class"))),
    };
    let classes = [
        class("Element")?,
        class("TreeBuilder")?,
        class("XMLParser")?,
    ];
    if classes[ELEMENT].native_ext.get().is_some() {
        return Ok(Object::None);
    }
    let path_ns = match get("ElementPath") {
        Some(Object::Module(m)) => m.dict.clone(),
        _ => return Err(type_error("install(): ElementPath missing")),
    };
    let path_funcs = ["find", "findall", "findtext", "iterfind"].map(|n| {
        path_ns
            .borrow()
            .get(&StrKey(n))
            .cloned()
            .unwrap_or(Object::None)
    });
    let mut orig = HashMap::new();
    for spec in SPECS {
        let v = classes[spec.cls]
            .dict
            .borrow()
            .get(&StrKey(spec.name))
            .cloned()
            .ok_or_else(|| type_error(format!("install(): no {}", spec.key)))?;
        orig.insert(spec.key, v);
    }
    orig.insert(
        "TreeBuilder._flush",
        classes[BUILDER]
            .dict
            .borrow()
            .get(&StrKey("_flush"))
            .cloned()
            .ok_or_else(|| type_error("install(): no TreeBuilder._flush"))?,
    );
    for (name, _) in FUNCS {
        let v = get(name).ok_or_else(|| type_error(format!("install(): no {name}")))?;
        orig.insert(name, v);
    }
    let state = Rc::new(State {
        classes: [
            Rc::downgrade(&classes[0]),
            Rc::downgrade(&classes[1]),
            Rc::downgrade(&classes[2]),
        ],
        class_ptrs: [
            Rc::as_ptr(&classes[0]) as usize,
            Rc::as_ptr(&classes[1]) as usize,
            Rc::as_ptr(&classes[2]) as usize,
        ],
        ns: Rc::downgrade(ns),
        path_ns: Rc::downgrade(&path_ns),
        names: Names::new(),
        orig,
        sealed: Default::default(),
        snapshot: std::sync::OnceLock::new(),
        path_funcs,
        handlers: std::sync::OnceLock::new(),
        keymaps: Default::default(),
    });
    let mut handlers = [0; 3];
    for spec in SPECS {
        let f = native_fn(
            &state,
            spec.name,
            spec.key,
            spec.native,
            true,
            spec.keywords,
        );
        if let (Some(k), Object::Builtin(b)) = (
            ["XMLParser._start", "XMLParser._end", "TreeBuilder.data"]
                .iter()
                .position(|key| *key == spec.key),
            &f,
        ) {
            handlers[k] = Rc::as_ptr(b) as usize;
        }
        classes[spec.cls]
            .dict
            .borrow_mut()
            .insert(DictKey(Object::Str(interned(spec.name))), f);
    }
    for (name, native) in FUNCS {
        let f = native_fn(&state, name, name, *native, false, true);
        // `__module__` as for the functions replaced (`__all__` checks and
        // pickling by reference look at it).
        crate::descr_registry::register_module(&f, "xml.etree.ElementTree");
        if *name == "_serialize_xml" {
            if let Some(Object::Dict(table)) = get("_serialize") {
                table
                    .borrow_mut()
                    .insert(DictKey(Object::from_static("xml")), f.clone());
            }
        }
        ns.borrow_mut()
            .insert(DictKey(Object::Str(interned(name))), f);
    }
    let _ = state.handlers.set(handlers);
    for cls in &classes {
        cls.bump_attr_version();
    }
    // What each class dictionary holds now (see `State::verified`).
    let _ = state.snapshot.set(std::array::from_fn(|k| {
        classes[k]
            .dict
            .borrow()
            .iter()
            .map(|(key, v)| (key.clone(), v.clone()))
            .collect()
    }));
    for cls in &classes {
        let _ = cls
            .native_ext
            .set(crate::rc_unsize!(state.clone() => dyn std::any::Any + Send + Sync));
    }
    // `__getitem__` is a native method now, which the VM's sequence
    // protocol takes for a mapping-only subscript unless the class says
    // otherwise (its subclasses iterate through it).
    classes[ELEMENT].c_sq_item.set(true);
    Ok(Object::None)
}

pub(crate) fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let mut dict = DictData::default();
    dict.insert(
        DictKey(Object::from_static("__name__")),
        Object::from_static("_weave_elementtree"),
    );
    dict.insert(
        DictKey(Object::from_static("install")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "install",
            binds_instance: false,
            call: Box::new(install),
            call_kw: None,
        })),
    );
    Rc::new(PyModule {
        name: "_weave_elementtree".to_owned(),
        filename: None,
        dict: Rc::new(RefCell::new(dict)),
    })
}
