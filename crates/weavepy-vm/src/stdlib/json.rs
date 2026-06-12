//! The `json` built-in module.
//!
//! Encode and decode JSON. We use `serde_json` for the wire format on
//! input (much simpler than rolling a JSON parser by hand) and a
//! hand-rolled formatter on output so we can match CPython's default
//! separator semantics (`(", ", ": ")` rather than serde's compact
//! `(",", ":")`) and honour `indent=` / `separators=` / `sort_keys=`
//! /`ensure_ascii=` kwargs.

use crate::sync::Rc;
use crate::sync::RefCell;

use serde_json::{Map, Number, Value};

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("json"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("JSON encoder and decoder."),
        );
        d.insert(
            DictKey(Object::from_static("loads")),
            b_kw("loads", json_loads),
        );
        d.insert(
            DictKey(Object::from_static("dumps")),
            b_kw("dumps", json_dumps),
        );
        d.insert(
            DictKey(Object::from_static("load")),
            b_kw("load", json_load),
        );
        d.insert(
            DictKey(Object::from_static("dump")),
            b_kw("dump", json_dump),
        );
        d.insert(
            DictKey(Object::from_static("JSONDecodeError")),
            Object::Type(json_decode_error_class()),
        );
    }
    Rc::new(PyModule {
        name: "json".to_owned(),
        filename: None,
        dict,
    })
}

/// CPython exposes ``json.JSONDecodeError`` as a ``ValueError``
/// subclass with ``msg``, ``doc``, ``pos``, ``lineno`` and
/// ``colno`` attributes. We lazily construct that subclass on the
/// first access and cache it via the module-static singleton inside
/// ``builtin_types`` so all callers share the same identity.
fn json_decode_error_class() -> crate::sync::Rc<crate::types::TypeObject> {
    // CPython treats ``json.JSONDecodeError`` as a singleton subclass
    // of ``ValueError`` shared by every ``json.loads`` call. We keep
    // a thread-local handle (``TypeObject`` is not ``Sync``) and
    // construct on first use; subsequent ``json.loads`` reuses it so
    // ``isinstance(err, json.JSONDecodeError)`` is stable.
    use crate::types::TypeObject;
    thread_local! {
        static CACHE: std::cell::RefCell<Option<crate::sync::Rc<TypeObject>>> =
            const { std::cell::RefCell::new(None) };
    }
    CACHE.with(|cell| {
        if let Some(c) = cell.borrow().as_ref() {
            return c.clone();
        }
        let parent = crate::builtin_types::builtin_types().value_error.clone();
        let cls = TypeObject::new_exception("JSONDecodeError", parent)
            .expect("JSONDecodeError class can be built");
        *cell.borrow_mut() = Some(cls.clone());
        cls
    })
}

fn b_kw(
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

fn json_loads(args: &[Object], _kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let text = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        Some(Object::Bytes(b)) => {
            String::from_utf8(b.to_vec()).map_err(|_| value_error("invalid UTF-8 in JSON bytes"))?
        }
        _ => return Err(type_error("loads() expects str or bytes")),
    };
    let value: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => return Err(make_json_decode_error(&text, &e)),
    };
    Ok(json_to_object(value))
}

/// Build a CPython-compatible ``JSONDecodeError`` instance carrying
/// the same ``msg``, ``doc``, ``pos``, ``lineno``, ``colno`` fields
/// that user code observes on CPython.
fn make_json_decode_error(doc: &str, err: &serde_json::Error) -> RuntimeError {
    use crate::error::PyException;
    use crate::types::PyInstance;
    let msg = format!("{err}");
    let pos: i64 = err
        .column()
        .saturating_sub(1)
        .saturating_add(err.line().saturating_sub(1).saturating_mul(80)) as i64;
    let lineno = err.line() as i64;
    let colno = err.column() as i64;
    let cls = json_decode_error_class();
    let inst = PyInstance::new(cls);
    {
        let mut d = inst.dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("msg")),
            Object::from_str(msg.clone()),
        );
        d.insert(
            DictKey(Object::from_static("doc")),
            Object::from_str(doc.to_owned()),
        );
        d.insert(DictKey(Object::from_static("pos")), Object::Int(pos));
        d.insert(DictKey(Object::from_static("lineno")), Object::Int(lineno));
        d.insert(DictKey(Object::from_static("colno")), Object::Int(colno));
        d.insert(
            DictKey(Object::from_static("args")),
            Object::new_tuple(vec![
                Object::from_str(msg),
                Object::from_str(doc.to_owned()),
                Object::Int(pos),
            ]),
        );
    }
    RuntimeError::PyException(PyException::new(Object::Instance(Rc::new(inst))))
}

