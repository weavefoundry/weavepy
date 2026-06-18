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
            DictKey(Object::from_static("DEFAULT_BUFFER_SIZE")),
            Object::Int(8192),
        );
        d.insert(DictKey(Object::from_static("SEEK_SET")), Object::Int(0));
        d.insert(DictKey(Object::from_static("SEEK_CUR")), Object::Int(1));
        d.insert(DictKey(Object::from_static("SEEK_END")), Object::Int(2));
        // `io.BlockingIOError` is the builtin exception re-exported (CPython
        // lists it first in `io.__all__`); `test_io` reads it at import time.
        d.insert(
            DictKey(Object::from_static("BlockingIOError")),
            Object::Type(
                crate::builtin_types::builtin_types()
                    .blocking_io_error
                    .clone(),
            ),
        );
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
        // RFC 0023/0038 — the real IOBase hierarchy with working mixin
        // methods (`__enter__`/`__exit__`/`__iter__`/`__next__`/
        // `writelines`/…) so Python subclasses (the `gzip`/`bz2`/`lzma`
        // wrappers, user `io.RawIOBase`/`BufferedIOBase` subclasses, the
        // `_compression` reader) inherit a usable stream protocol.
        let fam = build_iobase_family();
        for (name, cls) in [
            ("IOBase", &fam.iobase),
            ("RawIOBase", &fam.raw),
            ("BufferedIOBase", &fam.buffered),
            ("TextIOBase", &fam.text),
            ("FileIO", &fam.fileio),
        ] {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Type(cls.clone()),
            );
        }
        // `BytesIO`/`StringIO` are real subclassable types backed by a
        // native in-memory file (MRO: BytesIO → BufferedIOBase → IOBase →
        // object, StringIO → TextIOBase → IOBase → object), so the stdlib
        // and tests can `class C(io.BytesIO)`.
        d.insert(
            DictKey(Object::from_static("BytesIO")),
            Object::Type(make_memory_stream(
                "BytesIO",
                fam.buffered.clone(),
                bytesio_new,
                "BytesIO.__new__",
                bytesio_init,
                "BytesIO.__init__",
            )),
        );
        d.insert(
            DictKey(Object::from_static("StringIO")),
            Object::Type(make_memory_stream(
                "StringIO",
                fam.text.clone(),
                stringio_new,
                "StringIO.__new__",
                stringio_init,
                "StringIO.__init__",
            )),
        );
        d.insert(
            DictKey(Object::from_static("IncrementalNewlineDecoder")),
            Object::Type(make_io_protocol("IncrementalNewlineDecoder")),
        );
        // `io.UnsupportedOperation` is a real exception inheriting both
        // `OSError` and `ValueError` (CPython), so `except OSError`/
        // `except ValueError` catch it and it never escapes the test runner.
        d.insert(
            DictKey(Object::from_static("UnsupportedOperation")),
            Object::Type(unsupported_operation_class()),
        );
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
                Object::Type(make_buffered(name, fam.buffered.clone())),
            );
        }
        // A functional `TextIOWrapper`: a text layer over a binary buffer
        // (e.g. `io.TextIOWrapper(io.BytesIO())`). `write` encodes through to
        // the wrapped buffer; `.buffer` exposes it again.
        d.insert(
            DictKey(Object::from_static("TextIOWrapper")),
            Object::Type(make_text_io_wrapper(fam.text.clone())),
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

pub(crate) fn make_text_io_wrapper(
    base: Rc<crate::types::TypeObject>,
) -> Rc<crate::types::TypeObject> {
    use crate::types::{TypeFlags, TypeObject};
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
    method("writelines", iobase_writelines);
    method("read", tw_read);
    method("readline", tw_readline);
    method("readlines", iobase_readlines);
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
    let ty = TypeObject::new_with_flags(
        "TextIOWrapper",
        vec![base],
        dict,
        TypeFlags {
            is_exception: false,
            is_builtin: true,
        },
    )
    .expect("TextIOWrapper type");
    install_delegating_closed(&ty, tw_closed);
    ty
}

// ---------------------------------------------------------------------------
// BytesIO / StringIO — real, subclassable in-memory streams.
//
// A direct `io.BytesIO()` returns the native `Object::File` fast path the
// rest of the VM understands (`BufferedReader` wrapping, `TextIOWrapper`
// targets, `matches!(x, Object::File(_))` sites). A *subclass* instance
// (`class UnseekableIO(io.BytesIO)`, the `gzip`/`test_io` idiom) is a real
// `PyInstance` whose `native` payload is that file, so every inherited
// stream method (read/write/seek/getvalue/…) operates on the live buffer
// via `crate::builtins::file_self`, while user overrides win through the
// MRO. This is what lets the stdlib subclass `io.BytesIO` at all.
// ---------------------------------------------------------------------------

/// Construct the backing in-memory `PyFile` for a `BytesIO`, reading the
/// optional initial buffer from positionals/`initial_bytes=`.
fn bytesio_file(args: &[Object], kwargs: &[(String, Object)]) -> Result<Rc<PyFile>, RuntimeError> {
    let initial = args
        .get(1)
        .cloned()
        .or_else(|| kw_get(kwargs, "initial_bytes"));
    let data = match initial {
        Some(Object::Bytes(b)) => b.to_vec(),
        Some(Object::ByteArray(b)) => b.borrow().clone(),
        // CPython's `BytesIO(initial_bytes=None)` is explicitly allowed and
        // means "start empty" (the C arg is `O` with a NULL default that the
        // init treats as no data) — `test_tarfile`'s filter helper relies on
        // `io.BytesIO(None)`.
        Some(Object::None) | None => Vec::new(),
        Some(o) => o.as_bytes_view().ok_or_else(|| {
            type_error(format!(
                "a bytes-like object is required, not '{}'",
                o.type_name()
            ))
        })?,
    };
    let f = PyFile::new(
        "<bytes>",
        "rb+",
        FileBackend::MemBytes {
            data: Rc::new(crate::sync::RefCell::new(data)),
            pos: 0,
        },
    );
    f.no_name.set(true);
    Ok(Rc::new(f))
}

/// Construct the backing in-memory `PyFile` for a `StringIO`, reading the
/// optional initial value from positionals/`initial_value=`.
fn stringio_file(args: &[Object], kwargs: &[(String, Object)]) -> Result<Rc<PyFile>, RuntimeError> {
    let initial = args
        .get(1)
        .cloned()
        .or_else(|| kw_get(kwargs, "initial_value"));
    let data = match initial {
        Some(Object::Str(s)) => s.to_string(),
        Some(Object::None) | None => String::new(),
        Some(_) => return Err(type_error("initial_value must be str or None, not int")),
    };
    let f = PyFile::new("<string>", "r+", FileBackend::MemText { data, pos: 0 });
    f.no_name.set(true);
    Ok(Rc::new(f))
}

fn kw_get(kwargs: &[(String, Object)], name: &str) -> Option<Object> {
    kwargs
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
}

/// `BytesIO.__new__(cls, initial_bytes=b'')` — native `File` for the base
/// type, a `native`-wrapping instance for a subclass.
///
/// CPython's `bytesio_new` *ignores* its arguments (the optional initial buffer
/// is consumed by `bytesio_init`). We honour the initial buffer here only for
/// the canonical `io.BytesIO(...)`, whose native-`File` return never runs
/// `__init__`; a *subclass* gets an empty buffer and its (possibly inherited)
/// `__init__` fills it. That way a subclass with a custom `__init__` taking
/// unrelated arguments — e.g. test_pathlib's `DummyPathIO(files, path)` — is
/// never handed those as bogus "initial bytes".
fn bytesio_new(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let cls = match args.first() {
        Some(Object::Type(t)) => t.clone(),
        _ => return Err(type_error("BytesIO.__new__(X): X is not a type object")),
    };
    let file = if cls.flags.is_builtin {
        bytesio_file(args, kwargs)?
    } else {
        empty_mem_file(
            "<bytes>",
            "rb+",
            FileBackend::MemBytes {
                data: Rc::new(crate::sync::RefCell::new(Vec::new())),
                pos: 0,
            },
        )
    };
    Ok(wrap_memory_stream(&cls, file))
}

/// `StringIO.__new__(cls, initial_value='', newline='\n')`. See `bytesio_new`
/// for why subclasses get an empty buffer here.
fn stringio_new(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let cls = match args.first() {
        Some(Object::Type(t)) => t.clone(),
        _ => return Err(type_error("StringIO.__new__(X): X is not a type object")),
    };
    let file = if cls.flags.is_builtin {
        stringio_file(args, kwargs)?
    } else {
        empty_mem_file(
            "<string>",
            "r+",
            FileBackend::MemText {
                data: String::new(),
                pos: 0,
            },
        )
    };
    Ok(wrap_memory_stream(&cls, file))
}

/// Validate a `FileIO` mode string (CPython `_io.FileIO` rules) and return the
/// equivalent binary mode for the `open()` machinery. `FileIO` is always
/// binary: exactly one of `r`/`w`/`x`/`a` is required, at most one `+`, and a
/// `b` is accepted (and implied). Anything else — including `t` — is a
/// `ValueError`, so `argparse.FileType('b')('-x')` raises like CPython.
fn normalize_fileio_mode(mode: &str) -> Result<String, RuntimeError> {
    let mut rwxa = 0u32;
    let mut plus = 0u32;
    let mut base = 'r';
    for ch in mode.chars() {
        match ch {
            'r' | 'w' | 'x' | 'a' => {
                rwxa += 1;
                base = ch;
            }
            '+' => plus += 1,
            'b' => {}
            _ => return Err(crate::error::value_error(format!("invalid mode: '{mode}'"))),
        }
    }
    if rwxa != 1 || plus > 1 {
        return Err(crate::error::value_error(
            "Must have exactly one of create/read/write/append mode and at most one plus",
        ));
    }
    let mut out = String::with_capacity(3);
    out.push(base);
    if plus == 1 {
        out.push('+');
    }
    out.push('b');
    Ok(out)
}

/// `FileIO.__new__(cls, file, mode='r', closefd=True, opener=None)` — the raw,
/// always-binary file layer. WeavePy represents it with the same native
/// `Object::File` that `open()` returns, so the `(fd, mode)` form the stdlib's
/// pipe/socket code and `test_io` rely on (`io.FileIO(fd, "w")`) works and its
/// `close()` releases the descriptor.
fn fileio_new(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let cls = match args.first() {
        Some(Object::Type(t)) => t.clone(),
        _ => return Err(type_error("FileIO.__new__(X): X is not a type object")),
    };
    let file = args
        .get(1)
        .cloned()
        .or_else(|| kw_get(kwargs, "file"))
        .ok_or_else(|| type_error("FileIO() missing required argument 'file' (pos 1)"))?;
    let mode = match args.get(2).cloned().or_else(|| kw_get(kwargs, "mode")) {
        None | Some(Object::None) => "rb".to_string(),
        Some(Object::Str(s)) => s.to_string(),
        Some(o) => {
            return Err(type_error(format!(
                "FileIO() argument 'mode' must be str, not {}",
                o.type_name()
            )))
        }
    };
    let closefd = match args.get(3).cloned().or_else(|| kw_get(kwargs, "closefd")) {
        None | Some(Object::None) => true,
        Some(Object::Bool(b)) => b,
        Some(Object::Int(n)) => n != 0,
        Some(_) => true,
    };
    let opener = args
        .get(4)
        .cloned()
        .or_else(|| kw_get(kwargs, "opener"))
        .filter(|o| !matches!(o, Object::None));
    let binmode = normalize_fileio_mode(&mode)?;
    let is_fd = matches!(file, Object::Int(_) | Object::Bool(_));
    if !closefd && !is_fd {
        return Err(crate::error::value_error(
            "Cannot use closefd=False with file name",
        ));
    }
    let raw = match opener {
        Some(opener) => {
            // CPython calls `opener(file, flags)` → fd, then adopts that fd.
            let ptr = crate::vm_singletons::current_interpreter_ptr()
                .ok_or_else(|| crate::error::runtime_error("FileIO: no running interpreter"))?;
            // SAFETY: published by an enclosing VM frame on this thread.
            let interp = unsafe { &mut *ptr };
            let globals = interp.builtins_dict();
            let flags = fileio_open_flags(&binmode);
            let fd = interp.call_object_with_globals(
                &opener,
                &[file.clone(), Object::Int(flags)],
                &[],
                &globals,
            )?;
            crate::builtins::b_open(&[fd, Object::from_str(binmode)])?
        }
        None => crate::builtins::b_open(&[file.clone(), Object::from_str(binmode)])?,
    };
    match raw {
        Object::File(f) => {
            f.closefd.set(closefd);
            Ok(wrap_memory_stream(&cls, f))
        }
        other => Ok(other),
    }
}

/// `os.open` flag bits for a normalized binary `FileIO` mode — only used to
/// hand a plausible `flags` value to a user `opener` callback.
fn fileio_open_flags(mode: &str) -> i64 {
    const O_WRONLY: i64 = 1;
    const O_RDWR: i64 = 2;
    const O_CREAT: i64 = 64;
    const O_EXCL: i64 = 128;
    const O_TRUNC: i64 = 512;
    const O_APPEND: i64 = 1024;
    let mut flags = if mode.contains('+') {
        O_RDWR
    } else if mode.contains('w') || mode.contains('a') || mode.contains('x') {
        O_WRONLY
    } else {
        0
    };
    if mode.contains('a') {
        flags |= O_APPEND | O_CREAT;
    }
    if mode.contains('w') {
        flags |= O_CREAT | O_TRUNC;
    }
    if mode.contains('x') {
        flags |= O_CREAT | O_EXCL;
    }
    flags
}

/// Install `FileIO.__new__` so `io.FileIO(name|fd, mode, closefd, opener)` is
/// constructible (CPython's raw file). Done once, on the shared type.
fn install_fileio_ctor(ty: &Rc<crate::types::TypeObject>) {
    use crate::object::MethodWrapper;
    ty.dict.borrow_mut().insert(
        DictKey(Object::from_static("__new__")),
        Object::StaticMethod(MethodWrapper::new(Object::Builtin(Rc::new(BuiltinFn {
            name: "FileIO.__new__",
            binds_instance: false,
            call: Box::new(|a| fileio_new(a, &[])),
            call_kw: Some(Box::new(fileio_new)),
        })))),
    );
}

/// An empty in-memory backing file for a memory-stream *subclass* (the base
/// type's buffer is built directly in `__new__`).
fn empty_mem_file(name: &str, mode: &str, backend: FileBackend) -> Rc<PyFile> {
    let f = PyFile::new(name, mode, backend);
    f.no_name.set(true);
    Rc::new(f)
}

/// `BytesIO.__init__(self, initial_bytes=b'')` — fill the (empty) backing
/// buffer of a freshly-`__new__`'d stream and rewind to position 0. A no-op
/// when no initial buffer is supplied (the `super().__init__()` chain in
/// custom subclass `__init__`s).
fn bytesio_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let initial = args
        .get(1)
        .cloned()
        .or_else(|| kw_get(kwargs, "initial_bytes"));
    let initial = match initial {
        None | Some(Object::None) => return Ok(Object::None),
        Some(o) => o,
    };
    let is_bytes_like = matches!(initial, Object::Bytes(_) | Object::ByteArray(_))
        || initial.as_bytes_view().is_some();
    if !is_bytes_like {
        return Err(type_error(format!(
            "a bytes-like object is required, not '{}'",
            initial.type_name()
        )));
    }
    let me = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("BytesIO.__init__ requires a stream"))?;
    crate::builtins::file_write(&[me.clone(), initial])?;
    crate::builtins::file_seek(&[me, Object::Int(0)])?;
    Ok(Object::None)
}

