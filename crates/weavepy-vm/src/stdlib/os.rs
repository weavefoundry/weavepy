//! The `os` built-in module plus its `os.path` sub-module.
//!
//! Tracks CPython 3.13's `os` and `os.path` for the cross-platform
//! subset we need to bootstrap real scripts. The functions defer to
//! Rust's `std::env` and `std::path` so behaviour matches the host
//! OS — `os.sep` is `/` on POSIX and `\` on Windows, `os.linesep` is
//! `\n` / `\r\n` accordingly, etc.
//!
//! Anything that mutates host state (`os.chdir`, `os.environ` writes
//! propagating to spawned processes) is intentionally absent until
//! we have a clear story for sandboxing.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::error::{os_error, type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(cache: &ModuleCache) -> Rc<PyModule> {
    // `os.path` is a *separate* module that also gets cached in
    // `sys.modules` as `"os.path"` so that `import os.path` works.
    // Eagerly install it here.
    let path_mod = build_path(cache);
    cache.insert("os.path", Object::Module(path_mod.clone()));

    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("os"),
        );
        d.insert(
            DictKey(Object::from_static("__package__")),
            Object::from_static(""),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("OS routines for the host platform."),
        );

        d.insert(
            DictKey(Object::from_static("sep")),
            Object::from_static(if cfg!(windows) { "\\" } else { "/" }),
        );
        d.insert(
            DictKey(Object::from_static("linesep")),
            Object::from_static(if cfg!(windows) { "\r\n" } else { "\n" }),
        );
        d.insert(
            DictKey(Object::from_static("name")),
            Object::from_static(if cfg!(windows) { "nt" } else { "posix" }),
        );
        d.insert(
            DictKey(Object::from_static("pathsep")),
            Object::from_static(if cfg!(windows) { ";" } else { ":" }),
        );
        d.insert(
            DictKey(Object::from_static("curdir")),
            Object::from_static("."),
        );
        d.insert(
            DictKey(Object::from_static("pardir")),
            Object::from_static(".."),
        );
        d.insert(
            DictKey(Object::from_static("path")),
            Object::Module(path_mod),
        );
        d.insert(DictKey(Object::from_static("environ")), initial_environ());

        d.insert(
            DictKey(Object::from_static("getcwd")),
            builtin("getcwd", os_getcwd),
        );
        d.insert(
            DictKey(Object::from_static("getenv")),
            builtin("getenv", os_getenv),
        );
        d.insert(
            DictKey(Object::from_static("getpid")),
            builtin("getpid", os_getpid),
        );
        d.insert(
            DictKey(Object::from_static("remove")),
            builtin("remove", os_remove),
        );
        d.insert(
            DictKey(Object::from_static("unlink")),
            builtin("unlink", os_remove),
        );
        d.insert(
            DictKey(Object::from_static("mkdir")),
            builtin("mkdir", os_mkdir),
        );
        d.insert(
            DictKey(Object::from_static("makedirs")),
            builtin("makedirs", os_makedirs),
        );
        d.insert(
            DictKey(Object::from_static("rmdir")),
            builtin("rmdir", os_rmdir),
        );
        d.insert(
            DictKey(Object::from_static("rename")),
            builtin("rename", os_rename),
        );
        d.insert(
            DictKey(Object::from_static("listdir")),
            builtin("listdir", os_listdir),
        );
        d.insert(
            DictKey(Object::from_static("urandom")),
            builtin("urandom", os_urandom),
        );
        d.insert(
            DictKey(Object::from_static("close")),
            builtin("close", os_close_stub),
        );
        d.insert(
            DictKey(Object::from_static("open")),
            builtin("open", os_open_stub),
        );
        d.insert(
            DictKey(Object::from_static("stat")),
            builtin("stat", os_stat_stub),
        );
        d.insert(DictKey(Object::from_static("O_RDONLY")), Object::Int(0));
        d.insert(DictKey(Object::from_static("O_WRONLY")), Object::Int(1));
        d.insert(DictKey(Object::from_static("O_RDWR")), Object::Int(2));
        d.insert(DictKey(Object::from_static("O_CREAT")), Object::Int(64));
        d.insert(DictKey(Object::from_static("O_EXCL")), Object::Int(128));
        d.insert(DictKey(Object::from_static("O_TRUNC")), Object::Int(512));
        d.insert(DictKey(Object::from_static("O_APPEND")), Object::Int(1024));
    }
    Rc::new(PyModule {
        name: "os".to_owned(),
        filename: None,
        dict,
    })
}

