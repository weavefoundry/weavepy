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
        d.insert(
            DictKey(Object::from_static("text_encoding")),
            builtin("text_encoding", io_text_encoding),
        );
        // `io.open` is the canonical `open()`; `tempfile` and friends call
        // `io.open(...)` (often aliased `import io as _io`) with keyword
        // arguments. Share the `_io` implementation so both module faces
        // behave identically.
        for name in ["open", "open_code"] {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Builtin(Rc::new(BuiltinFn {
                    name,
                    binds_instance: false,
                    call: Box::new(crate::stdlib::io_full::io_open),
                    call_kw: Some(Box::new(crate::stdlib::io_full::io_open_kw)),
                })),
            );
        }
        // CPython's `io.__all__`; consumers (and `test_io`) read it directly.
        d.insert(
            DictKey(Object::from_static("__all__")),
            Object::new_list(
                [
                    "BlockingIOError",
                    "open",
                    "open_code",
                    "IOBase",
                    "RawIOBase",
                    "FileIO",
                    "BytesIO",
                    "StringIO",
                    "BufferedIOBase",
                    "BufferedReader",
                    "BufferedWriter",
                    "BufferedRWPair",
                    "BufferedRandom",
                    "TextIOBase",
                    "TextIOWrapper",
                    "UnsupportedOperation",
                    "SEEK_SET",
                    "SEEK_CUR",
                    "SEEK_END",
                    "DEFAULT_BUFFER_SIZE",
                    "IncrementalNewlineDecoder",
                    "text_encoding",
                ]
                .into_iter()
                .map(Object::from_static)
                .collect(),
            ),
        );
        // RFC 0023 — surface the IOBase hierarchy from `_io` so
        // `isinstance(open(...), io.IOBase)` and similar checks work.
        for name in [
            "IOBase",
            "RawIOBase",
            "BufferedIOBase",
            "TextIOBase",
            "FileIO",
            "IncrementalNewlineDecoder",
            "UnsupportedOperation",
        ] {
            let cls = make_io_protocol(name);
            d.insert(DictKey(Object::from_static(name)), Object::Type(cls));
        }
        // Functional buffered wrappers: a thin buffering layer over a raw
        // binary stream (e.g. `io.BufferedWriter(io.BytesIO())`). `.raw`
        // re-exposes the wrapped stream; read/write/seek delegate through.
        for name in [
            "BufferedReader",
            "BufferedWriter",
            "BufferedRandom",
            "BufferedRWPair",
        ] {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Type(make_buffered(name)),
            );
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
    use crate::object::MethodWrapper;
    use crate::types::{TypeFlags, TypeObject};
    let bt = builtin_types();
    let mut dict = DictData::new();
    // CPython's io ABCs are ABCMeta-based and expose `register` for
    // virtual-subclass registration; the Python `_pyio` shim calls
    // `io.IOBase.register(IOBase)` at import time. Provide a `register`
    // classmethod (the class binds first) so that import — and the ABC
    // registration idiom generally — works.
    dict.insert(
        DictKey(Object::from_static("register")),
        Object::ClassMethod(MethodWrapper::new(Object::Builtin(Rc::new(BuiltinFn {
            name: "register",
            binds_instance: true,
            call: Box::new(io_abc_register),
            call_kw: None,
        })))),
    );
    TypeObject::new_with_flags(
        name,
        vec![bt.object_.clone()],
        dict,
        TypeFlags {
            is_exception: name == "UnsupportedOperation",
            is_builtin: true,
        },
    )
    .expect("io protocol type")
}

/// `IOBase.register(subclass)` — ABC virtual-subclass registration. The
/// class binds first (classmethod); we record the subclass in the class's
/// `_abc_registry` set and return it so `register` also works as a
/// decorator, mirroring `_abc._abc_register`.
fn io_abc_register(args: &[Object]) -> Result<Object, RuntimeError> {
    let cls = args.first().cloned().unwrap_or(Object::None);
    let sub = args.get(1).cloned().unwrap_or(Object::None);
    if let Object::Type(t) = &cls {
        let key = DictKey(Object::from_static("_abc_registry"));
        let reg = {
            let existing = t.dict.borrow().get(&key).cloned();
            match existing {
                Some(Object::Set(s)) => s,
                _ => {
                    let s = Object::new_set();
                    t.dict.borrow_mut().insert(key, s.clone());
                    match s {
                        Object::Set(s) => s,
                        _ => return Ok(sub),
                    }
                }
            }
        };
        reg.borrow_mut().insert(DictKey(sub.clone()));
    }
    Ok(sub)
}

