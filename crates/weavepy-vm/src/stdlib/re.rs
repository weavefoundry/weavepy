//! The `re` built-in module.
//!
//! Backed by Rust's `regex` crate. The user-visible API mirrors
//! CPython's `re` module for the common functions (`match`,
//! `search`, `findall`, `finditer`, `sub`, `split`, `compile`).
//!
//! We do not support every CPython feature: backreferences in the
//! pattern (e.g. `(?P=name)`) and lookaround (`(?=...)` / `(?<=...)`)
//! are limited by the underlying engine. The dialect is close enough
//! that the vast majority of everyday patterns work as expected.

use crate::sync::Rc;
use crate::sync::RefCell;

use regex::{Captures, Regex};

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::types::{PyInstance, TypeObject};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("re"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Support for regular expressions."),
        );
        d.insert(DictKey(Object::from_static("IGNORECASE")), Object::Int(2));
        d.insert(DictKey(Object::from_static("I")), Object::Int(2));
        d.insert(DictKey(Object::from_static("MULTILINE")), Object::Int(8));
        d.insert(DictKey(Object::from_static("M")), Object::Int(8));
        d.insert(DictKey(Object::from_static("DOTALL")), Object::Int(16));
        d.insert(DictKey(Object::from_static("S")), Object::Int(16));
        d.insert(DictKey(Object::from_static("VERBOSE")), Object::Int(64));
        d.insert(DictKey(Object::from_static("X")), Object::Int(64));
        d.insert(DictKey(Object::from_static("ASCII")), Object::Int(256));
        d.insert(DictKey(Object::from_static("A")), Object::Int(256));
        d.insert(DictKey(Object::from_static("match")), b("match", re_match));
        d.insert(
            DictKey(Object::from_static("search")),
            b("search", re_search),
        );
        d.insert(
            DictKey(Object::from_static("fullmatch")),
            b("fullmatch", re_fullmatch),
        );
        d.insert(
            DictKey(Object::from_static("findall")),
            b("findall", re_findall),
        );
        d.insert(
            DictKey(Object::from_static("finditer")),
            b("finditer", re_finditer),
        );
        d.insert(DictKey(Object::from_static("sub")), b("sub", re_sub));
        d.insert(DictKey(Object::from_static("subn")), b("subn", re_subn));
        d.insert(DictKey(Object::from_static("split")), b("split", re_split));
        d.insert(
            DictKey(Object::from_static("compile")),
            b("compile", re_compile),
        );
        d.insert(
            DictKey(Object::from_static("escape")),
            b("escape", re_escape),
        );
        d.insert(
            DictKey(Object::from_static("error")),
            Object::Type(re_error_type()),
        );
    }
    Rc::new(PyModule {
        name: "re".to_owned(),
        filename: None,
        dict,
    })
}

fn b(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(body),
        call_kw: None,
    }))
}

fn re_error_type() -> Rc<TypeObject> {
    let bt = crate::builtin_types::builtin_types();
    TypeObject::new_user("error", vec![bt.value_error.clone()], DictData::new())
        .unwrap_or_else(|_| bt.value_error.clone())
}

/// Convert a Python regex pattern to one accepted by `regex`. We
/// rewrite the most common CPython-only shortcuts: `\A` (string
/// start) and `\Z` (string end) are kept as-is (regex supports them
/// as `\A` and `\z` respectively, but for our purposes we treat them
/// equivalently to anchors).
fn compile_pattern(pat: &str, flags: i64) -> Result<Regex, RuntimeError> {
    let mut translated = pat.replace("\\Z", "\\z");
    // Python's `(?P<name>...)` is supported by `regex` natively.
    let mut builder = regex::RegexBuilder::new(&translated);
    if flags & 2 != 0 {
        builder.case_insensitive(true);
    }
    if flags & 8 != 0 {
        builder.multi_line(true);
    }
    if flags & 16 != 0 {
        builder.dot_matches_new_line(true);
    }
    if flags & 64 != 0 {
        builder.ignore_whitespace(true);
    }
    // `regex` rejects some Python escapes (`\d` defaults to ASCII in
    // Python 3 unless `re.UNICODE`); our build treats `\d`/`\w`/`\s`
    // as Unicode-aware, matching CPython 3 defaults.
    builder.build().or_else(|_| {
        // Some patterns contain literal `(?P=name)` backrefs we can't
        // support; if so, fall back to a verbose error.
        translated = pat.to_owned();
        builder = regex::RegexBuilder::new(&translated);
        builder
            .build()
            .map_err(|e| value_error(format!("invalid pattern: {e}")))
    })
}