pub fn build_path(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("os.path"),
        );
        d.insert(
            DictKey(Object::from_static("__package__")),
            Object::from_static("os"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Operations on pathnames."),
        );
        d.insert(
            DictKey(Object::from_static("sep")),
            Object::from_static(if cfg!(windows) { "\\" } else { "/" }),
        );

        d.insert(
            DictKey(Object::from_static("join")),
            builtin("join", path_join),
        );
        d.insert(
            DictKey(Object::from_static("split")),
            builtin("split", path_split),
        );
        d.insert(
            DictKey(Object::from_static("splitext")),
            builtin("splitext", path_splitext),
        );
        d.insert(
            DictKey(Object::from_static("basename")),
            builtin("basename", path_basename),
        );
        d.insert(
            DictKey(Object::from_static("dirname")),
            builtin("dirname", path_dirname),
        );
        d.insert(
            DictKey(Object::from_static("exists")),
            builtin("exists", path_exists),
        );
        d.insert(
            DictKey(Object::from_static("isfile")),
            builtin("isfile", path_isfile),
        );
        d.insert(
            DictKey(Object::from_static("isdir")),
            builtin("isdir", path_isdir),
        );
        d.insert(
            DictKey(Object::from_static("abspath")),
            builtin("abspath", path_abspath),
        );
        d.insert(
            DictKey(Object::from_static("normpath")),
            builtin("normpath", path_normpath),
        );
    }
    Rc::new(PyModule {
        name: "os.path".to_owned(),
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

fn initial_environ() -> Object {
    let mut d = DictData::new();
    for (k, v) in std::env::vars() {
        d.insert(DictKey(Object::from_str(k)), Object::from_str(v));
    }
    Object::Dict(Rc::new(RefCell::new(d)))
}

fn os_getcwd(_args: &[Object]) -> Result<Object, RuntimeError> {
    let cwd = std::env::current_dir().map_err(|e| os_error(format!("getcwd: {e}")))?;
    Ok(Object::from_str(cwd.to_string_lossy().into_owned()))
}

fn os_getenv(args: &[Object]) -> Result<Object, RuntimeError> {
    let key = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("getenv() first arg must be str")),
    };
    let default = args.get(1).cloned().unwrap_or(Object::None);
    Ok(std::env::var_os(&key).map_or(default, |v| {
        Object::from_str(v.to_string_lossy().into_owned())
    }))
}

fn os_getpid(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(i64::from(std::process::id())))
}

fn os_remove(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = first_path(args, "remove")?;
    std::fs::remove_file(&p).map_err(|e| crate::error::io_error_to_py(&e))?;
    Ok(Object::None)
}

fn os_mkdir(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = first_path(args, "mkdir")?;
    std::fs::create_dir(&p).map_err(|e| crate::error::io_error_to_py(&e))?;
    Ok(Object::None)
}

fn os_makedirs(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = first_path(args, "makedirs")?;
    std::fs::create_dir_all(&p).map_err(|e| crate::error::io_error_to_py(&e))?;
    Ok(Object::None)
}

fn os_rmdir(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = first_path(args, "rmdir")?;
    std::fs::remove_dir(&p).map_err(|e| crate::error::io_error_to_py(&e))?;
    Ok(Object::None)
}

fn os_rename(args: &[Object]) -> Result<Object, RuntimeError> {
    let src = first_path(args, "rename")?;
    let dst = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("rename() second arg must be str")),
    };
    std::fs::rename(&src, &dst).map_err(|e| crate::error::io_error_to_py(&e))?;
    Ok(Object::None)
}

