//! The `glob` built-in module.
//!
//! Implements `glob.glob`, `glob.iglob`, `glob.escape` on top of
//! the host filesystem. The pattern surface matches CPython's
//! `glob.glob(pattern, recursive=False)` — `*`, `?`, `[...]`, and
//! (with `recursive=True`) `**`.
//!
//! Internally we walk directories iteratively and apply the
//! per-segment `fnmatch` translation from `fnmatch_mod`.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::path::{Component, Path, PathBuf};

use regex::Regex;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::stdlib::fnmatch_mod::translate_pattern;

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("glob"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Filename globbing utility."),
        );
        d.insert(DictKey(Object::from_static("glob")), b("glob", glob_fn));
        d.insert(DictKey(Object::from_static("iglob")), b("iglob", glob_fn));
        d.insert(DictKey(Object::from_static("escape")), b("escape", escape));
        d.insert(
            DictKey(Object::from_static("has_magic")),
            b("has_magic", has_magic),
        );
    }
    Rc::new(PyModule {
        name: "glob".to_owned(),
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

fn glob_fn(args: &[Object]) -> Result<Object, RuntimeError> {
    let pattern = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("glob: pattern must be str")),
    };
    let recursive = match args.get(1) {
        Some(Object::Bool(b)) => *b,
        _ => false,
    };
    let mut out = Vec::new();
    walk_pattern(Path::new(&pattern), recursive, &mut out)?;
    Ok(Object::new_list(
        out.into_iter()
            .map(|p| Object::from_str(p.to_string_lossy().into_owned()))
            .collect(),
    ))
}

fn escape(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("escape: arg must be str")),
    };
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '*' | '?' | '[') {
            out.push('[');
            out.push(c);
            out.push(']');
        } else {
            out.push(c);
        }
    }
    Ok(Object::from_str(out))
}

fn has_magic(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("has_magic: arg must be str")),
    };
    Ok(Object::Bool(
        s.chars().any(|c| matches!(c, '*' | '?' | '[')),
    ))
}

fn walk_pattern(
    pattern: &Path,
    recursive: bool,
    out: &mut Vec<PathBuf>,
) -> Result<(), RuntimeError> {
    // Split the pattern into a literal base prefix (no magic chars)
    // and a list of magic-bearing segments. `PathBuf::push` handles
    // `Prefix` (Windows drive letters) and `RootDir` correctly, so
    // we just push every non-magic component verbatim until we hit
    // the first segment that needs globbing.
    let mut base = PathBuf::new();
    let mut segments: Vec<String> = Vec::new();
    let mut hit_magic = false;

    for c in pattern.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => {
                base.push(c.as_os_str());
            }
            Component::CurDir => {
                if hit_magic {
                    segments.push(".".to_owned());
                }
            }
            Component::ParentDir => {
                if hit_magic {
                    segments.push("..".to_owned());
                } else {
                    base.push("..");
                }
            }
            Component::Normal(s) => {
                let s = s.to_string_lossy().into_owned();
                let is_magic = has_magic_in(&s) || (recursive && s == "**");
                if hit_magic || is_magic {
                    hit_magic = true;
                    segments.push(s);
                } else {
                    base.push(&s);
                }
            }
        }
    }

    if base.as_os_str().is_empty() {
        base = PathBuf::from(".");
    }

    if segments.is_empty() {
        if base.exists() {
            out.push(strip_leading_dot(&base));
        }
        return Ok(());
    }

    let seg_refs: Vec<&str> = segments.iter().map(String::as_str).collect();
    expand_segments(base, &seg_refs, recursive, out);
    Ok(())
}

fn expand_segments(base: PathBuf, segments: &[&str], recursive: bool, out: &mut Vec<PathBuf>) {
    if segments.is_empty() {
        if base.exists() {
            // Trim leading "./" so the caller sees patterns identical
            // to what CPython produces.
            let cleaned = strip_leading_dot(&base);
            out.push(cleaned);
        }
        return;
    }
    let (seg, rest) = (segments[0], &segments[1..]);
    if seg.is_empty() {
        // Anchored to root; skip.
        expand_segments(base, rest, recursive, out);
        return;
    }
    if recursive && seg == "**" {
        // ** matches zero or more directory components.
        // Zero components: continue with `rest` from `base`.
        expand_segments(base.clone(), rest, recursive, out);
        // One-plus: for each descendant directory of `base`, retry.
        walk_descendants(&base, &mut |child| {
            expand_segments(child.to_path_buf(), rest, recursive, out);
        });
        return;
    }
    if has_magic_in(seg) {
        let re = match Regex::new(&translate_pattern(seg)) {
            Ok(r) => r,
            Err(_) => return,
        };
        if let Ok(entries) = std::fs::read_dir(&base) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if re.is_match(&name) {
                    expand_segments(base.join(name.as_ref()), rest, recursive, out);
                }
            }
        }
    } else {
        let next = base.join(seg);
        if rest.is_empty() {
            if next.exists() {
                out.push(strip_leading_dot(&next));
            }
        } else {
            expand_segments(next, rest, recursive, out);
        }
    }
}

fn walk_descendants(base: &Path, visit: &mut impl FnMut(&Path)) {
    if let Ok(entries) = std::fs::read_dir(base) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path);
                walk_descendants(&path, visit);
            }
        }
    }
}

fn has_magic_in(seg: &str) -> bool {
    seg.chars().any(|c| matches!(c, '*' | '?' | '['))
}

fn strip_leading_dot(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix("./") {
        PathBuf::from(rest)
    } else if s == "." {
        PathBuf::from("")
    } else {
        p.to_path_buf()
    }
}

fn _avoid_unused() {
    let _ = value_error("");
}