/// Compile with the `fancy-regex` engine. Used as a fallback when
/// the base `regex` crate rejects the pattern — typically because
/// of CPython features `regex` doesn't implement (lookaround,
/// backreferences). Returned eagerly so callers can decide whether
/// to fall back without paying the cost on every successful
/// compile.
fn compile_pattern_fancy(pat: &str, flags: i64) -> Result<fancy_regex::Regex, RuntimeError> {
    let mut translated = pat.replace("\\Z", "\\z");
    // Apply inline flag prefix so the same CPython flag bits steer
    // the fancy engine.
    let mut prefix = String::new();
    if flags & 2 != 0 {
        prefix.push('i');
    }
    if flags & 8 != 0 {
        prefix.push('m');
    }
    if flags & 16 != 0 {
        prefix.push('s');
    }
    if flags & 64 != 0 {
        prefix.push('x');
    }
    if !prefix.is_empty() {
        translated = format!("(?{prefix}){translated}");
    }
    fancy_regex::Regex::new(&translated).map_err(|e| value_error(format!("invalid pattern: {e}")))
}

/// Public alias exposed to the VM dispatcher so it can route
/// callable-replacement ``re.sub`` calls itself.
pub fn extract_pattern_pub(arg: &Object) -> Result<(String, i64), RuntimeError> {
    extract_pattern(arg)
}

/// Public helper: collect every non-overlapping match span +
/// captures of ``pat`` over ``text``. Used by the VM-routed
/// ``re.sub`` callable path so the actual ``repl(match)`` calls
/// happen on the interpreter side.
pub fn collect_all_matches(
    pat: &str,
    flags: i64,
    text: &str,
) -> Result<Vec<(usize, usize, Vec<Option<(usize, usize)>>)>, RuntimeError> {
    let mut out: Vec<(usize, usize, Vec<Option<(usize, usize)>>)> = Vec::new();
    let mut on_match = |s: usize, e: usize, groups: &[Option<(usize, usize)>]| {
        out.push((s, e, groups.to_vec()));
    };
    iter_all_matches(pat, flags, text, &mut on_match)?;
    Ok(out)
}

/// Build a ``re.Match`` object compatible with the rest of the
/// module from a pre-extracted set of group spans.
pub fn build_match_object(
    pat: &str,
    text: &str,
    groups: &[Option<(usize, usize)>],
    _full_start: usize,
    _full_end: usize,
) -> Object {
    let caps = DualCaptures {
        groups: groups.to_vec(),
        named: Vec::new(),
    };
    make_match_from_captured(pat, text, &caps, text, 0)
}

fn extract_pattern(arg: &Object) -> Result<(String, i64), RuntimeError> {
    match arg {
        Object::Str(s) => Ok((s.to_string(), 0)),
        Object::Instance(inst) if inst.class.name == "Pattern" => {
            let pat = inst
                .dict
                .borrow()
                .get(&DictKey(Object::from_static("pattern")))
                .cloned()
                .unwrap_or(Object::from_static(""));
            let flags = inst
                .dict
                .borrow()
                .get(&DictKey(Object::from_static("flags")))
                .cloned()
                .unwrap_or(Object::Int(0));
            let p = match pat {
                Object::Str(s) => s.to_string(),
                _ => return Err(type_error("invalid Pattern object")),
            };
            let f = match flags {
                Object::Int(i) => i,
                _ => 0,
            };
            Ok((p, f))
        }
        _ => Err(type_error(
            "first argument must be string or compiled pattern",
        )),
    }
}

thread_local! {
    static PATTERN_CLASS: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
    static MATCH_CLASS: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
}

fn pattern_class() -> Rc<TypeObject> {
    PATTERN_CLASS.with(|slot| {
        if let Some(c) = slot.borrow().as_ref() {
            return c.clone();
        }
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::new();
        for (name, method) in pattern_methods() {
            dict.insert(DictKey(Object::from_str(name)), method);
        }
        let cls =
            TypeObject::new_user("Pattern", vec![bt.object_.clone()], dict).expect("Pattern type");
        *slot.borrow_mut() = Some(cls.clone());
        cls
    })
}

