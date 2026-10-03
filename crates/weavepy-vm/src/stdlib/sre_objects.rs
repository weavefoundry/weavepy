//! Native method bodies for `re.Pattern`, `re.Match` and the scanner.
//!
//! The classes are Python classes in the frozen `re._engine` module, with
//! `__slots__` storage, so their reprs, attributes, pickling and the rarer
//! paths stay in Python. `_sre.install` (called at the end of the module
//! body) saves the Python methods it replaces, puts native bodies in the
//! class dicts, and hangs this module's [`State`] on each class. Every
//! pattern, match and scanner is built natively over a shared slot layout
//! (see `SlotStorage::from_layout`), so a native reads all of an instance's
//! fields after one layout check.
//!
//! A native serves the common shapes: an exact `str` or `bytes` subject of
//! the pattern's type, plain `int` positions and counts, `int` or `str`
//! group names, and `str`/`bytes` replacement templates. Those fast halves
//! run no Python code, so they are registered as leaf entries the dispatch
//! loop calls inline. A callable replacement, or a template not parsed
//! yet, takes the native full body, which calls into the interpreter.
//! Anything else (other subject types, `__index__` positions, unexpected
//! keywords, and every error message the Python code owns) calls the
//! Python method the native replaced, so behavior is unchanged.
//!
//! The instances are never cycle-collector tracked, like `datetime`'s
//! natively built objects: a match holds its pattern and subject, a
//! pattern holds strings, ints and its name tables, and none of them can
//! reach back to a match.

use std::collections::HashMap;

use crate::error::{index_error, stop_iteration, type_error, RuntimeError};
use crate::object::{BuiltinFn, DictData, DictKey, Object, StrKey};
use crate::shared_value::{SharedSlice, SharedStr};
use crate::sync::{Rc, RefCell, Weak};
use crate::types::{PyInstance, SlotStorage, TypeObject};

use super::sre_mod::{self, CompiledCode, Found};

// Slot order of the natively built instances (the `__slots__` order the
// classes declare).
const PATTERN_SLOTS: [&str; 6] = [
    "_code",
    "pattern",
    "flags",
    "groups",
    "groupindex",
    "_indexgroup",
];
const P_CODE: usize = 0;
const P_GROUPINDEX: usize = 4;

const MATCH_SLOTS: [&str; 6] = ["re", "string", "pos", "endpos", "_marks", "_lastindex"];
const M_RE: usize = 0;
const M_STRING: usize = 1;
const M_MARKS: usize = 4;

const SCANNER_SLOTS: [&str; 6] = [
    "pattern",
    "_string",
    "_start",
    "_pos",
    "_endpos",
    "_must_advance",
];
const S_PATTERN: usize = 0;
const S_STRING: usize = 1;
const S_START: usize = 2;
const S_POS: usize = 3;
const S_ENDPOS: usize = 4;
const S_MUST: usize = 5;

/// A parsed replacement template: literals and group references.
enum Piece {
    Lit(Object),
    Group(usize),
}

/// One interpreter's `re` classes (held weakly: the natives live in their
/// dicts), the instance layouts, and the Python methods the natives
/// replaced.
pub(crate) struct State {
    match_cls: Weak<TypeObject>,
    scanner_cls: Weak<TypeObject>,
    pattern_layout: SharedSlice<DictKey>,
    match_layout: SharedSlice<DictKey>,
    scanner_layout: SharedSlice<DictKey>,
    /// `"Pattern.search"` and so on: the replaced Python method.
    orig: HashMap<&'static str, Object>,
    /// `re._engine._compile_template(pattern, repl)`.
    compile_template: Object,
    /// Per pattern handle, the last replacement template it expanded.
    templates: parking_lot::Mutex<HashMap<i64, (Object, Rc<Vec<Piece>>)>>,
}

#[inline]
fn state_of(cls: &TypeObject) -> Option<&State> {
    cls.native_ext.get()?.downcast_ref::<State>()
}

type Ext = Rc<dyn std::any::Any + Send + Sync>;

/// The state hanging on `inst`'s class, held without borrowing the class
/// cell (a callable replacement may run any Python code).
#[inline]
fn class_ext(inst: &PyInstance) -> Option<Ext> {
    inst.class.borrow().native_ext.get().cloned()
}

/// Run `f` with the state on `inst`'s class, read in place. `f` must run
/// no Python code (nothing can then reassign `__class__` and free the
/// class while it runs).
#[inline]
fn with_state<R>(inst: &PyInstance, f: impl FnOnce(&State) -> R) -> Option<R> {
    if crate::gil::free_threading_enabled() {
        let ext = class_ext(inst)?;
        return Some(f(ext.downcast_ref::<State>()?));
    }
    Some(f(state_of(inst.cls_raw())?))
}

fn with_interp<R>(
    f: impl FnOnce(&mut crate::Interpreter) -> Result<R, RuntimeError>,
) -> Result<R, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| type_error("re: no active interpreter"))?;
    // SAFETY: published by the enclosing VM frame on this thread.
    f(unsafe { &mut *ptr })
}

fn build(cls: Rc<TypeObject>, layout: &SharedSlice<DictKey>, values: Vec<Object>) -> Object {
    let mut i = PyInstance::new(cls);
    i.slots = RefCell::new(SlotStorage::from_layout(layout.clone(), values));
    Object::Instance(Rc::new(i))
}

// ---------------------------------------------------------------------
// Subjects.

/// Whether `o` is a subject the fast paths serve for `cc`: an exact `str`
/// or `bytes` of the type the pattern accepts.
#[inline]
fn fast_subject(o: &Object, cc: &CompiledCode) -> bool {
    match o {
        Object::Str(_) => cc.is_str != Some(false),
        Object::Bytes(_) => cc.is_str != Some(true),
        _ => false,
    }
}

fn empty_like(o: &Object) -> Object {
    match o {
        Object::Bytes(_) => Object::new_bytes(Vec::new()),
        _ => Object::Str(SharedStr::from_ascii("")),
    }
}

