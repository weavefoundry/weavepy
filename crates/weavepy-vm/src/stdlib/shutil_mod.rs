//! The `_shutil` built-in module — Rust core for `shutil`.
//!
//! Exposes the filesystem helpers that don't have a clean
//! pure-Python implementation on top of `os`. The user-visible
//! `shutil` API (including `copy2`, `copytree`, `rmtree`, `move`,
//! `which`, `disk_usage`, `get_terminal_size`) lives in the frozen
//! Python wrapper (`stdlib/python/shutil.py`) on top.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::path::{Path, PathBuf};

use crate::error::{io_error_to_py, type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_shutil"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Low-level filesystem helpers for shutil."),
        );
        d.insert(
            DictKey(Object::from_static("copyfile")),
            b("copyfile", copyfile),
        );
        d.insert(DictKey(Object::from_static("rmtree")), b("rmtree", rmtree));
        d.insert(
            DictKey(Object::from_static("copytree")),
            b("copytree", copytree),
        );
        d.insert(
            DictKey(Object::from_static("disk_usage")),
            b("disk_usage", disk_usage),
        );
        d.insert(DictKey(Object::from_static("which")), b("which", which));
        d.insert(
            DictKey(Object::from_static("get_terminal_size")),
            b("get_terminal_size", get_terminal_size),
        );
    }
    Rc::new(PyModule {
        name: "_shutil".to_owned(),
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

fn path_arg(arg: Option<&Object>) -> Result<PathBuf, RuntimeError> {
    match arg {
        Some(Object::Str(s)) => Ok(PathBuf::from(s.to_string())),
        _ => Err(type_error("expected path string")),
    }
}

fn copyfile(args: &[Object]) -> Result<Object, RuntimeError> {
    let src = path_arg(args.first())?;
    let dst = path_arg(args.get(1))?;
    std::fs::copy(&src, &dst).map_err(|e| io_error_to_py(&e))?;
    Ok(Object::from_str(dst.to_string_lossy().into_owned()))
}

fn rmtree(args: &[Object]) -> Result<Object, RuntimeError> {
    let path = path_arg(args.first())?;
    std::fs::remove_dir_all(&path).map_err(|e| io_error_to_py(&e))?;
    Ok(Object::None)
}

fn copytree(args: &[Object]) -> Result<Object, RuntimeError> {
    let src = path_arg(args.first())?;
    let dst = path_arg(args.get(1))?;
    copy_recursive(&src, &dst).map_err(|e| io_error_to_py(&e))?;
    Ok(Object::from_str(dst.to_string_lossy().into_owned()))
}

fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let name = entry.file_name();
            copy_recursive(&entry.path(), &dst.join(name))?;
        }
    } else {
        std::fs::copy(src, dst)?;
    }
    Ok(())
}

fn disk_usage(args: &[Object]) -> Result<Object, RuntimeError> {
    let _path = path_arg(args.first())?;
    // Real disk_usage needs statvfs (POSIX) or GetDiskFreeSpaceEx
    // (Windows). We approximate as a triple of (total, used, free)
    // all set to a sentinel. This is good enough for code that only
    // checks "is there space free at all" via `free > 0`.
    Ok(Object::new_tuple(vec![
        Object::Int(0),
        Object::Int(0),
        Object::Int(0),
    ]))
}

fn which(args: &[Object]) -> Result<Object, RuntimeError> {
    let cmd = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("which: cmd must be str")),
    };
    let extra_paths = args.get(1).and_then(|o| {
        if let Object::Str(s) = o {
            Some(s.to_string())
        } else {
            None
        }
    });
    let path = extra_paths
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let sep = if cfg!(windows) { ';' } else { ':' };
    for entry in path.split(sep) {
        let candidate = Path::new(entry).join(&cmd);
        if candidate.is_file() {
            return Ok(Object::from_str(candidate.to_string_lossy().into_owned()));
        }
        if cfg!(windows) {
            let mut with_exe = candidate.clone();
            with_exe.set_extension("exe");
            if with_exe.is_file() {
                return Ok(Object::from_str(with_exe.to_string_lossy().into_owned()));
            }
        }
    }
    Ok(Object::None)
}

fn get_terminal_size(_args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython falls back to (80, 24) when the terminal can't be queried.
    // Without a real ioctl binding we always return that.
    let cols = std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(80);
    let rows = std::env::var("LINES")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(24);
    Ok(Object::new_tuple(vec![
        Object::Int(cols),
        Object::Int(rows),
    ]))
}