fn match_class() -> Rc<TypeObject> {
    MATCH_CLASS.with(|slot| {
        if let Some(c) = slot.borrow().as_ref() {
            return c.clone();
        }
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::new();
        for (name, method) in match_methods() {
            dict.insert(DictKey(Object::from_str(name)), method);
        }
        let cls =
            TypeObject::new_user("Match", vec![bt.object_.clone()], dict).expect("Match type");
        *slot.borrow_mut() = Some(cls.clone());
        cls
    })
}

fn re_compile(args: &[Object]) -> Result<Object, RuntimeError> {
    let pat = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("compile() expects str pattern")),
    };
    let flags = match args.get(1) {
        Some(Object::Int(i)) => *i,
        None => 0,
        _ => return Err(type_error("flags must be int")),
    };
    // Validate by compiling now; we store the source.
    let _ = compile_pattern(&pat, flags)?;
    Ok(make_pattern(pat, flags))
}

fn make_pattern(pattern: String, flags: i64) -> Object {
    let inst = PyInstance::new(pattern_class());
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("pattern")),
        Object::from_str(pattern),
    );
    inst.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("flags")), Object::Int(flags));
    Object::Instance(Rc::new(inst))
}

fn pattern_methods() -> Vec<(&'static str, Object)> {
    vec![
        ("match", b("match", pattern_match)),
        ("search", b("search", pattern_search)),
        ("fullmatch", b("fullmatch", pattern_fullmatch)),
        ("findall", b("findall", pattern_findall)),
        ("finditer", b("finditer", pattern_finditer)),
        ("sub", b("sub", pattern_sub)),
        ("split", b("split", pattern_split)),
    ]
}

fn pattern_match(args: &[Object]) -> Result<Object, RuntimeError> {
    run_match(args, true, false)
}
fn pattern_search(args: &[Object]) -> Result<Object, RuntimeError> {
    run_match(args, false, false)
}
fn pattern_fullmatch(args: &[Object]) -> Result<Object, RuntimeError> {
    run_match(args, true, true)
}
fn pattern_findall(args: &[Object]) -> Result<Object, RuntimeError> {
    re_findall(args)
}
fn pattern_finditer(args: &[Object]) -> Result<Object, RuntimeError> {
    re_finditer(args)
}
fn pattern_sub(args: &[Object]) -> Result<Object, RuntimeError> {
    re_sub(args)
}
fn pattern_split(args: &[Object]) -> Result<Object, RuntimeError> {
    re_split(args)
}

fn re_escape(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("escape() expects str")),
    };
    Ok(Object::from_str(regex::escape(&s)))
}

fn run_match(
    args: &[Object],
    require_start: bool,
    fullmatch: bool,
) -> Result<Object, RuntimeError> {
    let first = args
        .first()
        .ok_or_else(|| type_error("expected pattern argument"))?;
    let from_pattern = matches!(first, Object::Instance(inst) if inst.class.name == "Pattern");
    let (pat, default_flags) = extract_pattern(first)?;
    let text = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("expected str input")),
    };
    // Pattern method form: `pattern.match(s, pos=0, endpos=len(s))`.
    // Module-level form: `re.match(pattern, s, flags=0)`.
    let (flags, pos, endpos) = if from_pattern {
        let pos = match args.get(2) {
            Some(Object::Int(i)) => *i,
            _ => 0,
        };
        let endpos = match args.get(3) {
            Some(Object::Int(i)) => *i,
            _ => text.chars().count() as i64,
        };
        (default_flags, pos, endpos)
    } else {
        let flags = match args.get(2) {
            Some(Object::Int(i)) => *i,
            _ => default_flags,
        };
        (flags, 0i64, text.chars().count() as i64)
    };
    let start_byte = char_index_to_byte(&text, pos.max(0) as usize);
    let end_byte = char_index_to_byte(&text, endpos.max(0) as usize);
    if start_byte > end_byte || start_byte > text.len() {
        return Ok(Object::None);
    }
    let slice_end = end_byte.min(text.len());
    let slice = &text[start_byte..slice_end];
    let captured = match dual_captures(&pat, flags, slice)? {
        Some(c) => c,
        None => return Ok(Object::None),
    };
    let span0 = captured.groups[0].expect("group 0 always present");
    if require_start && span0.0 != 0 {
        return Ok(Object::None);
    }
    if fullmatch && (span0.0 != 0 || span0.1 != slice.len()) {
        return Ok(Object::None);
    }
    Ok(make_match_from_captured(
        &pat, &text, &captured, slice, start_byte,
    ))
}