/// `StringIO.__init__(self, initial_value='', newline='\n')`. See
/// `bytesio_init`; `newline` translation is handled by the backing text file.
fn stringio_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let initial = args
        .get(1)
        .cloned()
        .or_else(|| kw_get(kwargs, "initial_value"));
    let initial = match initial {
        None | Some(Object::None) => return Ok(Object::None),
        Some(s @ Object::Str(_)) => s,
        Some(o) => {
            return Err(type_error(format!(
                "initial_value must be str or None, not {}",
                o.type_name()
            )))
        }
    };
    let me = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("StringIO.__init__ requires a stream"))?;
    crate::builtins::file_write(&[me.clone(), initial])?;
    crate::builtins::file_seek(&[me, Object::Int(0)])?;
    Ok(Object::None)
}

/// Return the native `File` directly for the canonical built-in type, or a
/// `PyInstance` carrying it as `native` for a user subclass.
fn wrap_memory_stream(cls: &Rc<crate::types::TypeObject>, file: Rc<PyFile>) -> Object {
    if cls.flags.is_builtin {
        Object::File(file)
    } else {
        Object::Instance(Rc::new(crate::types::PyInstance::with_native(
            cls.clone(),
            Object::File(file),
        )))
    }
}

/// `__enter__` / `__iter__` for a memory stream subclass: return `self`
/// (CPython returns the stream itself, preserving the subclass identity —
/// the native `file_*` helpers would otherwise hand back a bare `File`).
fn mem_return_self(args: &[Object]) -> Result<Object, RuntimeError> {
    args.first()
        .cloned()
        .ok_or_else(|| type_error("expected stream receiver"))
}

/// Install a `closed` property whose getter reads the native backing
/// file (shadowing the IOBase `_iobase_closed`-flag property via MRO).
fn install_mem_closed_property(ty: &Rc<crate::types::TypeObject>) {
    fn closed_get(args: &[Object]) -> Result<Object, RuntimeError> {
        match crate::builtins::file_self(args) {
            Ok(f) => Ok(Object::Bool(*f.closed.borrow())),
            Err(_) => Ok(Object::Bool(false)),
        }
    }
    let prop = Object::Property(Rc::new(crate::object::PyProperty::new(
        Object::Builtin(Rc::new(BuiltinFn {
            name: "closed",
            binds_instance: true,
            call: Box::new(closed_get),
            call_kw: None,
        })),
        Object::None,
        Object::None,
        Object::from_static("True if the stream is closed"),
    )));
    crate::descr_registry::register(
        &prop,
        crate::descr_registry::DescrKind::GetSet,
        ty.clone(),
        "closed",
        None,
    );
    ty.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("closed")), prop);
}