/// `s[a:b]` in code points, or `None` when the span is out of range.
fn str_slice(s: &SharedStr, a: usize, b: usize) -> Option<Object> {
    if b <= a {
        return (a <= SharedStr::char_count(s)).then(|| Object::Str(SharedStr::from_ascii("")));
    }
    if SharedStr::is_ascii(s) {
        let bytes = s.as_bytes();
        if b > bytes.len() {
            return None;
        }
        if b - a == 1 {
            return Some(Object::from_char(char::from(bytes[a])));
        }
        return Some(Object::Str(SharedStr::from_ascii(&s[a..b])));
    }
    let (mut ba, mut bb) = (None, None);
    for (n, (i, _)) in s.char_indices().enumerate() {
        if n == a {
            ba = Some(i);
        }
        if n == b {
            bb = Some(i);
            break;
        }
    }
    let ba = ba?;
    let bb = match bb {
        Some(i) => i,
        None if b == SharedStr::char_count(s) => s.len(),
        None => return None,
    };
    Some(Object::Str(SharedStr::with_count(&s[ba..bb], b - a)))
}

/// The subject's `[a:b]` slice (CPython's `getslice`), for an exact `str`
/// or `bytes`; `None` for any other subject or an out-of-range span.
fn subject_slice(o: &Object, a: usize, b: usize) -> Option<Object> {
    match o {
        Object::Str(s) => str_slice(s, a, b),
        Object::Bytes(bs) => {
            if b > bs.len() || a > b {
                return None;
            }
            if a == b {
                return Some(Object::new_bytes(Vec::new()));
            }
            Some(Object::Bytes(SharedSlice::from(&bs[a..b])))
        }
        _ => None,
    }
}

/// The code units a subject is matched over: the bytes of a `bytes` or
/// of an ASCII `str` (borrowed in place), or the code points of any other
/// subject.
enum Units<'a> {
    Narrow(&'a [u8]),
    Wide(Rc<Vec<u32>>),
}

/// A subject ready for matching, with cheap slicing for `str`/`bytes`.
struct Subject<'a> {
    obj: &'a Object,
    units: Units<'a>,
}

impl<'a> Subject<'a> {
    fn new(obj: &'a Object) -> Result<Self, RuntimeError> {
        let units = match obj {
            Object::Str(s) if SharedStr::is_ascii(s) => Units::Narrow(s.as_bytes()),
            Object::Bytes(b) => Units::Narrow(&b[..]),
            _ => Units::Wide(sre_mod::decode_subject_cached(obj)?),
        };
        Ok(Self { obj, units })
    }

    fn len(&self) -> usize {
        match &self.units {
            Units::Narrow(b) => b.len(),
            Units::Wide(w) => w.len(),
        }
    }

    /// One match attempt (see [`sre_mod::exec_found`]).
    fn exec<R>(
        &self,
        cc: &CompiledCode,
        pos: usize,
        endpos: usize,
        mode: i64,
        must_advance: bool,
        f: impl FnOnce(&Found<'_>) -> R,
    ) -> Result<Option<R>, RuntimeError> {
        match &self.units {
            Units::Narrow(b) => sre_mod::exec_found(cc, b, pos, endpos, mode, must_advance, f),
            Units::Wide(w) => sre_mod::exec_found(cc, &w[..], pos, endpos, mode, must_advance, f),
        }
    }

    /// `[a:b]` as a new object (the subject itself for the whole range).
    fn slice(&self, a: usize, b: usize) -> Object {
        let (a, b) = (a.min(self.len()), b.min(self.len()));
        if a == 0 && b == self.len() {
            return self.obj.clone();
        }
        match (self.obj, &self.units) {
            (Object::Str(_), _) if b <= a => Object::Str(SharedStr::from_ascii("")),
            (Object::Str(s), Units::Narrow(_)) => {
                if b - a == 1 {
                    Object::from_char(char::from(s.as_bytes()[a]))
                } else {
                    Object::Str(SharedStr::from_ascii(&s[a..b]))
                }
            }
            (Object::Str(_), Units::Wide(w)) => {
                let mut t = String::with_capacity(b - a);
                push_cps(&mut t, &w[a..b]);
                Object::Str(SharedStr::with_count(&t, b - a))
            }
            (Object::Bytes(_), _) if b <= a => Object::new_bytes(Vec::new()),
            (Object::Bytes(bs), _) => Object::Bytes(SharedSlice::from(&bs[a..b])),
            _ => Object::None,
        }
    }

    /// Group `g`'s text from `marks`, or `default` when it did not match.
    fn group(&self, found: &Found<'_>, g: usize, default: &Object) -> Object {
        let (a, b) = found.span(g);
        if a < 0 || b < 0 {
            default.clone()
        } else {
            self.slice(a as usize, b as usize)
        }
    }
}

fn push_cps(t: &mut String, cps: &[u32]) {
    for &c in cps {
        t.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
    }
}

/// An output buffer for `sub`: text for a `str` subject, bytes for a
/// `bytes` one, or a list of parts to `join` through the interpreter once
/// a replacement of another type turns up.
enum Sink {
    Str(String),
    Bytes(Vec<u8>),
    Parts(Vec<Object>),
}

impl Sink {
    fn new(subject: &Subject<'_>) -> Self {
        match subject.obj {
            Object::Bytes(_) => Sink::Bytes(Vec::new()),
            _ => Sink::Str(String::new()),
        }
    }

    fn push_span(&mut self, subject: &Subject<'_>, a: usize, b: usize) {
        if b <= a {
            return;
        }
        match (self, subject.obj, &subject.units) {
            (Sink::Str(t), Object::Str(s), Units::Narrow(_)) => t.push_str(&s[a..b]),
            (Sink::Str(t), Object::Str(_), Units::Wide(w)) => push_cps(t, &w[a..b]),
            (Sink::Bytes(v), Object::Bytes(bs), _) => v.extend_from_slice(&bs[a..b]),
            (Sink::Parts(parts), _, _) => parts.push(subject.slice(a, b)),
            _ => {}
        }
    }

    fn push_obj(&mut self, o: &Object) {
        match (&mut *self, o) {
            (Sink::Str(t), Object::Str(s)) => t.push_str(s),
            (Sink::Bytes(v), Object::Bytes(b)) => v.extend_from_slice(b),
            (Sink::Parts(parts), _) => parts.push(o.clone()),
            _ => {
                let done = std::mem::replace(self, Sink::Parts(Vec::new())).finish_plain();
                if let Sink::Parts(parts) = self {
                    parts.push(done);
                    parts.push(o.clone());
                }
            }
        }
    }

    fn finish_plain(self) -> Object {
        match self {
            Sink::Str(t) => Object::Str(SharedStr::from(t.as_str())),
            Sink::Bytes(v) => Object::new_bytes(v),
            Sink::Parts(_) => Object::None,
        }
    }