fn os_listdir(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        None => ".".to_string(),
        _ => return Err(type_error("listdir() arg must be str")),
    };
    let mut out = Vec::new();
    let iter = std::fs::read_dir(&p).map_err(|e| crate::error::io_error_to_py(&e))?;
    for entry in iter {
        let entry = entry.map_err(|e| crate::error::io_error_to_py(&e))?;
        out.push(Object::from_str(
            entry.file_name().to_string_lossy().into_owned(),
        ));
    }
    Ok(Object::new_list(out))
}

fn os_urandom(args: &[Object]) -> Result<Object, RuntimeError> {
    let n = match args.first() {
        Some(Object::Int(n)) => *n as usize,
        _ => return Err(type_error("urandom() arg must be int")),
    };
    #[cfg(unix)]
    {
        use std::io::Read;
        let mut out = vec![0u8; n];
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            if f.read_exact(&mut out).is_ok() {
                return Ok(Object::new_bytes(out));
            }
        }
        // Fallback if /dev/urandom isn't readable.
        for b in out.iter_mut() {
            *b = (std::process::id() as u8).wrapping_add(*b);
        }
        Ok(Object::new_bytes(out))
    }
    #[cfg(not(unix))]
    {
        let mut out = vec![0u8; n];
        for (i, b) in out.iter_mut().enumerate() {
            *b = ((std::process::id() as usize + i) & 0xff) as u8;
        }
        Ok(Object::new_bytes(out))
    }
}

fn os_close_stub(_args: &[Object]) -> Result<Object, RuntimeError> {
    // We don't expose raw fds yet; `close(fd)` is a no-op for the
    // string-shaped tokens we hand out from mkstemp.
    Ok(Object::None)
}

fn os_open_stub(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(crate::error::not_implemented_error(
        "os.open(): raw fd interface is not implemented in WeavePy yet",
    ))
}

fn os_stat_stub(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = first_path(args, "stat")?;
    let meta = std::fs::metadata(&p).map_err(|e| crate::error::io_error_to_py(&e))?;
    let mut d = DictData::new();
    d.insert(
        DictKey(Object::from_static("st_size")),
        Object::Int(meta.len() as i64),
    );
    d.insert(
        DictKey(Object::from_static("st_mode")),
        Object::Int(if meta.is_dir() { 0o040_755 } else { 0o100_644 }),
    );
    d.insert(
        DictKey(Object::from_static("st_mtime")),
        meta.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(Object::Float(0.0), |d| Object::Float(d.as_secs_f64())),
    );
    Ok(Object::Dict(Rc::new(RefCell::new(d))))
}

// ---------- os.path ----------

fn as_str(obj: &Object, func: &str) -> Result<String, RuntimeError> {
    match obj {
        Object::Str(s) => Ok(s.to_string()),
        _ => Err(type_error(format!(
            "{func}() argument must be str, not '{}'",
            obj.type_name()
        ))),
    }
}

fn path_join(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut path = PathBuf::new();
    for (i, arg) in args.iter().enumerate() {
        let s = as_str(arg, "join")?;
        if i == 0 {
            path.push(&s);
        } else {
            let p = Path::new(&s);
            if p.is_absolute() {
                path = p.to_path_buf();
            } else {
                path.push(p);
            }
        }
    }
    Ok(Object::from_str(path.to_string_lossy().into_owned()))
}

fn path_split(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "split")?;
    let p = PathBuf::from(&s);
    let head = p
        .parent()
        .map_or(String::new(), |x| x.to_string_lossy().into_owned());
    let tail = p
        .file_name()
        .map_or(String::new(), |x| x.to_string_lossy().into_owned());
    Ok(Object::new_tuple(vec![
        Object::from_str(head),
        Object::from_str(tail),
    ]))
}

fn path_splitext(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "splitext")?;
    if let Some(dot) = find_ext_dot(&s) {
        let (root, ext) = s.split_at(dot);
        Ok(Object::new_tuple(vec![
            Object::from_str(root.to_owned()),
            Object::from_str(ext.to_owned()),
        ]))
    } else {
        Ok(Object::new_tuple(vec![
            Object::from_str(s),
            Object::from_static(""),
        ]))
    }
}