/// Install a read-only property `attr` whose getter is `getter`. Used for the
/// stream attributes (`closed`/`name`/`mode`) that CPython's `Buffered*` /
/// `TextIOWrapper` expose by delegating to the wrapped stream.
fn install_delegating_attr(
    ty: &Rc<crate::types::TypeObject>,
    attr: &'static str,
    doc: &'static str,
    getter: fn(&[Object]) -> Result<Object, RuntimeError>,
) {
    let prop = Object::Property(Rc::new(crate::object::PyProperty::new(
        Object::Builtin(Rc::new(BuiltinFn {
            name: attr,
            binds_instance: true,
            call: Box::new(getter),
            call_kw: None,
        })),
        Object::None,
        Object::None,
        Object::from_static(doc),
    )));
    crate::descr_registry::register(
        &prop,
        crate::descr_registry::DescrKind::GetSet,
        ty.clone(),
        attr,
        None,
    );
    ty.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static(attr)), prop);
}

/// Install a `closed` property whose getter is `getter` (delegates to the
/// wrapped raw/buffer stream for the `Buffered*`/`TextIOWrapper` types,
/// which CPython exposes as `closed`).
fn install_delegating_closed(
    ty: &Rc<crate::types::TypeObject>,
    getter: fn(&[Object]) -> Result<Object, RuntimeError>,
) {
    install_delegating_attr(ty, "closed", "True if the stream is closed", getter);
}

/// `Buffered*.closed` — delegates to the wrapped raw stream (CPython
/// `buffered_closed_get` reads `self.raw.closed`).
fn bw_closed(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    match bw_target(&inst) {
        Ok(RawTarget::Native(raw)) => Ok(Object::Bool(*raw.closed.borrow())),
        Ok(RawTarget::Py(raw)) => py_get_attr(&raw, "closed"),
        Err(_) => Ok(Object::Bool(true)),
    }
}

/// `TextIOWrapper.closed` — delegates to the wrapped buffer's `closed`.
fn tw_closed(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    match tw_get(&inst, "buffer") {
        Some(buf) => py_get_attr(&buf, "closed"),
        None => Ok(Object::Bool(true)),
    }
}

/// Build a subclassable in-memory stream type (`BytesIO`/`StringIO`).
#[allow(clippy::type_complexity)]
fn make_memory_stream(
    name: &'static str,
    base: Rc<crate::types::TypeObject>,
    new_fn: fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>,
    new_name: &'static str,
    init_fn: fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>,
    init_name: &'static str,
) -> Rc<crate::types::TypeObject> {
    use crate::object::MethodWrapper;
    use crate::types::{TypeFlags, TypeObject};
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
    method("read", crate::builtins::file_read);
    method("read1", crate::builtins::file_read);
    method("readline", crate::builtins::file_readline);
    method("readlines", crate::builtins::file_readlines);
    method("readinto", crate::builtins::file_readinto);
    method("readinto1", crate::builtins::file_readinto);
    method("write", crate::builtins::file_write);
    method("writelines", crate::builtins::file_writelines);
    method("seek", crate::builtins::file_seek);
    method("tell", crate::builtins::file_tell);
    method("truncate", crate::builtins::file_truncate);
    method("flush", crate::builtins::file_flush);
    method("close", crate::builtins::file_close);
    method("getvalue", crate::builtins::file_getvalue);
    // `getbuffer()` is a binary-stream method only — `StringIO` lacks it.
    if name == "BytesIO" {
        method("getbuffer", crate::builtins::file_getbuffer);
    }
    method("readable", crate::builtins::file_readable);
    method("writable", crate::builtins::file_writable);
    method("seekable", crate::builtins::file_seekable);
    method("isatty", crate::builtins::file_isatty);
    method("fileno", crate::builtins::file_fileno);
    method("__next__", crate::builtins::file_next);
    method("__iter__", mem_return_self);
    method("__enter__", mem_return_self);
    method("__exit__", crate::builtins::file_exit);
    dict.insert(
        DictKey(Object::from_static("__new__")),
        Object::StaticMethod(MethodWrapper::new(Object::Builtin(Rc::new(BuiltinFn {
            name: new_name,
            binds_instance: false,
            call: Box::new(move |a| new_fn(a, &[])),
            call_kw: Some(Box::new(new_fn)),
        })))),
    );
    // A real `__init__` (CPython `bytesio_init`/`stringio_init`): `__new__`
    // ignores the initial buffer for subclasses, so `__init__` is what applies
    // it — both for `class C(BytesIO): pass; C(b'x')` (inherited) and for the
    // base type's own `super().__init__()` chains.
    dict.insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: init_name,
            binds_instance: true,
            call: Box::new(move |a| init_fn(a, &[])),
            call_kw: Some(Box::new(init_fn)),
        })),
    );
    let ty = TypeObject::new_with_flags(
        name,
        vec![base],
        dict,
        TypeFlags {
            is_exception: false,
            is_builtin: true,
        },
    )
    .expect("memory stream type");
    install_mem_closed_property(&ty);
    ty
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

/// Resolve the wrapped buffer to either the native `PyFile` or an arbitrary
/// Python binary stream object (e.g. a `BZ2File`) to delegate to.
fn tw_buffer_target(inst: &crate::types::PyInstance) -> Result<RawTarget, RuntimeError> {
    let buf =
        tw_get(inst, "buffer").ok_or_else(|| value_error("underlying buffer has been detached"))?;
    raw_target(&buf).map_err(|_| value_error("underlying buffer has been detached"))
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

// ===========================================================================
// IOBase hierarchy + Python-object stream delegation (RFC 0038)
//
// CPython's `io` ABCs (`IOBase`/`RawIOBase`/`BufferedIOBase`/`TextIOBase`)
// ship a pile of *mixin* methods — `__enter__`/`__exit__`/`__iter__`/
// `__next__`/`writelines`/`readlines`/`flush`/`close`/`seekable`/… — that
// every Python stream subclass inherits. The frozen `gzip`/`bz2`/`lzma`
// wrappers (and `_compression`) subclass these and rely on the mixins, and
// they wrap pure-Python raw streams (`_compression.DecompressReader`) in
// `io.BufferedReader`. So WeavePy's `io` needs (1) a real IOBase hierarchy
// carrying those mixins and (2) `Buffered*`/`TextIOWrapper` that delegate to
// an arbitrary Python stream object — not just the native `PyFile` fast path.
// ===========================================================================

#[derive(Clone)]
pub(crate) struct IoFamily {
    pub iobase: Rc<crate::types::TypeObject>,
    pub raw: Rc<crate::types::TypeObject>,
    pub buffered: Rc<crate::types::TypeObject>,
    pub text: Rc<crate::types::TypeObject>,
    pub fileio: Rc<crate::types::TypeObject>,
}

/// The process-wide `io`/`_io` ABC hierarchy, built once. Memoising it is
/// load-bearing for *identity*: CPython exposes the very same type objects on
/// both `io` and `_io` (`io.IOBase is _io.IOBase`), and `isinstance` of a
/// native stream against `io.BufferedIOBase`/`TextIOBase` (the `pathlib`/`io`
/// suites) compares against these exact objects.
pub(crate) fn build_iobase_family() -> IoFamily {
    thread_local! {
        static IO_FAMILY: RefCell<Option<IoFamily>> = const { RefCell::new(None) };
    }
    IO_FAMILY.with(|slot| {
        if let Some(f) = slot.borrow().as_ref() {
            return f.clone();
        }
        let fam = build_iobase_family_inner();
        *slot.borrow_mut() = Some(fam.clone());
        fam
    })
}

/// Whether a native [`PyFile`] should be considered an instance of one of the
/// `io` ABCs (`info` is the candidate class). `None` means "not an io ABC"
/// (the caller falls back to ordinary MRO matching). A native binary stream is
/// reported as a `BufferedIOBase` (the common `open('rb')`/`io.BytesIO` shape);
/// a text stream as `TextIOBase`; every stream as `IOBase`.
pub(crate) fn file_io_abc_match(
    file: &crate::object::PyFile,
    info: &Rc<crate::types::TypeObject>,
) -> Option<bool> {
    let fam = build_iobase_family();
    let is = |t: &Rc<crate::types::TypeObject>| Rc::ptr_eq(t, info);
    if is(&fam.iobase) {
        return Some(true);
    }
    if is(&fam.buffered) {
        return Some(file.binary);
    }
    if is(&fam.text) {
        return Some(!file.binary);
    }
    if is(&fam.raw) || is(&fam.fileio) {
        // We can't distinguish a raw `FileIO` from a buffered `open('rb')`
        // stream at the `Object::File` level; treat native streams as buffered.
        return Some(false);
    }
    None
}

/// Build the `IOBase → {RawIOBase, BufferedIOBase, TextIOBase} → FileIO`
/// hierarchy with the CPython mixin methods installed on the root.
fn build_iobase_family_inner() -> IoFamily {
    use crate::object::MethodWrapper;
    use crate::types::{TypeFlags, TypeObject};
    let bt = crate::builtin_types::builtin_types();
    let mut dict = DictData::new();
    install_iobase_mixins(&mut dict);
    // `io.IOBase.register(...)` — ABC virtual-subclass registration.
    dict.insert(
        DictKey(Object::from_static("register")),
        Object::ClassMethod(MethodWrapper::new(Object::Builtin(Rc::new(BuiltinFn {
            name: "register",
            binds_instance: true,
            call: Box::new(io_abc_register),
            call_kw: None,
        })))),
    );
    let flags = || TypeFlags {
        is_exception: false,
        is_builtin: true,
    };
    let iobase = TypeObject::new_with_flags("IOBase", vec![bt.object_.clone()], dict, flags())
        .expect("IOBase must linearise");
    install_closed_property(&iobase);
    let child = |name: &'static str, base: &Rc<TypeObject>| {
        TypeObject::new_with_flags(name, vec![base.clone()], DictData::new(), flags())
            .expect("io child type must linearise")
    };
    let raw = child("RawIOBase", &iobase);
    // `BufferedIOBase` carries the default `readinto`/`readinto1` that delegate
    // to `read`/`read1` (CPython's `_bufferediobase_readinto_generic`), so a
    // pure-Python subclass that only implements `read`/`read1` still supports
    // `readinto`.
    let buffered = {
        let mut bd = DictData::new();
        iobase_method(&mut bd, "readinto", bufferediobase_readinto);
        iobase_method(&mut bd, "readinto1", bufferediobase_readinto1);
        TypeObject::new_with_flags("BufferedIOBase", vec![iobase.clone()], bd, flags())
            .expect("io child type must linearise")
    };
    let text = child("TextIOBase", &iobase);
    let fileio = child("FileIO", &raw);
    install_fileio_ctor(&fileio);
    IoFamily {
        iobase,
        raw,
        buffered,
        text,
        fileio,
    }
}