    fn finish(self, empty: &Object) -> Result<Object, RuntimeError> {
        match self {
            Sink::Parts(parts) => with_interp(|interp| {
                let join = interp.load_attr_public(empty, "join")?;
                interp.call_object(join, &[Object::new_list(parts)], &[])
            }),
            plain => Ok(plain.finish_plain()),
        }
    }
}

// ---------------------------------------------------------------------
// Receivers and arguments.

/// The pattern's compiled code and handle.
fn pattern_code(st: &State, inst: &PyInstance) -> Option<(&'static CompiledCode, i64)> {
    let slots = inst.slots.borrow();
    let v = slots.values_for_layout(&st.pattern_layout)?;
    let Object::Int(h) = v[P_CODE] else {
        return None;
    };
    Some((sre_mod::compiled(h)?, h))
}

/// A position argument: a plain `int` (others take the Python path).
#[inline]
fn pos_arg(o: Option<&Object>, default: i64) -> Option<i64> {
    match o {
        None => Some(default),
        Some(Object::Int(i)) => Some(*i),
        Some(Object::Bool(b)) => Some(i64::from(*b)),
        _ => None,
    }
}

#[inline]
fn clamp(pos: i64, endpos: i64, len: usize) -> (usize, usize) {
    let n = len as i64;
    (pos.clamp(0, n) as usize, endpos.clamp(0, n) as usize)
}

/// Lay keyword arguments out positionally after `args` (receiver first)
/// following `names`, when they fill the next positions exactly.
fn merge_kwargs(args: &[Object], kw: &[(String, Object)], names: &[&str]) -> Option<Vec<Object>> {
    let given = args.len().checked_sub(1)?;
    if given + kw.len() > names.len() {
        return None;
    }
    let mut out = args.to_vec();
    for name in &names[given..given + kw.len()] {
        let (_, v) = kw.iter().find(|(k, _)| k == name)?;
        out.push(v.clone());
    }
    Some(out)
}

/// The flat spans tuple a match keeps (`_marks`), built in one
/// fixed-size allocation for the common group counts.
fn marks_tuple(found: &Found<'_>) -> Object {
    use crate::tuple_storage::TupleStorage;
    let n = 2 * (found.groups() + 1);
    macro_rules! sized {
        ($($k:literal),*) => {
            match n {
                $($k => TupleStorage::from_array::<$k>(std::array::from_fn(|i| {
                    Object::Int(found.mark(i))
                })),)*
                _ => TupleStorage::from_exact_iter((0..n).map(|i| Object::Int(found.mark(i)))),
            }
        };
    }
    Object::Tuple(sized!(2, 4, 6, 8, 10, 12))
}

fn new_match(
    st: &State,
    pattern: &Object,
    string: &Object,
    pos: usize,
    endpos: usize,
    found: &Found<'_>,
) -> Result<Object, RuntimeError> {
    let cls = st
        .match_cls
        .upgrade()
        .ok_or_else(|| type_error("re.Match is gone"))?;
    let values = vec![
        pattern.clone(),
        string.clone(),
        Object::Int(pos as i64),
        Object::Int(endpos as i64),
        marks_tuple(found),
        Object::Int(found.lastindex as i64),
    ];
    Ok(build(cls, &st.match_layout, values))
}

// ---------------------------------------------------------------------
// Pattern methods.

/// The receiver, subject and position arguments of `match`/`search`/
/// `fullmatch`/`findall`/`finditer`/`scanner`, when the fast path serves
/// them.
struct ExecArgs<'a> {
    recv: &'a Object,
    string: &'a Object,
    pos: i64,
    endpos: i64,
}

fn exec_args(args: &[Object]) -> Option<ExecArgs<'_>> {
    let [recv, string, rest @ ..] = args else {
        return None;
    };
    if rest.len() > 2 {
        return None;
    }
    Some(ExecArgs {
        recv,
        string,
        pos: pos_arg(rest.first(), 0)?,
        endpos: pos_arg(rest.get(1), i64::MAX)?,
    })
}

