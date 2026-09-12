//! The `_json` accelerator.
//!
//! WeavePy ships the verbatim pure-Python `json` package
//! (`stdlib/python/json/`). Its `scanner`/`decoder`/`encoder` submodules
//! each `from _json import ...` with an `ImportError` fallback to the
//! pure-Python implementation, exactly like CPython. This module provides
//! the C-accelerator surface those imports expect:
//!
//! - `scanstring(string, end, strict=True) -> (str, end)`
//! - `encode_basestring(s) -> str`
//! - `encode_basestring_ascii(s) -> str`
//! - `make_scanner(context) -> scanner_callable`
//! - `make_encoder(markers, default, encoder, indent, key_separator,
//!   item_separator, sort_keys, skipkeys, allow_nan) -> encoder_callable`
//!
//! Behaviour mirrors `Modules/_json.c`, using the pure-Python reference
//! (`json/decoder.py`, `json/scanner.py`, `json/encoder.py`) for shared
//! semantics. Native details, including the position of invalid escapes,
//! follow the C implementation. The C-specific behaviours `test_speedups`
//! pins (eager encoding, `encoder()` must return a `str`, `make_encoder`
//! argument validation, `make_scanner` reading attributes eagerly) are
//! reproduced explicitly.

use std::collections::HashSet;

use crate::builtin_types::builtin_types;
use crate::builtins::class_of;
use crate::error::{overflow_error, type_error, value_error, PyException, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::sync::Rc;
use crate::sync::RefCell;
use crate::Interpreter;

// ---------------------------------------------------------------------------
// Module build
// ---------------------------------------------------------------------------

fn key(s: &'static str) -> DictKey {
    DictKey(Object::from_static(s))
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));

    let scanstring = bkw("scanstring", json_scanstring);
    let eb = b("encode_basestring", json_encode_basestring);
    let eba = b("encode_basestring_ascii", json_encode_basestring_ascii);
    let mk_scanner = b("make_scanner", json_make_scanner);
    // Keep the original function identities even if Python replaces the
    // module attributes. Only these two encoders can bypass a Python call.
    let encoders = [eb.clone(), eba.clone()];
    let kw_encoders = encoders.clone();
    let mk_encoder = Object::Builtin(Rc::new(BuiltinFn {
        name: "make_encoder",
        binds_instance: false,
        call: Box::new(move |a| json_make_encoder(a, &[], &encoders)),
        call_kw: Some(Box::new(move |a, kw| {
            json_make_encoder(a, kw, &kw_encoders)
        })),
    }));

    // `__module__ == "_json"` for each public function — `test_json`'s
    // `TestCTest.test_cjson` and `test_speedups` assert exactly this.
    for f in [&scanstring, &eb, &eba, &mk_scanner, &mk_encoder] {
        crate::descr_registry::register_module(f, "_json");
    }

    {
        let mut d = dict.borrow_mut();
        d.insert(key("__name__"), Object::from_static("_json"));
        d.insert(key("__doc__"), Object::from_static("json speedups\n"));
        d.insert(key("scanstring"), scanstring);
        d.insert(key("encode_basestring"), eb);
        d.insert(key("encode_basestring_ascii"), eba);
        d.insert(key("make_scanner"), mk_scanner);
        d.insert(key("make_encoder"), mk_encoder);
    }

    Rc::new(PyModule {
        name: "_json".to_owned(),
        filename: None,
        dict,
    })
}

/// A module-level positional-only function.
fn b(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

/// A module-level function that also accepts keyword arguments.
fn bkw(
    name: &'static str,
    body: fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>,
) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(move |args| body(args, &[])),
        call_kw: Some(Box::new(body)),
    }))
}

/// Borrow the active interpreter published on this thread by the dispatch
/// loop. Always present while a builtin runs.
fn with_interp<F, R>(f: F) -> Result<R, RuntimeError>
where
    F: FnOnce(&mut Interpreter) -> Result<R, RuntimeError>,
{
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| type_error("_json: no active interpreter"))?;
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    f(interp)
}

// ---------------------------------------------------------------------------
// String helpers (code-point addressed, matching Python str semantics)
// ---------------------------------------------------------------------------

/// Parse borrowed UTF-8 bytes or surrogate-bearing Unicode code points.
/// Translate byte offsets only at the public boundary and on errors.
trait JsonChar: Copy + Into<u32> {
    fn string(s: &[Self]) -> Object;
    fn key(s: &[Self], memo: &mut KeyMemo) -> Object {
        memo.intern(Self::string(s))
    }
    fn text(s: &[Self]) -> std::borrow::Cow<'_, str>;
    fn position(s: &[Self], offset: usize) -> usize;
    fn append(s: &[Self], out: &mut Vec<u32>);
    fn first(s: &[Self]) -> u32;
}

impl JsonChar for u8 {
    fn string(s: &[Self]) -> Object {
        Object::Str(Rc::from(std::str::from_utf8(s).expect("UTF-8 JSON span")))
    }

    fn key(s: &[Self], memo: &mut KeyMemo) -> Object {
        memo.utf8(std::str::from_utf8(s).expect("UTF-8 JSON span"))
    }

    fn text(s: &[Self]) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed(std::str::from_utf8(s).expect("UTF-8 JSON span"))
    }

    fn position(s: &[Self], offset: usize) -> usize {
        let end = offset.min(s.len());
        s[..end].iter().filter(|&&c| c & 0xc0 != 0x80).count() + (offset - end)
    }

    fn append(s: &[Self], out: &mut Vec<u32>) {
        out.extend(Self::text(s).chars().map(u32::from));
    }

    fn first(s: &[Self]) -> u32 {
        Self::text(s).chars().next().map_or(0, u32::from)
    }
}

impl JsonChar for u32 {
    fn string(s: &[Self]) -> Object {
        Object::str_from_codepoints(s.to_vec())
    }

    fn text(s: &[Self]) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Owned(s.iter().filter_map(|&c| char::from_u32(c)).collect())
    }

    fn position(_s: &[Self], offset: usize) -> usize {
        offset
    }

    fn append(s: &[Self], out: &mut Vec<u32>) {
        out.extend_from_slice(s);
    }

    fn first(s: &[Self]) -> u32 {
        s.first().copied().unwrap_or(0)
    }
}

/// Code points of a `str`/`WStr` (or a `str` *subclass* instance), or a
/// `TypeError` naming the bad argument like CPython's `_json`.
fn arg_codepoints(
    interp: &mut Interpreter,
    o: &Object,
    what: &str,
) -> Result<Vec<u32>, RuntimeError> {
    if let Some(cps) = o.str_codepoints() {
        return Ok(cps);
    }
    // `str` subclass instance: coerce through `str(o)` (CPython's C code
    // accepts any `PyUnicode`, including subclasses).
    if let Object::Instance(_) = o {
        if class_of(o).is_subclass_of(&builtin_types().str_) {
            let s = interp.call_object(
                Object::Type(builtin_types().str_.clone()),
                std::slice::from_ref(o),
                &[],
            )?;
            if let Some(cps) = s.str_codepoints() {
                return Ok(cps);
            }
        }
    }
    Err(type_error(format!(
        "{what} must be a string, not {}",
        o.type_name_owned()
    )))
}

thread_local! {
    /// One-entry decode cache for [`json_scanstring`]. The pure-Python
    /// `JSONObject`/`JSONArray` call the module-level `scanstring` (this
    /// accelerator) once per key/value on the *same* document string;
    /// without memoisation each call re-decodes the whole document to code
    /// points — O(n) per call, O(n^2) over a parse. Keyed on the string's
    /// heap identity (holding the object so the pointer can't be reused).
    static SCANSTRING_CACHE: std::cell::RefCell<Option<(usize, Object, Rc<Vec<u32>>)>> =
        const { std::cell::RefCell::new(None) };
}

/// Code points of `o`, reusing the thread-local cache when the same
/// immutable string is scanned repeatedly (the pure-Python decoder's hot
/// path). Falls back to a fresh decode for `str` subclasses.
fn arg_codepoints_cached(
    interp: &mut Interpreter,
    o: &Object,
    what: &str,
) -> Result<Rc<Vec<u32>>, RuntimeError> {
    let key = match o {
        Object::Str(s) => Some(Rc::as_ptr(s).cast::<u8>() as usize),
        Object::WStr(s) => Some(Rc::as_ptr(s).cast::<u8>() as usize),
        _ => None,
    };
    if let Some(k) = key {
        if let Some(v) = SCANSTRING_CACHE.with(|c| match &*c.borrow() {
            Some((ck, _, v)) if *ck == k => Some(v.clone()),
            _ => None,
        }) {
            return Ok(v);
        }
        let v = Rc::new(arg_codepoints(interp, o, what)?);
        SCANSTRING_CACHE.with(|c| *c.borrow_mut() = Some((k, o.clone(), v.clone())));
        return Ok(v);
    }
    Ok(Rc::new(arg_codepoints(interp, o, what)?))
}