fn iobase_method(
    dict: &mut DictData,
    name: &'static str,
    body: fn(&[Object]) -> Result<Object, RuntimeError>,
) {
    dict.insert(
        DictKey(Object::from_static(name)),
        Object::Builtin(Rc::new(BuiltinFn {
            name,
            binds_instance: true,
            call: Box::new(body),
            call_kw: None,
        })),
    );
}

/// Install the `io.IOBase` mixin methods onto a fresh type dict.
fn install_iobase_mixins(dict: &mut DictData) {
    iobase_method(dict, "__enter__", iobase_enter);
    iobase_method(dict, "__exit__", iobase_exit);
    iobase_method(dict, "__iter__", iobase_iter);
    iobase_method(dict, "__next__", iobase_next);
    iobase_method(dict, "writelines", iobase_writelines);
    iobase_method(dict, "readline", iobase_readline);
    iobase_method(dict, "readlines", iobase_readlines);
    iobase_method(dict, "flush", iobase_flush);
    iobase_method(dict, "close", iobase_close);
    iobase_method(dict, "seekable", iobase_false);
    iobase_method(dict, "readable", iobase_false);
    iobase_method(dict, "writable", iobase_false);
    iobase_method(dict, "isatty", iobase_false);
    iobase_method(dict, "tell", iobase_tell);
    iobase_method(dict, "seek", iobase_unsupported);
    iobase_method(dict, "truncate", iobase_unsupported);
    iobase_method(dict, "fileno", iobase_unsupported);
    iobase_method(dict, "_checkClosed", iobase_check_closed);
    iobase_method(dict, "_checkReadable", iobase_check_readable);
    iobase_method(dict, "_checkWritable", iobase_check_writable);
    iobase_method(dict, "_checkSeekable", iobase_check_seekable);
}

/// `IOBase.closed` — reads the `_iobase_closed` flag set by `close()`
/// (subclasses with their own `closed` property override this via MRO).
fn install_closed_property(ty: &Rc<crate::types::TypeObject>) {
    fn closed_get(args: &[Object]) -> Result<Object, RuntimeError> {
        match args.first() {
            Some(Object::Instance(i)) => Ok(Object::Bool(matches!(
                tw_get(i, "_iobase_closed"),
                Some(Object::Bool(true))
            ))),
            Some(Object::File(f)) => Ok(Object::Bool(*f.closed.borrow())),
            _ => Ok(Object::Bool(false)),
        }
    }
    let prop = Object::Property(Rc::new(crate::object::PyProperty::new(
        Object::Builtin(Rc::new(BuiltinFn {
            name: "closed",
            binds_instance: true,
            call: Box::new(closed_get),
            call_kw: None,
        })),
        Object::None,
        Object::None,
        Object::from_static("True if the stream is closed"),
    )));
    crate::descr_registry::register(
        &prop,
        crate::descr_registry::DescrKind::GetSet,
        ty.clone(),
        "closed",
        None,
    );
    ty.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("closed")), prop);
}

/// Invoke `obj.<name>(*args)` through the running interpreter. Used by the
/// IOBase mixins and the Python-stream delegation paths.
fn py_call(obj: &Object, name: &str, args: &[Object]) -> Result<Object, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| value_error("no running interpreter for stream delegation"))?;
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    let method = interp.load_attr_public(obj, name)?;
    interp.call_object(method, args, &[])
}

/// Read `obj.<name>` through the running interpreter (evaluating any
/// descriptor / property). Unlike [`py_call`] this does *not* call the
/// result — used for property reads like `closed`.
fn py_get_attr(obj: &Object, name: &str) -> Result<Object, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| value_error("no running interpreter for stream delegation"))?;
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    interp.load_attr_public(obj, name)
}

fn obj_is_empty(o: &Object) -> bool {
    match o {
        Object::Bytes(b) => b.is_empty(),
        Object::ByteArray(b) => b.borrow().is_empty(),
        Object::Str(s) => s.is_empty(),
        Object::None => true,
        _ => false,
    }
}

fn iobase_self(args: &[Object]) -> Result<Object, RuntimeError> {
    args.first()
        .cloned()
        .ok_or_else(|| type_error("unbound IOBase method requires an instance"))
}

fn iobase_enter(args: &[Object]) -> Result<Object, RuntimeError> {
    let me = iobase_self(args)?;
    iobase_check_closed(args)?;
    Ok(me)
}

fn iobase_exit(args: &[Object]) -> Result<Object, RuntimeError> {
    let me = iobase_self(args)?;
    py_call(&me, "close", &[])?;
    Ok(Object::Bool(false))
}

fn iobase_iter(args: &[Object]) -> Result<Object, RuntimeError> {
    let me = iobase_self(args)?;
    iobase_check_closed(args)?;
    Ok(me)
}

fn iobase_next(args: &[Object]) -> Result<Object, RuntimeError> {
    let me = iobase_self(args)?;
    let line = py_call(&me, "readline", &[])?;
    if obj_is_empty(&line) {
        return Err(crate::error::stop_iteration());
    }
    Ok(line)
}

fn iobase_writelines(args: &[Object]) -> Result<Object, RuntimeError> {
    let me = iobase_self(args)?;
    let lines = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("writelines() takes exactly one argument"))?;
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| value_error("no running interpreter for stream delegation"))?;
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    let iter = interp.iter_object(lines)?;
    while let Some(item) = interp.iter_next_object(iter.clone())? {
        let write = interp.load_attr_public(&me, "write")?;
        interp.call_object(write, &[item], &[])?;
    }
    Ok(Object::None)
}

/// Default `IOBase.readline(size=-1)` — reads a byte/char at a time via
/// `self.read(1)`. Concrete streams (`BufferedReader`, `BZ2File`) override
/// this; it only backs bare IOBase subclasses.
fn iobase_readline(args: &[Object]) -> Result<Object, RuntimeError> {
    let me = iobase_self(args)?;
    let limit = match args.get(1) {
        Some(Object::Int(n)) => Some(*n),
        _ => None,
    };
    let mut out: Vec<u8> = Vec::new();
    loop {
        if let Some(l) = limit {
            if l >= 0 && out.len() as i64 >= l {
                break;
            }
        }
        let chunk = py_call(&me, "read", &[Object::Int(1)])?;
        let bytes = chunk.as_bytes_view().unwrap_or_default();
        if bytes.is_empty() {
            break;
        }
        let nl = bytes[0] == b'\n';
        out.extend_from_slice(&bytes);
        if nl {
            break;
        }
    }
    Ok(Object::new_bytes(out))
}

fn iobase_readlines(args: &[Object]) -> Result<Object, RuntimeError> {
    let me = iobase_self(args)?;
    let hint = match args.get(1) {
        Some(Object::Int(n)) => *n,
        _ => -1,
    };
    let mut lines: Vec<Object> = Vec::new();
    let mut total: i64 = 0;
    loop {
        let line = py_call(&me, "readline", &[])?;
        if obj_is_empty(&line) {
            break;
        }
        total += line.as_bytes_view().map(|b| b.len() as i64).unwrap_or(0);
        lines.push(line);
        if hint > 0 && total >= hint {
            break;
        }
    }
    Ok(Object::new_list(lines))
}

/// Extract a writable byte buffer (`bytearray` or a writable, contiguous
/// `memoryview` over one) as `(storage, start, capacity)` — the argument shape
/// CPython's `readinto`/`readinto1` accept.
fn readinto_writable_buffer(
    arg: Option<&Object>,
) -> Result<(Rc<RefCell<Vec<u8>>>, usize, usize), RuntimeError> {
    match arg {
        Some(Object::ByteArray(dst)) => {
            let cap = dst.borrow().len();
            Ok((dst.clone(), 0, cap))
        }
        Some(Object::MemoryView(mv)) => {
            if mv.released.get() {
                return Err(value_error(
                    "operation forbidden on released memoryview object",
                ));
            }
            if mv.readonly.get() || !mv.is_c_contiguous() {
                return Err(type_error(
                    "readinto() argument must be a writable bytes-like object",
                ));
            }
            match &mv.buffer {
                crate::object::MemoryViewBuffer::ByteArray(b) => {
                    Ok((b.clone(), mv.start.get(), mv.len.get()))
                }
                crate::object::MemoryViewBuffer::Bytes(_) => Err(type_error(
                    "readinto() argument must be a writable bytes-like object",
                )),
            }
        }
        _ => Err(type_error(
            "readinto() argument must be a writable bytes-like object",
        )),
    }
}