fn builtin(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

/// `io.text_encoding(encoding, stacklevel=2)` — return a usable encoding
/// name, defaulting `None` to `"utf-8"`. CPython returns the sentinel
/// `"locale"` here, but WeavePy's text layer always operates in UTF-8, so
/// we resolve straight to it. `tempfile` and many stdlib call sites pass
/// the result of this through to `TextIOWrapper`.
pub(crate) fn io_text_encoding(args: &[Object]) -> Result<Object, RuntimeError> {
    match args.first() {
        None | Some(Object::None) => Ok(Object::from_static("utf-8")),
        Some(Object::Str(s)) => Ok(Object::Str(s.clone())),
        Some(_) => Err(type_error("text_encoding() argument must be str or None")),
    }
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

pub(crate) fn make_text_io_wrapper() -> Rc<crate::types::TypeObject> {
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

/// Resolve the wrapped binary buffer to the underlying `PyFile`. The buffer
/// may be a raw `BytesIO`/`FileIO` (`Object::File`) directly, or a
/// `Buffered*` wrapper instance whose `.raw` resolves (recursively) to one —
/// so `TextIOWrapper(BufferedWriter(BytesIO()))` works like CPython's stack.
fn tw_buffer(inst: &crate::types::PyInstance) -> Result<Rc<PyFile>, RuntimeError> {
    match tw_get(inst, "buffer") {
        Some(obj) => resolve_raw_pyfile(&obj)
            .map_err(|_| crate::error::value_error("underlying buffer has been detached")),
        None => Err(crate::error::value_error(
            "underlying buffer has been detached",
        )),
    }
}

/// Follow `Object::File` directly, or a `Buffered*` instance's `.raw`
/// attribute (recursively) down to the concrete backing `PyFile`.
fn resolve_raw_pyfile(obj: &Object) -> Result<Rc<PyFile>, RuntimeError> {
    match obj {
        Object::File(f) => Ok(f.clone()),
        Object::Instance(i) => {
            let raw = tw_get(i, "raw")
                .ok_or_else(|| crate::error::value_error("raw stream unavailable"))?;
            resolve_raw_pyfile(&raw)
        }
        _ => Err(crate::error::value_error("raw stream unavailable")),
    }
}

fn tw_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    let positional = &args[1..];
    let kw = |name: &str| {
        kwargs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
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

// ---------------------------------------------------------------------------
// Buffered{Reader,Writer,Random,RWPair} — a buffering layer over a raw binary
// stream (`io.BufferedWriter(io.BytesIO())`). WeavePy's `BytesIO` / `FileIO`
// are already fully buffered in memory / by the OS, so these wrappers store
// the raw stream on `self.raw` and delegate read / write / seek / flush
// straight through — behaviourally indistinguishable for the streams these
// consumers build, and enough to back the stdlib's stream-capture helpers
// (e.g. `TextIOWrapper(BufferedWriter(BytesIO()))`).
// ---------------------------------------------------------------------------

/// Build a functional buffered-stream type (`BufferedReader` etc.). All four
/// share one method table; the distinctions (read-only vs write-only) don't
/// matter for the in-memory streams WeavePy wraps here.
pub(crate) fn make_buffered(name: &'static str) -> Rc<crate::types::TypeObject> {
    use crate::builtin_types::builtin_types;
    use crate::types::{TypeFlags, TypeObject};
    let bt = builtin_types();
    let mut dict = DictData::new();
    let mut method = |n: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>| {
        dict.insert(
            DictKey(Object::from_static(n)),
            Object::Builtin(Rc::new(BuiltinFn {
                name: n,
                binds_instance: true,
                call: Box::new(body),
                call_kw: None,
            })),
        );
    };
    method("write", bw_write);
    method("read", bw_read);
    method("read1", bw_read);
    method("readinto", bw_readinto);
    method("readline", bw_readline);
    method("peek", bw_peek);
    method("flush", bw_flush);
    method("close", bw_close);
    method("seek", bw_seek);
    method("tell", bw_tell);
    method("truncate", bw_tell);
    method("readable", bw_true);
    method("writable", bw_true);
    method("seekable", bw_true);
    method("isatty", bw_false);
    method("fileno", bw_fileno);
    method("detach", bw_detach);
    method("__iter__", tw_iter);
    method("__enter__", tw_enter);
    method("__exit__", tw_exit);
    // `__init__(raw, buffer_size=DEFAULT_BUFFER_SIZE)` stores the raw stream.
    dict.insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            binds_instance: true,
            call: Box::new(|args| bw_init(args, &[])),
            call_kw: Some(Box::new(bw_init)),
        })),
    );
    TypeObject::new_with_flags(
        name,
        vec![bt.object_.clone()],
        dict,
        TypeFlags {
            is_exception: false,
            is_builtin: true,
        },
    )
    .expect("buffered type")
}