fn hex_val(c: u32) -> Option<u32> {
    match c {
        0x30..=0x39 => Some(c - 0x30),
        0x41..=0x46 => Some(c - 0x41 + 10),
        0x61..=0x66 => Some(c - 0x61 + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// JSONDecodeError construction
// ---------------------------------------------------------------------------

/// Build a `json.decoder.JSONDecodeError(msg, doc, pos)` and wrap it as a
/// `RuntimeError`, mirroring `_json.c`'s `raise_errmsg`.
fn decode_error<C: JsonChar>(
    interp: &mut Interpreter,
    msg: &str,
    doc: &Object,
    cps: &[C],
    pos: usize,
) -> RuntimeError {
    let pos = C::position(cps, pos);
    let module = match interp.import_path("json.decoder") {
        Ok(m) => m,
        Err(e) => return e,
    };
    let cls = match &module {
        Object::Module(m) => m
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("JSONDecodeError")))
            .cloned(),
        _ => None,
    };
    let Some(cls) = cls else {
        return value_error(msg.to_owned());
    };
    match interp.call_object(
        cls,
        &[Object::from_str(msg), doc.clone(), Object::Int(pos as i64)],
        &[],
    ) {
        Ok(inst) => RuntimeError::PyException(PyException::new(inst)),
        Err(e) => e,
    }
}

// ---------------------------------------------------------------------------
// scanstring
// ---------------------------------------------------------------------------

/// `_json.scanstring(string, end, strict=True)` → `(decoded, end)`.
fn json_scanstring(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let s = args
        .first()
        .ok_or_else(|| type_error("scanstring() missing 'string' argument"))?;
    let end = args
        .get(1)
        .ok_or_else(|| type_error("scanstring() missing 'end' argument"))?;
    let end = match end {
        Object::Int(n) => *n,
        Object::Bool(bv) => i64::from(*bv),
        // An `int` past the `ssize_t` range (e.g. `sys.maxsize + 1`) is an
        // `OverflowError`, matching `PyLong_AsSsize_t` in `_json.c`'s
        // `scanstring` — `test_scanstring.test_overflow`.
        Object::Long(_) => {
            return Err(overflow_error(
                "Python int too large to convert to C ssize_t",
            ))
        }
        _ => return Err(type_error("end argument must be an integer")),
    };
    let mut strict = true;
    if let Some(v) = args.get(2) {
        strict = v.is_truthy();
    }
    for (k, v) in kwargs {
        if k == "strict" {
            strict = v.is_truthy();
        }
    }
    with_interp(|interp| {
        let cps = arg_codepoints_cached(interp, s, "first argument")?;
        let (decoded, new_end) = scan_string(interp, s, &cps[..], end.max(0) as usize, strict)?;
        Ok(Object::new_tuple_array([
            decoded,
            Object::Int(new_end as i64),
        ]))
    })
}

/// Core string scan (shared by `scanstring` and the scanner). `end` is the
/// index of the first character after the opening quote; returns the decoded
/// string and the index after the closing quote.
fn scan_string<C: JsonChar>(
    interp: &mut Interpreter,
    doc: &Object,
    cps: &[C],
    end: usize,
    strict: bool,
) -> Result<(Object, usize), RuntimeError> {
    scan_string_with_memo(interp, doc, cps, end, strict, None)
}

fn scan_string_with_memo<C: JsonChar>(
    interp: &mut Interpreter,
    doc: &Object,
    cps: &[C],
    mut end: usize,
    strict: bool,
    memo: Option<&mut KeyMemo>,
) -> Result<(Object, usize), RuntimeError> {
    let begin = end.wrapping_sub(1);
    let mut out: Vec<u32> = Vec::new();
    loop {
        // Scan for the next terminator: '"', '\\', or a control char (<0x20).
        let chunk_start = end;
        let mut i = end;
        loop {
            if i >= cps.len() {
                return Err(decode_error(
                    interp,
                    "Unterminated string starting at",
                    doc,
                    cps,
                    begin,
                ));
            }
            let c = cps[i].into();
            if c == 0x22 || c == 0x5c || c < 0x20 {
                break;
            }
            i += 1;
        }
        if cps[i].into() == 0x22 && out.is_empty() && chunk_start == begin.wrapping_add(1) {
            let span = &cps[chunk_start..i];
            let value = match memo {
                Some(memo) => C::key(span, memo),
                None => C::string(span),
            };
            return Ok((value, i + 1));
        }
        C::append(&cps[chunk_start..i], &mut out);
        let terminator = cps[i].into();
        end = i + 1;
        if terminator == 0x22 {
            // closing quote
            break;
        } else if terminator != 0x5c {
            // a literal control character
            if strict {
                let ch = char::from_u32(terminator).unwrap_or('\u{fffd}');
                let msg = format!("Invalid control character {:?} at", ch);
                return Err(decode_error(interp, &msg, doc, cps, end - 1));
            }
            out.push(terminator);
            continue;
        }
        // Backslash escape: `end` points at the char after the backslash.
        if end >= cps.len() {
            return Err(decode_error(
                interp,
                "Unterminated string starting at",
                doc,
                cps,
                begin,
            ));
        }
        let esc = cps[end].into();
        if esc != 0x75 {
            // not '\u': a one-char escape from the lookup table
            let ch = match esc {
                0x22 => 0x22, // "
                0x5c => 0x5c, // \
                0x2f => 0x2f, // /
                0x62 => 0x08, // \b
                0x66 => 0x0c, // \f
                0x6e => 0x0a, // \n
                0x72 => 0x0d, // \r
                0x74 => 0x09, // \t
                _ => {
                    let c = char::from_u32(C::first(&cps[end..])).unwrap_or('\u{fffd}');
                    let msg = format!("Invalid \\escape: {:?}", c);
                    return Err(decode_error(interp, &msg, doc, cps, end - 1));
                }
            };
            end += 1;
            out.push(ch);
        } else {
            // '\uXXXX'
            let mut uni = decode_uxxxx(interp, doc, cps, end)?;
            end += 5;
            if (0xd800..=0xdbff).contains(&uni)
                && end + 1 < cps.len()
                && cps[end].into() == 0x5c
                && cps[end + 1].into() == 0x75
            {
                let uni2 = decode_uxxxx(interp, doc, cps, end + 1)?;
                if (0xdc00..=0xdfff).contains(&uni2) {
                    uni = 0x10000 + (((uni - 0xd800) << 10) | (uni2 - 0xdc00));
                    end += 6;
                }
            }
            out.push(uni);
        }
    }
    let value = Object::str_from_codepoints(out);
    let value = match memo {
        Some(memo) => memo.intern(value),
        None => value,
    };
    Ok((value, end))
}

/// `_decode_uXXXX(s, pos)` — `pos` is the index of the `'u'`; reads four hex
/// digits at `pos+1`.
fn decode_uxxxx<C: JsonChar>(
    interp: &mut Interpreter,
    doc: &Object,
    cps: &[C],
    pos: usize,
) -> Result<u32, RuntimeError> {
    if pos + 4 < cps.len() {
        let mut val = 0u32;
        let mut ok = true;
        for k in 1..=4 {
            match hex_val(cps[pos + k].into()) {
                Some(d) => val = (val << 4) | d,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            return Ok(val);
        }
    }
    Err(decode_error(
        interp,
        "Invalid \\uXXXX escape",
        doc,
        cps,
        pos,
    ))
}

// ---------------------------------------------------------------------------
// encode_basestring / encode_basestring_ascii
// ---------------------------------------------------------------------------

/// Append the JSON escape for a control/`"`/`\\` code point to `out` (as
/// individual code points). Returns `true` if `c` was escaped.
fn push_basic_escape(out: &mut Vec<u32>, c: u32) -> bool {
    let s: &[u8] = match c {
        0x22 => b"\\\"",
        0x5c => b"\\\\",
        0x08 => b"\\b",
        0x0c => b"\\f",
        0x0a => b"\\n",
        0x0d => b"\\r",
        0x09 => b"\\t",
        _ if c < 0x20 => {
            out.push(0x5c);
            out.push(0x75); // u
            for shift in [12, 8, 4, 0] {
                let nib = (c >> shift) & 0xf;
                out.push(hex_digit(nib));
            }
            return true;
        }
        _ => return false,
    };
    for &byte in s {
        out.push(u32::from(byte));
    }
    true
}

fn hex_digit(nib: u32) -> u32 {
    if nib < 10 {
        0x30 + nib
    } else {
        0x61 + (nib - 10)
    }
}

fn json_encode_basestring(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = args
        .first()
        .ok_or_else(|| type_error("encode_basestring() missing argument"))?;
    if let Object::Str(s) = s {
        let mut out = JsonWriter::default();
        out.quoted_utf8(s, false);
        return Ok(out.finish());
    }
    with_interp(|interp| {
        let cps = arg_codepoints(interp, s, "first argument")?;
        let mut out: Vec<u32> = Vec::with_capacity(cps.len() + 2);
        out.push(0x22);
        for c in cps {
            if !push_basic_escape(&mut out, c) {
                out.push(c);
            }
        }
        out.push(0x22);
        Ok(Object::str_from_codepoints(out))
    })
}

fn json_encode_basestring_ascii(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = args
        .first()
        .ok_or_else(|| type_error("encode_basestring_ascii() missing argument"))?;
    if let Object::Str(s) = s {
        let mut out = JsonWriter::default();
        out.quoted_utf8(s, true);
        return Ok(out.finish());
    }
    with_interp(|interp| {
        let cps = arg_codepoints(interp, s, "first argument")?;
        let mut out = String::with_capacity(cps.len() + 2);
        out.push('"');
        for c in cps {
            if c == 0x22 {
                out.push_str("\\\"");
            } else if c == 0x5c {
                out.push_str("\\\\");
            } else if c == 0x08 {
                out.push_str("\\b");
            } else if c == 0x0c {
                out.push_str("\\f");
            } else if c == 0x0a {
                out.push_str("\\n");
            } else if c == 0x0d {
                out.push_str("\\r");
            } else if c == 0x09 {
                out.push_str("\\t");
            } else if c < 0x20 {
                out.push_str(&format!("\\u{:04x}", c));
            } else if (0x20..=0x7e).contains(&c) {
                out.push(c as u8 as char);
            } else if c < 0x10000 {
                out.push_str(&format!("\\u{:04x}", c));
            } else {
                let n = c - 0x10000;
                let s1 = 0xd800 | ((n >> 10) & 0x3ff);
                let s2 = 0xdc00 | (n & 0x3ff);
                out.push_str(&format!("\\u{:04x}\\u{:04x}", s1, s2));
            }
        }
        out.push('"');
        Ok(Object::from_str(out))
    })
}

// ---------------------------------------------------------------------------
// make_scanner
// ---------------------------------------------------------------------------

struct ScannerCfg {
    strict: bool,
    object_hook: Object,
    object_pairs_hook: Object,
    parse_float: Object,
    parse_int: Object,
    native_float: bool,
    native_int: bool,
    parse_constant: Object,
}

/// Internal scan error channel: either a real exception or the "no value
/// here" signal carrying the offending index (CPython's `StopIteration`).
enum ScanErr {
    NoValue(usize),
    Err(RuntimeError),
}
impl From<RuntimeError> for ScanErr {
    fn from(e: RuntimeError) -> Self {
        ScanErr::Err(e)
    }
}

/// `RecursionError` raised when a deeply nested document drives the native
/// scanner/encoder past the Python recursion limit. CPython's `_json.c`
/// guards `_parse_object`/`_parse_array` with `_Py_EnterRecursiveCall`;
/// `test_recursion.test_highly_nested_objects_*` pins the `RecursionError`.
fn recursion_overflow() -> RuntimeError {
    RuntimeError::PyException(PyException::new(crate::builtin_types::make_exception(
        "RecursionError",
        "maximum recursion depth exceeded while decoding a JSON object \
         from a unicode string",
    )))
}

fn json_make_scanner(args: &[Object]) -> Result<Object, RuntimeError> {
    let ctx = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("make_scanner() missing 'context' argument"))?;
    with_interp(|interp| {
        // Read the decoder context's attributes *eagerly* (CPython's
        // `scanner_init`): `make_scanner(1)` raises AttributeError here,
        // before any parsing — `test_speedups.TestDecode.test_make_scanner`.
        // `bool(ctx.strict)` honours `__bool__` (CPython's `PyObject_IsTrue`),
        // so a `strict=BadBool()` decoder propagates the exception —
        // `test_speedups.TestDecode.test_bad_bool_args`.
        let strict_obj = interp.load_attr_public(&ctx, "strict")?;
        let strict = interp.op_truth(&strict_obj)?;
        let object_hook = interp.load_attr_public(&ctx, "object_hook")?;
        let object_pairs_hook = interp.load_attr_public(&ctx, "object_pairs_hook")?;
        let parse_float = interp.load_attr_public(&ctx, "parse_float")?;
        let parse_int = interp.load_attr_public(&ctx, "parse_int")?;
        let parse_constant = interp.load_attr_public(&ctx, "parse_constant")?;
        let native_float =
            matches!(&parse_float, Object::Type(t) if Rc::ptr_eq(t, &builtin_types().float_));
        let native_int =
            matches!(&parse_int, Object::Type(t) if Rc::ptr_eq(t, &builtin_types().int_));
        let cfg = Rc::new(ScannerCfg {
            strict,
            object_hook,
            object_pairs_hook,
            parse_float,
            parse_int,
            native_float,
            native_int,
            parse_constant,
        });
        Ok(Object::Builtin(Rc::new(BuiltinFn {
            name: "scanner",
            binds_instance: false,
            call: Box::new(move |a| scanner_call(&cfg, a)),
            call_kw: None,
        })))
    })
}

/// The scanner callable: `scan_once(string, idx) -> (obj, end)`.
fn scanner_call(cfg: &ScannerCfg, args: &[Object]) -> Result<Object, RuntimeError> {
    let s = args
        .first()
        .ok_or_else(|| type_error("scanner missing 'string' argument"))?
        .clone();
    let idx = match args.get(1) {
        Some(Object::Int(n)) => *n,
        Some(Object::Bool(bv)) => i64::from(*bv),
        _ => return Err(type_error("second argument must be an integer")),
    };
    with_interp(|interp| {
        let mut memo = KeyMemo::default();
        let idx = idx.max(0) as usize;
        let result = match &s {
            Object::Str(text) => {
                let ascii = text.is_ascii();
                let byte_idx = if ascii || idx == 0 {
                    idx
                } else {
                    // Include the end boundary: raw_decode at len(s) must
                    // report that character offset, including after UTF-8.
                    let Some(offset) = text
                        .char_indices()
                        .map(|(i, _)| i)
                        .chain(std::iter::once(text.len()))
                        .nth(idx)
                    else {
                        return Err(stop_iteration_value(interp, idx));
                    };
                    offset
                };
                let result = scan_once(interp, cfg, &s, text.as_bytes(), byte_idx, &mut memo);
                if ascii {
                    result
                } else {
                    match result {
                        Ok((obj, end)) => Ok((obj, u8::position(text.as_bytes(), end))),
                        Err(ScanErr::NoValue(i)) => {
                            Err(ScanErr::NoValue(u8::position(text.as_bytes(), i)))
                        }
                        Err(e) => Err(e),
                    }
                }
            }
            Object::WStr(cps) => scan_once(interp, cfg, &s, cps, idx, &mut memo),
            _ => {
                let cps = arg_codepoints(interp, &s, "first argument")?;
                scan_once(interp, cfg, &s, &cps, idx, &mut memo)
            }
        };
        match result {
            Ok((obj, end)) => Ok(Object::new_tuple_array([obj, Object::Int(end as i64)])),
            Err(ScanErr::NoValue(i)) => Err(stop_iteration_value(interp, i)),
            Err(ScanErr::Err(e)) => Err(e),
        }
    })
}

/// Raise `StopIteration(idx)` so `json.decoder.raw_decode` converts it to a
/// `JSONDecodeError("Expecting value", s, idx)`.
fn stop_iteration_value(interp: &mut Interpreter, idx: usize) -> RuntimeError {
    let cls = Object::Type(builtin_types().stop_iteration.clone());
    match interp.call_object(cls, &[Object::Int(idx as i64)], &[]) {
        Ok(inst) => RuntimeError::PyException(PyException::new(inst)),
        Err(e) => e,
    }
}

fn is_set(o: &Object) -> bool {
    !matches!(o, Object::None)
}

fn scan_once<C: JsonChar>(
    interp: &mut Interpreter,
    cfg: &ScannerCfg,
    doc: &Object,
    cps: &[C],
    idx: usize,
    memo: &mut KeyMemo,
) -> Result<(Object, usize), ScanErr> {
    if idx >= cps.len() {
        return Err(ScanErr::NoValue(idx));
    }
    let c = cps[idx].into();
    match c {
        0x22 => {
            // '"'
            let (s, end) = scan_string(interp, doc, cps, idx + 1, cfg.strict)?;
            Ok((s, end))
        }
        // '{' / '[' — recurse for the nested container. Grow the native
        // stack on demand (`stacker`) so the only ceiling is the Python
        // recursion limit enforced inside `parse_object`/`parse_array`,
        // not the platform stack size — mirroring the interpreter's
        // dispatch loop so `infinite_recursion()` raises `RecursionError`
        // instead of overflowing the native stack.
        0x7b => stacker::maybe_grow(256 * 1024, 4 * 1024 * 1024, || {
            parse_object(interp, cfg, doc, cps, idx + 1, memo)
        }),
        0x5b => stacker::maybe_grow(256 * 1024, 4 * 1024 * 1024, || {
            parse_array(interp, cfg, doc, cps, idx + 1, memo)
        }),
        0x6e if matches_kw(cps, idx, "null") => Ok((Object::None, idx + 4)),
        0x74 if matches_kw(cps, idx, "true") => Ok((Object::Bool(true), idx + 4)),
        0x66 if matches_kw(cps, idx, "false") => Ok((Object::Bool(false), idx + 5)),
        _ => {
            if let Some((end, is_float)) = match_number(cps, idx) {
                let numstr = C::text(&cps[idx..end]);
                if is_float && cfg.native_float {
                    if let Ok(value) = numstr.parse::<f64>() {
                        return Ok((Object::Float(value), end));
                    }
                } else if !is_float && cfg.native_int {
                    if let Ok(value) = numstr.parse::<i64>() {
                        return Ok((Object::Int(value), end));
                    }
                }
                let parser = if is_float {
                    cfg.parse_float.clone()
                } else {
                    cfg.parse_int.clone()
                };
                let res = interp.call_object(parser, &[Object::from_str(numstr.as_ref())], &[])?;
                return Ok((res, end));
            }
            if matches_kw(cps, idx, "NaN") {
                Ok((parse_constant(interp, cfg, "NaN")?, idx + 3))
            } else if matches_kw(cps, idx, "Infinity") {
                Ok((parse_constant(interp, cfg, "Infinity")?, idx + 8))
            } else if matches_kw(cps, idx, "-Infinity") {
                Ok((parse_constant(interp, cfg, "-Infinity")?, idx + 9))
            } else {
                Err(ScanErr::NoValue(idx))
            }
        }
    }
}

fn parse_constant(
    interp: &mut Interpreter,
    cfg: &ScannerCfg,
    name: &str,
) -> Result<Object, RuntimeError> {
    interp.call_object(cfg.parse_constant.clone(), &[Object::from_str(name)], &[])
}

fn matches_kw<C: JsonChar>(cps: &[C], idx: usize, kw: &str) -> bool {
    let kb = kw.as_bytes();
    if idx + kb.len() > cps.len() {
        return false;
    }
    for (k, &byte) in kb.iter().enumerate() {
        if cps[idx + k].into() != u32::from(byte) {
            return false;
        }
    }
    true
}

fn is_digit(c: u32) -> bool {
    (0x30..=0x39).contains(&c)
}

/// Match `NUMBER_RE = (-?(?:0|[1-9][0-9]*))(\.[0-9]+)?([eE][-+]?[0-9]+)?`.
/// Returns `(end_index, is_float)` or `None` if no integer part matches.
fn match_number<C: JsonChar>(cps: &[C], idx: usize) -> Option<(usize, bool)> {
    let n = cps.len();
    let mut i = idx;
    if i < n && cps[i].into() == 0x2d {
        i += 1; // '-'
    }
    if i >= n {
        return None;
    }
    if cps[i].into() == 0x30 {
        i += 1; // '0'
    } else if (0x31..=0x39).contains(&cps[i].into()) {
        i += 1;
        while i < n && is_digit(cps[i].into()) {
            i += 1;
        }
    } else {
        return None;
    }
    let mut is_float = false;
    // fraction
    if i < n && cps[i].into() == 0x2e {
        let mut j = i + 1;
        if j < n && is_digit(cps[j].into()) {
            j += 1;
            while j < n && is_digit(cps[j].into()) {
                j += 1;
            }
            i = j;
            is_float = true;
        }
    }
    // exponent
    if i < n && (cps[i].into() == 0x65 || cps[i].into() == 0x45) {
        let mut j = i + 1;
        if j < n && (cps[j].into() == 0x2b || cps[j].into() == 0x2d) {
            j += 1;
        }
        if j < n && is_digit(cps[j].into()) {
            j += 1;
            while j < n && is_digit(cps[j].into()) {
                j += 1;
            }
            i = j;
            is_float = true;
        }
    }
    Some((i, is_float))
}

const WS: [u32; 4] = [0x20, 0x09, 0x0a, 0x0d];

fn is_ws(c: u32) -> bool {
    WS.contains(&c)
}

fn skip_ws<C: JsonChar>(cps: &[C], mut i: usize) -> usize {
    while i < cps.len() && is_ws(cps[i].into()) {
        i += 1;
    }
    i
}

/// `JSONObject` — `idx` points just past the opening `{`.
fn parse_object<C: JsonChar>(
    interp: &mut Interpreter,
    cfg: &ScannerCfg,
    doc: &Object,
    cps: &[C],
    mut end: usize,
    memo: &mut KeyMemo,
) -> Result<(Object, usize), ScanErr> {
    let _depth_guard = match crate::recursion::enter() {
        crate::recursion::Enter::Ok(g) => g,
        crate::recursion::Enter::Overflow => return Err(ScanErr::Err(recursion_overflow())),
    };
    let mut object = if is_set(&cfg.object_pairs_hook) {
        DecodedObject::Pairs(Vec::new())
    } else {
        DecodedObject::Dict(DictData::default())
    };
    let mut nextchar = cps.get(end).copied().map(Into::into);

    if nextchar != Some(0x22) {
        if let Some(c) = nextchar {
            if is_ws(c) {
                end = skip_ws(cps, end);
                nextchar = cps.get(end).copied().map(Into::into);
            }
        }
        if nextchar == Some(0x7d) {
            // empty object
            let result = finish_object(interp, cfg, object)?;
            return Ok((result, end + 1));
        } else if nextchar != Some(0x22) {
            return Err(ScanErr::Err(decode_error(
                interp,
                "Expecting property name enclosed in double quotes",
                doc,
                cps,
                end,
            )));
        }
    }
    end += 1;
    loop {
        // key
        let (key_obj, new_end) =
            scan_string_with_memo(interp, doc, cps, end, cfg.strict, Some(memo))?;
        end = new_end;

        // ':'
        if cps.get(end).copied().map(Into::into) != Some(0x3a) {
            end = skip_ws(cps, end);
            if cps.get(end).copied().map(Into::into) != Some(0x3a) {
                return Err(ScanErr::Err(decode_error(
                    interp,
                    "Expecting ':' delimiter",
                    doc,
                    cps,
                    end,
                )));
            }
        }
        end += 1;

        // optional whitespace before value
        if let Some(c) = cps.get(end).copied().map(Into::into) {
            if is_ws(c) {
                end += 1;
                if let Some(c2) = cps.get(end).copied().map(Into::into) {
                    if is_ws(c2) {
                        end = skip_ws(cps, end + 1);
                    }
                }
            }
        }

        // value
        let (value, new_end) = match scan_once(interp, cfg, doc, cps, end, memo) {
            Ok(v) => v,
            Err(ScanErr::NoValue(i)) => {
                return Err(ScanErr::Err(decode_error(
                    interp,
                    "Expecting value",
                    doc,
                    cps,
                    i,
                )))
            }
            Err(e) => return Err(e),
        };
        end = new_end;
        match &mut object {
            DecodedObject::Dict(dict) => {
                if let Some(previous) = dict.insert(DictKey(key_obj), value) {
                    // CPython replaces duplicate values as it parses. Run
                    // a displaced value's finalizer before the next field,
                    // with no table borrow held across user code.
                    interp.prompt_reap_dropped(previous);
                }
            }
            DecodedObject::Pairs(pairs) => pairs.push((key_obj, value)),
        }

        // delimiter
        let mut nc = cps.get(end).copied().map(Into::into);
        if let Some(c) = nc {
            if is_ws(c) {
                end = skip_ws(cps, end + 1);
                nc = cps.get(end).copied().map(Into::into);
            }
        }
        end += 1;

        match nc {
            Some(0x7d) => break, // '}'
            Some(0x2c) => {}     // ','
            _ => {
                return Err(ScanErr::Err(decode_error(
                    interp,
                    "Expecting ',' delimiter",
                    doc,
                    cps,
                    end - 1,
                )))
            }
        }
        let comma_idx = end - 1;
        end = skip_ws(cps, end);
        nextchar = cps.get(end).copied().map(Into::into);
        end += 1;
        if nextchar != Some(0x22) {
            if nextchar == Some(0x7d) {
                return Err(ScanErr::Err(decode_error(
                    interp,
                    "Illegal trailing comma before end of object",
                    doc,
                    cps,
                    comma_idx,
                )));
            }
            return Err(ScanErr::Err(decode_error(
                interp,
                "Expecting property name enclosed in double quotes",
                doc,
                cps,
                end - 1,
            )));
        }
    }
    let result = finish_object(interp, cfg, object)?;
    Ok((result, end))
}

/// Only immutable string storage can enter the decoder's key memo.
/// Separate tables allow lookup with borrowed input text before allocating
/// a string. Escaped spellings still share the same canonical storage.
#[derive(Default)]
struct KeyMemo {
    utf8: HashSet<Rc<str>>,
    wide: HashSet<Rc<[u32]>>,
}

impl KeyMemo {
    fn utf8(&mut self, text: &str) -> Object {
        if let Some(existing) = self.utf8.get(text) {
            return Object::Str(existing.clone());
        }
        let value: Rc<str> = Rc::from(text);
        self.utf8.insert(value.clone());
        Object::Str(value)
    }

    fn intern(&mut self, value: Object) -> Object {
        match value {
            Object::Str(text) => {
                if let Some(existing) = self.utf8.get(text.as_ref()) {
                    return Object::Str(existing.clone());
                }
                self.utf8.insert(text.clone());
                Object::Str(text)
            }
            Object::WStr(cps) => {
                if let Some(existing) = self.wide.get(cps.as_ref()) {
                    return Object::WStr(existing.clone());
                }
                self.wide.insert(cps.clone());
                Object::WStr(cps)
            }
            other => other,
        }
    }
}

/// Only the pairs hook needs staging. Ordinary objects build their final
/// dictionary directly, avoiding a temporary vector and a second traversal.
enum DecodedObject {
    Dict(DictData),
    Pairs(Vec<(Object, Object)>),
}

/// Apply `object_pairs_hook` / `object_hook` to the parsed object.
fn finish_object(
    interp: &mut Interpreter,
    cfg: &ScannerCfg,
    object: DecodedObject,
) -> Result<Object, RuntimeError> {
    let d = match object {
        DecodedObject::Pairs(pairs) => {
            let pair_objs: Vec<Object> = pairs
                .into_iter()
                .map(|(k, v)| Object::new_tuple_array([k, v]))
                .collect();
            let arg = Object::new_list(pair_objs);
            return interp.call_object(cfg.object_pairs_hook.clone(), &[arg], &[]);
        }
        DecodedObject::Dict(dict) => dict,
    };
    let dict_obj = Object::Dict(Rc::new(RefCell::new(d)));
    if is_set(&cfg.object_hook) {
        return interp.call_object(cfg.object_hook.clone(), &[dict_obj], &[]);
    }
    Ok(dict_obj)
}

/// `JSONArray` — `idx` points just past the opening `[`.
fn parse_array<C: JsonChar>(
    interp: &mut Interpreter,
    cfg: &ScannerCfg,
    doc: &Object,
    cps: &[C],
    mut end: usize,
    memo: &mut KeyMemo,
) -> Result<(Object, usize), ScanErr> {
    let _depth_guard = match crate::recursion::enter() {
        crate::recursion::Enter::Ok(g) => g,
        crate::recursion::Enter::Overflow => return Err(ScanErr::Err(recursion_overflow())),
    };
    let mut values: Vec<Object> = Vec::new();
    let mut nextchar = cps.get(end).copied().map(Into::into);
    if let Some(c) = nextchar {
        if is_ws(c) {
            end = skip_ws(cps, end + 1);
            nextchar = cps.get(end).copied().map(Into::into);
        }
    }
    if nextchar == Some(0x5d) {
        // ']'
        return Ok((Object::new_list(values), end + 1));
    }
    loop {
        let (value, new_end) = match scan_once(interp, cfg, doc, cps, end, memo) {
            Ok(v) => v,
            Err(ScanErr::NoValue(i)) => {
                return Err(ScanErr::Err(decode_error(
                    interp,
                    "Expecting value",
                    doc,
                    cps,
                    i,
                )))
            }
            Err(e) => return Err(e),
        };
        end = new_end;
        values.push(value);

        let mut nc = cps.get(end).copied().map(Into::into);
        if let Some(c) = nc {
            if is_ws(c) {
                end = skip_ws(cps, end + 1);
                nc = cps.get(end).copied().map(Into::into);
            }
        }
        end += 1;
        match nc {
            Some(0x5d) => break, // ']'
            Some(0x2c) => {}     // ','
            _ => {
                return Err(ScanErr::Err(decode_error(
                    interp,
                    "Expecting ',' delimiter",
                    doc,
                    cps,
                    end - 1,
                )))
            }
        }
        let comma_idx = end - 1;
        // Skip whitespace before the next value (advances `end`), matching
        // `JSONArray`'s 1–2-char fast path.
        if let Some(c) = cps.get(end).copied().map(Into::into) {
            if is_ws(c) {
                end += 1;
                if let Some(c2) = cps.get(end).copied().map(Into::into) {
                    if is_ws(c2) {
                        end = skip_ws(cps, end + 1);
                    }
                }
            }
        }
        if cps.get(end).copied().map(Into::into) == Some(0x5d) {
            return Err(ScanErr::Err(decode_error(
                interp,
                "Illegal trailing comma before end of array",
                doc,
                cps,
                comma_idx,
            )));
        }
    }
    Ok((Object::new_list(values), end))
}

// ---------------------------------------------------------------------------
// make_encoder
// ---------------------------------------------------------------------------

/// Accumulate directly into UTF-8, widening only if an unescaped surrogate
/// occurs. This also preserves lone surrogates returned by custom encoders.
#[derive(Default)]
struct JsonWriter {
    text: String,
    wide: Option<Vec<u32>>,
}

impl std::fmt::Write for JsonWriter {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.push_str(text);
        Ok(())
    }
}