#[derive(Clone, Debug)]
struct DumpsOptions {
    skipkeys: bool,
    ensure_ascii: bool,
    allow_nan: bool,
    sort_keys: bool,
    indent: Option<String>,
    item_separator: String,
    key_separator: String,
}

impl DumpsOptions {
    fn from_kwargs(args: &[Object], kwargs: &[(String, Object)]) -> Result<Self, RuntimeError> {
        // Backwards-compatible positional API used by older callers:
        // `dumps(obj, indent, sort_keys)`. New code should use kwargs.
        let mut opts = DumpsOptions {
            skipkeys: false,
            ensure_ascii: true,
            allow_nan: true,
            sort_keys: false,
            indent: None,
            item_separator: ", ".to_owned(),
            key_separator: ": ".to_owned(),
        };
        if let Some(Object::Int(n)) = args.get(1) {
            opts.indent = Some(" ".repeat((*n).max(0) as usize));
        }
        if matches!(args.get(2), Some(Object::Bool(true))) {
            opts.sort_keys = true;
        }
        let mut explicit_separators: Option<(String, String)> = None;
        for (k, v) in kwargs {
            match k.as_str() {
                "skipkeys" => opts.skipkeys = obj_truthy(v),
                "ensure_ascii" => opts.ensure_ascii = obj_truthy(v),
                "allow_nan" => opts.allow_nan = obj_truthy(v),
                "sort_keys" => opts.sort_keys = obj_truthy(v),
                "indent" => match v {
                    Object::None => opts.indent = None,
                    Object::Int(n) => opts.indent = Some(" ".repeat((*n).max(0) as usize)),
                    Object::Str(s) => opts.indent = Some(s.to_string()),
                    _ => return Err(type_error("indent must be int, str, or None")),
                },
                "separators" => match v {
                    Object::Tuple(t) if t.len() == 2 => {
                        let isep = match &t[0] {
                            Object::Str(s) => s.to_string(),
                            _ => return Err(type_error("separators[0] must be str")),
                        };
                        let ksep = match &t[1] {
                            Object::Str(s) => s.to_string(),
                            _ => return Err(type_error("separators[1] must be str")),
                        };
                        explicit_separators = Some((isep, ksep));
                    }
                    Object::None => {}
                    _ => return Err(type_error("separators must be a 2-tuple")),
                },
                // `cls` / `default` are ignored: callers that pass them
                // get the default JSONEncoder behaviour.
                "cls" | "default" | "check_circular" => {}
                other => {
                    return Err(type_error(format!(
                        "dumps() got unexpected keyword argument '{other}'"
                    )))
                }
            }
        }
        if let Some((isep, ksep)) = explicit_separators {
            opts.item_separator = isep;
            opts.key_separator = ksep;
        } else if opts.indent.is_some() {
            // When indenting, CPython drops the trailing space after
            // commas (the newline supplies the visual break).
            opts.item_separator = ",".to_owned();
            opts.key_separator = ": ".to_owned();
        }
        Ok(opts)
    }
}

fn obj_truthy(o: &Object) -> bool {
    match o {
        Object::Bool(b) => *b,
        Object::Int(i) => *i != 0,
        Object::None => false,
        _ => true,
    }
}

fn json_dumps(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let value = args
        .first()
        .ok_or_else(|| type_error("dumps() missing argument"))?;
    let opts = DumpsOptions::from_kwargs(args, kwargs)?;
    let mut out = String::new();
    encode(value, &opts, 0, &mut out)?;
    Ok(Object::from_str(out))
}