/// Default `BufferedIOBase.readinto(b)` / `readinto1(b)` (CPython's
/// `_bufferediobase_readinto_generic`): read up to `len(b)` bytes via the
/// stream's own `read`/`read1` and copy them into the buffer, returning the
/// count. Only used by subclasses that implement `read`/`read1` but not
/// `readinto` (concrete native streams register their own).
fn bufferediobase_readinto_via(args: &[Object], read_method: &str) -> Result<Object, RuntimeError> {
    let me = iobase_self(args)?;
    let (dst, start, cap) = readinto_writable_buffer(args.get(1))?;
    let data = py_call(&me, read_method, &[Object::Int(cap as i64)])?;
    let bytes = data
        .as_bytes_view()
        .ok_or_else(|| type_error(format!("{read_method}() should return bytes")))?;
    let n = bytes.len().min(cap);
    dst.borrow_mut()[start..start + n].copy_from_slice(&bytes[..n]);
    Ok(Object::Int(n as i64))
}

fn bufferediobase_readinto(args: &[Object]) -> Result<Object, RuntimeError> {
    bufferediobase_readinto_via(args, "read")
}

fn bufferediobase_readinto1(args: &[Object]) -> Result<Object, RuntimeError> {
    bufferediobase_readinto_via(args, "read1")
}

fn iobase_flush(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

fn iobase_close(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Some(Object::Instance(i)) = args.first() {
        if matches!(tw_get(i, "_iobase_closed"), Some(Object::Bool(true))) {
            return Ok(Object::None);
        }
        let me = Object::Instance(i.clone());
        let _ = py_call(&me, "flush", &[]);
        tw_set(i, "_iobase_closed", Object::Bool(true));
    }
    Ok(Object::None)
}

fn iobase_false(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(false))
}

fn iobase_tell(args: &[Object]) -> Result<Object, RuntimeError> {
    let me = iobase_self(args)?;
    py_call(&me, "seek", &[Object::Int(0), Object::Int(1)])
}

fn iobase_unsupported(_args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython's `IOBase.{seek,truncate,fileno}` raise `io.UnsupportedOperation`
    // (a subclass of both OSError and ValueError), not a plain OSError — code
    // does `self.assertRaises(io.UnsupportedOperation, fid.fileno)`.
    Err(unsupported_op("I/O operation not supported on this stream"))
}

fn iobase_check_closed(args: &[Object]) -> Result<Object, RuntimeError> {
    let me = iobase_self(args)?;
    // `closed` is usually a property — read (don't call) it.
    let is_closed = match py_get_attr(&me, "closed") {
        Ok(v) => v.is_truthy(),
        Err(_) => match &me {
            Object::Instance(i) => matches!(tw_get(i, "_iobase_closed"), Some(Object::Bool(true))),
            _ => false,
        },
    };
    if is_closed {
        return Err(value_error("I/O operation on closed file."));
    }
    Ok(Object::None)
}

fn iobase_check_readable(args: &[Object]) -> Result<Object, RuntimeError> {
    let me = iobase_self(args)?;
    if !matches!(py_call(&me, "readable", &[])?, Object::Bool(true)) {
        return Err(unsupported_op("File or stream is not readable."));
    }
    Ok(Object::None)
}

fn iobase_check_writable(args: &[Object]) -> Result<Object, RuntimeError> {
    let me = iobase_self(args)?;
    if !matches!(py_call(&me, "writable", &[])?, Object::Bool(true)) {
        return Err(unsupported_op("File or stream is not writable."));
    }
    Ok(Object::None)
}

fn iobase_check_seekable(args: &[Object]) -> Result<Object, RuntimeError> {
    let me = iobase_self(args)?;
    if !matches!(py_call(&me, "seekable", &[])?, Object::Bool(true)) {
        return Err(unsupported_op("File or stream is not seekable."));
    }
    Ok(Object::None)
}

/// The shared `io.UnsupportedOperation` type (an exception inheriting both
/// `OSError` and `ValueError`, as in CPython). Memoised so the same type
/// identity is reused across module rebuilds and reachable from core builtins.
pub fn unsupported_operation_class() -> Rc<crate::types::TypeObject> {
    static CLS: std::sync::OnceLock<Rc<crate::types::TypeObject>> = std::sync::OnceLock::new();
    CLS.get_or_init(|| {
        let bt = crate::builtin_types::builtin_types();
        crate::types::TypeObject::new_with_flags(
            "UnsupportedOperation",
            vec![bt.os_error.clone(), bt.value_error.clone()],
            DictData::new(),
            crate::types::TypeFlags {
                is_exception: true,
                is_builtin: true,
            },
        )
        .expect("UnsupportedOperation must linearise")
    })
    .clone()
}

/// Raise an `io.UnsupportedOperation` instance with `msg`.
pub fn unsupported_op(msg: &str) -> RuntimeError {
    let inst = crate::builtin_types::make_exception_with_class(unsupported_operation_class(), msg);
    RuntimeError::PyException(crate::error::PyException::new(inst))
}

// --- Python-stream delegation target for Buffered*/TextIOWrapper ----------

/// What a `Buffered*`/`TextIOWrapper` wraps: either WeavePy's native
/// in-memory/OS file (fast path) or an arbitrary Python stream object we
/// drive through its `read`/`write`/`seek`/… methods.
enum RawTarget {
    Native(Rc<PyFile>),
    Py(Object),
}

fn raw_target(obj: &Object) -> Result<RawTarget, RuntimeError> {
    match resolve_raw_pyfile(obj) {
        Ok(f) => Ok(RawTarget::Native(f)),
        Err(_) => match obj {
            Object::Instance(_) => Ok(RawTarget::Py(obj.clone())),
            _ => Err(value_error("raw stream unavailable")),
        },
    }
}

fn rdbuf_get(inst: &crate::types::PyInstance) -> Vec<u8> {
    match tw_get(inst, "_rdbuf") {
        Some(o) => o.as_bytes_view().unwrap_or_default(),
        None => Vec::new(),
    }
}

fn rdbuf_set(inst: &crate::types::PyInstance, v: Vec<u8>) {
    tw_set(inst, "_rdbuf", Object::new_bytes(v));
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
    // CPython resolves the codec in `TextIOWrapper.__init__`, so an unknown
    // encoding raises `LookupError` at construction (e.g.
    // `SpooledTemporaryFile(mode='w+', encoding='bad-encoding')`).
    crate::stdlib::io_full::validate_text_encoding(&encoding)?;
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
    // `newlines` starts `None` and is populated as terminators are read.
    tw_set(&inst, "newlines", Object::None);
    tw_set(&inst, "_detached", Object::Bool(false));
    Ok(Object::None)
}

/// CPython's `newline` modes for `TextIOWrapper`, controlling how line
/// endings are recognised on read and translated on write.
#[derive(Clone, Copy, PartialEq)]
enum NewlineMode {
    /// `newline=None` — universal: recognise `\r`, `\r\n`, `\n`; translate
    /// every recognised ending to `\n` on read, and `\n`→`os.linesep` on
    /// write (a no-op on Unix targets).
    Universal,
    /// `newline=''` — universal recognition on read, but **no** translation.
    Passthrough,
    /// `newline='\n'` — only `\n` is a line ending; no translation.
    Lf,
    /// `newline='\r'` — only `\r` is a line ending; `\n`→`\r` on write.
    Cr,
    /// `newline='\r\n'` — only `\r\n` is a line ending; `\n`→`\r\n` on write.
    CrLf,
}

fn tw_newline_mode(inst: &crate::types::PyInstance) -> NewlineMode {
    match tw_get(inst, "newline") {
        Some(Object::Str(s)) => match &*s {
            "" => NewlineMode::Passthrough,
            "\n" => NewlineMode::Lf,
            "\r" => NewlineMode::Cr,
            "\r\n" => NewlineMode::CrLf,
            // An invalid `newline=` would have been rejected at construction;
            // fall back to universal so reads still make progress.
            _ => NewlineMode::Universal,
        },
        _ => NewlineMode::Universal,
    }
}

// CPython's `IncrementalNewlineDecoder` records which line terminators it has
// seen so `TextIOWrapper.newlines` can report them. The flags are OR-ed as
// terminators are decoded and mapped to `None`/str/tuple on read.
const SEEN_CR: i64 = 1;
const SEEN_LF: i64 = 2;
const SEEN_CRLF: i64 = 4;

/// Map the OR-ed `SEEN_*` bitmask to CPython's `TextIOWrapper.newlines`
/// value: `None` when nothing was seen, a single string for one terminator,
/// or a `(\r, \n, \r\n)`-ordered tuple of those present.
fn newlines_value(mask: i64) -> Object {
    let mut parts: Vec<Object> = Vec::new();
    if mask & SEEN_CR != 0 {
        parts.push(Object::from_static("\r"));
    }
    if mask & SEEN_LF != 0 {
        parts.push(Object::from_static("\n"));
    }
    if mask & SEEN_CRLF != 0 {
        parts.push(Object::from_static("\r\n"));
    }
    match parts.len() {
        0 => Object::None,
        1 => parts.into_iter().next().unwrap(),
        _ => Object::new_tuple(parts),
    }
}