impl JsonWriter {
    fn push_str(&mut self, s: &str) {
        if let Some(wide) = &mut self.wide {
            wide.extend(s.chars().map(u32::from));
        } else {
            self.text.push_str(s);
        }
    }

    fn push_codepoints(&mut self, cps: &[u32]) {
        let wide = self.wide.get_or_insert_with(|| {
            let cps = self.text.chars().map(u32::from).collect();
            self.text = String::new();
            cps
        });
        wide.extend_from_slice(cps);
    }

    fn push_object(&mut self, o: &Object) -> Result<(), RuntimeError> {
        match o {
            Object::Str(s) => self.push_str(s),
            Object::WStr(cps) => self.push_codepoints(cps),
            Object::Instance(inst) => match inst.native.get() {
                Some(s @ (Object::Str(_) | Object::WStr(_))) => self.push_object(s)?,
                _ => return Err(type_error("encoder produced a non-str chunk")),
            },
            _ => return Err(type_error("encoder produced a non-str chunk")),
        }
        Ok(())
    }

    fn unicode_escape(&mut self, c: u32) {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let bytes = [
            b'\\',
            b'u',
            HEX[((c >> 12) & 15) as usize],
            HEX[((c >> 8) & 15) as usize],
            HEX[((c >> 4) & 15) as usize],
            HEX[(c & 15) as usize],
        ];
        self.push_str(std::str::from_utf8(&bytes).expect("ASCII escape"));
    }