/// A capture result that hides which engine produced it. Spans are
/// byte offsets into the *slice* the caller passed; the caller adds
/// any base offset back.
struct DualCaptures {
    groups: Vec<Option<(usize, usize)>>,
    /// Ordered ``(name, Option<group_idx>)`` pairs for named groups.
    /// Group indices line up with ``groups``.
    named: Vec<(String, usize)>,
}

fn dual_captures(pat: &str, flags: i64, slice: &str) -> Result<Option<DualCaptures>, RuntimeError> {
    if let Ok(re) = compile_pattern(pat, flags) {
        if let Some(caps) = re.captures(slice) {
            let mut groups = Vec::with_capacity(caps.len());
            for i in 0..caps.len() {
                groups.push(caps.get(i).map(|m| (m.start(), m.end())));
            }
            let mut named = Vec::new();
            for (i, name) in re.capture_names().enumerate() {
                if let Some(n) = name {
                    named.push((n.to_owned(), i));
                }
            }
            return Ok(Some(DualCaptures { groups, named }));
        }
        return Ok(None);
    }
    // Fallback to fancy-regex.
    let re = compile_pattern_fancy(pat, flags)?;
    let cap = re
        .captures(slice)
        .map_err(|e| value_error(format!("regex error: {e}")))?;
    let caps = match cap {
        Some(c) => c,
        None => return Ok(None),
    };
    let mut groups = Vec::with_capacity(caps.len());
    for i in 0..caps.len() {
        groups.push(caps.get(i).map(|m| (m.start(), m.end())));
    }
    let mut named = Vec::new();
    for (i, name) in re.capture_names().enumerate() {
        if let Some(n) = name {
            named.push((n.to_owned(), i));
        }
    }
    Ok(Some(DualCaptures { groups, named }))
}

fn make_match_from_captured(
    pat: &str,
    text: &str,
    caps: &DualCaptures,
    slice: &str,
    base_offset: usize,
) -> Object {
    let inst = PyInstance::new(match_class());
    let span0 = caps.groups[0].expect("group 0 always present");
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("string")),
        Object::from_str(text.to_owned()),
    );
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("re")),
        Object::from_str(pat.to_owned()),
    );
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("pos")),
        Object::Int(base_offset as i64),
    );
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("endpos")),
        Object::Int(text.len() as i64),
    );
    let mut groups: Vec<Object> = Vec::new();
    let mut spans: Vec<Object> = Vec::new();
    for span in &caps.groups {
        match span {
            Some((s, e)) => {
                groups.push(Object::from_str(slice[*s..*e].to_owned()));
                spans.push(Object::new_tuple(vec![
                    Object::Int((s + base_offset) as i64),
                    Object::Int((e + base_offset) as i64),
                ]));
            }
            None => {
                groups.push(Object::None);
                spans.push(Object::new_tuple(vec![Object::Int(-1), Object::Int(-1)]));
            }
        }
    }
    let mut named_dict = DictData::new();
    for (name, idx) in &caps.named {
        let val = match caps.groups.get(*idx).copied().flatten() {
            Some((s, e)) => Object::from_str(slice[s..e].to_owned()),
            None => Object::None,
        };
        named_dict.insert(DictKey(Object::from_str(name.clone())), val);
    }
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("_groups")),
        Object::new_tuple(groups),
    );
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("_spans")),
        Object::new_tuple(spans),
    );
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("_named")),
        Object::Dict(Rc::new(RefCell::new(named_dict))),
    );
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("_full_start")),
        Object::Int((span0.0 + base_offset) as i64),
    );
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("_full_end")),
        Object::Int((span0.1 + base_offset) as i64),
    );
    Object::Instance(Rc::new(inst))
}

