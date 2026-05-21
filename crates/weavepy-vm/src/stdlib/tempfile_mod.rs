//! The `_tempfile` built-in module.
//!
//! Low-level temp-file primitives — `mkstemp`, `mkdtemp`, `gettempdir`,
//! `gettempprefix`. The user-visible `tempfile.NamedTemporaryFile`,
//! `TemporaryDirectory`, `SpooledTemporaryFile`, and the
//! context-manager helpers live in the frozen Python wrapper
//! (`stdlib/python/tempfile.py`) on top of this surface.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use crate::error::{io_error_to_py, type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_tempfile"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Low-level temporary file primitives."),
        );
        d.insert(
            DictKey(Object::from_static("mkstemp")),
            b("mkstemp", mkstemp),
        );
        d.insert(
            DictKey(Object::from_static("mkdtemp")),
            b("mkdtemp", mkdtemp),
        );
        d.insert(
            DictKey(Object::from_static("gettempdir")),
            b("gettempdir", gettempdir),
        );
        d.insert(
            DictKey(Object::from_static("gettempprefix")),
            b("gettempprefix", gettempprefix),
        );
        d.insert(DictKey(Object::from_static("tempdir")), Object::Bool(false));
    }
    Rc::new(PyModule {
        name: "_tempfile".to_owned(),
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

fn gettempdir(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::from_str(
        std::env::temp_dir().to_string_lossy().into_owned(),
    ))
}

fn gettempprefix(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::from_static("tmp"))
}

/// Generate a unique name suffix using nanosecond-resolution time.
/// Not cryptographically random — but matches what CPython's
/// `tempfile._RandomNameSequence` produces in spirit.
fn unique_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut state = nanos as u64;
    let mut out = String::with_capacity(8);
    for _ in 0..8 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let idx = ((state >> 28) & 0x3F) as usize;
        // Use [a-z0-9] alphabet — matches CPython's character set.
        let ch = match idx {
            0..=25 => (b'a' + idx as u8) as char,
            26..=51 => (b'A' + (idx as u8 - 26)) as char,
            _ => (b'0' + (idx as u8 - 52) % 10) as char,
        };
        out.push(ch);
    }
    out
}

fn mkstemp(args: &[Object]) -> Result<Object, RuntimeError> {
    let suffix = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        Some(Object::None) | None => String::new(),
        _ => return Err(type_error("mkstemp: suffix must be str or None")),
    };
    let prefix = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        Some(Object::None) | None => "tmp".to_string(),
        _ => return Err(type_error("mkstemp: prefix must be str or None")),
    };
    let dir = match args.get(2) {
        Some(Object::Str(s)) => PathBuf::from(s.to_string()),
        Some(Object::None) | None => std::env::temp_dir(),
        _ => return Err(type_error("mkstemp: dir must be str or None")),
    };
    let _text = match args.get(3) {
        Some(Object::Bool(b)) => *b,
        _ => false,
    };
    // Loop until we hit a non-existing name (extremely rare race; we
    // bail after 100 attempts).
    for _ in 0..100 {
        let name = format!("{prefix}{}{suffix}", unique_suffix());
        let path = dir.join(&name);
        if !path.exists() {
            // Create the file exclusively.
            let f = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| io_error_to_py(&e))?;
            // The Python wrapper expects (fd, path); we don't expose
            // raw fds nicely, so we hand back the path twice (the
            // Python wrapper turns the first into an io stream via
            // `os.fdopen`-style code we ship inside `tempfile.py`).
            drop(f);
            return Ok(Object::new_tuple(vec![
                Object::from_str(path.to_string_lossy().into_owned()),
                Object::from_str(path.to_string_lossy().into_owned()),
            ]));
        }
    }
    Err(crate::error::os_error("mkstemp: no usable temp name"))
}

fn mkdtemp(args: &[Object]) -> Result<Object, RuntimeError> {
    let suffix = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        Some(Object::None) | None => String::new(),
        _ => return Err(type_error("mkdtemp: suffix must be str or None")),
    };
    let prefix = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        Some(Object::None) | None => "tmp".to_string(),
        _ => return Err(type_error("mkdtemp: prefix must be str or None")),
    };
    let dir = match args.get(2) {
        Some(Object::Str(s)) => PathBuf::from(s.to_string()),
        Some(Object::None) | None => std::env::temp_dir(),
        _ => return Err(type_error("mkdtemp: dir must be str or None")),
    };
    for _ in 0..100 {
        let name = format!("{prefix}{}{suffix}", unique_suffix());
        let path = dir.join(&name);
        if !path.exists() {
            std::fs::create_dir(&path).map_err(|e| io_error_to_py(&e))?;
            return Ok(Object::from_str(path.to_string_lossy().into_owned()));
        }
    }
    Err(crate::error::os_error("mkdtemp: no usable name"))
}