    fn escape(&mut self, c: u32) {
        match c {
            0x22 => self.push_str("\\\""),
            0x5c => self.push_str("\\\\"),
            0x08 => self.push_str("\\b"),
            0x0c => self.push_str("\\f"),
            0x0a => self.push_str("\\n"),
            0x0d => self.push_str("\\r"),
            0x09 => self.push_str("\\t"),
            0..=0xffff => self.unicode_escape(c),
            _ => {
                let n = c - 0x10000;
                self.unicode_escape(0xd800 | (n >> 10));
                self.unicode_escape(0xdc00 | (n & 0x3ff));
            }
        }
    }

    fn quoted_utf8(&mut self, s: &str, ascii: bool) {
        self.push_str("\"");
        let mut start = 0;
        if ascii {
            for (i, c) in s.char_indices() {
                if c == '"' || c == '\\' || !(0x20..=0x7e).contains(&u32::from(c)) {
                    self.push_str(&s[start..i]);
                    self.escape(u32::from(c));
                    start = i + c.len_utf8();
                }
            }
        } else {
            // Non-ASCII UTF-8 bytes cannot be quotes, slashes, or controls.
            for (i, &c) in s.as_bytes().iter().enumerate() {
                if c == b'"' || c == b'\\' || c < 0x20 {
                    self.push_str(&s[start..i]);
                    self.escape(u32::from(c));
                    start = i + 1;
                }
            }
        }
        self.push_str(&s[start..]);
        self.push_str("\"");
    }