/// Run `f` with the pattern's state and code, when the receiver is a
/// natively built pattern and the subject a fast one. `f` runs no Python
/// code, so the state is read through the class in place.
fn with_pattern<R>(
    recv: &Object,
    string: &Object,
    f: impl FnOnce(&State, &'static CompiledCode, i64) -> R,
) -> Option<R> {
    let Object::Instance(inst) = recv else {
        return None;
    };
    with_state(inst, |st| {
        let (cc, handle) = pattern_code(st, inst)?;
        if !fast_subject(string, cc) {
            return None;
        }
        Some(f(st, cc, handle))
    })?
}

/// [`with_pattern`] for a body that may run Python code: the state is
/// held by its own reference, not through the class.
fn with_pattern_held<R>(
    recv: &Object,
    string: &Object,
    f: impl FnOnce(&State, &'static CompiledCode, i64) -> R,
) -> Option<R> {
    let Object::Instance(inst) = recv else {
        return None;
    };
    let ext = class_ext(inst)?;
    let st = ext.downcast_ref::<State>()?;
    let (cc, handle) = pattern_code(st, inst)?;
    if !fast_subject(string, cc) {
        return None;
    }
    Some(f(st, cc, handle))
}

fn pattern_exec(args: &[Object], mode: i64) -> Option<Result<Object, RuntimeError>> {
    let a = exec_args(args)?;
    with_pattern(a.recv, a.string, |st, cc, _| {
        let subject = Subject::new(a.string)?;
        let (p, e) = clamp(a.pos, a.endpos, subject.len());
        subject
            .exec(cc, p, e, mode, false, |f| {
                new_match(st, a.recv, a.string, p, e, f)
            })?
            .unwrap_or(Ok(Object::None))
    })
}

fn pattern_match(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    pattern_exec(args, 1)
}

fn pattern_fullmatch(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    pattern_exec(args, 2)
}

fn pattern_search(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    pattern_exec(args, 0)
}

fn pattern_findall(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let a = exec_args(args)?;
    with_pattern(a.recv, a.string, |_, cc, _| {
        let subject = Subject::new(a.string)?;
        let (mut p, e) = clamp(a.pos, a.endpos, subject.len());
        let empty = empty_like(a.string);
        let mut out = Vec::with_capacity(8);
        let mut must_advance = false;
        while p <= e {
            let Some((s, en)) = subject.exec(cc, p, e, 0, must_advance, |f| {
                out.push(match cc.groups {
                    0 => subject.slice(f.start, f.end),
                    1 => subject.group(f, 1, &empty),
                    n => Object::new_tuple((1..=n).map(|g| subject.group(f, g, &empty)).collect()),
                });
                (f.start, f.end)
            })?
            else {
                break;
            };
            must_advance = s == en;
            p = en;
        }
        Ok(Object::new_list(out))
    })
}

/// `finditer` and `scanner`: a scanner over a fast subject.
fn pattern_scanner(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let a = exec_args(args)?;
    with_pattern(a.recv, a.string, |st, _, _| {
        let len = match a.string {
            Object::Str(s) => SharedStr::char_count(s),
            Object::Bytes(b) => b.len(),
            _ => 0,
        };
        let (p, e) = clamp(a.pos, a.endpos, len);
        let cls = st
            .scanner_cls
            .upgrade()
            .ok_or_else(|| type_error("re scanner type is gone"))?;
        Ok(build(
            cls,
            &st.scanner_layout,
            vec![
                a.recv.clone(),
                a.string.clone(),
                Object::Int(p as i64),
                Object::Int(p as i64),
                Object::Int(e as i64),
                Object::Bool(false),
            ],
        ))
    })
}

/// Subjects up to this many code units have their matches found when
/// `finditer` is called.
const EAGER_FINDITER: usize = 512;

/// `finditer`: over a short subject, an iterator over the matches found
/// up front (a native tuple iterator, which the dispatch loop steps and
/// ends without a call or a `StopIteration`); over a longer one, a lazy
/// scanner. Only an immutable `str` or `bytes` subject is served, so
/// finding the matches early cannot observe a different subject.
fn pattern_finditer(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let a = exec_args(args)?;
    let len = match a.string {
        Object::Str(s) => SharedStr::char_count(s),
        Object::Bytes(b) => b.len(),
        _ => return None,
    };
    if len > EAGER_FINDITER {
        return pattern_scanner(args);
    }
    with_pattern(a.recv, a.string, |st, cc, _| {
        let subject = Subject::new(a.string)?;
        let (mut p, e) = clamp(a.pos, a.endpos, subject.len());
        let opos = p;
        let mut out = Vec::with_capacity(8);
        let mut must_advance = false;
        while p <= e {
            let Some((m, s, en)) = subject.exec(cc, p, e, 0, must_advance, |f| {
                (new_match(st, a.recv, a.string, opos, e, f), f.start, f.end)
            })?
            else {
                break;
            };
            out.push(m?);
            must_advance = s == en;
            p = en;
        }
        let items = crate::tuple_storage::TupleStorage::from_vec(out);
        Ok(Object::Iter(Rc::new(RefCell::new(
            crate::object::PyIterator::Tuple { items, index: 0 },
        ))))
    })
}

/// How `sub` produces each replacement.
enum Filter<'a> {
    Literal(&'a Object),
    Template(Rc<Vec<Piece>>),
    Callable(&'a Object),
}

/// The cached template for `repl` (a `str` or `bytes` with a backslash).
fn cached_template(st: &State, handle: i64, repl: &Object) -> Option<Rc<Vec<Piece>>> {
    let cache = st.templates.lock();
    let (key, t) = cache.get(&handle)?;
    let same = match (key, repl) {
        (Object::Str(a), Object::Str(b)) => a == b,
        (Object::Bytes(a), Object::Bytes(b)) => a[..] == b[..],
        _ => false,
    };
    same.then(|| t.clone())
}

/// Parse `repl` through `re._engine._compile_template` and cache it.
fn parse_template(
    st: &State,
    pattern: &Object,
    handle: i64,
    repl: &Object,
) -> Result<Option<Rc<Vec<Piece>>>, RuntimeError> {
    let f = st.compile_template.clone();
    let parsed = with_interp(|i| i.call_object(f, &[pattern.clone(), repl.clone()], &[]))?;
    let items: Vec<Object> = match &parsed {
        Object::List(l) => l.borrow().clone(),
        Object::Tuple(t) => t.to_vec(),
        _ => return Ok(None),
    };
    let mut pieces = Vec::with_capacity(items.len());
    for it in items {
        match it {
            Object::Int(g) if g >= 0 => pieces.push(Piece::Group(g as usize)),
            Object::Str(ref s) if s.is_empty() => {}
            Object::Bytes(ref b) if b.is_empty() => {}
            o @ (Object::Str(_) | Object::Bytes(_)) => pieces.push(Piece::Lit(o)),
            _ => return Ok(None),
        }
    }
    let t = Rc::new(pieces);
    let mut cache = st.templates.lock();
    if cache.len() >= 512 {
        cache.clear();
    }
    cache.insert(handle, (repl.clone(), t.clone()));
    Ok(Some(t))
}

fn has_backslash(repl: &Object) -> bool {
    match repl {
        Object::Str(s) => s.as_bytes().contains(&b'\\'),
        Object::Bytes(b) => b.contains(&b'\\'),
        _ => true,
    }
}

/// The receiver, replacement, subject and count of `sub`/`subn`.
fn sub_args(args: &[Object]) -> Option<(&Object, &Object, &Object, i64)> {
    let [recv, repl, string, rest @ ..] = args else {
        return None;
    };
    let count = match rest {
        [] => 0,
        [Object::Int(c)] => *c,
        [Object::Bool(b)] => i64::from(*b),
        _ => return None,
    };
    Some((recv, repl, string, count))
}

/// Whether `repl` is a `str`/`bytes` replacement of the subject's type.
fn plain_repl(repl: &Object, string: &Object) -> bool {
    matches!(
        (repl, string),
        (Object::Str(_), Object::Str(_)) | (Object::Bytes(_), Object::Bytes(_))
    )
}

#[allow(clippy::too_many_arguments)]
fn run_sub(
    st: &State,
    cc: &CompiledCode,
    recv: &Object,
    string: &Object,
    filter: &Filter<'_>,
    count: i64,
    subn: bool,
) -> Result<Object, RuntimeError> {
    let subject = Subject::new(string)?;
    let e = subject.len();
    let mut sink = Sink::new(&subject);
    let (mut p, mut last, mut n) = (0usize, 0usize, 0i64);
    let mut must_advance = false;
    while p <= e && (count == 0 || n < count) {
        // The literal and template replacements are written while the
        // match is at hand; a callable gets a Match object, called after.
        let Some((s, en, m)) = subject.exec(cc, p, e, 0, must_advance, |f| {
            sink.push_span(&subject, last, f.start);
            let m = match filter {
                Filter::Literal(l) => {
                    sink.push_obj(l);
                    None
                }
                Filter::Template(t) => {
                    for piece in t.iter() {
                        match piece {
                            Piece::Lit(l) => sink.push_obj(l),
                            Piece::Group(g) => {
                                let (a, b) = f.span(*g);
                                if a >= 0 && b >= 0 {
                                    sink.push_span(&subject, a as usize, b as usize);
                                }
                            }
                        }
                    }
                    None
                }
                Filter::Callable(_) => Some(new_match(st, recv, string, 0, e, f)),
            };
            (f.start, f.end, m)
        })?
        else {
            break;
        };
        match (filter, m) {
            (Filter::Callable(f), Some(m)) => {
                let m = m?;
                // A builtin call runs with the interpreter and its thread
                // handles already published, so the callable is called
                // directly (as `functools` does).
                let item = with_interp(|i| {
                    let globals = i.builtins_dict();
                    i.call(f, &[m], &[], &globals)
                })?;
                if !matches!(item, Object::None) {
                    sink.push_obj(&item);
                }
            }
            _ => {}
        }
        last = en;
        n += 1;
        must_advance = s == en;
        p = en;
    }
    sink.push_span(&subject, last, e);
    let result = if n == 0 && !matches!(sink, Sink::Parts(_)) {
        // No replacement: CPython hands back the subject itself.
        string.clone()
    } else {
        sink.finish(&empty_like(string))?
    };
    Ok(if subn {
        Object::new_tuple_array([result, Object::Int(n)])
    } else {
        result
    })
}

fn sub_fast(args: &[Object], subn: bool) -> Option<Result<Object, RuntimeError>> {
    let (recv, repl, string, count) = sub_args(args)?;
    if count < 0 || !plain_repl(repl, string) {
        return None;
    }
    with_pattern(recv, string, |st, cc, handle| {
        let filter = if has_backslash(repl) {
            Filter::Template(cached_template(st, handle, repl)?)
        } else {
            Filter::Literal(repl)
        };
        Some(run_sub(st, cc, recv, string, &filter, count, subn))
    })?
}

/// `sub`'s full body: a callable replacement, or a template to parse.
fn sub_full(args: &[Object], subn: bool) -> Option<Result<Object, RuntimeError>> {
    let (recv, repl, string, count) = sub_args(args)?;
    if count < 0 {
        return None;
    }
    let callable = !plain_repl(repl, string);
    if callable && !crate::builtins::object_is_callable(repl) {
        return None;
    }
    with_pattern_held(recv, string, |st, cc, handle| {
        let filter = if callable {
            Filter::Callable(repl)
        } else if has_backslash(repl) {
            match cached_template(st, handle, repl) {
                Some(t) => Filter::Template(t),
                None => match parse_template(st, recv, handle, repl) {
                    Ok(Some(t)) => Filter::Template(t),
                    Ok(None) => return None,
                    Err(e) => return Some(Err(e)),
                },
            }
        } else {
            Filter::Literal(repl)
        };
        Some(run_sub(st, cc, recv, string, &filter, count, subn))
    })?
}

fn pattern_sub(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    sub_fast(args, false)
}

fn pattern_subn(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    sub_fast(args, true)
}

fn pattern_sub_full(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    sub_full(args, false)
}

fn pattern_subn_full(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    sub_full(args, true)
}

fn pattern_split(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [recv, string, rest @ ..] = args else {
        return None;
    };
    let maxsplit = match rest {
        [] => 0,
        [Object::Int(m)] => *m,
        [Object::Bool(b)] => i64::from(*b),
        _ => return None,
    };
    with_pattern(recv, string, |_, cc, _| {
        let subject = Subject::new(string)?;
        let e = subject.len();
        let mut out = Vec::with_capacity(8);
        let (mut p, mut last, mut n) = (0usize, 0usize, 0i64);
        let mut must_advance = false;
        while p <= e && (maxsplit == 0 || n < maxsplit) {
            let Some((s, en)) = subject.exec(cc, p, e, 0, must_advance, |f| {
                out.push(subject.slice(last, f.start));
                for g in 1..=cc.groups {
                    out.push(subject.group(f, g, &Object::None));
                }
                (f.start, f.end)
            })?
            else {
                break;
            };
            n += 1;
            must_advance = s == en;
            p = en;
            last = en;
        }
        out.push(subject.slice(last, e));
        Ok(Object::new_list(out))
    })
}

// ---------------------------------------------------------------------
// Match methods.

/// Run `f` over a natively built match's slot values and its marks.
fn with_match<R>(
    recv: &Object,
    f: impl FnOnce(&State, &[Object], &[Object]) -> Option<R>,
) -> Option<R> {
    let Object::Instance(inst) = recv else {
        return None;
    };
    with_state(inst, |st| {
        let slots = inst.slots.borrow();
        let v = slots.values_for_layout(&st.match_layout)?;
        let Object::Tuple(marks) = &v[M_MARKS] else {
            return None;
        };
        if marks.len() < 2 || marks.len() % 2 != 0 {
            return None;
        }
        f(st, v, marks)
    })?
}

enum GroupIdx {
    Index(usize),
    NoSuch,
    Slow,
}

/// CPython's `match_getindex` for an `int` or `str` group.
fn group_index(st: &State, v: &[Object], marks: &[Object], g: &Object) -> GroupIdx {
    let groups = marks.len() / 2 - 1;
    let i = match g {
        Object::Int(i) => *i,
        Object::Bool(b) => i64::from(*b),
        Object::Str(name) => {
            let Object::Instance(p) = &v[M_RE] else {
                return GroupIdx::Slow;
            };
            let slots = p.slots.borrow();
            let Some(pv) = slots.values_for_layout(&st.pattern_layout) else {
                return GroupIdx::Slow;
            };
            let Object::MappingProxy(d) = &pv[P_GROUPINDEX] else {
                return GroupIdx::Slow;
            };
            let found = match d.borrow().get(&StrKey(name)) {
                Some(Object::Int(i)) => Some(*i),
                Some(_) => return GroupIdx::Slow,
                None => None,
            };
            match found {
                Some(i) => i,
                None => return GroupIdx::NoSuch,
            }
        }
        _ => return GroupIdx::Slow,
    };
    if i < 0 || i as usize > groups {
        GroupIdx::NoSuch
    } else {
        GroupIdx::Index(i as usize)
    }
}

/// Group `idx`'s span.
fn span_of(marks: &[Object], idx: usize) -> Option<(i64, i64)> {
    match (&marks[2 * idx], &marks[2 * idx + 1]) {
        (Object::Int(a), Object::Int(b)) => Some((*a, *b)),
        _ => None,
    }
}

/// Group `idx`'s text, or `default` when it did not match.
fn group_text(v: &[Object], marks: &[Object], idx: usize, default: &Object) -> Option<Object> {
    let (a, b) = span_of(marks, idx)?;
    if a < 0 || b < 0 {
        return Some(default.clone());
    }
    subject_slice(&v[M_STRING], a as usize, b as usize)
}

fn no_such_group() -> RuntimeError {
    index_error("no such group")
}

fn match_group(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (recv, rest) = args.split_first()?;
    with_match(recv, |st, v, marks| match rest {
        [] => Some(Ok(group_text(v, marks, 0, &Object::None)?)),
        [g] => match group_index(st, v, marks, g) {
            GroupIdx::Index(i) => Some(Ok(group_text(v, marks, i, &Object::None)?)),
            GroupIdx::NoSuch => Some(Err(no_such_group())),
            GroupIdx::Slow => None,
        },
        many => {
            let mut out = Vec::with_capacity(many.len());
            for g in many {
                match group_index(st, v, marks, g) {
                    GroupIdx::Index(i) => out.push(group_text(v, marks, i, &Object::None)?),
                    GroupIdx::NoSuch => return Some(Err(no_such_group())),
                    GroupIdx::Slow => return None,
                }
            }
            Some(Ok(Object::new_tuple(out)))
        }
    })
}

fn match_getitem(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    match args {
        [_, _] => match_group(args),
        _ => None,
    }
}

fn match_groups(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (recv, rest) = args.split_first()?;
    let default = match rest {
        [] => &Object::None,
        [d] => d,
        _ => return None,
    };
    with_match(recv, |_, v, marks| {
        let n = marks.len() / 2 - 1;
        let mut out = Vec::with_capacity(n);
        for i in 1..=n {
            out.push(group_text(v, marks, i, default)?);
        }
        Some(Ok(Object::new_tuple(out)))
    })
}

fn match_groupdict(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let (recv, rest) = args.split_first()?;
    let default = match rest {
        [] => &Object::None,
        [d] => d,
        _ => return None,
    };
    with_match(recv, |st, v, marks| {
        let Object::Instance(p) = &v[M_RE] else {
            return None;
        };
        let slots = p.slots.borrow();
        let pv = slots.values_for_layout(&st.pattern_layout)?;
        let Object::MappingProxy(d) = &pv[P_GROUPINDEX] else {
            return None;
        };
        let groups = marks.len() / 2 - 1;
        let mut out = DictData::default();
        for (k, idx) in d.borrow().iter() {
            let Object::Int(i) = idx else {
                return None;
            };
            if *i < 0 || *i as usize > groups {
                return None;
            }
            out.insert(k.clone(), group_text(v, marks, *i as usize, default)?);
        }
        Some(Ok(Object::Dict(Rc::new(RefCell::new(out)))))
    })
}

/// `start` (0), `end` (1) and `span` (2).
fn match_position(args: &[Object], which: u8) -> Option<Result<Object, RuntimeError>> {
    let (recv, rest) = args.split_first()?;
    with_match(recv, |st, v, marks| {
        let idx = match rest {
            [] => 0,
            [g] => match group_index(st, v, marks, g) {
                GroupIdx::Index(i) => i,
                GroupIdx::NoSuch => return Some(Err(no_such_group())),
                GroupIdx::Slow => return None,
            },
            _ => return None,
        };
        let (a, b) = span_of(marks, idx)?;
        Some(Ok(match which {
            0 => Object::Int(a),
            1 => Object::Int(b),
            _ => Object::new_tuple_array([Object::Int(a), Object::Int(b)]),
        }))
    })
}

fn match_start(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    match_position(args, 0)
}

fn match_end(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    match_position(args, 1)
}

fn match_span(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    match_position(args, 2)
}

// ---------------------------------------------------------------------
// The scanner (`finditer`, `Pattern.scanner`).

/// One `match()` / `search()` step of a scanner; `iter` turns exhaustion
/// into `StopIteration` for `__next__`.
fn scanner_step(args: &[Object], mode: i64, iter: bool) -> Result<Object, RuntimeError> {
    let done = || {
        if iter {
            Err(stop_iteration())
        } else {
            Ok(Object::None)
        }
    };
    let [Object::Instance(inst)] = args else {
        return Err(type_error("SRE_Scanner method takes no arguments"));
    };
    let bad = || type_error("descriptor requires a '_sre.SRE_Scanner' object");
    let ext = class_ext(inst).ok_or_else(bad)?;
    let st = ext.downcast_ref::<State>().ok_or_else(bad)?;
    let (pattern, string, start, pos, endpos, must_advance) = {
        let slots = inst.slots.borrow();
        let v = slots
            .values_for_layout(&st.scanner_layout)
            .ok_or_else(bad)?;
        let int = |o: &Object| o.as_i64().unwrap_or(-1);
        (
            v[S_PATTERN].clone(),
            v[S_STRING].clone(),
            int(&v[S_START]),
            int(&v[S_POS]),
            int(&v[S_ENDPOS]),
            matches!(v[S_MUST], Object::Bool(true)),
        )
    };
    if start < 0 || start > endpos {
        return done();
    }
    let Object::Instance(pinst) = &pattern else {
        return Err(bad());
    };
    let (cc, _) = pattern_code(st, pinst).ok_or_else(bad)?;
    if !fast_subject(&string, cc) {
        return Err(bad());
    }
    let subject = Subject::new(&string)?;
    let endpos = (endpos as usize).min(subject.len());
    let start = start as usize;
    let pos = pos.max(0) as usize;
    let r = subject.exec(cc, start, endpos, mode, must_advance, |f| {
        (
            new_match(st, &pattern, &string, pos, endpos, f),
            f.start,
            f.end,
        )
    })?;
    let mut slots = inst.slots.borrow_mut();
    let v = slots
        .values_for_layout_mut(&st.scanner_layout)
        .ok_or_else(bad)?;
    let Some((m, s, e)) = r else {
        v[S_START] = Object::Int(endpos as i64 + 1);
        return done();
    };
    v[S_START] = Object::Int(e as i64);
    v[S_MUST] = Object::Bool(s == e);
    drop(slots);
    m
}

fn scanner_match(args: &[Object]) -> Result<Object, RuntimeError> {
    scanner_step(args, 1, false)
}

fn scanner_search(args: &[Object]) -> Result<Object, RuntimeError> {
    scanner_step(args, 0, false)
}

fn scanner_next(args: &[Object]) -> Result<Object, RuntimeError> {
    scanner_step(args, 0, true)
}

fn scanner_iter(args: &[Object]) -> Result<Object, RuntimeError> {
    match args {
        [it] => Ok(it.clone()),
        _ => Err(type_error("__iter__ takes no arguments")),
    }
}

// ---------------------------------------------------------------------
// Module functions.

type Fast = fn(&[Object]) -> Option<Result<Object, RuntimeError>>;

/// One native method of `Pattern` (class 0) or `Match` (class 1).
struct Spec {
    cls: usize,
    name: &'static str,
    key: &'static str,
    fast: Fast,
    /// The body that may call into the interpreter, tried after `fast`.
    full: Option<Fast>,
    /// Parameter names after the receiver, for keyword arguments.
    params: &'static [&'static str],
}

const EXEC_PARAMS: &[&str] = &["string", "pos", "endpos"];

const SPECS: &[Spec] = &[
    Spec {
        cls: 0,
        name: "match",
        key: "Pattern.match",
        fast: pattern_match,
        full: None,
        params: EXEC_PARAMS,
    },
    Spec {
        cls: 0,
        name: "fullmatch",
        key: "Pattern.fullmatch",
        fast: pattern_fullmatch,
        full: None,
        params: EXEC_PARAMS,
    },
    Spec {
        cls: 0,
        name: "search",
        key: "Pattern.search",
        fast: pattern_search,
        full: None,
        params: EXEC_PARAMS,
    },
    Spec {
        cls: 0,
        name: "findall",
        key: "Pattern.findall",
        fast: pattern_findall,
        full: None,
        params: EXEC_PARAMS,
    },
    Spec {
        cls: 0,
        name: "finditer",
        key: "Pattern.finditer",
        fast: pattern_finditer,
        full: None,
        params: EXEC_PARAMS,
    },
    Spec {
        cls: 0,
        name: "scanner",
        key: "Pattern.scanner",
        fast: pattern_scanner,
        full: None,
        params: EXEC_PARAMS,
    },
    Spec {
        cls: 0,
        name: "sub",
        key: "Pattern.sub",
        fast: pattern_sub,
        full: Some(pattern_sub_full as Fast),
        params: &["repl", "string", "count"],
    },
    Spec {
        cls: 0,
        name: "subn",
        key: "Pattern.subn",
        fast: pattern_subn,
        full: Some(pattern_subn_full as Fast),
        params: &["repl", "string", "count"],
    },
    Spec {
        cls: 0,
        name: "split",
        key: "Pattern.split",
        fast: pattern_split,
        full: None,
        params: &["string", "maxsplit"],
    },
    Spec {
        cls: 1,
        name: "group",
        key: "Match.group",
        fast: match_group,
        full: None,
        params: &[],
    },
    Spec {
        cls: 1,
        name: "__getitem__",
        key: "Match.__getitem__",
        fast: match_getitem,
        full: None,
        params: &[],
    },
    Spec {
        cls: 1,
        name: "groups",
        key: "Match.groups",
        fast: match_groups,
        full: None,
        params: &["default"],
    },
    Spec {
        cls: 1,
        name: "groupdict",
        key: "Match.groupdict",
        fast: match_groupdict,
        full: None,
        params: &["default"],
    },
    Spec {
        cls: 1,
        name: "start",
        key: "Match.start",
        fast: match_start,
        full: None,
        params: &["group"],
    },
    Spec {
        cls: 1,
        name: "end",
        key: "Match.end",
        fast: match_end,
        full: None,
        params: &["group"],
    },
    Spec {
        cls: 1,
        name: "span",
        key: "Match.span",
        fast: match_span,
        full: None,
        params: &["group"],
    },
];

fn dispatch(
    st: &State,
    spec: &Spec,
    args: &[Object],
    kw: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let merged;
    let pos_args = if kw.is_empty() {
        Some(args)
    } else {
        merged = merge_kwargs(args, kw, spec.params);
        merged.as_deref()
    };
    if let Some(a) = pos_args {
        if let Some(r) = (spec.fast)(a) {
            return r;
        }
        if let Some(r) = spec.full.and_then(|full| full(a)) {
            return r;
        }
    }
    let f = st
        .orig
        .get(spec.key)
        .cloned()
        .ok_or_else(|| type_error(format!("{} is not available", spec.key)))?;
    with_interp(|i| i.call_object(f, args, kw))
}

fn interned(s: &str) -> SharedStr {
    match crate::stdlib::sys::intern_name(s) {
        Object::Str(s) => s,
        _ => SharedStr::from(s),
    }
}

fn layout(names: &[&str]) -> SharedSlice<DictKey> {
    names
        .iter()
        .map(|n| DictKey(Object::Str(interned(n))))
        .collect::<Vec<_>>()
        .into()
}

fn native(name: &'static str, f: fn(&[Object]) -> Result<Object, RuntimeError>) -> Rc<BuiltinFn> {
    Rc::new(BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(f),
        call_kw: None,
    })
}

/// `_sre.install(Pattern, Match, SRE_Scanner, compile_template)`: put the
/// native bodies in the classes (see the module docs).
pub(crate) fn install(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Type(pattern), Object::Type(matchc), Object::Type(scanner), compile_template] =
        args
    else {
        return Err(type_error(
            "install() expects Pattern, Match, the scanner type and _compile_template",
        ));
    };
    if pattern.native_ext.get().is_some() {
        return Ok(Object::None);
    }
    let classes = [pattern, matchc];
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
    let state = Rc::new(State {
        match_cls: Rc::downgrade(matchc),
        scanner_cls: Rc::downgrade(scanner),
        pattern_layout: layout(&PATTERN_SLOTS),
        match_layout: layout(&MATCH_SLOTS),
        scanner_layout: layout(&SCANNER_SLOTS),
        orig,
        compile_template: compile_template.clone(),
        templates: parking_lot::Mutex::new(HashMap::new()),
    });
    for cls in [pattern, matchc, scanner] {
        let _ = cls
            .native_ext
            .set(crate::rc_unsize!(state.clone() => dyn std::any::Any + Send + Sync));
    }
    for spec in SPECS {
        let (st, st_kw) = (state.clone(), state.clone());
        // A method with no keyword parameters takes no keyword entry (the
        // dispatch loop's native subscript requires that of
        // `__getitem__`); the VM rejects keywords for it.
        let call_kw: Option<Box<dyn Fn(&[Object], &[(String, Object)]) -> _ + Send + Sync>> =
            if spec.params.is_empty() {
                None
            } else {
                Some(Box::new(move |a: &[Object], kw: &[(String, Object)]| {
                    dispatch(&st_kw, spec, a, kw)
                }))
            };
        let b = Rc::new(BuiltinFn {
            name: spec.name,
            binds_instance: true,
            call: Box::new(move |a: &[Object]| dispatch(&st, spec, a, &[])),
            call_kw,
        });
        crate::leaf_builtins::register_fast(&b, spec.fast);
        classes[spec.cls].dict.borrow_mut().insert(
            DictKey(Object::Str(interned(spec.name))),
            Object::Builtin(b),
        );
    }
    for (name, f) in [
        (
            "match",
            scanner_match as fn(&[Object]) -> Result<Object, RuntimeError>,
        ),
        ("search", scanner_search),
        ("__next__", scanner_next),
        ("__iter__", scanner_iter),
    ] {
        let b = native(name, f);
        crate::leaf_builtins::register(&b);
        scanner
            .dict
            .borrow_mut()
            .insert(DictKey(Object::Str(interned(name))), Object::Builtin(b));
    }
    for cls in [pattern, matchc, scanner] {
        cls.bump_attr_version();
    }
    Ok(Object::None)
}