fn char_index_to_byte(s: &str, n: usize) -> usize {
    for (count, (i, _)) in s.char_indices().enumerate() {
        if count == n {
            return i;
        }
    }
    s.len()
}

fn re_match(args: &[Object]) -> Result<Object, RuntimeError> {
    run_match(args, true, false)
}

fn re_search(args: &[Object]) -> Result<Object, RuntimeError> {
    run_match(args, false, false)
}

fn re_fullmatch(args: &[Object]) -> Result<Object, RuntimeError> {
    run_match(args, true, true)
}

#[allow(dead_code)]
fn make_match(
    pat: &str,
    text: &str,
    caps: &Captures<'_>,
    re: &Regex,
    base_offset: usize,
) -> Object {
    let inst = PyInstance::new(match_class());
    let m0 = caps.get(0).expect("at least one capture");
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("string")),
        Object::from_str(text.to_owned()),
    );
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("re")),
        Object::from_str(pat.to_owned()),
    );
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("pos")),
        Object::Int(base_offset as i64),
    );
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("endpos")),
        Object::Int(text.len() as i64),
    );
    let mut groups: Vec<Object> = Vec::new();
    for i in 0..caps.len() {
        match caps.get(i) {
            Some(m) => groups.push(Object::from_str(m.as_str().to_owned())),
            None => groups.push(Object::None),
        }
    }
    let mut spans: Vec<Object> = Vec::new();
    for i in 0..caps.len() {
        match caps.get(i) {
            Some(m) => spans.push(Object::new_tuple(vec![
                Object::Int((m.start() + base_offset) as i64),
                Object::Int((m.end() + base_offset) as i64),
            ])),
            None => spans.push(Object::new_tuple(vec![Object::Int(-1), Object::Int(-1)])),
        }
    }
    let mut named = DictData::new();
    for name in re.capture_names().flatten() {
        let val = caps
            .name(name)
            .map(|m| Object::from_str(m.as_str().to_owned()))
            .unwrap_or(Object::None);
        named.insert(DictKey(Object::from_str(name.to_owned())), val);
    }
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("_groups")),
        Object::new_tuple(groups),
    );
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("_spans")),
        Object::new_tuple(spans),
    );
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("_named")),
        Object::Dict(Rc::new(RefCell::new(named))),
    );
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("_full_start")),
        Object::Int((m0.start() + base_offset) as i64),
    );
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("_full_end")),
        Object::Int((m0.end() + base_offset) as i64),
    );
    Object::Instance(Rc::new(inst))
}

fn match_methods() -> Vec<(&'static str, Object)> {
    vec![
        ("group", b("group", match_group)),
        ("groups", b("groups", match_groups_method)),
        ("groupdict", b("groupdict", match_groupdict)),
        ("start", b("start", match_start)),
        ("end", b("end", match_end)),
        ("span", b("span", match_span)),
    ]
}

fn match_self(args: &[Object]) -> Result<Rc<PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) if i.class.name == "Match" => Ok(i.clone()),
        _ => Err(type_error("expected Match receiver")),
    }
}

fn match_group(args: &[Object]) -> Result<Object, RuntimeError> {
    let m = match_self(args)?;
    let groups = m
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_groups")))
        .cloned();
    let named = m
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_named")))
        .cloned();
    let groups_tuple = match groups {
        Some(Object::Tuple(t)) => t,
        _ => return Err(type_error("invalid Match groups")),
    };
    let lookup = |idx: &Object| -> Result<Object, RuntimeError> {
        match idx {
            Object::Int(i) => groups_tuple
                .get(*i as usize)
                .cloned()
                .ok_or_else(|| value_error("no such group")),
            Object::Str(s) => match named {
                Some(Object::Dict(ref d)) => d
                    .borrow()
                    .get(&DictKey(Object::from_str(s.to_string())))
                    .cloned()
                    .ok_or_else(|| value_error("no such group")),
                _ => Err(value_error("no named groups")),
            },
            _ => Err(type_error("group key must be int or str")),
        }
    };
    let arg_indices = &args[1..];
    if arg_indices.is_empty() {
        return Ok(groups_tuple.first().cloned().unwrap_or(Object::None));
    }
    if arg_indices.len() == 1 {
        return lookup(&arg_indices[0]);
    }
    let mut out = Vec::with_capacity(arg_indices.len());
    for a in arg_indices {
        out.push(lookup(a)?);
    }
    Ok(Object::new_tuple(out))
}