    fn quoted_wide(&mut self, cps: &[u32], ascii: bool) {
        self.push_str("\"");
        let mut start = 0;
        for (i, &c) in cps.iter().enumerate() {
            if c == 0x22 || c == 0x5c || c < 0x20 || (ascii && c > 0x7e) {
                if start < i {
                    if ascii {
                        for &c in &cps[start..i] {
                            self.push_str(char::from_u32(c).unwrap().encode_utf8(&mut [0; 4]));
                        }
                    } else {
                        self.push_codepoints(&cps[start..i]);
                    }
                }
                self.escape(c);
                start = i + 1;
            }
        }
        if ascii {
            for &c in &cps[start..] {
                self.push_str(char::from_u32(c).unwrap().encode_utf8(&mut [0; 4]));
            }
        } else {
            self.push_codepoints(&cps[start..]);
        }
        self.push_str("\"");
    }

    fn finish(self) -> Object {
        match self.wide {
            Some(cps) => Object::str_from_codepoints(cps),
            None => Object::from_str(self.text),
        }
    }
}

struct EncoderCfg {
    markers: Object, // dict or None
    default: Object,
    encoder: Object,
    /// Some(ensure_ascii) only for the original native string encoders.
    native_encoder: Option<bool>,
    indent: Option<Object>,
    key_separator: Object,
    item_separator: Object,
    sort_keys: bool,
    skipkeys: bool,
    allow_nan: bool,
}

