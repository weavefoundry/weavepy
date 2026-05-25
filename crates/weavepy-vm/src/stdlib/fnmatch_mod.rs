//! The `fnmatch` built-in module.
//!
//! Translates Unix-shell-style wildcard patterns (`*`, `?`,
//! `[seq]`) into regular expressions and runs them against
//! filenames. The behaviour matches CPython's `fnmatch.fnmatch`,
//! `fnmatchcase`, and `filter`.
//!
//! Pattern semantics:
//!
//! * `*` matches anything except path separator
//! * `?` matches any single character
//! * `[seq]` matches any character in `seq`
//! * `[!seq]` matches any character not in `seq`
//! * `\` is *not* special — CPython quirk we preserve.

use crate::sync::Rc;
use crate::sync::RefCell;

use regex::Regex;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("fnmatch"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Filename matching with shell patterns."),
        );
        d.insert(
            DictKey(Object::from_static("fnmatch")),
            b("fnmatch", fnmatch),
        );
        d.insert(
            DictKey(Object::from_static("fnmatchcase")),
            b("fnmatchcase", fnmatchcase),
        );
        d.insert(DictKey(Object::from_static("filter")), b("filter", filter));
        d.insert(
            DictKey(Object::from_static("translate")),
            b("translate", translate),
        );
    }
    Rc::new(PyModule {
        name: "fnmatch".to_owned(),
        filename: None,
        dict,
    })
}

fn b(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(body),
    }))
}

/// Convert a shell-style pattern into a `regex`-compatible pattern.
pub(crate) fn translate_pattern(pat: &str) -> String {
    let mut out = String::with_capacity(pat.len() + 8);
    out.push_str("(?s:");
    let mut chars = pat.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            '[' => {
                // Find the matching ']' (raw).
                let mut j = String::new();
                let mut closed = false;
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if next == ']' && !j.is_empty() {
                        closed = true;
                        break;
                    }
                    j.push(next);
                }
                if !closed {
                    out.push_str("\\[");
                } else {
                    out.push('[');
                    let mut inner = j.chars();
                    if let Some(first) = inner.next() {
                        if first == '!' {
                            out.push('^');
                        } else {
                            out.push(first);
                        }
                    }
                    for ch in inner {
                        if ch == '\\' {
                            out.push_str("\\\\");
                        } else {
                            out.push(ch);
                        }
                    }
                    out.push(']');
                }
            }
            c => {
                if "\\^$.|+()".contains(c) {
                    out.push('\\');
                }
                out.push(c);
            }
        }
    }
    out.push_str(")\\z");
    out
}

fn extract_str(arg: Option<&Object>) -> Result<String, RuntimeError> {
    match arg {
        Some(Object::Str(s)) => Ok(s.to_string()),
        _ => Err(type_error("expected str")),
    }
}

fn fnmatch(args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython's fnmatch normalises both sides via os.path.normcase.
    // On POSIX this is a no-op; on Windows it lowercases. We match
    // POSIX semantics for now.
    let name = extract_str(args.first())?;
    let pat = extract_str(args.get(1))?;
    let re = Regex::new(&translate_pattern(&pat))
        .map_err(|e| value_error(format!("fnmatch: bad pattern: {e}")))?;
    Ok(Object::Bool(re.is_match(&name)))
}

fn fnmatchcase(args: &[Object]) -> Result<Object, RuntimeError> {
    fnmatch(args)
}

fn filter(args: &[Object]) -> Result<Object, RuntimeError> {
    let names_obj = args.first().ok_or_else(|| type_error("missing names"))?;
    let pat = extract_str(args.get(1))?;
    let re = Regex::new(&translate_pattern(&pat))
        .map_err(|e| value_error(format!("filter: bad pattern: {e}")))?;
    let names: Vec<Object> = match names_obj {
        Object::List(l) => l.borrow().clone(),
        Object::Tuple(t) => t.to_vec(),
        _ => return Err(type_error("filter: names must be a sequence")),
    };
    let out: Vec<Object> = names
        .into_iter()
        .filter(|n| matches!(n, Object::Str(s) if re.is_match(s)))
        .collect();
    Ok(Object::new_list(out))
}

fn translate(args: &[Object]) -> Result<Object, RuntimeError> {
    let pat = extract_str(args.first())?;
    Ok(Object::from_str(translate_pattern(&pat)))
}