fn match_groups_method(args: &[Object]) -> Result<Object, RuntimeError> {
    let m = match_self(args)?;
    let groups = m
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_groups")))
        .cloned();
    let default = args.get(1).cloned().unwrap_or(Object::None);
    match groups {
        Some(Object::Tuple(t)) => {
            let out: Vec<Object> = t
                .iter()
                .skip(1)
                .cloned()
                .map(|v| {
                    if matches!(v, Object::None) {
                        default.clone()
                    } else {
                        v
                    }
                })
                .collect();
            Ok(Object::new_tuple(out))
        }
        _ => Err(type_error("invalid Match groups")),
    }
}

fn match_groupdict(args: &[Object]) -> Result<Object, RuntimeError> {
    let m = match_self(args)?;
    let named = m
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_named")))
        .cloned();
    match named {
        Some(Object::Dict(d)) => Ok(Object::Dict(d.clone())),
        _ => Ok(Object::new_dict()),
    }
}

fn match_start(args: &[Object]) -> Result<Object, RuntimeError> {
    let m = match_self(args)?;
    let idx = args.get(1).cloned().unwrap_or(Object::Int(0));
    let i = match idx {
        Object::Int(i) => i,
        _ => return Err(type_error("start() expected int")),
    };
    let spans = m
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_spans")))
        .cloned();
    match spans {
        Some(Object::Tuple(spans)) => match spans.get(i as usize) {
            Some(Object::Tuple(t)) => Ok(t[0].clone()),
            _ => Err(value_error("no such group")),
        },
        _ => Err(type_error("invalid Match spans")),
    }
}

fn match_end(args: &[Object]) -> Result<Object, RuntimeError> {
    let m = match_self(args)?;
    let idx = args.get(1).cloned().unwrap_or(Object::Int(0));
    let i = match idx {
        Object::Int(i) => i,
        _ => return Err(type_error("end() expected int")),
    };
    let spans = m
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_spans")))
        .cloned();
    match spans {
        Some(Object::Tuple(spans)) => match spans.get(i as usize) {
            Some(Object::Tuple(t)) => Ok(t[1].clone()),
            _ => Err(value_error("no such group")),
        },
        _ => Err(type_error("invalid Match spans")),
    }
}

fn match_span(args: &[Object]) -> Result<Object, RuntimeError> {
    let m = match_self(args)?;
    let idx = args.get(1).cloned().unwrap_or(Object::Int(0));
    let i = match idx {
        Object::Int(i) => i,
        _ => return Err(type_error("span() expected int")),
    };
    let spans = m
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_spans")))
        .cloned();
    match spans {
        Some(Object::Tuple(spans)) => spans
            .get(i as usize)
            .cloned()
            .ok_or_else(|| value_error("no such group")),
        _ => Err(type_error("invalid Match spans")),
    }
}

fn re_findall(args: &[Object]) -> Result<Object, RuntimeError> {
    let (pat, default_flags) =
        extract_pattern(args.first().ok_or_else(|| type_error("expected pattern"))?)?;
    let text = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("expected str")),
    };
    let flags = match args.get(2) {
        Some(Object::Int(i)) => *i,
        _ => default_flags,
    };
    let mut out = Vec::new();
    let mut on_match = |_s: usize, _e: usize, groups: &[Option<(usize, usize)>]| {
        let has_groups = groups.len() > 1;
        if has_groups {
            let group_count = groups.len() - 1;
            if group_count == 1 {
                let s = groups[1].map_or(String::new(), |(s, e)| text[s..e].to_owned());
                out.push(Object::from_str(s));
            } else {
                let mut tup = Vec::with_capacity(group_count);
                for g in groups.iter().skip(1).take(group_count) {
                    let s = g.map_or(String::new(), |(s, e)| text[s..e].to_owned());
                    tup.push(Object::from_str(s));
                }
                out.push(Object::new_tuple(tup));
            }
        } else {
            let s = groups[0].map_or(String::new(), |(s, e)| text[s..e].to_owned());
            out.push(Object::from_str(s));
        }
    };
    iter_all_matches(&pat, flags, &text, &mut on_match)?;
    Ok(Object::new_list(out))
}