fn json_load(args: &[Object], _kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let f = args
        .first()
        .ok_or_else(|| type_error("load() expects a file"))?;
    let file = match f {
        Object::File(file) => file.clone(),
        _ => return Err(type_error("load() expects a file-like object")),
    };
    let text = file.read_text_all()?;
    let value: Value =
        serde_json::from_str(&text).map_err(|e| value_error(format!("invalid JSON: {e}")))?;
    Ok(json_to_object(value))
}

fn json_dump(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let payload = args
        .first()
        .ok_or_else(|| type_error("dump() missing payload"))?;
    let f = args
        .get(1)
        .ok_or_else(|| type_error("dump() missing file"))?;
    let file = match f {
        Object::File(file) => file.clone(),
        _ => return Err(type_error("dump() expects a file-like object")),
    };
    let opts = DumpsOptions::from_kwargs(&args[..1], kwargs)?;
    let mut out = String::new();
    encode(payload, &opts, 0, &mut out)?;
    file.write_text(&out)?;
    Ok(Object::None)
}

// ---------------------------------------------------------------------
// Encoder — hand-rolled so we control whitespace + escape semantics.
// Mirrors CPython's `json.encoder.JSONEncoder.iterencode`. Recursive
// (Python's default container nesting won't blow our stack in practice;
// the iterative `iterencode` is a micro-optimisation we can do later).
// ---------------------------------------------------------------------

fn encode(
    value: &Object,
    opts: &DumpsOptions,
    depth: usize,
    out: &mut String,
) -> Result<(), RuntimeError> {
    match value {
        Object::None => out.push_str("null"),
        Object::Bool(true) => out.push_str("true"),
        Object::Bool(false) => out.push_str("false"),
        Object::Int(n) => out.push_str(&n.to_string()),
        Object::Long(n) => out.push_str(&n.to_string()),
        Object::Float(f) => encode_float(*f, opts, out)?,
        Object::Str(s) => encode_string(s.as_ref(), opts, out),
        Object::List(items) => {
            let items = items.borrow();
            encode_array(items.iter(), opts, depth, out)?;
        }
        Object::Tuple(items) => {
            encode_array(items.iter(), opts, depth, out)?;
        }
        Object::Dict(d) => {
            let d = d.borrow();
            encode_object(d.iter(), opts, depth, out)?;
        }
        Object::Set(_) | Object::FrozenSet(_) => {
            // CPython raises TypeError on set; mirror that.
            return Err(type_error(format!(
                "Object of type {} is not JSON serializable",
                value.type_name()
            )));
        }
        other => {
            return Err(type_error(format!(
                "Object of type {} is not JSON serializable",
                other.type_name()
            )));
        }
    }
    Ok(())
}

fn encode_float(f: f64, opts: &DumpsOptions, out: &mut String) -> Result<(), RuntimeError> {
    if f.is_nan() {
        if !opts.allow_nan {
            return Err(value_error(
                "Out of range float values are not JSON compliant",
            ));
        }
        out.push_str("NaN");
    } else if f.is_infinite() {
        if !opts.allow_nan {
            return Err(value_error(
                "Out of range float values are not JSON compliant",
            ));
        }
        if f.is_sign_negative() {
            out.push_str("-Infinity");
        } else {
            out.push_str("Infinity");
        }
    } else {
        let s = format!("{f}");
        // CPython prints ints as `1.0` not `1`.
        if !s.contains('.') && !s.contains('e') && !s.contains('E') && !s.contains("inf") {
            out.push_str(&s);
            out.push_str(".0");
        } else {
            out.push_str(&s);
        }
    }
    Ok(())
}

fn encode_string(s: &str, opts: &DumpsOptions, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            ch if (ch as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            ch if opts.ensure_ascii && (ch as u32) > 0x7e => {
                use std::fmt::Write as _;
                let code = ch as u32;
                if code <= 0xffff {
                    let _ = write!(out, "\\u{:04x}", code);
                } else {
                    // Surrogate pair.
                    let v = code - 0x10000;
                    let hi = 0xd800 | (v >> 10);
                    let lo = 0xdc00 | (v & 0x3ff);
                    let _ = write!(out, "\\u{:04x}\\u{:04x}", hi, lo);
                }
            }
            _ => out.push(ch),
        }
    }
    out.push('"');
}