/// Scan the *untranslated* source text just consumed by a read and fold the
/// terminators it contains into the wrapper's `newlines` state. Only the
/// universal-recognition modes (`newline=None` and `newline=''`) track
/// newlines, matching CPython; the explicit modes leave `newlines` `None`.
fn tw_record_newlines(inst: &crate::types::PyInstance, src: &str) {
    if src.is_empty() {
        return;
    }
    let mut mask = match tw_get(inst, "_newlines_seen") {
        Some(Object::Int(n)) => n,
        _ => 0,
    };
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\r' => {
                if i + 1 < b.len() && b[i + 1] == b'\n' {
                    mask |= SEEN_CRLF;
                    i += 2;
                } else {
                    mask |= SEEN_CR;
                    i += 1;
                }
            }
            b'\n' => {
                mask |= SEEN_LF;
                i += 1;
            }
            _ => i += 1,
        }
    }
    tw_set(inst, "_newlines_seen", Object::Int(mask));
    tw_set(inst, "newlines", newlines_value(mask));
}

/// Translate decoded text on read under universal-newline mode (`\r\n` and
/// lone `\r` both become `\n`), stopping after `max_chars` output characters
/// if given. Returns the translated text and the number of **source bytes**
/// consumed so the caller can advance its cursor.
fn translate_universal(s: &str, max_chars: Option<usize>) -> (String, usize) {
    let mut out = String::new();
    let mut out_count = 0usize;
    let mut consumed = 0usize;
    let mut iter = s.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        match c {
            '\r' => {
                out.push('\n');
                if let Some(&(j, '\n')) = iter.peek() {
                    iter.next();
                    consumed = j + 1;
                } else {
                    consumed = i + 1;
                }
            }
            other => {
                out.push(other);
                consumed = i + other.len_utf8();
            }
        }
        out_count += 1;
        if max_chars.is_some_and(|m| out_count >= m) {
            break;
        }
    }
    (out, consumed)
}

/// Take up to `max_chars` characters from `s` verbatim (no newline
/// translation). Returns the slice and the number of bytes consumed.
fn take_chars(s: &str, max_chars: Option<usize>) -> (&str, usize) {
    match max_chars {
        None => (s, s.len()),
        Some(m) => {
            let end = s.char_indices().nth(m).map(|(i, _)| i).unwrap_or(s.len());
            (&s[..end], end)
        }
    }
}

/// Find the first line in `s` (including its terminator) per the given
/// newline mode. Returns the line slice and the number of bytes it spans.
/// `\r`/`\n` only ever appear as themselves in UTF-8/ASCII/Latin-1, so a
/// byte scan is safe.
fn find_line(s: &str, mode: NewlineMode) -> (&str, usize) {
    let b = s.as_bytes();
    let span = match mode {
        NewlineMode::Lf => b.iter().position(|&x| x == b'\n').map(|k| k + 1),
        NewlineMode::Cr => b.iter().position(|&x| x == b'\r').map(|k| k + 1),
        NewlineMode::CrLf => {
            let mut i = 0;
            let mut found = None;
            while i + 1 < b.len() {
                if b[i] == b'\r' && b[i + 1] == b'\n' {
                    found = Some(i + 2);
                    break;
                }
                i += 1;
            }
            found
        }
        NewlineMode::Universal | NewlineMode::Passthrough => {
            let mut i = 0;
            let mut found = None;
            while i < b.len() {
                match b[i] {
                    b'\n' => {
                        found = Some(i + 1);
                        break;
                    }
                    b'\r' => {
                        found = Some(if i + 1 < b.len() && b[i + 1] == b'\n' {
                            i + 2
                        } else {
                            i + 1
                        });
                        break;
                    }
                    _ => i += 1,
                }
            }
            found
        }
    };
    let end = span.unwrap_or(s.len());
    (&s[..end], end)
}

/// Materialise the underlying binary stream into a decoded character buffer
/// on first read. CPython's `TextIOWrapper` keeps an incrementally-decoded
/// snapshot buffer and serves `read`/`readline` from it; we read the whole
/// (finite) stream once and cache the decoded text plus a byte cursor. The
/// text is stored **untranslated** so newline handling can be applied per
/// the mode at serve time.
fn tw_ensure_decoded(inst: &Rc<crate::types::PyInstance>) -> Result<(), RuntimeError> {
    if matches!(tw_get(inst, "_dec_done"), Some(Object::Bool(true))) {
        return Ok(());
    }
    let raw = match tw_buffer_target(inst)? {
        RawTarget::Native(file) => file.read_bytes(None)?,
        RawTarget::Py(buffer) => py_call(&buffer, "read", &[])?
            .as_bytes_view()
            .unwrap_or_default(),
    };
    let encoding = tw_encoding(inst);
    let errors = tw_errors(inst);
    let text = crate::stdlib::codecs_mod::decode_bytes(&raw, &encoding, &errors)?;
    tw_set(inst, "_dec_buf", Object::from_str(text));
    tw_set(inst, "_dec_pos", Object::Int(0));
    tw_set(inst, "_dec_done", Object::Bool(true));
    Ok(())
}

/// Drop the decoded-snapshot cache so the next read re-materialises from the
/// (newly repositioned) underlying buffer.
fn tw_reset_decoded(inst: &crate::types::PyInstance) {
    let mut d = inst.dict.borrow_mut();
    for k in ["_dec_buf", "_dec_pos", "_dec_done"] {
        d.shift_remove(&DictKey(Object::from_static(k)));
    }
}

fn tw_dec_state(inst: &crate::types::PyInstance) -> (Rc<str>, usize) {
    let buf = match tw_get(inst, "_dec_buf") {
        Some(Object::Str(s)) => s,
        _ => Rc::from(""),
    };
    let pos = match tw_get(inst, "_dec_pos") {
        Some(Object::Int(n)) => n.max(0) as usize,
        _ => 0,
    };
    let pos = pos.min(buf.len());
    (buf, pos)
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
    // Newline translation on write: only `\r`/`\r\n` modes rewrite `\n`.
    // `None` maps `\n`→os.linesep, which is `\n` on WeavePy's Unix targets.
    let translated = match tw_newline_mode(&inst) {
        NewlineMode::Cr => text.replace('\n', "\r"),
        NewlineMode::CrLf => text.replace('\n', "\r\n"),
        _ => text.clone(),
    };
    let encoding = tw_encoding(&inst);
    let errors = tw_errors(&inst);
    let bytes = crate::stdlib::codecs_mod::encode_str(&translated, &encoding, &errors)?;
    match tw_buffer_target(&inst)? {
        RawTarget::Native(file) => {
            file.write_bytes(&bytes)?;
        }
        RawTarget::Py(buffer) => {
            py_call(&buffer, "write", &[Object::new_bytes(bytes)])?;
        }
    }
    // TextIOWrapper.write returns the number of characters written (the
    // length of the *original* argument, before newline translation).
    Ok(Object::Int(text.chars().count() as i64))
}

fn tw_read(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    tw_ensure_decoded(&inst)?;
    let mode = tw_newline_mode(&inst);
    let (buf, pos) = tw_dec_state(&inst);
    let remaining = &buf[pos..];
    // Size is measured in characters of the *translated* output; a missing,
    // `None`, or negative size means "read all".
    let size = match args.get(1) {
        Some(Object::Int(n)) if *n >= 0 => Some(*n as usize),
        _ => None,
    };
    let (out, consumed) = match mode {
        NewlineMode::Universal => translate_universal(remaining, size),
        _ => {
            let (slice, c) = take_chars(remaining, size);
            (slice.to_string(), c)
        }
    };
    if matches!(mode, NewlineMode::Universal | NewlineMode::Passthrough) {
        tw_record_newlines(&inst, &remaining[..consumed]);
    }
    tw_set(&inst, "_dec_pos", Object::Int((pos + consumed) as i64));
    Ok(Object::from_str(out))
}

fn tw_readline(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    tw_ensure_decoded(&inst)?;
    let mode = tw_newline_mode(&inst);
    let (buf, pos) = tw_dec_state(&inst);
    let remaining = &buf[pos..];
    if remaining.is_empty() {
        return Ok(Object::from_str(String::new()));
    }
    let (line, consumed) = find_line(remaining, mode);
    let out = match mode {
        NewlineMode::Universal => translate_universal(line, None).0,
        _ => line.to_string(),
    };
    if matches!(mode, NewlineMode::Universal | NewlineMode::Passthrough) {
        tw_record_newlines(&inst, &remaining[..consumed]);
    }
    tw_set(&inst, "_dec_pos", Object::Int((pos + consumed) as i64));
    Ok(Object::from_str(out))
}

fn tw_flush(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    match tw_buffer_target(&inst) {
        Ok(RawTarget::Native(file)) => {
            file.flush()?;
        }
        Ok(RawTarget::Py(buffer)) => {
            let _ = py_call(&buffer, "flush", &[]);
        }
        Err(_) => {}
    }
    Ok(Object::None)
}