fn re_finditer(args: &[Object]) -> Result<Object, RuntimeError> {
    let (pat, default_flags) =
        extract_pattern(args.first().ok_or_else(|| type_error("expected pattern"))?)?;
    let text = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("expected str")),
    };
    let flags = match args.get(2) {
        Some(Object::Int(i)) => *i,
        _ => default_flags,
    };
    let mut out = Vec::new();
    let mut consume_groups = |start: usize, _end: usize, groups: &[Option<(usize, usize)>]| {
        let groups_vec = groups.to_vec();
        let _ = start;
        let caps = DualCaptures {
            groups: groups_vec,
            named: Vec::new(),
        };
        out.push(make_match_from_captured(&pat, &text, &caps, &text, 0));
    };
    iter_all_matches(&pat, flags, &text, &mut consume_groups)?;
    Ok(Object::new_list(out))
}

/// Walk every non-overlapping match in ``text`` and invoke ``f``
/// with the byte span and capture groups. Falls back to the
/// ``fancy_regex`` engine if the base ``regex`` can't compile the
/// pattern.
fn iter_all_matches(
    pat: &str,
    flags: i64,
    text: &str,
    f: &mut dyn FnMut(usize, usize, &[Option<(usize, usize)>]),
) -> Result<(), RuntimeError> {
    match compile_pattern(pat, flags) {
        Ok(re) => {
            for caps in re.captures_iter(text) {
                let mut groups = Vec::with_capacity(caps.len());
                for i in 0..caps.len() {
                    groups.push(caps.get(i).map(|m| (m.start(), m.end())));
                }
                let m = caps.get(0).unwrap();
                f(m.start(), m.end(), &groups);
            }
            Ok(())
        }
        Err(_) => {
            let re = compile_pattern_fancy(pat, flags)?;
            for caps in re.captures_iter(text) {
                let caps = caps.map_err(|e| value_error(format!("regex error: {e}")))?;
                let mut groups = Vec::with_capacity(caps.len());
                for i in 0..caps.len() {
                    groups.push(caps.get(i).map(|m| (m.start(), m.end())));
                }
                let m = caps.get(0).unwrap();
                f(m.start(), m.end(), &groups);
            }
            Ok(())
        }
    }
}

fn re_sub(args: &[Object]) -> Result<Object, RuntimeError> {
    let (s, _) = re_sub_impl(args)?;
    Ok(Object::from_str(s))
}

fn re_subn(args: &[Object]) -> Result<Object, RuntimeError> {
    let (s, n) = re_sub_impl(args)?;
    Ok(Object::new_tuple(vec![Object::from_str(s), Object::Int(n)]))
}

fn re_sub_impl(args: &[Object]) -> Result<(String, i64), RuntimeError> {
    let (pat, default_flags) =
        extract_pattern(args.first().ok_or_else(|| type_error("expected pattern"))?)?;
    let repl = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        Some(Object::Function(_)) | Some(Object::Builtin(_)) | Some(Object::BoundMethod(_)) => {
            // ``re.sub`` with a callable replacement requires
            // calling back into the VM. The VM intercepts the
            // ``sub`` builtin (see ``do_re_sub_call`` in
            // ``lib.rs``) and routes those calls itself, so the
            // pure-data path here only services the string form.
            return Err(type_error(
                "internal: callable re.sub should be handled at the VM dispatch layer",
            ));
        }
        _ => return Err(type_error("repl must be str or callable")),
    };
    let text = match args.get(2) {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("expected str")),
    };
    let count = match args.get(3) {
        Some(Object::Int(i)) => *i,
        _ => 0,
    };
    let flags = match args.get(4) {
        Some(Object::Int(i)) => *i,
        _ => default_flags,
    };
    let mut out = String::new();
    let mut last_end = 0usize;
    let mut replacements = 0i64;
    let mut on_match = |s: usize, e: usize, groups: &[Option<(usize, usize)>]| {
        if count > 0 && replacements >= count {
            return;
        }
        out.push_str(&text[last_end..s]);
        out.push_str(&expand_replacement_from_groups(&repl, groups, &text));
        last_end = e;
        replacements += 1;
    };
    iter_all_matches(&pat, flags, &text, &mut on_match)?;
    out.push_str(&text[last_end..]);
    Ok((out, replacements))
}

