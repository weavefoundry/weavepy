//! The `_string` accelerator module — RFC 0023.
//!
//! Mirrors CPython's `_string`, used internally by `string.Formatter`.
//! Provides `formatter_field_name_split(field_name)` and
//! `formatter_parser(format_string)` so Python-side `string.Formatter`
//! can dispatch through Rust for the hot parsing loop.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_string"),
        );
        d.insert(
            DictKey(Object::from_static("formatter_field_name_split")),
            builtin("formatter_field_name_split", formatter_field_name_split),
        );
        d.insert(
            DictKey(Object::from_static("formatter_parser")),
            builtin("formatter_parser", formatter_parser),
        );
    }
    Rc::new(PyModule {
        name: "_string".to_owned(),
        filename: None,
        dict,
    })
}

fn builtin(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(body),
    }))
}

/// Parse a format string into `(literal_text, field_name, format_spec, conversion)` tuples.
/// Implements the same surface as `_string.formatter_parser` so
/// `string.Formatter.parse` can call into us.
fn formatter_parser(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("formatter_parser() argument must be str")),
    };
    let mut out: Vec<Object> = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut literal = String::new();
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'{' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                literal.push('{');
                i += 2;
                continue;
            }
            // Field. Find the matching `}` taking nesting into account.
            i += 1;
            let mut field = String::new();
            let mut depth = 1i32;
            while i < bytes.len() && depth > 0 {
                let ch = bytes[i] as char;
                if ch == '{' {
                    depth += 1;
                    field.push(ch);
                } else if ch == '}' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    field.push(ch);
                } else {
                    field.push(ch);
                }
                i += 1;
            }
            if i >= bytes.len() {
                return Err(value_error("expected '}' before end of string"));
            }
            i += 1; // consume the `}`
                    // Split field into name + format_spec + conversion.
            let (field_name, format_spec, conversion) = split_field(&field);
            out.push(Object::new_tuple(vec![
                Object::from_str(std::mem::take(&mut literal)),
                Object::from_str(field_name),
                Object::from_str(format_spec),
                match conversion {
                    Some(c) => Object::from_str(c.to_string()),
                    None => Object::None,
                },
            ]));
        } else if c == b'}' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'}' {
                literal.push('}');
                i += 2;
                continue;
            }
            return Err(value_error("single '}' encountered in format string"));
        } else {
            literal.push(c as char);
            i += 1;
        }
    }
    if !literal.is_empty() {
        out.push(Object::new_tuple(vec![
            Object::from_str(literal),
            Object::None,
            Object::None,
            Object::None,
        ]));
    }
    Ok(Object::new_list(out))
}

fn split_field(field: &str) -> (String, String, Option<char>) {
    // Format: name[!conversion][:format_spec]
    // Conversion comes before format_spec.
    let mut name = String::new();
    let mut conv: Option<char> = None;
    let mut spec = String::new();
    let mut state = 0u8;
    let mut depth = 0i32;
    for ch in field.chars() {
        match state {
            0 => match ch {
                '{' => {
                    depth += 1;
                    name.push(ch);
                }
                '}' => {
                    depth -= 1;
                    name.push(ch);
                }
                '!' if depth == 0 => state = 1,
                ':' if depth == 0 => state = 2,
                _ => name.push(ch),
            },
            1 => {
                if conv.is_none() {
                    conv = Some(ch);
                } else if ch == ':' {
                    state = 2;
                }
            }
            2 => spec.push(ch),
            _ => {}
        }
    }
    (name, spec, conv)
}

/// Parse `obj.attr[idx][2]` into the leading name + an iterator of
/// (is_attr, value) pairs. Matches CPython's `_string.formatter_field_name_split`.
fn formatter_field_name_split(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => {
            return Err(type_error(
                "formatter_field_name_split() argument must be str",
            ))
        }
    };
    // Leading first/auto: either an identifier (string) or a number.
    let mut first = String::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c == '.' || c == '[' {
            break;
        }
        first.push(c);
        chars.next();
    }
    let head: Object = if !first.is_empty() && first.chars().all(|c| c.is_ascii_digit()) {
        Object::Int(first.parse().unwrap_or(0))
    } else {
        Object::from_str(first)
    };
    // Iterator of (is_attribute, value).
    let mut pieces: Vec<Object> = Vec::new();
    while let Some(c) = chars.next() {
        if c == '.' {
            let mut buf = String::new();
            while let Some(&nc) = chars.peek() {
                if nc == '.' || nc == '[' {
                    break;
                }
                buf.push(nc);
                chars.next();
            }
            pieces.push(Object::new_tuple(vec![
                Object::Bool(true),
                Object::from_str(buf),
            ]));
        } else if c == '[' {
            let mut buf = String::new();
            while let Some(&nc) = chars.peek() {
                if nc == ']' {
                    break;
                }
                buf.push(nc);
                chars.next();
            }
            chars.next(); // ']'
            let val: Object = if buf.chars().all(|c| c.is_ascii_digit()) && !buf.is_empty() {
                Object::Int(buf.parse().unwrap_or(0))
            } else {
                Object::from_str(buf)
            };
            pieces.push(Object::new_tuple(vec![Object::Bool(false), val]));
        }
    }
    Ok(Object::new_tuple(vec![head, Object::new_list(pieces)]))
}