fn tw_flush_noop(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

fn tw_close(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    match tw_buffer_target(&inst) {
        Ok(RawTarget::Native(file)) => {
            let _ = file.flush();
            file.close();
        }
        Ok(RawTarget::Py(buffer)) => {
            let _ = py_call(&buffer, "close", &[]);
        }
        Err(_) => {}
    }
    Ok(Object::None)
}

fn tw_seek(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    // Repositioning the underlying buffer invalidates the decoded snapshot.
    tw_reset_decoded(&inst);
    let offset = match args.get(1) {
        Some(Object::Int(n)) => *n,
        _ => 0,
    };
    let whence = match args.get(2) {
        Some(Object::Int(n)) => *n as i32,
        _ => 0,
    };
    match tw_buffer_target(&inst)? {
        RawTarget::Native(file) => {
            let pos = file.seek(offset as isize, whence)?;
            Ok(Object::Int(pos as i64))
        }
        RawTarget::Py(buffer) => py_call(
            &buffer,
            "seek",
            &[Object::Int(offset), Object::Int(i64::from(whence))],
        ),
    }
}

fn tw_tell(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    match tw_buffer_target(&inst)? {
        RawTarget::Native(file) => {
            // SEEK_CUR with 0 offset reports the current position.
            let pos = file.seek(0, 1)?;
            Ok(Object::Int(pos as i64))
        }
        RawTarget::Py(buffer) => py_call(&buffer, "tell", &[]),
    }
}

fn tw_fileno(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(value_error("underlying stream has no fileno"))
}

fn tw_false(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(false))
}

fn tw_readable(args: &[Object]) -> Result<Object, RuntimeError> {
    tw_capability(args, "readable")
}

fn tw_writable(args: &[Object]) -> Result<Object, RuntimeError> {
    tw_capability(args, "writable")
}

fn tw_seekable(args: &[Object]) -> Result<Object, RuntimeError> {
    tw_capability(args, "seekable")
}

fn tw_capability(args: &[Object], name: &str) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    match tw_buffer_target(&inst) {
        Ok(RawTarget::Native(_)) => Ok(Object::Bool(true)),
        Ok(RawTarget::Py(buffer)) => Ok(Object::Bool(matches!(
            py_call(&buffer, name, &[]),
            Ok(Object::Bool(true))
        ))),
        Err(_) => Ok(Object::Bool(false)),
    }
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

/// `TextIOWrapper.__exit__` — flush + close the wrapper (and thus the
/// underlying buffer). Essential for stacked compression streams whose
/// final bytes are only emitted on close.
fn tw_exit(args: &[Object]) -> Result<Object, RuntimeError> {
    tw_close(args)?;
    Ok(Object::Bool(false))
}

/// `Buffered*.__exit__` — flush + close the buffered wrapper.
fn bw_exit(args: &[Object]) -> Result<Object, RuntimeError> {
    bw_close(args)?;
    Ok(Object::Bool(false))
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
pub(crate) fn make_buffered(
    name: &'static str,
    base: Rc<crate::types::TypeObject>,
) -> Rc<crate::types::TypeObject> {
    use crate::types::{TypeFlags, TypeObject};
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
    method("read1", bw_read1);
    method("readinto", bw_readinto);
    method("readinto1", bw_readinto);
    method("readline", bw_readline);
    method("readlines", iobase_readlines);
    method("writelines", iobase_writelines);
    method("peek", bw_peek);
    method("flush", bw_flush);
    method("close", bw_close);
    method("seek", bw_seek);
    method("tell", bw_tell);
    method("truncate", bw_truncate);
    method("readable", bw_readable);
    method("writable", bw_writable);
    method("seekable", bw_seekable);
    method("isatty", bw_false);
    method("fileno", bw_fileno);
    method("detach", bw_detach);
    method("__iter__", tw_iter);
    method("__next__", bw_next);
    method("__enter__", tw_enter);
    method("__exit__", bw_exit);
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
    let ty = TypeObject::new_with_flags(
        name,
        vec![base],
        dict,
        TypeFlags {
            is_exception: false,
            is_builtin: true,
        },
    )
    .expect("buffered type");
    install_delegating_closed(&ty, bw_closed);
    // CPython's `Buffered*` expose `.name` / `.mode` by forwarding to the
    // wrapped raw stream (`buffered_name_get`/`buffered_mode_get`), so e.g.
    // `tarfile.ExFileObject(BufferedReader).name` resolves to the underlying
    // `_FileInFile.name`, and `open('f','rb').name == 'f'`.
    install_delegating_attr(&ty, "name", "Name of the underlying stream", bw_name);
    install_delegating_attr(&ty, "mode", "Mode of the underlying stream", bw_mode);
    ty
}

/// `Buffered*.name` — forwards to the wrapped raw stream's `name`, raising
/// `AttributeError` when the raw stream has none (CPython delegates the
/// attribute lookup and lets the `AttributeError` propagate).
fn bw_name(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let raw =
        tw_get(&inst, "raw").ok_or_else(|| crate::error::attribute_error("'name'".to_owned()))?;
    py_get_attr(&raw, "name")
}

/// `Buffered*.mode` — forwards to the wrapped raw stream's `mode`.
fn bw_mode(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let raw =
        tw_get(&inst, "raw").ok_or_else(|| crate::error::attribute_error("'mode'".to_owned()))?;
    py_get_attr(&raw, "mode")
}

fn bw_self(args: &[Object]) -> Result<Rc<crate::types::PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) => Ok(i.clone()),
        _ => Err(type_error(
            "unbound buffered method requires a buffered-stream instance",
        )),
    }
}

/// Resolve `self.raw` to either the native backing file or an arbitrary
/// Python stream object to delegate to.
fn bw_target(inst: &crate::types::PyInstance) -> Result<RawTarget, RuntimeError> {
    let raw = tw_get(inst, "raw").ok_or_else(|| value_error("raw stream unavailable"))?;
    raw_target(&raw)
}

const BW_CHUNK: usize = 8192;

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
        Some(o) => o.as_bytes_view().ok_or_else(|| {
            type_error(format!(
                "a bytes-like object is required, not '{}'",
                o.type_name()
            ))
        }),
        None => Err(type_error("write() takes exactly one argument")),
    }
}

fn bw_write(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    match bw_target(&inst)? {
        RawTarget::Native(raw) => {
            let bytes = bw_bytes_arg(args.get(1))?;
            let n = raw.write_bytes(&bytes)?;
            Ok(Object::Int(n as i64))
        }
        RawTarget::Py(raw) => {
            let data = args
                .get(1)
                .cloned()
                .ok_or_else(|| type_error("write() takes exactly one argument"))?;
            py_call(&raw, "write", &[data])
        }
    }
}

/// Parse a read-size argument: `None`/absent → read-all; an int (or
/// `__index__`-able) → that count; anything else (e.g. `float`) → TypeError,
/// matching CPython's `BufferedReader.read`.
fn bw_size_arg(arg: Option<&Object>) -> Result<Option<i64>, RuntimeError> {
    match arg {
        None | Some(Object::None) => Ok(None),
        Some(Object::Int(n)) => Ok(Some(*n)),
        Some(Object::Bool(b)) => Ok(Some(i64::from(*b))),
        Some(other) => crate::builtins::coerce_index_i64(other)
            .map(Some)
            .map_err(|_| {
                type_error(format!(
                    "argument should be integer or None, not '{}'",
                    other.type_name()
                ))
            }),
    }
}

fn bw_read(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let size_opt = bw_size_arg(args.get(1))?;
    let read_all = size_opt.is_none_or(|n| n < 0);
    let size = size_opt.filter(|n| *n >= 0).unwrap_or(0) as usize;
    match bw_target(&inst)? {
        RawTarget::Native(raw) => {
            let data = if read_all {
                raw.read_bytes(None)?
            } else {
                raw.read_bytes(Some(size))?
            };
            Ok(Object::new_bytes(data))
        }
        RawTarget::Py(raw) => {
            let mut buf = rdbuf_get(&inst);
            if read_all {
                let rest = py_call(&raw, "read", &[])?;
                buf.extend_from_slice(&rest.as_bytes_view().unwrap_or_default());
                rdbuf_set(&inst, Vec::new());
                return Ok(Object::new_bytes(buf));
            }
            while buf.len() < size {
                let need = size - buf.len();
                let chunk = py_call(&raw, "read", &[Object::Int(need as i64)])?;
                let cb = chunk.as_bytes_view().unwrap_or_default();
                if cb.is_empty() {
                    break;
                }
                buf.extend_from_slice(&cb);
            }
            let take = size.min(buf.len());
            let out = buf[..take].to_vec();
            rdbuf_set(&inst, buf[take..].to_vec());
            Ok(Object::new_bytes(out))
        }
    }
}

/// `read1(size=-1)` — at most one underlying read.
fn bw_read1(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    match bw_target(&inst)? {
        RawTarget::Native(_) => bw_read(args),
        RawTarget::Py(raw) => {
            let size = match bw_size_arg(args.get(1))? {
                Some(n) if n >= 0 => n as usize,
                _ => BW_CHUNK,
            };
            let mut buf = rdbuf_get(&inst);
            if buf.is_empty() {
                let chunk = py_call(&raw, "read", &[Object::Int(size as i64)])?;
                buf.extend_from_slice(&chunk.as_bytes_view().unwrap_or_default());
            }
            let take = size.min(buf.len());
            let out = buf[..take].to_vec();
            rdbuf_set(&inst, buf[take..].to_vec());
            Ok(Object::new_bytes(out))
        }
    }
}

fn bw_peek(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    match bw_target(&inst)? {
        RawTarget::Native(raw) => {
            // Read a chunk and rewind so the bytes stay available — a faithful
            // `peek()` for the seekable in-memory streams we wrap.
            let data = raw.read_bytes(None)?;
            if !data.is_empty() {
                raw.seek(-(data.len() as isize), 1)?;
            }
            Ok(Object::new_bytes(data))
        }
        RawTarget::Py(raw) => {
            let mut buf = rdbuf_get(&inst);
            if buf.is_empty() {
                let chunk = py_call(&raw, "read", &[Object::Int(BW_CHUNK as i64)])?;
                buf.extend_from_slice(&chunk.as_bytes_view().unwrap_or_default());
                rdbuf_set(&inst, buf.clone());
            }
            Ok(Object::new_bytes(buf))
        }
    }
}