/// Mirror CPython's `os.path.splitext`: split on the *last* dot, but
/// only when that dot follows a non-dot character (`.profile` keeps
/// the leading dot).
fn find_ext_dot(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = bytes.len();
    while i > 0 {
        i -= 1;
        let c = bytes[i];
        if c == b'/' || (cfg!(windows) && c == b'\\') {
            return None;
        }
        if c == b'.' {
            // Skip leading-dot files (`.bashrc`) and dot runs.
            if i == 0 {
                return None;
            }
            let prev = bytes[i - 1];
            if prev == b'/' || (cfg!(windows) && prev == b'\\') {
                return None;
            }
            if prev == b'.' {
                continue;
            }
            return Some(i);
        }
    }
    None
}

fn path_basename(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "basename")?;
    let name = Path::new(&s)
        .file_name()
        .map_or(String::new(), |x| x.to_string_lossy().into_owned());
    Ok(Object::from_str(name))
}

fn path_dirname(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "dirname")?;
    let dir = Path::new(&s)
        .parent()
        .map_or(String::new(), |x| x.to_string_lossy().into_owned());
    Ok(Object::from_str(dir))
}

fn path_exists(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "exists")?;
    Ok(Object::Bool(Path::new(&s).exists()))
}

fn path_isfile(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "isfile")?;
    Ok(Object::Bool(Path::new(&s).is_file()))
}

fn path_isdir(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "isdir")?;
    Ok(Object::Bool(Path::new(&s).is_dir()))
}

fn path_abspath(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "abspath")?;
    let p = PathBuf::from(&s);
    let abs = if p.is_absolute() {
        p
    } else {
        std::env::current_dir()
            .map_err(|e| os_error(format!("abspath: {e}")))?
            .join(p)
    };
    Ok(Object::from_str(abs.to_string_lossy().into_owned()))
}

fn path_normpath(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = first_path(args, "normpath")?;
    let normalised = normpath_lexical(&s);
    Ok(Object::from_str(normalised))
}

fn first_path(args: &[Object], func: &str) -> Result<String, RuntimeError> {
    match args.first() {
        Some(obj) => as_str(obj, func),
        None => Err(type_error(format!("{func}() requires a path argument"))),
    }
}

/// Lexical path normalisation matching CPython's `os.path.normpath`:
/// drops `.` components, collapses `..` against earlier parts, and
/// collapses redundant separators. Does not touch the filesystem.
fn normpath_lexical(s: &str) -> String {
    let sep_str = if cfg!(windows) { "\\" } else { "/" };
    let is_sep = |c: char| c == '/' || (cfg!(windows) && c == '\\');
    let is_abs = s.starts_with(is_sep);
    let mut stack: Vec<&str> = Vec::new();
    for part in s.split(is_sep) {
        match part {
            "" | "." => continue,
            ".." => {
                if let Some(top) = stack.last() {
                    if *top != ".." {
                        stack.pop();
                        continue;
                    }
                }
                if !is_abs {
                    stack.push("..");
                }
            }
            other => stack.push(other),
        }
    }
    let mut out = if is_abs {
        sep_str.to_owned()
    } else {
        String::new()
    };
    for (i, p) in stack.iter().enumerate() {
        if i > 0 || (i == 0 && !is_abs) {
            if i > 0 {
                out.push_str(sep_str);
            }
        }
        out.push_str(p);
    }
    if out.is_empty() {
        ".".to_owned()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitext_handles_simple_extensions() {
        let s = "foo.txt".to_owned();
        assert_eq!(find_ext_dot(&s), Some(3));
        let s = "foo".to_owned();
        assert_eq!(find_ext_dot(&s), None);
        let s = ".bashrc".to_owned();
        assert_eq!(find_ext_dot(&s), None);
        let s = "a/b/c.gz".to_owned();
        assert_eq!(find_ext_dot(&s), Some(5));
    }

    #[test]
    fn normpath_collapses_dots() {
        // `normpath_lexical` mirrors CPython: `ntpath.normpath` joins
        // with `\` on Windows, `posixpath.normpath` with `/` elsewhere.
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(normpath_lexical("a/./b"), format!("a{sep}b"));
        assert_eq!(normpath_lexical("a/b/../c"), format!("a{sep}c"));
        assert_eq!(normpath_lexical("./"), ".");
    }
}