fn string_value(o: &Object) -> Option<Object> {
    match o {
        Object::Str(_) | Object::WStr(_) => Some(o.clone()),
        Object::Instance(inst) => match inst.native.get() {
            Some(s @ (Object::Str(_) | Object::WStr(_))) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn write_indent(out: &mut JsonWriter, indent: &Object, level: usize) -> Result<(), RuntimeError> {
    out.push_str("\n");
    for _ in 0..level {
        out.push_object(indent)?;
    }
    Ok(())
}

/// Parameter names of `_json.make_encoder`, in clinic order — it is
/// positional-or-keyword (test_speedups.test_current_indent_level builds
/// one with keywords only).
const MAKE_ENCODER_PARAMS: [&str; 9] = [
    "markers",
    "default",
    "encoder",
    "indent",
    "key_separator",
    "item_separator",
    "sort_keys",
    "skipkeys",
    "allow_nan",
];

fn json_make_encoder(
    positional: &[Object],
    kwargs: &[(String, Object)],
    encoders: &[Object; 2],
) -> Result<Object, RuntimeError> {
    if positional.len() > 9 {
        return Err(type_error(format!(
            "make_encoder() takes at most 9 arguments ({} given)",
            positional.len()
        )));
    }
    let mut bound: Vec<Option<Object>> = positional.iter().cloned().map(Some).collect();
    bound.resize(9, None);
    for (k, v) in kwargs {
        let Some(idx) = MAKE_ENCODER_PARAMS.iter().position(|p| p == k) else {
            return Err(type_error(format!(
                "make_encoder() got an unexpected keyword argument '{k}'"
            )));
        };
        if bound[idx].is_some() {
            return Err(type_error(format!(
                "argument for make_encoder() given by name ('{k}') and position ({})",
                idx + 1
            )));
        }
        bound[idx] = Some(v.clone());
    }
    let mut args: Vec<Object> = Vec::with_capacity(9);
    for (i, slot) in bound.into_iter().enumerate() {
        match slot {
            Some(v) => args.push(v),
            None => {
                return Err(type_error(format!(
                    "make_encoder() missing required argument '{}' (pos {})",
                    MAKE_ENCODER_PARAMS[i],
                    i + 1
                )))
            }
        }
    }
    let args = &args[..];
    let markers = args[0].clone();
    match &markers {
        Object::None | Object::Dict(_) => {}
        other => {
            return Err(type_error(format!(
                "make_encoder() argument 1 must be dict or None, not {}",
                other.type_name_owned()
            )))
        }
    }
    let indent = match &args[3] {
        Object::None => None,
        s => Some(string_value(s).ok_or_else(|| type_error("indent must be a string or None"))?),
    };
    let key_separator = string_value(&args[4])
        .ok_or_else(|| type_error("make_encoder() argument 5 must be str"))?;
    let item_separator = string_value(&args[5])
        .ok_or_else(|| type_error("make_encoder() argument 6 must be str"))?;
    // `bool(...)` honours `__bool__` for the flag arguments (CPython's
    // `PyObject_IsTrue` in `encoder_init`), so a `JSONEncoder(sort_keys=…)`
    // whose flag raises in `__bool__` propagates —
    // `test_speedups.TestEncode.test_bad_bool_args`.
    let (sort_keys, skipkeys, allow_nan) = with_interp(|interp| {
        Ok((
            interp.op_truth(&args[6])?,
            interp.op_truth(&args[7])?,
            interp.op_truth(&args[8])?,
        ))
    })?;
    let cfg = Rc::new(EncoderCfg {
        markers,
        default: args[1].clone(),
        encoder: args[2].clone(),
        native_encoder: encoders
            .iter()
            .position(|encoder| match (encoder, &args[2]) {
                (Object::Builtin(a), Object::Builtin(b)) => Rc::ptr_eq(a, b),
                _ => false,
            })
            .map(|i| i == 1),
        indent,
        key_separator,
        item_separator,
        sort_keys,
        skipkeys,
        allow_nan,
    });
    Ok(Object::Builtin(Rc::new(BuiltinFn {
        name: "encoder",
        binds_instance: false,
        call: Box::new(move |a| encoder_call(&cfg, a)),
        call_kw: None,
    })))
}

/// The encoder callable: `_iterencode(obj, _current_indent_level) -> list`.
fn encoder_call(cfg: &EncoderCfg, args: &[Object]) -> Result<Object, RuntimeError> {
    let obj = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("encoder missing 'obj' argument"))?;
    // `_current_indent_level` is a required `Py_ssize_t` (clinic): a
    // float or a missing argument is a TypeError, a negative level is
    // clamped by the indentation arithmetic
    // (test_speedups.test_current_indent_level).
    let level = match args.get(1) {
        Some(Object::Int(n)) => *n,
        Some(Object::Bool(bv)) => i64::from(*bv),
        Some(Object::Long(_)) => {
            return Err(overflow_error(
                "Python int too large to convert to C ssize_t",
            ))
        }
        Some(other) => {
            return Err(type_error(format!(
                "'{}' object cannot be interpreted as an integer",
                other.type_name_owned()
            )))
        }
        None => {
            return Err(type_error(
                "_iterencode() missing required argument '_current_indent_level' (pos 2)",
            ))
        }
    };
    with_interp(|interp| {
        let mut out = JsonWriter::default();
        let mut cycle: HashSet<usize> = HashSet::new();
        let track = !matches!(cfg.markers, Object::None);
        encode_value(
            interp,
            cfg,
            &obj,
            level.max(0) as usize,
            &mut out,
            track,
            &mut cycle,
        )?;
        // `_json.c` writes into one `PyUnicodeWriter` and returns
        // `(result,)`: a single joined chunk, as a tuple.
        Ok(Object::new_tuple_array([out.finish()]))
    })
}

#[derive(PartialEq)]
enum Kind {
    Str,
    Null,
    True,
    False,
    Int,
    Float,
    List,
    Dict,
    Other,
}

fn json_kind(o: &Object) -> Kind {
    match o {
        Object::Str(_) | Object::WStr(_) => Kind::Str,
        Object::None => Kind::Null,
        Object::Bool(true) => Kind::True,
        Object::Bool(false) => Kind::False,
        Object::Int(_) | Object::Long(_) => Kind::Int,
        Object::Float(_) => Kind::Float,
        Object::List(_) | Object::Tuple(_) => Kind::List,
        Object::Dict(_) => Kind::Dict,
        Object::Instance(_) => {
            let bt = builtin_types();
            let cls = class_of(o);
            if cls.is_subclass_of(&bt.str_) {
                Kind::Str
            } else if cls.is_subclass_of(&bt.int_) {
                Kind::Int
            } else if cls.is_subclass_of(&bt.float_) {
                Kind::Float
            } else if cls.is_subclass_of(&bt.list_) || cls.is_subclass_of(&bt.tuple_) {
                Kind::List
            } else if cls.is_subclass_of(&bt.dict_) {
                Kind::Dict
            } else {
                Kind::Other
            }
        }
        _ => Kind::Other,
    }
}

fn obj_id(o: &Object) -> usize {
    match o {
        Object::List(r) => Rc::as_ptr(r).cast::<()>() as usize,
        Object::Dict(r) => Rc::as_ptr(r).cast::<()>() as usize,
        Object::Tuple(r) => Rc::as_ptr(r).cast::<()>() as usize,
        Object::Instance(r) => Rc::as_ptr(r).cast::<()>() as usize,
        // Anything else routed through `default()` (a class, a module,
        // `Ellipsis`, ...) needs a real identity too: the `markers` cycle
        // check keys on it, and a shared placeholder made every second
        // such object a false "Circular reference detected"
        // (test_json.test_default.test_bad_default nests type → module →
        // list → Ellipsis → NotImplemented through `default`).
        _ => crate::weakref_registry::id_of(o) as usize,
    }
}

/// Call the captured string `encoder` and require it to return a `str`
/// (CPython's C encoder validates this — `test_speedups.test_bad_str_encoder`).
fn call_encoder(
    interp: &mut Interpreter,
    cfg: &EncoderCfg,
    s: &Object,
) -> Result<Object, RuntimeError> {
    let r = interp.call_object(cfg.encoder.clone(), std::slice::from_ref(s), &[])?;
    match &r {
        Object::Str(_) | Object::WStr(_) => Ok(r),
        Object::Instance(_) if class_of(&r).is_subclass_of(&builtin_types().str_) => Ok(r),
        _ => Err(type_error(format!(
            "encoder() must return a str, not {}",
            r.type_name_owned()
        ))),
    }
}

fn write_encoded_string(
    interp: &mut Interpreter,
    cfg: &EncoderCfg,
    s: &Object,
    out: &mut JsonWriter,
) -> Result<(), RuntimeError> {
    match (cfg.native_encoder, s) {
        (Some(ascii), Object::Str(s)) => out.quoted_utf8(s, ascii),
        (Some(ascii), Object::WStr(cps)) => out.quoted_wide(cps, ascii),
        _ => out.push_object(&call_encoder(interp, cfg, s)?)?,
    }
    Ok(())
}

fn intstr(interp: &mut Interpreter, o: &Object) -> Result<Object, RuntimeError> {
    match o {
        Object::Int(i) => Ok(Object::from_str(i.to_string())),
        Object::Long(_) => Ok(Object::from_str(interp.repr_object(o)?)),
        Object::Instance(_) => {
            let iv = interp.call_object(
                Object::Type(builtin_types().int_.clone()),
                std::slice::from_ref(o),
                &[],
            )?;
            intstr(interp, &iv)
        }
        _ => Ok(Object::from_str(interp.repr_object(o)?)),
    }
}

fn floatstr(interp: &mut Interpreter, o: &Object, allow_nan: bool) -> Result<Object, RuntimeError> {
    let x = match o {
        Object::Float(x) => *x,
        Object::Instance(_) => {
            let fv = interp.call_object(
                Object::Type(builtin_types().float_.clone()),
                std::slice::from_ref(o),
                &[],
            )?;
            match fv {
                Object::Float(x) => x,
                _ => return Err(type_error("expected float")),
            }
        }
        _ => return Err(type_error("expected float")),
    };
    if x.is_nan() {
        if !allow_nan {
            return Err(value_error(format!(
                "Out of range float values are not JSON compliant: {}",
                interp.repr_object(&Object::Float(x))?
            )));
        }
        return Ok(Object::from_static("NaN"));
    }
    if x.is_infinite() {
        if !allow_nan {
            return Err(value_error(format!(
                "Out of range float values are not JSON compliant: {}",
                interp.repr_object(&Object::Float(x))?
            )));
        }
        return Ok(Object::from_static(if x > 0.0 {
            "Infinity"
        } else {
            "-Infinity"
        }));
    }
    Ok(Object::from_str(interp.repr_object(&Object::Float(x))?))
}

fn list_items(interp: &mut Interpreter, o: &Object) -> Result<Vec<Object>, RuntimeError> {
    match o {
        Object::List(l) => Ok(l.borrow().clone()),
        Object::Tuple(t) => Ok(t.to_vec()),
        _ => {
            // list/tuple subclass instance — materialise via list(o)
            let lst = interp.call_object(
                Object::Type(builtin_types().list_.clone()),
                std::slice::from_ref(o),
                &[],
            )?;
            match lst {
                Object::List(l) => Ok(l.borrow().clone()),
                _ => Ok(Vec::new()),
            }
        }
    }
}

fn dict_items(interp: &mut Interpreter, o: &Object) -> Result<Vec<(Object, Object)>, RuntimeError> {
    // Exact `dict`: iterate the native map directly, preserving insertion
    // order (CPython's `PyDict_Next` fast path).
    if let Object::Dict(d) = o {
        let items = d
            .borrow()
            .iter()
            .map(|(k, v)| (k.0.clone(), v.clone()))
            .collect();
        return Ok(items);
    }
    // dict *subclass* / arbitrary mapping (e.g. `OrderedDict`): use
    // `o.items()` (CPython's `PyMapping_Items`) so the subclass's own
    // ordering is honoured — `dict(o)` would drop a `move_to_end`
    // reordering (`test_default.test_ordereddict`).
    let items_method = interp.load_attr_public(o, "items")?;
    let items_obj = interp.call_object(items_method, &[], &[])?;
    let listed = interp.call_object(
        Object::Type(builtin_types().list_.clone()),
        std::slice::from_ref(&items_obj),
        &[],
    )?;
    let mut out = Vec::new();
    if let Object::List(l) = listed {
        for it in l.borrow().iter() {
            match it {
                Object::Tuple(t) if t.len() == 2 => out.push((t[0].clone(), t[1].clone())),
                _ => {
                    let k = interp.accel_subscript(it, &Object::Int(0))?;
                    let v = interp.accel_subscript(it, &Object::Int(1))?;
                    out.push((k, v));
                }
            }
        }
    }
    Ok(out)
}

/// Recursion hub for the encoder. Guards Python call depth (so a
/// `default()` that keeps wrapping its argument raises `RecursionError`
/// rather than overflowing — `test_recursion.test_endless_recursion`) and
/// grows the native stack on demand so the limit, not the platform stack,
/// is the ceiling — exactly like the decoder and the interpreter loop.
#[allow(clippy::too_many_arguments)]
fn encode_value(
    interp: &mut Interpreter,
    cfg: &EncoderCfg,
    o: &Object,
    level: usize,
    out: &mut JsonWriter,
    track: bool,
    cycle: &mut HashSet<usize>,
) -> Result<(), RuntimeError> {
    let _depth_guard = match crate::recursion::enter() {
        crate::recursion::Enter::Ok(g) => g,
        crate::recursion::Enter::Overflow => {
            return Err(RuntimeError::PyException(PyException::new(
                crate::builtin_types::make_exception(
                    "RecursionError",
                    "maximum recursion depth exceeded while encoding a JSON object",
                ),
            )))
        }
    };
    stacker::maybe_grow(256 * 1024, 4 * 1024 * 1024, || {
        encode_value_impl(interp, cfg, o, level, out, track, cycle)
    })
}

#[allow(clippy::too_many_arguments)]
fn encode_value_impl(
    interp: &mut Interpreter,
    cfg: &EncoderCfg,
    o: &Object,
    level: usize,
    out: &mut JsonWriter,
    track: bool,
    cycle: &mut HashSet<usize>,
) -> Result<(), RuntimeError> {
    match json_kind(o) {
        Kind::Str => write_encoded_string(interp, cfg, o, out)?,
        Kind::Null => out.push_str("null"),
        Kind::True => out.push_str("true"),
        Kind::False => out.push_str("false"),
        Kind::Int => match o {
            Object::Int(value) => out.push_str(itoa::Buffer::new().format(*value)),
            _ => out.push_object(&intstr(interp, o)?)?,
        },
        Kind::Float => match o {
            Object::Float(value) if value.is_finite() => {
                crate::object::write_float_repr(out, *value).expect("JSON output cannot fail");
            }
            _ => out.push_object(&floatstr(interp, o, cfg.allow_nan)?)?,
        },
        Kind::List => {
            encode_list(interp, cfg, o, level, out, track, cycle)?;
        }
        Kind::Dict => {
            let items = dict_items(interp, o)?;
            encode_dict(interp, cfg, o, items, level, out, track, cycle)?;
        }
        Kind::Other => {
            let id = obj_id(o);
            if track && !cycle.insert(id) {
                return Err(value_error("Circular reference detected"));
            }
            let o2 = interp.call_object(cfg.default.clone(), std::slice::from_ref(o), &[])?;
            // The note covers a failure while encoding `default()`'s
            // result, not a failure of `default()` itself (`dumps(sys)`
            // carries no notes; test_fail.test_not_serializable).
            encode_value(interp, cfg, &o2, level, out, track, cycle).map_err(|e| {
                serializing_note(e, || {
                    format!("when serializing {} object", o.type_name_owned())
                })
            })?;
            if track {
                cycle.remove(&id);
            }
        }
    }
    Ok(())
}

/// 3.14 (gh-122163): `_json.c`'s `_PyErr_FormatNote("when serializing …")`
/// annotates an encoding failure with the container item or object it was
/// raised under, so a deep failure reports its path via PEP 678 notes.
fn serializing_note(err: RuntimeError, note: impl FnOnce() -> String) -> RuntimeError {
    if let RuntimeError::PyException(exc) = &err {
        exc.add_note(note());
    }
    err
}

#[allow(clippy::too_many_arguments)]
fn encode_list(
    interp: &mut Interpreter,
    cfg: &EncoderCfg,
    container: &Object,
    level: usize,
    out: &mut JsonWriter,
    track: bool,
    cycle: &mut HashSet<usize>,
) -> Result<(), RuntimeError> {
    // A native `list` is indexed *live* each iteration (re-reading its
    // current length), exactly like `_json.c`'s `PyList_GET_SIZE` loop, so a
    // `default()` that mutates the list mid-encode matches CPython
    // (`test_dump.test_encode_mutated`). Tuples are immutable and list/tuple
    // subclasses are snapshotted via `list(o)`.
    let snapshot: Option<Vec<Object>> = match container {
        Object::List(_) | Object::Tuple(_) => None,
        _ => Some(list_items(interp, container)?),
    };
    let current_len = |i: usize| -> Option<Object> {
        match container {
            Object::List(l) => {
                let b = l.borrow();
                (i < b.len()).then(|| b[i].clone())
            }
            Object::Tuple(t) => (i < t.len()).then(|| t[i].clone()),
            _ => snapshot.as_ref().and_then(|s| s.get(i).cloned()),
        }
    };

    if current_len(0).is_none() {
        out.push_str("[]");
        return Ok(());
    }
    let id = obj_id(container);
    if track && !cycle.insert(id) {
        return Err(value_error("Circular reference detected"));
    }
    let mut level = level;
    if cfg.indent.is_some() {
        level += 1;
    }
    out.push_str("[");
    if let Some(ind) = &cfg.indent {
        write_indent(out, ind, level)?;
    }
    let mut first = true;
    let mut i = 0;
    while let Some(value) = current_len(i) {
        if first {
            first = false;
        } else {
            out.push_object(&cfg.item_separator)?;
            if let Some(ind) = &cfg.indent {
                write_indent(out, ind, level)?;
            }
        }
        encode_value(interp, cfg, &value, level, out, track, cycle).map_err(|e| {
            serializing_note(e, || {
                format!("when serializing {} item {i}", container.type_name_owned())
            })
        })?;
        i += 1;
    }
    if let Some(ind) = &cfg.indent {
        level -= 1;
        write_indent(out, ind, level)?;
    }
    out.push_str("]");
    if track {
        cycle.remove(&id);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn encode_dict(
    interp: &mut Interpreter,
    cfg: &EncoderCfg,
    container: &Object,
    mut items: Vec<(Object, Object)>,
    level: usize,
    out: &mut JsonWriter,
    track: bool,
    cycle: &mut HashSet<usize>,
) -> Result<(), RuntimeError> {
    if items.is_empty() {
        out.push_str("{}");
        return Ok(());
    }
    let id = obj_id(container);
    if track && !cycle.insert(id) {
        return Err(value_error("Circular reference detected"));
    }
    out.push_str("{");
    let mut level = level;
    if cfg.indent.is_some() {
        level += 1;
    }

    if cfg.sort_keys {
        items = sort_items(interp, items)?;
    }

    let mut first = true;
    for (k, v) in items {
        // Coerce the key to a string, mirroring `_iterencode_dict`.
        let key_str: Object = match json_kind(&k) {
            Kind::Str => k.clone(),
            Kind::Float => floatstr(interp, &k, cfg.allow_nan)?,
            Kind::True => Object::from_static("true"),
            Kind::False => Object::from_static("false"),
            Kind::Null => Object::from_static("null"),
            Kind::Int => intstr(interp, &k)?,
            _ => {
                if cfg.skipkeys {
                    continue;
                }
                return Err(type_error(format!(
                    "keys must be str, int, float, bool or None, not {}",
                    k.type_name_owned()
                )));
            }
        };
        if first {
            first = false;
        } else {
            out.push_object(&cfg.item_separator)?;
        }
        if let Some(ind) = &cfg.indent {
            write_indent(out, ind, level)?;
        }
        write_encoded_string(interp, cfg, &key_str, out)?;
        out.push_object(&cfg.key_separator)?;
        // The note names the *original* key (`dict item 1`, not `'1'`).
        encode_value(interp, cfg, &v, level, out, track, cycle).map_err(|e| {
            serializing_note(e, || {
                format!(
                    "when serializing {} item {}",
                    container.type_name_owned(),
                    k.repr()
                )
            })
        })?;
    }
    if !first {
        if let Some(ind) = &cfg.indent {
            level -= 1;
            write_indent(out, ind, level)?;
        }
    }
    out.push_str("}");
    if track {
        cycle.remove(&id);
    }
    Ok(())
}

/// `sorted(dct.items())` — sort the (key, value) pairs by the default
/// comparison, raising `TypeError` for unorderable keys
/// (`test_speedups.test_unsortable_keys`).
fn sort_items(
    interp: &mut Interpreter,
    items: Vec<(Object, Object)>,
) -> Result<Vec<(Object, Object)>, RuntimeError> {
    let tuples: Vec<Object> = items
        .into_iter()
        .map(|(k, v)| Object::new_tuple_array([k, v]))
        .collect();
    let list = Object::new_list(tuples);
    let sort = interp.load_attr_public(&list, "sort")?;
    interp.call_object(sort, &[], &[])?;
    let Object::List(l) = &list else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for t in l.borrow().iter() {
        if let Object::Tuple(pair) = t {
            if pair.len() == 2 {
                out.push((pair[0].clone(), pair[1].clone()));
            }
        }
    }
    Ok(out)
}