fn bw_self(args: &[Object]) -> Result<Rc<crate::types::PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) => Ok(i.clone()),
        _ => Err(type_error(
            "unbound buffered method requires a buffered-stream instance",
        )),
    }
}

/// Resolve `self.raw` down to the concrete backing `PyFile`.
fn bw_raw(inst: &crate::types::PyInstance) -> Result<Rc<PyFile>, RuntimeError> {
    match tw_get(inst, "raw") {
        Some(obj) => resolve_raw_pyfile(&obj),
        None => Err(value_error("raw stream unavailable")),
    }
}

fn bw_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let positional = &args[1..];
    let raw = positional
        .first()
        .cloned()
        .or_else(|| {
            kwargs
                .iter()
                .find(|(k, _)| k == "raw")
                .map(|(_, v)| v.clone())
        })
        .ok_or_else(|| type_error("a raw stream is required"))?;
    tw_set(&inst, "raw", raw);
    Ok(Object::None)
}

fn bw_bytes_arg(arg: Option<&Object>) -> Result<Vec<u8>, RuntimeError> {
    match arg {
        Some(Object::Bytes(b)) => Ok(b.to_vec()),
        Some(Object::ByteArray(b)) => Ok(b.borrow().clone()),
        Some(other) => Err(type_error(format!(
            "a bytes-like object is required, not '{}'",
            other.type_name()
        ))),
        None => Err(type_error("write() takes exactly one argument")),
    }
}

fn bw_write(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let raw = bw_raw(&inst)?;
    let bytes = bw_bytes_arg(args.get(1))?;
    let n = raw.write_bytes(&bytes)?;
    Ok(Object::Int(n as i64))
}

fn bw_read(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let raw = bw_raw(&inst)?;
    let data = match args.get(1) {
        Some(Object::Int(n)) if *n >= 0 => raw.read_bytes(Some(*n as usize))?,
        _ => raw.read_bytes(None)?,
    };
    Ok(Object::Bytes(Rc::from(data.as_slice())))
}

fn bw_peek(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let raw = bw_raw(&inst)?;
    // Read a chunk and rewind so the bytes stay available — a faithful
    // `peek()` for the seekable in-memory streams we wrap.
    let data = raw.read_bytes(None)?;
    if !data.is_empty() {
        raw.seek(-(data.len() as isize), 1)?;
    }
    Ok(Object::Bytes(Rc::from(data.as_slice())))
}

fn bw_readinto(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let raw = bw_raw(&inst)?;
    match args.get(1) {
        Some(Object::ByteArray(dst)) => {
            let cap = dst.borrow().len();
            let data = raw.read_bytes(Some(cap))?;
            let n = data.len();
            dst.borrow_mut()[..n].copy_from_slice(&data);
            Ok(Object::Int(n as i64))
        }
        _ => Err(type_error(
            "readinto() argument must be a writable bytes-like object",
        )),
    }
}

fn bw_readline(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let raw = bw_raw(&inst)?;
    let mut line: Vec<u8> = Vec::new();
    loop {
        let b = raw.read_bytes(Some(1))?;
        if b.is_empty() {
            break;
        }
        line.push(b[0]);
        if b[0] == b'\n' {
            break;
        }
    }
    Ok(Object::Bytes(Rc::from(line.as_slice())))
}

fn bw_flush(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    if let Ok(raw) = bw_raw(&inst) {
        raw.flush()?;
    }
    Ok(Object::None)
}

fn bw_close(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    if let Ok(raw) = bw_raw(&inst) {
        let _ = raw.flush();
        raw.close();
    }
    Ok(Object::None)
}

fn bw_seek(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let raw = bw_raw(&inst)?;
    let offset = match args.get(1) {
        Some(Object::Int(n)) => *n as isize,
        _ => 0,
    };
    let whence = match args.get(2) {
        Some(Object::Int(n)) => *n as i32,
        _ => 0,
    };
    let pos = raw.seek(offset, whence)?;
    Ok(Object::Int(pos as i64))
}

fn bw_tell(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let raw = bw_raw(&inst)?;
    let pos = raw.seek(0, 1)?;
    Ok(Object::Int(pos as i64))
}

fn bw_true(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    Ok(Object::Bool(bw_raw(&inst).is_ok()))
}

fn bw_false(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(false))
}

fn bw_fileno(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(value_error("underlying stream has no fileno"))
}

fn bw_detach(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let raw = tw_get(&inst, "raw").ok_or_else(|| value_error("raw stream unavailable"))?;
    inst.dict
        .borrow_mut()
        .shift_remove(&DictKey(Object::from_static("raw")));
    Ok(raw)
}