/// `_sre.make_pattern(Pattern, pattern, flags, code, groups, groupindex,
/// indexgroup)`: compile `code` and build the pattern object.
pub(crate) fn make_pattern(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Type(cls), pattern, flags, code, groups, groupindex, indexgroup] = args else {
        return Err(type_error("make_pattern() takes 7 arguments"));
    };
    let st = state_of(cls).ok_or_else(|| type_error("make_pattern(): re is not installed"))?;
    let handle = sre_mod::compile_code(pattern, code, groups)?;
    Ok(build(
        cls.clone(),
        &st.pattern_layout,
        vec![
            Object::Int(handle),
            pattern.clone(),
            flags.clone(),
            groups.clone(),
            groupindex.clone(),
            indexgroup.clone(),
        ],
    ))
}

/// `_sre.exec_match(pattern, string, pos, endpos, mode, must_advance,
/// mpos)`: one match attempt over any subject, as a `Match` reporting
/// `mpos` and `endpos`, or `None`. The Python paths' engine entry.
pub(crate) fn exec_match(args: &[Object]) -> Result<Object, RuntimeError> {
    let [pattern, string, pos, endpos, mode, must_advance, mpos] = args else {
        return Err(type_error("exec_match() takes 7 arguments"));
    };
    let Object::Instance(inst) = pattern else {
        return Err(type_error("exec_match(): expected a pattern"));
    };
    let ext = class_ext(inst).ok_or_else(|| type_error("exec_match(): expected a pattern"))?;
    let st = ext
        .downcast_ref::<State>()
        .ok_or_else(|| type_error("exec_match(): expected a pattern"))?;
    let (cc, _) =
        pattern_code(st, inst).ok_or_else(|| type_error("exec_match(): expected a pattern"))?;
    let subject = Subject::new(string)?;
    let int = |o: &Object| {
        o.as_i64()
            .ok_or_else(|| type_error("exec_match(): expected an int"))
    };
    let (p, e) = clamp(int(pos)?, int(endpos)?, subject.len());
    let must = must_advance.as_i64().unwrap_or(0) != 0;
    let mpos = int(mpos)?.max(0) as usize;
    subject
        .exec(cc, p, e, int(mode)?, must, |f| {
            new_match(st, pattern, string, mpos, e, f)
        })?
        .unwrap_or(Ok(Object::None))
}