/// Same expansion rules as ``expand_replacement`` but driven by
/// pre-extracted group spans rather than a regex ``Captures``.
fn expand_replacement_from_groups(
    repl: &str,
    groups: &[Option<(usize, usize)>],
    text: &str,
) -> String {
    let mut out = String::new();
    let bytes = repl.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            if next.is_ascii_digit() {
                let idx = (next - b'0') as usize;
                if let Some(Some((s, e))) = groups.get(idx).copied() {
                    out.push_str(&text[s..e]);
                }
                i += 2;
            } else if next == b'g' && i + 2 < bytes.len() && bytes[i + 2] == b'<' {
                let close = bytes[i + 3..]
                    .iter()
                    .position(|b| *b == b'>')
                    .map(|p| i + 3 + p);
                if let Some(end) = close {
                    let name = &repl[i + 3..end];
                    if let Ok(n) = name.parse::<usize>() {
                        if let Some(Some((s, e))) = groups.get(n).copied() {
                            out.push_str(&text[s..e]);
                        }
                    }
                    i = end + 1;
                    continue;
                }
                out.push('\\');
                i += 1;
            } else if next == b'n' {
                out.push('\n');
                i += 2;
            } else if next == b't' {
                out.push('\t');
                i += 2;
            } else if next == b'\\' {
                out.push('\\');
                i += 2;
            } else {
                out.push('\\');
                out.push(next as char);
                i += 2;
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Expand `\1` / `\g<name>` etc. inside a `re.sub` replacement.
#[allow(dead_code)]
fn expand_replacement(repl: &str, caps: &Captures<'_>) -> String {
    let mut out = String::new();
    let bytes = repl.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            if next.is_ascii_digit() {
                let idx = (next - b'0') as usize;
                if let Some(m) = caps.get(idx) {
                    out.push_str(m.as_str());
                }
                i += 2;
            } else if next == b'g' && i + 2 < bytes.len() && bytes[i + 2] == b'<' {
                let close = bytes[i + 3..]
                    .iter()
                    .position(|b| *b == b'>')
                    .map(|p| i + 3 + p);
                if let Some(end) = close {
                    let name = &repl[i + 3..end];
                    if let Ok(n) = name.parse::<usize>() {
                        if let Some(m) = caps.get(n) {
                            out.push_str(m.as_str());
                        }
                    } else if let Some(m) = caps.name(name) {
                        out.push_str(m.as_str());
                    }
                    i = end + 1;
                    continue;
                }
                out.push('\\');
                i += 1;
            } else if next == b'n' {
                out.push('\n');
                i += 2;
            } else if next == b't' {
                out.push('\t');
                i += 2;
            } else if next == b'\\' {
                out.push('\\');
                i += 2;
            } else {
                out.push('\\');
                out.push(next as char);
                i += 2;
            }
        } else {
            let ch_len = if bytes[i] < 0x80 { 1 } else { 1 };
            out.push(bytes[i] as char);
            i += ch_len;
        }
    }
    out
}

fn re_split(args: &[Object]) -> Result<Object, RuntimeError> {
    let (pat, default_flags) =
        extract_pattern(args.first().ok_or_else(|| type_error("expected pattern"))?)?;
    let text = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("expected str")),
    };
    let maxsplit = match args.get(2) {
        Some(Object::Int(i)) => *i,
        _ => 0,
    };
    let flags = match args.get(3) {
        Some(Object::Int(i)) => *i,
        _ => default_flags,
    };
    let re = compile_pattern(&pat, flags)?;
    let mut out = Vec::new();
    let mut last_end = 0;
    for (splits, caps) in re.captures_iter(&text).enumerate() {
        if maxsplit > 0 && splits as i64 >= maxsplit {
            break;
        }
        let m = caps.get(0).expect("capture 0");
        out.push(Object::from_str(text[last_end..m.start()].to_owned()));
        // Include captured groups as separate output elements (Python
        // semantics).
        for i in 1..caps.len() {
            match caps.get(i) {
                Some(g) => out.push(Object::from_str(g.as_str().to_owned())),
                None => out.push(Object::None),
            }
        }
        last_end = m.end();
    }
    out.push(Object::from_str(text[last_end..].to_owned()));
    Ok(Object::new_list(out))
}