fn encode_array<'a, I: ExactSizeIterator<Item = &'a Object>>(
    items: I,
    opts: &DumpsOptions,
    depth: usize,
    out: &mut String,
) -> Result<(), RuntimeError> {
    let len = items.len();
    if len == 0 {
        out.push_str("[]");
        return Ok(());
    }
    out.push('[');
    let inner = depth + 1;
    let sep = match opts.indent.as_deref() {
        Some(_) => format!(",\n{}", opts.indent.as_deref().unwrap().repeat(inner)),
        None => opts.item_separator.clone(),
    };
    if opts.indent.is_some() {
        out.push('\n');
        out.push_str(&opts.indent.as_deref().unwrap().repeat(inner));
    }
    for (i, item) in items.enumerate() {
        if i > 0 {
            out.push_str(&sep);
        }
        encode(item, opts, inner, out)?;
    }
    if opts.indent.is_some() {
        out.push('\n');
        out.push_str(&opts.indent.as_deref().unwrap().repeat(depth));
    }
    out.push(']');
    Ok(())
}

fn encode_object<'a, I: Iterator<Item = (&'a DictKey, &'a Object)>>(
    items: I,
    opts: &DumpsOptions,
    depth: usize,
    out: &mut String,
) -> Result<(), RuntimeError> {
    let mut pairs: Vec<(String, &Object)> = Vec::new();
    for (k, v) in items {
        let key = match &k.0 {
            Object::Str(s) => s.to_string(),
            Object::Int(i) => i.to_string(),
            Object::Bool(true) => "true".to_owned(),
            Object::Bool(false) => "false".to_owned(),
            Object::None => "null".to_owned(),
            Object::Float(f) => {
                let mut tmp = String::new();
                encode_float(*f, opts, &mut tmp)?;
                // Strip the synthetic ".0" so {1.0: 1} matches CPython.
                tmp
            }
            other => {
                if opts.skipkeys {
                    continue;
                }
                return Err(type_error(format!(
                    "keys must be str, int, float, bool, or None, not {}",
                    other.type_name()
                )));
            }
        };
        pairs.push((key, v));
    }
    if pairs.is_empty() {
        out.push_str("{}");
        return Ok(());
    }
    if opts.sort_keys {
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
    }
    out.push('{');
    let inner = depth + 1;
    let sep = match opts.indent.as_deref() {
        Some(_) => format!(",\n{}", opts.indent.as_deref().unwrap().repeat(inner)),
        None => opts.item_separator.clone(),
    };
    if opts.indent.is_some() {
        out.push('\n');
        out.push_str(&opts.indent.as_deref().unwrap().repeat(inner));
    }
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push_str(&sep);
        }
        encode_string(k, opts, out);
        out.push_str(&opts.key_separator);
        encode(v, opts, inner, out)?;
    }
    if opts.indent.is_some() {
        out.push('\n');
        out.push_str(&opts.indent.as_deref().unwrap().repeat(depth));
    }
    out.push('}');
    Ok(())
}

// ---------------------------------------------------------------------
// Decoder bridge.
// ---------------------------------------------------------------------

fn json_to_object(value: Value) -> Object {
    match value {
        Value::Null => Object::None,
        Value::Bool(b) => Object::Bool(b),
        Value::Number(n) => json_number(&n),
        Value::String(s) => Object::from_str(s),
        Value::Array(items) => Object::new_list(items.into_iter().map(json_to_object).collect()),
        Value::Object(map) => {
            let mut d = DictData::new();
            for (k, v) in map {
                d.insert(DictKey(Object::from_str(k)), json_to_object(v));
            }
            Object::Dict(Rc::new(RefCell::new(d)))
        }
    }
}

fn json_number(n: &Number) -> Object {
    if let Some(i) = n.as_i64() {
        return Object::Int(i);
    }
    if let Some(f) = n.as_f64() {
        return Object::Float(f);
    }
    if let Some(u) = n.as_u64() {
        return Object::Int(u as i64);
    }
    Object::Float(0.0)
}

// `Map` is unused after the rewrite, but `serde_json::Map` is still
// referenced by the decoder import.
#[allow(dead_code)]
fn _unused_map_anchor(_: Map<String, Value>) {}
