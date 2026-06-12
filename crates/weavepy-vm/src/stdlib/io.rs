//! The `io` built-in module.
//!
//! Ships in-memory text and byte streams plus a path-based `open()`
//! that mirrors the top-level builtin. Real file objects live in
//! [`crate::object::PyFile`]; this module just exposes the factory
//! callables that wrap them.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, FileBackend, Object, PyFile, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("io"),
        );
        d.insert(
            DictKey(Object::from_static("__package__")),
            Object::from_static(""),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Core tools for working with streams."),
        );
        d.insert(
            DictKey(Object::from_static("StringIO")),
            builtin("StringIO", io_stringio),
        );
        d.insert(
            DictKey(Object::from_static("BytesIO")),
            builtin("BytesIO", io_bytesio),
        );
        d.insert(
            DictKey(Object::from_static("DEFAULT_BUFFER_SIZE")),
            Object::Int(8192),
        );
        d.insert(DictKey(Object::from_static("SEEK_SET")), Object::Int(0));
        d.insert(DictKey(Object::from_static("SEEK_CUR")), Object::Int(1));
        d.insert(DictKey(Object::from_static("SEEK_END")), Object::Int(2));
        // RFC 0023 — surface the IOBase hierarchy from `_io` so
        // `isinstance(open(...), io.IOBase)` and similar checks work.
        for name in [
            "IOBase",
            "RawIOBase",
            "BufferedIOBase",
            "TextIOBase",
            "FileIO",
            "BufferedReader",
            "BufferedWriter",
            "BufferedRandom",
            "BufferedRWPair",
            "IncrementalNewlineDecoder",
            "UnsupportedOperation",
        ] {
            let cls = make_io_protocol(name);
            d.insert(DictKey(Object::from_static(name)), Object::Type(cls));
        }
        // A functional `TextIOWrapper`: a text layer over a binary buffer
        // (e.g. `io.TextIOWrapper(io.BytesIO())`). `write` encodes through to
        // the wrapped buffer; `.buffer` exposes it again.
        d.insert(
            DictKey(Object::from_static("TextIOWrapper")),
            Object::Type(make_text_io_wrapper()),
        );
    }
    Rc::new(PyModule {
        name: "io".to_owned(),
        filename: None,
        dict,
    })
}

/// Build a protocol-only TypeObject for one of the `io.*` ABCs. This
/// returns the same shape as `_io.IOBase` etc. so the two surface
/// imports resolve to identical class identity. (We don't try to
/// share `Rc<TypeObject>` instances across module builds because
/// `io.IOBase` and `_io.IOBase` are recreated per-VM.)
fn make_io_protocol(name: &'static str) -> Rc<crate::types::TypeObject> {
    use crate::builtin_types::builtin_types;
    use crate::types::{TypeFlags, TypeObject};
    let bt = builtin_types();
    TypeObject::new_with_flags(
        name,
        vec![bt.object_.clone()],
        DictData::new(),
        TypeFlags {
            is_exception: name == "UnsupportedOperation",
            is_builtin: true,
        },
    )
    .expect("io protocol type")
}

fn builtin(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

fn io_stringio(args: &[Object]) -> Result<Object, RuntimeError> {
    let initial = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        Some(Object::None) | None => String::new(),
        _ => return Err(type_error("StringIO() argument must be str")),
    };
    Ok(Object::File(Rc::new(PyFile::new(
        "<string>",
        "r+",
        FileBackend::MemText {
            data: initial,
            pos: 0,
        },
    ))))
}

fn io_bytesio(args: &[Object]) -> Result<Object, RuntimeError> {
    let initial = match args.first() {
        Some(Object::Bytes(b)) => b.to_vec(),
        Some(Object::ByteArray(b)) => b.borrow().clone(),
        Some(Object::None) | None => Vec::new(),
        _ => return Err(value_error("BytesIO() argument must be bytes")),
    };
    // CPython positions the read cursor at 0 even when an initial
    // buffer is supplied, so the caller can `read()` it back.
    Ok(Object::File(Rc::new(PyFile::new(
        "<bytes>",
        "r+b",
        FileBackend::MemBytes {
            data: initial,
            pos: 0,
        },
    ))))
}

// ---------------------------------------------------------------------------
// TextIOWrapper — a text layer over a binary buffer.
//
// `io.TextIOWrapper(buffer, encoding=None, errors=None, newline=None, ...)`
// wraps a binary stream (e.g. `io.BytesIO`) and presents a text interface:
// `write(str)` encodes through to the buffer, `read()` decodes back, and
// `.buffer` re-exposes the wrapped stream. We store the buffer + codec
// settings on the instance `__dict__` so the methods (Rust builtins) can
// recover them from `self`.
// ---------------------------------------------------------------------------