/// `_sre.clear_templates(Pattern)`: forget the parsed templates
/// (`re.purge()`).
pub(crate) fn clear_templates(args: &[Object]) -> Result<Object, RuntimeError> {
    if let [Object::Type(cls)] = args {
        if let Some(st) = state_of(cls) {
            st.templates.lock().clear();
        }
    }
    Ok(Object::None)
}

// ---------------------------------------------------------------------
// The module-level cache (`re.search(pattern, ...)` and friends).

/// The fast half of [`compile_cached`]: a hit in `re._cache2`.
fn compile_cached_fast(args: &[Object]) -> Option<Result<Object, RuntimeError>> {
    let [Object::Tuple(front), pattern, flags] = args else {
        return None;
    };
    let [Object::Dict(cache), _, Object::Type(flag_type)] = &front[..] else {
        return None;
    };
    let bt = crate::builtin_types::builtin_types();
    let ty = match pattern {
        Object::Str(_) => bt.str_.clone(),
        Object::Bytes(_) => bt.bytes_.clone(),
        _ => return None,
    };
    // `_compile` keys on `flags.value` for a `RegexFlag`, and on the flags
    // themselves otherwise (any other int subclass keeps the Python path).
    let flags = match flags {
        Object::Int(_) | Object::Bool(_) => flags.clone(),
        Object::Instance(i) if Rc::ptr_eq(&i.cls(), flag_type) => match i.native.get() {
            Some(v @ Object::Int(_)) => v.clone(),
            _ => return None,
        },
        _ => return None,
    };
    let key = DictKey(Object::new_tuple_array([
        Object::Type(ty),
        pattern.clone(),
        flags,
    ]));
    let hit = cache.borrow().get(&key).cloned();
    hit.map(Ok)
}

/// `_sre.compile_cached((cache, compile, RegexFlag), pattern, flags)`:
/// `re._compile(pattern, flags)`, answered from the `_cache2` dict when
/// the pattern is there (the hit `_compile` would return first), else by
/// calling `_compile`.
pub(crate) fn compile_cached(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Some(r) = compile_cached_fast(args) {
        return r;
    }
    let [Object::Tuple(front), pattern, flags] = args else {
        return Err(type_error("compile_cached() takes 3 arguments"));
    };
    let compile = front
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("compile_cached(): bad cache front"))?;
    with_interp(|i| i.call_object(compile, &[pattern.clone(), flags.clone()], &[]))
}

/// The `compile_cached` builtin, with its fast half registered as a leaf.
pub(crate) fn compile_cached_builtin() -> Object {
    let b = Rc::new(BuiltinFn {
        name: "compile_cached",
        binds_instance: false,
        call: Box::new(compile_cached),
        call_kw: None,
    });
    crate::leaf_builtins::register_fast(&b, compile_cached_fast);
    Object::Builtin(b)
}