fn bw_readinto(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    // Accept a bytearray or a writable, contiguous `memoryview` window over
    // one (CPython parity — `readinto` takes any read-write buffer).
    let (dst, start, cap) = match args.get(1) {
        Some(Object::ByteArray(dst)) => {
            let cap = dst.borrow().len();
            (dst.clone(), 0usize, cap)
        }
        Some(Object::MemoryView(mv)) => {
            if mv.released.get() {
                return Err(value_error(
                    "operation forbidden on released memoryview object",
                ));
            }
            if mv.readonly.get() || !mv.is_c_contiguous() {
                return Err(type_error(
                    "readinto() argument must be a writable bytes-like object",
                ));
            }
            match &mv.buffer {
                crate::object::MemoryViewBuffer::ByteArray(b) => {
                    (b.clone(), mv.start.get(), mv.len.get())
                }
                crate::object::MemoryViewBuffer::Bytes(_) => {
                    return Err(type_error(
                        "readinto() argument must be a writable bytes-like object",
                    ))
                }
            }
        }
        _ => {
            return Err(type_error(
                "readinto() argument must be a writable bytes-like object",
            ))
        }
    };
    match bw_target(&inst)? {
        RawTarget::Native(raw) => {
            let data = raw.read_bytes(Some(cap))?;
            let n = data.len();
            dst.borrow_mut()[start..start + n].copy_from_slice(&data);
            Ok(Object::Int(n as i64))
        }
        RawTarget::Py(_) => {
            // Delegate through `read(cap)` then copy into the buffer.
            let data = bw_read(&[args[0].clone(), Object::Int(cap as i64)])?;
            let bytes = data.as_bytes_view().unwrap_or_default();
            let n = bytes.len().min(cap);
            dst.borrow_mut()[start..start + n].copy_from_slice(&bytes[..n]);
            Ok(Object::Int(n as i64))
        }
    }
}

fn bw_readline(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let limit = match args.get(1) {
        Some(Object::Int(n)) if *n >= 0 => Some(*n as usize),
        _ => None,
    };
    match bw_target(&inst)? {
        RawTarget::Native(raw) => {
            let mut line: Vec<u8> = Vec::new();
            loop {
                if let Some(l) = limit {
                    if line.len() >= l {
                        break;
                    }
                }
                let b = raw.read_bytes(Some(1))?;
                if b.is_empty() {
                    break;
                }
                line.push(b[0]);
                if b[0] == b'\n' {
                    break;
                }
            }
            Ok(Object::new_bytes(line))
        }
        RawTarget::Py(raw) => {
            let mut out: Vec<u8> = Vec::new();
            loop {
                if let Some(l) = limit {
                    if out.len() >= l {
                        break;
                    }
                }
                let mut buf = rdbuf_get(&inst);
                if buf.is_empty() {
                    let chunk = py_call(&raw, "read", &[Object::Int(BW_CHUNK as i64)])?;
                    buf = chunk.as_bytes_view().unwrap_or_default();
                    if buf.is_empty() {
                        rdbuf_set(&inst, Vec::new());
                        break;
                    }
                }
                if let Some(nl) = buf.iter().position(|&c| c == b'\n') {
                    out.extend_from_slice(&buf[..=nl]);
                    rdbuf_set(&inst, buf[nl + 1..].to_vec());
                    break;
                }
                out.extend_from_slice(&buf);
                rdbuf_set(&inst, Vec::new());
            }
            if let Some(l) = limit {
                if out.len() > l {
                    let extra = out.split_off(l);
                    let mut buf = extra;
                    buf.extend_from_slice(&rdbuf_get(&inst));
                    rdbuf_set(&inst, buf);
                }
            }
            Ok(Object::new_bytes(out))
        }
    }
}

fn bw_next(args: &[Object]) -> Result<Object, RuntimeError> {
    let line = bw_readline(args)?;
    if obj_is_empty(&line) {
        return Err(crate::error::stop_iteration());
    }
    Ok(line)
}

fn bw_flush(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    match bw_target(&inst) {
        Ok(RawTarget::Native(raw)) => {
            raw.flush()?;
        }
        Ok(RawTarget::Py(raw)) => {
            let _ = py_call(&raw, "flush", &[]);
        }
        Err(_) => {}
    }
    Ok(Object::None)
}

fn bw_close(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    match bw_target(&inst) {
        Ok(RawTarget::Native(raw)) => {
            let _ = raw.flush();
            raw.close();
        }
        Ok(RawTarget::Py(raw)) => {
            let _ = py_call(&raw, "flush", &[]);
            let _ = py_call(&raw, "close", &[]);
        }
        Err(_) => {}
    }
    Ok(Object::None)
}

fn bw_seek(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    // `offset` must be an integer (or `__index__`-able): `None`, `bytes`, a
    // tuple or a `float` are all `TypeError`, matching CPython's argument
    // clinic. `whence` defaults to SEEK_SET and must be 0/1/2.
    let offset = match args.get(1) {
        Some(o) => crate::builtins::coerce_index_i64(o)?,
        None => return Err(type_error("seek() missing required argument 'offset'")),
    };
    let whence = match args.get(2) {
        None | Some(Object::None) => 0,
        Some(o) => crate::builtins::coerce_index_i64(o)? as i32,
    };
    if !matches!(whence, 0..=2) {
        return Err(value_error(format!(
            "Invalid whence ({whence}, should be 0, 1 or 2)"
        )));
    }
    match bw_target(&inst)? {
        RawTarget::Native(raw) => {
            let pos = raw.seek(offset as isize, whence)?;
            Ok(Object::Int(pos as i64))
        }
        RawTarget::Py(raw) => {
            let buffered = rdbuf_get(&inst).len() as i64;
            // Discard the read-ahead buffer; recompute the absolute target for
            // SEEK_CUR (the buffer hides bytes already consumed from `raw`).
            rdbuf_set(&inst, Vec::new());
            let pos = if whence == 1 {
                let cur = py_call(&raw, "tell", &[])?.as_i64().unwrap_or(0);
                let target = cur - buffered + offset;
                py_call(&raw, "seek", &[Object::Int(target), Object::Int(0)])?
            } else {
                py_call(
                    &raw,
                    "seek",
                    &[Object::Int(offset), Object::Int(i64::from(whence))],
                )?
            };
            Ok(pos)
        }
    }
}

fn bw_tell(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    match bw_target(&inst)? {
        RawTarget::Native(raw) => {
            let pos = raw.seek(0, 1)?;
            Ok(Object::Int(pos as i64))
        }
        RawTarget::Py(raw) => {
            let buffered = rdbuf_get(&inst).len() as i64;
            let cur = py_call(&raw, "tell", &[])?.as_i64().unwrap_or(0);
            Ok(Object::Int(cur - buffered))
        }
    }
}

/// `BufferedWriter/Random.truncate(pos=None)` — flush the buffer, then
/// resize the underlying raw stream (default: current position). CPython
/// returns the new size and leaves the stream position unchanged.
fn bw_truncate(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let size = args.get(1).cloned();
    match bw_target(&inst)? {
        RawTarget::Native(raw) => {
            raw.flush()?;
            let pos = match size.as_ref() {
                None | Some(Object::None) => None,
                Some(o) => Some(crate::builtins::coerce_index_i64(o)?.max(0) as u64),
            };
            Ok(Object::Int(raw.truncate(pos)? as i64))
        }
        RawTarget::Py(raw) => {
            let _ = py_call(&raw, "flush", &[]);
            match size.as_ref() {
                None | Some(Object::None) => py_call(&raw, "truncate", &[]),
                Some(o) => py_call(&raw, "truncate", std::slice::from_ref(o)),
            }
        }
    }
}

fn bw_readable(args: &[Object]) -> Result<Object, RuntimeError> {
    bw_capability(args, "readable")
}

fn bw_writable(args: &[Object]) -> Result<Object, RuntimeError> {
    bw_capability(args, "writable")
}

fn bw_seekable(args: &[Object]) -> Result<Object, RuntimeError> {
    bw_capability(args, "seekable")
}

fn bw_capability(args: &[Object], name: &str) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    match bw_target(&inst) {
        Ok(RawTarget::Native(_)) => Ok(Object::Bool(true)),
        // CPython's `Buffered*.{readable,writable,seekable}` return
        // `self.raw.<name>()` and let any exception propagate — a stream-mode
        // `tarfile._FileInFile.seekable()` calls the underlying `_Stream`'s
        // (absent) `seekable`, so the `AttributeError` must surface, not be
        // silently coerced to `False` (`test_extractfile_attrs`).
        Ok(RawTarget::Py(raw)) => Ok(Object::Bool(py_call(&raw, name, &[])?.is_truthy())),
        Err(_) => Ok(Object::Bool(false)),
    }
}

fn bw_false(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(false))
}

fn bw_fileno(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    match bw_target(&inst)? {
        RawTarget::Native(_) => Err(value_error("underlying stream has no fileno")),
        RawTarget::Py(raw) => py_call(&raw, "fileno", &[]),
    }
}

fn bw_detach(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let raw = tw_get(&inst, "raw").ok_or_else(|| value_error("raw stream unavailable"))?;
    inst.dict
        .borrow_mut()
        .shift_remove(&DictKey(Object::from_static("raw")));
    Ok(raw)
}