fn make_text_io_wrapper() -> Rc<crate::types::TypeObject> {
    use crate::builtin_types::builtin_types;
    use crate::types::{TypeFlags, TypeObject};
    let bt = builtin_types();
    let mut dict = DictData::new();
    let mut method = |name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>| {
        dict.insert(
            DictKey(Object::from_static(name)),
            Object::Builtin(Rc::new(BuiltinFn {
                name,
                binds_instance: true,
                call: Box::new(body),
                call_kw: None,
            })),
        );
    };
    method("write", tw_write);
    method("read", tw_read);
    method("readline", tw_readline);
    method("flush", tw_flush);
    method("close", tw_close);
    method("seek", tw_seek);
    method("tell", tw_tell);
    method("truncate", tw_flush_noop);
    method("fileno", tw_fileno);
    method("isatty", tw_false);
    method("readable", tw_readable);
    method("writable", tw_writable);
    method("seekable", tw_seekable);
    method("detach", tw_detach);
    method("__iter__", tw_iter);
    method("__next__", tw_next);
    method("__enter__", tw_enter);
    method("__exit__", tw_exit);
    method("reconfigure", tw_reconfigure);
    // `__init__` needs keyword arguments (encoding=, errors=, newline=, …).
    dict.insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            binds_instance: true,
            call: Box::new(|args| tw_init(args, &[])),
            call_kw: Some(Box::new(tw_init)),
        })),
    );
    TypeObject::new_with_flags(
        "TextIOWrapper",
        vec![bt.object_.clone()],
        dict,
        TypeFlags {
            is_exception: false,
            is_builtin: true,
        },
    )
    .expect("TextIOWrapper type")
}

/// Pull `self` (a `TextIOWrapper` instance) out of the argument list.
fn tw_self(args: &[Object]) -> Result<Rc<crate::types::PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) => Ok(i.clone()),
        _ => Err(type_error(
            "unbound method TextIOWrapper requires a TextIOWrapper instance",
        )),
    }
}

fn tw_get(inst: &crate::types::PyInstance, name: &str) -> Option<Object> {
    inst.dict
        .borrow()
        .get(&DictKey(Object::from_str(name)))
        .cloned()
}

fn tw_set(inst: &crate::types::PyInstance, name: &'static str, value: Object) {
    inst.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static(name)), value);
}

fn tw_encoding(inst: &crate::types::PyInstance) -> String {
    match tw_get(inst, "encoding") {
        Some(Object::Str(s)) => s.to_string(),
        _ => "utf-8".to_owned(),
    }
}

fn tw_errors(inst: &crate::types::PyInstance) -> String {
    match tw_get(inst, "errors") {
        Some(Object::Str(s)) => s.to_string(),
        _ => "strict".to_owned(),
    }
}

/// Resolve the wrapped binary buffer to the underlying `PyFile`.
fn tw_buffer(inst: &crate::types::PyInstance) -> Result<Rc<PyFile>, RuntimeError> {
    match tw_get(inst, "buffer") {
        Some(Object::File(f)) => Ok(f),
        _ => Err(crate::error::value_error(
            "underlying buffer has been detached",
        )),
    }
}

fn tw_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    let positional = &args[1..];
    let kw = |name: &str| kwargs.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    let buffer = positional
        .first()
        .cloned()
        .or_else(|| kw("buffer"))
        .ok_or_else(|| type_error("TextIOWrapper() missing required argument 'buffer'"))?;
    // encoding (positional index 1 or keyword); `None`/missing → utf-8.
    let encoding = positional.get(1).cloned().or_else(|| kw("encoding"));
    let encoding = match encoding {
        Some(Object::Str(s)) => s.to_string(),
        _ => "utf-8".to_owned(),
    };
    let errors = positional.get(2).cloned().or_else(|| kw("errors"));
    let errors = match errors {
        Some(Object::Str(s)) => s.to_string(),
        _ => "strict".to_owned(),
    };
    let newline = positional.get(3).cloned().or_else(|| kw("newline"));
    tw_set(&inst, "buffer", buffer);
    tw_set(&inst, "encoding", Object::from_str(encoding));
    tw_set(&inst, "errors", Object::from_str(errors));
    tw_set(&inst, "newline", newline.unwrap_or(Object::None));
    tw_set(&inst, "_detached", Object::Bool(false));
    Ok(Object::None)
}

fn tw_write(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    let text = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        Some(other) => {
            return Err(type_error(format!(
                "write() argument must be str, not {}",
                other.type_name()
            )))
        }
        None => return Err(type_error("write() takes exactly one argument")),
    };
    let encoding = tw_encoding(&inst);
    let errors = tw_errors(&inst);
    let bytes = crate::stdlib::codecs_mod::encode_str(&text, &encoding, &errors)?;
    let file = tw_buffer(&inst)?;
    file.write_bytes(&bytes)?;
    // TextIOWrapper.write returns the number of characters written.
    Ok(Object::Int(text.chars().count() as i64))
}

fn tw_read(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    let file = tw_buffer(&inst)?;
    // Text size is measured in characters; we only support the
    // read-everything form (size omitted / None / negative), which is
    // what stream-capture helpers use.
    let want_all = !matches!(args.get(1), Some(Object::Int(n)) if *n >= 0);
    let raw = if want_all {
        file.read_bytes(None)?
    } else {
        // Approximate: read the requested count of bytes. Good enough
        // for ASCII-heavy capture; exact char counting would require
        // incremental decoding.
        match args.get(1) {
            Some(Object::Int(n)) => file.read_bytes(Some(*n as usize))?,
            _ => file.read_bytes(None)?,
        }
    };
    let encoding = tw_encoding(&inst);
    let errors = tw_errors(&inst);
    let text = crate::stdlib::codecs_mod::decode_bytes(&raw, &encoding, &errors)?;
    Ok(Object::from_str(text))
}

fn tw_readline(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    let file = tw_buffer(&inst)?;
    // Read byte-by-byte until newline; fine for the small captured
    // streams these tests exercise.
    let mut line: Vec<u8> = Vec::new();
    loop {
        let b = file.read_bytes(Some(1))?;
        if b.is_empty() {
            break;
        }
        line.push(b[0]);
        if b[0] == b'\n' {
            break;
        }
    }
    let encoding = tw_encoding(&inst);
    let errors = tw_errors(&inst);
    let text = crate::stdlib::codecs_mod::decode_bytes(&line, &encoding, &errors)?;
    Ok(Object::from_str(text))
}

fn tw_flush(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    if let Ok(file) = tw_buffer(&inst) {
        file.flush()?;
    }
    Ok(Object::None)
}

fn tw_flush_noop(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

fn tw_close(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    if let Ok(file) = tw_buffer(&inst) {
        let _ = file.flush();
        file.close();
    }
    Ok(Object::None)
}

fn tw_seek(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    let file = tw_buffer(&inst)?;
    let offset = match args.get(1) {
        Some(Object::Int(n)) => *n as isize,
        _ => 0,
    };
    let whence = match args.get(2) {
        Some(Object::Int(n)) => *n as i32,
        _ => 0,
    };
    let pos = file.seek(offset, whence)?;
    Ok(Object::Int(pos as i64))
}

fn tw_tell(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    let file = tw_buffer(&inst)?;
    // SEEK_CUR with 0 offset reports the current position.
    let pos = file.seek(0, 1)?;
    Ok(Object::Int(pos as i64))
}

fn tw_fileno(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(value_error("underlying stream has no fileno"))
}

fn tw_false(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(false))
}

fn tw_readable(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    Ok(Object::Bool(tw_buffer(&inst).is_ok()))
}

fn tw_writable(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    Ok(Object::Bool(tw_buffer(&inst).is_ok()))
}

fn tw_seekable(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    Ok(Object::Bool(tw_buffer(&inst).is_ok()))
}

fn tw_detach(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    let buffer = tw_buffer(&inst)?;
    tw_set(&inst, "_detached", Object::Bool(true));
    inst.dict
        .borrow_mut()
        .shift_remove(&DictKey(Object::from_static("buffer")));
    Ok(Object::File(buffer))
}

fn tw_iter(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(args[0].clone())
}

fn tw_next(args: &[Object]) -> Result<Object, RuntimeError> {
    let line = tw_readline(args)?;
    match &line {
        Object::Str(s) if s.is_empty() => Err(crate::error::stop_iteration()),
        _ => Ok(line),
    }
}

fn tw_enter(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(args[0].clone())
}

fn tw_exit(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

fn tw_reconfigure(args: &[Object]) -> Result<Object, RuntimeError> {
    // Accept (and ignore) encoding/newline reconfiguration requests; the
    // common case in tests is `reconfigure(newline='')`.
    let _ = tw_self(args)?;
    Ok(Object::None)
}
