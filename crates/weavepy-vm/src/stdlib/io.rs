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
        // and tests can `class C(io.BytesIO)`. Exported from the memoised
        // family so they are the same objects `class_of` reports for a native
        // `io.BytesIO()`/`io.StringIO()` (`type(io.BytesIO()) is io.BytesIO`).
        d.insert(
            DictKey(Object::from_static("BytesIO")),
            Object::Type(fam.bytes_io.clone()),
        );
        d.insert(
            DictKey(Object::from_static("StringIO")),
            Object::Type(fam.string_io.clone()),
        );
        d.insert(
            DictKey(Object::from_static("IncrementalNewlineDecoder")),
            Object::Type(make_incremental_newline_decoder()),
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
        // `BufferedReader`/`BufferedWriter`/`BufferedRandom` come from the
        // memoised family (shared with `class_of`); only `BufferedRWPair`
        // (which no native `Object::File` maps to) is minted here.
        let buffered_rw_pair = make_buffered("BufferedRWPair", fam.buffered.clone());
        set_type_module(&buffered_rw_pair, "_io");
        for (name, cls) in [
            ("BufferedReader", fam.buffered_reader.clone()),
            ("BufferedWriter", fam.buffered_writer.clone()),
            ("BufferedRandom", fam.buffered_random.clone()),
            ("BufferedRWPair", buffered_rw_pair),
        ] {
            d.insert(DictKey(Object::from_static(name)), Object::Type(cls));
        }
        // A functional `TextIOWrapper`: a text layer over a binary buffer
        // (e.g. `io.TextIOWrapper(io.BytesIO())`). `write` encodes through to
        // the wrapped buffer; `.buffer` exposes it again. Shared with
        // `class_of` (`type(open(p,'r')) is io.TextIOWrapper`).
        d.insert(
            DictKey(Object::from_static("TextIOWrapper")),
            Object::Type(fam.text_io_wrapper.clone()),
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
    method("truncate", tw_truncate);
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
    method("__repr__", tw_repr);
    // `reconfigure(*, encoding, errors, newline, line_buffering, write_through)`
    // is keyword-only in CPython — register with a `call_kw` so the kwargs form
    // (`reconfigure(newline='')`, `reconfigure(encoding='utf-8')`) works.
    dict.insert(
        DictKey(Object::from_static("reconfigure")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "reconfigure",
            binds_instance: true,
            call: Box::new(tw_reconfigure),
            call_kw: Some(Box::new(tw_reconfigure_kw)),
        })),
    );
    // `__new__` only allocates (CPython `textiowrapper_new`); `__init__` binds
    // the buffer. Keeping them separate lets `tp.__new__(tp)` yield an
    // uninitialized object whose methods raise `ValueError`
    // (`test_uninitialized`) instead of eagerly requiring `buffer`.
    dict.insert(
        DictKey(Object::from_static("__new__")),
        Object::StaticMethod(crate::object::MethodWrapper::new(Object::Builtin(Rc::new(
            BuiltinFn {
                name: "TextIOWrapper.__new__",
                binds_instance: false,
                call: Box::new(bw_new),
                call_kw: Some(Box::new(|args, _kw| bw_new(args))),
            },
        )))),
    );
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
    // `TextIOWrapper.buffer` is a read-only getset in CPython
    // (`textiowrapper_buffer`), so `txt.buffer = b` raises `AttributeError`
    // (`test_readonly_attributes`); reading it after `detach()` raises
    // `ValueError` (`CHECK_ATTACHED`).
    install_delegating_attr(&ty, "buffer", "The underlying buffer", tw_buffer_get);
    install_delegating_attr(&ty, "name", "Name of the underlying stream", tw_name);
    ty
}

/// `TextIOWrapper.buffer` getter — returns the wrapped buffer, raising
/// `ValueError` once detached. Installed read-only so assignment is rejected.
fn tw_buffer_get(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    tw_get(&inst, "buffer").ok_or_else(|| value_error("underlying buffer has been detached"))
}

/// `TextIOWrapper.name` getter — delegates to the wrapped buffer's `name`
/// (CPython's `textiowrapper_name_get`), raising `AttributeError` when neither
/// the wrapper nor the buffer carries one.
fn tw_name(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    let buffer = tw_get(&inst, "buffer")
        .ok_or_else(|| crate::error::attribute_error("'name'".to_owned()))?;
    py_get_attr(&buffer, "name")
}

/// Whether `o` is a bare descriptor/callable object — used to filter the
/// attribute-lookup leak where an in-memory stream returns its type's unbound
/// `name`/`mode` property object instead of raising `AttributeError`.
fn is_descriptor_object(o: &Object) -> bool {
    matches!(
        o,
        Object::Property(_)
            | Object::Function(_)
            | Object::Builtin(_)
            | Object::ClassMethod(_)
            | Object::StaticMethod(_)
            | Object::SlotDescriptor(_)
            | Object::BoundMethod(_)
    )
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
    file.set_io_kind(crate::object::IoKind::BytesIO);
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
    file.set_io_kind(crate::object::IoKind::StringIO);
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
    let raw = fileio_open_raw(&file, &binmode, opener.as_ref())?;
    match raw {
        Object::File(f) => {
            f.closefd.set(closefd);
            // `io.FileIO` is the raw, unbuffered layer (a `RawIOBase`).
            f.set_io_kind(crate::object::IoKind::Raw);
            Ok(wrap_memory_stream(&cls, f))
        }
        other => Ok(other),
    }
}

/// Open the raw descriptor for a `FileIO` from `(file, binmode, opener)` —
/// shared by `FileIO.__new__` and the in-place `FileIO.__init__`. With an
/// `opener` it calls `opener(file, flags) -> fd` and adopts the fd; otherwise
/// it opens the path/fd directly through the builtin `open`.
fn fileio_open_raw(
    file: &Object,
    binmode: &str,
    opener: Option<&Object>,
) -> Result<Object, RuntimeError> {
    match opener {
        Some(opener) => {
            // CPython calls `opener(file, flags)` → fd, then adopts that fd.
            let ptr = crate::vm_singletons::current_interpreter_ptr()
                .ok_or_else(|| crate::error::runtime_error("FileIO: no running interpreter"))?;
            // SAFETY: published by an enclosing VM frame on this thread.
            let interp = unsafe { &mut *ptr };
            let globals = interp.builtins_dict();
            let flags = fileio_open_flags(binmode);
            let fd = interp.call_object_with_globals(
                opener,
                &[file.clone(), Object::Int(flags)],
                &[],
                &globals,
            )?;
            crate::builtins::b_open(&[fd, Object::from_str(binmode.to_owned())])
        }
        None => crate::builtins::b_open(&[file.clone(), Object::from_str(binmode.to_owned())]),
    }
}

/// `FileIO.__init__(file, mode='r', closefd=True, opener=None)` — CPython's
/// raw file constructor may be *re-run* on a live object to re-point it at a
/// new descriptor (`test_io.test_fileio_closefd`). We open the replacement
/// stream and transplant its descriptor into the existing instance, releasing
/// the old fd per the instance's current `closefd`.
fn fileio_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let f = match args.first() {
        Some(Object::File(f)) => f.clone(),
        Some(Object::Instance(i)) => match &i.native {
            Some(Object::File(f)) => f.clone(),
            _ => return Err(type_error("FileIO.__init__() requires a FileIO instance")),
        },
        _ => return Err(type_error("FileIO.__init__() requires a FileIO instance")),
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
    let raw = fileio_open_raw(&file, &binmode, opener.as_ref())?;
    let new_pf = match raw {
        Object::File(nf) => nf,
        _ => return Err(type_error("FileIO.__init__(): could not open file")),
    };
    // Steal the freshly-opened descriptor out of the temporary `PyFile` (so its
    // drop won't close the fd) and move it into the existing instance.
    let new_backend = std::mem::replace(
        &mut *new_pf.backend.borrow_mut(),
        crate::object::FileBackend::MemBytes {
            data: Rc::new(RefCell::new(Vec::new())),
            pos: 0,
        },
    );
    f.reinit_fileio(new_backend, closefd);
    f.set_io_kind(crate::object::IoKind::Raw);
    Ok(Object::None)
}

/// `os.open` flag bits for a normalized binary `FileIO` mode — only used to
/// hand a plausible `flags` value to a user `opener` callback. Delegates to the
/// shared host-platform builder so the bits match the `os.O_*` constants.
fn fileio_open_flags(mode: &str) -> i64 {
    crate::stdlib::os::open_flags_for_mode(mode)
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
    // CPython's `FileIO.__init__` is callable again on a live object to
    // re-point it at a new descriptor (`test_io.test_fileio_closefd`); install
    // it so the call doesn't fall through to `object.__init__` (which rejects
    // the `file`/`closefd` arguments).
    ty.dict.borrow_mut().insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "FileIO.__init__",
            binds_instance: true,
            call: Box::new(|a| fileio_init(a, &[])),
            call_kw: Some(Box::new(fileio_init)),
        })),
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
/// `buffered_closed_get` reads `self.raw.closed`). `BufferedRWPair.closed`
/// reads `self.writer.closed` instead (`test_writer_close_error_on_close`).
fn bw_closed(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let target = if bw_typename(&inst) == "BufferedRWPair" {
        bw_writer_target(&inst)
    } else {
        bw_target(&inst)
    };
    match target {
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

/// CPython's `CHECK_INITIALIZED`: a `TextIOWrapper` produced by `__new__`
/// without a following `__init__` has no `encoding` recorded, and every I/O
/// method (and `repr`) raises `ValueError("I/O operation on uninitialized
/// object")` (`test_io.test_uninitialized`). A *detached* wrapper keeps its
/// `encoding` (only `buffer` is removed), so this guard does not fire for it.
fn tw_require_init(inst: &crate::types::PyInstance) -> Result<(), RuntimeError> {
    if !matches!(tw_get(inst, "_initialized"), Some(Object::Bool(true))) {
        return Err(value_error("I/O operation on uninitialized object"));
    }
    Ok(())
}

/// Invoke `self.flush()` *virtually* so a monkeypatched instance-level `flush`
/// runs (CPython's `IOBase.close` calls `self.flush()` through the type slot).
/// `test_io.test_flush_error_on_close` assigns `txt.flush = bad_flush` and
/// expects `close()` to run it and re-raise its `OSError`.
fn tw_virtual_flush(me: &Object) -> Result<Object, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| crate::error::runtime_error("no running interpreter"))?;
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    let flush_fn = interp.load_attr_public(me, "flush")?;
    interp.call_object(flush_fn, &[], &[])
}

/// CPython's `_checkReadable`: `TextIOWrapper.read*` first asserts the wrapped
/// buffer is readable, raising `io.UnsupportedOperation` (an `OSError`) if not
/// (`test_io.test_unreadable`).
fn tw_check_readable(inst: &crate::types::PyInstance) -> Result<(), RuntimeError> {
    let readable = match tw_buffer_target(inst) {
        Ok(RawTarget::Native(file)) => file.readable(),
        Ok(RawTarget::Py(buffer)) => {
            matches!(py_call(&buffer, "readable", &[]), Ok(Object::Bool(true)))
        }
        Err(_) => false,
    };
    if !readable {
        return Err(unsupported_op("not readable"));
    }
    Ok(())
}

/// Raise `ValueError` if the text layer (or its wrapped buffer) is closed —
/// CPython's `CHECK_CLOSED` / `CHECK_INITIALIZED` guard on read/write methods.
fn tw_check_open(inst: &crate::types::PyInstance, _what: &str) -> Result<(), RuntimeError> {
    let closed = match tw_get(inst, "buffer") {
        Some(buf) => py_get_attr(&buf, "closed")
            .map(|c| c.is_truthy())
            .unwrap_or(false),
        None => true,
    };
    if closed {
        return Err(value_error("I/O operation on closed file."));
    }
    Ok(())
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
    // Pickling/copy support, inherited by user subclasses. A subclass instance
    // reduces through `object.__reduce_ex__`'s `__newobj__` path (which calls
    // `cls.__new__(cls)`, not `__init__`, so a subclass whose `__init__` takes
    // required args — test_memoryio's `PickleTestMemIO` — still round-trips):
    // it calls these `__getstate__`/`__setstate__` hooks to capture/restore the
    // buffer, position, newline, and instance `__dict__`.
    method("__getstate__", crate::builtins::file_getstate_mem);
    method("__setstate__", crate::builtins::file_setstate_mem);
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

// ===========================================================================
// IncrementalNewlineDecoder — CPython's `_io.IncrementalNewlineDecoder`.
//
// A faithful native port of `_pyio.IncrementalNewlineDecoder`: it wraps an
// underlying incremental decoder (or `None`, for direct `str` input) and
// performs universal-newline translation, carrying a pending lone `\r` across
// `decode()` calls so a `\r\n` split over a buffer boundary still collapses to
// one `\n`. `test_io`'s `CIncrementalNewlineDecoderTest` exercises
// `decode`/`getstate`/`setstate`/`reset`/`newlines` against this directly, and
// `TextIOWrapper` uses the same algorithm internally.
// ===========================================================================

const IND_LF: i64 = 1;
const IND_CR: i64 = 2;
const IND_CRLF: i64 = 4;

fn ind_self(args: &[Object]) -> Result<Rc<crate::types::PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) => Ok(i.clone()),
        _ => Err(type_error(
            "unbound method IncrementalNewlineDecoder requires an instance",
        )),
    }
}

/// Methods raise `ValueError` on an instance produced by a bare `__new__`
/// without a following `__init__` (CPython `test_uninitialized`).
fn ind_check_init(inst: &crate::types::PyInstance) -> Result<(), RuntimeError> {
    if matches!(tw_get(inst, "_ind_init"), Some(Object::Bool(true))) {
        Ok(())
    } else {
        Err(value_error("IncrementalNewlineDecoder.__init__() not called"))
    }
}

/// Decompose a 2-tuple `(buffer, flag)`; anything else is a `TypeError`
/// (`decoder.setstate(42)` must raise, per `test_newline_decoder`).
fn ind_tuple2(obj: &Object) -> Result<(Object, Object), RuntimeError> {
    match obj {
        Object::Tuple(items) if items.len() == 2 => Ok((items[0].clone(), items[1].clone())),
        _ => Err(type_error("a tuple of length 2 is required")),
    }
}

fn ind_newlines_value(seennl: i64) -> Object {
    let s = |x| Object::from_static(x);
    let pair = |a, b| Object::new_tuple(vec![Object::from_static(a), Object::from_static(b)]);
    match seennl {
        x if x == IND_LF => s("\n"),
        x if x == IND_CR => s("\r"),
        x if x == IND_CR | IND_LF => pair("\r", "\n"),
        x if x == IND_CRLF => s("\r\n"),
        x if x == IND_LF | IND_CRLF => pair("\n", "\r\n"),
        x if x == IND_CR | IND_CRLF => pair("\r", "\r\n"),
        x if x == IND_CR | IND_LF | IND_CRLF => Object::new_tuple(vec![
            Object::from_static("\r"),
            Object::from_static("\n"),
            Object::from_static("\r\n"),
        ]),
        _ => Object::None,
    }
}

fn ind_new(args: &[Object]) -> Result<Object, RuntimeError> {
    let cls = match args.first() {
        Some(Object::Type(t)) => t.clone(),
        _ => {
            return Err(type_error(
                "IncrementalNewlineDecoder.__new__(X): X is not a type object",
            ))
        }
    };
    let inst = Object::Instance(Rc::new(crate::types::PyInstance::new(cls)));
    crate::gc_trace::track(inst.clone());
    Ok(inst)
}

fn ind_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = ind_self(args)?;
    let positional = &args[1..];
    let kw = |name: &str| {
        kwargs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let decoder = positional
        .first()
        .cloned()
        .or_else(|| kw("decoder"))
        .ok_or_else(|| {
            type_error("IncrementalNewlineDecoder() missing required argument 'decoder' (pos 1)")
        })?;
    let translate = positional
        .get(1)
        .cloned()
        .or_else(|| kw("translate"))
        .ok_or_else(|| {
            type_error("IncrementalNewlineDecoder() missing required argument 'translate' (pos 2)")
        })?;
    let errors = positional
        .get(2)
        .cloned()
        .or_else(|| kw("errors"))
        .unwrap_or_else(|| Object::from_static("strict"));
    let errors_s = match errors {
        Object::Str(s) => s.to_string(),
        Object::None => "strict".to_owned(),
        other => {
            return Err(type_error(format!(
                "errors must be str, not {}",
                other.type_name()
            )))
        }
    };
    tw_set(&inst, "_ind_decoder", decoder);
    tw_set(&inst, "_ind_translate", Object::Bool(translate.is_truthy()));
    tw_set(&inst, "_ind_errors", Object::from_str(errors_s));
    tw_set(&inst, "_ind_seennl", Object::Int(0));
    tw_set(&inst, "_ind_pendingcr", Object::Bool(false));
    tw_set(&inst, "_ind_init", Object::Bool(true));
    Ok(Object::None)
}

fn ind_decode(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = ind_self(args)?;
    ind_check_init(&inst)?;
    let input = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("decode() missing 1 required positional argument: 'input'"))?;
    let final_ = kwargs
        .iter()
        .find(|(k, _)| k == "final")
        .map(|(_, v)| v.is_truthy())
        .or_else(|| args.get(2).map(|v| v.is_truthy()))
        .unwrap_or(false);
    let decoder = tw_get(&inst, "_ind_decoder").unwrap_or(Object::None);
    // With no underlying decoder the input is already text; otherwise route it
    // through the wrapped incremental decoder (passing `final` positionally,
    // matching `codecs.IncrementalDecoder.decode(input, final=False)`).
    let mut output = if matches!(decoder, Object::None) {
        match input {
            Object::Str(s) => s.to_string(),
            other => {
                return Err(type_error(format!(
                    "decode() argument must be str when no decoder is set, not {}",
                    other.type_name()
                )))
            }
        }
    } else {
        match py_call(&decoder, "decode", &[input, Object::Bool(final_)])? {
            Object::Str(s) => s.to_string(),
            other => {
                return Err(type_error(format!(
                    "decoder.decode() should return str, not {}",
                    other.type_name()
                )))
            }
        }
    };
    let pendingcr = matches!(tw_get(&inst, "_ind_pendingcr"), Some(Object::Bool(true)));
    if pendingcr && (!output.is_empty() || final_) {
        output.insert(0, '\r');
        tw_set(&inst, "_ind_pendingcr", Object::Bool(false));
    }
    // Retain a trailing `\r` (not final) so the next pass can see a `\r\n`
    // straddling the boundary as a unit.
    if !final_ && output.ends_with('\r') {
        output.pop();
        tw_set(&inst, "_ind_pendingcr", Object::Bool(true));
    }
    let crlf = output.matches("\r\n").count() as i64;
    let cr = output.matches('\r').count() as i64 - crlf;
    let lf = output.matches('\n').count() as i64 - crlf;
    let mut seennl = match tw_get(&inst, "_ind_seennl") {
        Some(Object::Int(n)) => n,
        _ => 0,
    };
    if lf != 0 {
        seennl |= IND_LF;
    }
    if cr != 0 {
        seennl |= IND_CR;
    }
    if crlf != 0 {
        seennl |= IND_CRLF;
    }
    tw_set(&inst, "_ind_seennl", Object::Int(seennl));
    if matches!(tw_get(&inst, "_ind_translate"), Some(Object::Bool(true))) {
        if crlf != 0 {
            output = output.replace("\r\n", "\n");
        }
        if cr != 0 {
            output = output.replace('\r', "\n");
        }
    }
    Ok(Object::from_str(output))
}

fn ind_getstate(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = ind_self(args)?;
    ind_check_init(&inst)?;
    let decoder = tw_get(&inst, "_ind_decoder").unwrap_or(Object::None);
    let (buf, flag) = if matches!(decoder, Object::None) {
        (Object::Bytes(Rc::from(&b""[..])), 0i64)
    } else {
        let st = py_call(&decoder, "getstate", &[])?;
        let (b, f) = ind_tuple2(&st)?;
        let f = match f {
            Object::Int(n) => n,
            other => {
                return Err(type_error(format!(
                    "getstate() flag should be int, not {}",
                    other.type_name()
                )))
            }
        };
        (b, f)
    };
    let mut flag = flag << 1;
    if matches!(tw_get(&inst, "_ind_pendingcr"), Some(Object::Bool(true))) {
        flag |= 1;
    }
    Ok(Object::new_tuple(vec![buf, Object::Int(flag)]))
}

fn ind_setstate(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = ind_self(args)?;
    ind_check_init(&inst)?;
    let state = args
        .get(1)
        .ok_or_else(|| type_error("setstate() missing 1 required positional argument: 'state'"))?;
    let (buf, flag) = ind_tuple2(state)?;
    let flag = match flag {
        Object::Int(n) => n,
        other => {
            return Err(type_error(format!(
                "setstate() flag should be int, not {}",
                other.type_name()
            )))
        }
    };
    tw_set(&inst, "_ind_pendingcr", Object::Bool(flag & 1 != 0));
    let decoder = tw_get(&inst, "_ind_decoder").unwrap_or(Object::None);
    if !matches!(decoder, Object::None) {
        let inner = Object::new_tuple(vec![buf, Object::Int(flag >> 1)]);
        py_call(&decoder, "setstate", &[inner])?;
    }
    Ok(Object::None)
}

fn ind_reset(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = ind_self(args)?;
    ind_check_init(&inst)?;
    tw_set(&inst, "_ind_seennl", Object::Int(0));
    tw_set(&inst, "_ind_pendingcr", Object::Bool(false));
    let decoder = tw_get(&inst, "_ind_decoder").unwrap_or(Object::None);
    if !matches!(decoder, Object::None) {
        py_call(&decoder, "reset", &[])?;
    }
    Ok(Object::None)
}

fn ind_newlines_get(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = ind_self(args)?;
    let seennl = match tw_get(&inst, "_ind_seennl") {
        Some(Object::Int(n)) => n,
        _ => 0,
    };
    Ok(ind_newlines_value(seennl))
}

/// Build the functional `IncrementalNewlineDecoder` type (`_io`/`io`). Mirrors
/// CPython's C accelerator: a real, subclassable type with working
/// `decode`/`getstate`/`setstate`/`reset` methods and a `newlines` property.
pub(crate) fn make_incremental_newline_decoder() -> Rc<crate::types::TypeObject> {
    use crate::object::MethodWrapper;
    use crate::types::{TypeFlags, TypeObject};
    let bt = crate::builtin_types::builtin_types();
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
    method("getstate", ind_getstate);
    method("setstate", ind_setstate);
    method("reset", ind_reset);
    dict.insert(
        DictKey(Object::from_static("decode")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "decode",
            binds_instance: true,
            call: Box::new(|a| ind_decode(a, &[])),
            call_kw: Some(Box::new(ind_decode)),
        })),
    );
    dict.insert(
        DictKey(Object::from_static("__new__")),
        Object::StaticMethod(MethodWrapper::new(Object::Builtin(Rc::new(BuiltinFn {
            name: "__new__",
            binds_instance: false,
            call: Box::new(ind_new),
            call_kw: Some(Box::new(|a, _kw| ind_new(a))),
        })))),
    );
    dict.insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            binds_instance: true,
            call: Box::new(|a| ind_init(a, &[])),
            call_kw: Some(Box::new(ind_init)),
        })),
    );
    let ty = TypeObject::new_with_flags(
        "IncrementalNewlineDecoder",
        vec![bt.object_.clone()],
        dict,
        TypeFlags {
            is_exception: false,
            is_builtin: true,
        },
    )
    .expect("IncrementalNewlineDecoder type");
    let prop = Object::Property(Rc::new(crate::object::PyProperty::new(
        Object::Builtin(Rc::new(BuiltinFn {
            name: "newlines",
            binds_instance: true,
            call: Box::new(ind_newlines_get),
            call_kw: None,
        })),
        Object::None,
        Object::None,
        Object::from_static("The newlines seen so far (None / str / tuple)."),
    )));
    crate::descr_registry::register(
        &prop,
        crate::descr_registry::DescrKind::GetSet,
        ty.clone(),
        "newlines",
        None,
    );
    ty.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("newlines")), prop);
    set_type_module(&ty, "_io");
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
#[allow(dead_code)]
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
/// CPython's `io.open`/`TextIOWrapper` accept the special encoding name
/// `"locale"`, which resolves to the current locale's encoding (the result of
/// `locale.getpreferredencoding(False)`); `codecs.lookup("locale")` itself
/// raises. WeavePy's text layer is UTF-8, so `"locale"` maps to `"utf-8"`
/// (`test_io.test_reconfigure_locale`).
fn resolve_locale_encoding(name: &str) -> String {
    if name.eq_ignore_ascii_case("locale") {
        "utf-8".to_owned()
    } else {
        name.to_owned()
    }
}

fn tw_buffer_target(inst: &crate::types::PyInstance) -> Result<RawTarget, RuntimeError> {
    let buf =
        tw_get(inst, "buffer").ok_or_else(|| value_error("underlying buffer has been detached"))?;
    // Address the *immediate* buffer, never reaching *through* an intermediate
    // `Buffered*` layer to the concrete `PyFile`. CPython's `TextIOWrapper`
    // calls `self.buffer.write/read/flush`, so a `TextIOWrapper(BufferedWriter(
    // BytesIO()))` must route writes through the `BufferedWriter`'s own buffer
    // (`test_io.test_detach`/`test_internal_buffer_size`/`test_bufio_write_through`)
    // rather than landing bytes straight in the `BytesIO`.
    bw_raw_target(&buf).map_err(|_| value_error("underlying buffer has been detached"))
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
    // The concrete, instantiable stream classes. These are the very objects
    // the `io`/`_io` module exposes *and* the classes `class_of` reports for a
    // native `Object::File` (keyed by its `IoKind`), so `type(open(p,'rb')) is
    // io.BufferedReader`, `type(io.BytesIO()) is io.BytesIO`, and
    // `isinstance(open(p,'rb'), io.BufferedReader)` all hold like CPython.
    pub buffered_reader: Rc<crate::types::TypeObject>,
    pub buffered_writer: Rc<crate::types::TypeObject>,
    pub buffered_random: Rc<crate::types::TypeObject>,
    pub text_io_wrapper: Rc<crate::types::TypeObject>,
    pub bytes_io: Rc<crate::types::TypeObject>,
    pub string_io: Rc<crate::types::TypeObject>,
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
    use crate::object::IoKind;
    let fam = build_iobase_family();
    let is = |t: &Rc<crate::types::TypeObject>| Rc::ptr_eq(t, info);
    let kind = file.io_kind.get();
    if is(&fam.iobase) {
        return Some(true);
    }
    if is(&fam.raw) || is(&fam.fileio) {
        // Only an unbuffered `FileIO` (`open(..., buffering=0)`,
        // `io.FileIO(fd)`) is a `RawIOBase`; the buffered/text layers are not.
        return Some(kind == IoKind::Raw);
    }
    if is(&fam.buffered) {
        // `BufferedIOBase`: the buffered binary layers and `BytesIO`.
        return Some(matches!(
            kind,
            IoKind::BufferedReader
                | IoKind::BufferedWriter
                | IoKind::BufferedRandom
                | IoKind::BytesIO
        ));
    }
    if is(&fam.text) {
        // `TextIOBase`: `TextIOWrapper` and `StringIO`.
        return Some(matches!(kind, IoKind::Text | IoKind::StringIO));
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
    // `RawIOBase` carries the default `read`/`readall` that delegate to the
    // subclass `readinto` (CPython's `_pyio.RawIOBase.read`/`readall`), so a
    // pure-Python subclass that only implements `readinto` still supports
    // `read()`/`readall()` — and the layered `_pyio.BufferedReader` can call
    // `raw.readall()` on such a raw.
    let raw = {
        let mut rd = DictData::new();
        iobase_method(&mut rd, "read", rawiobase_read);
        iobase_method(&mut rd, "readall", rawiobase_readall);
        TypeObject::new_with_flags("RawIOBase", vec![iobase.clone()], rd, flags())
            .expect("io child type must linearise")
    };
    // `BufferedIOBase` carries the default `readinto`/`readinto1` that delegate
    // to `read`/`read1` (CPython's `_bufferediobase_readinto_generic`), so a
    // pure-Python subclass that only implements `read`/`read1` still supports
    // `readinto`.
    let buffered = {
        let mut bd = DictData::new();
        iobase_method(&mut bd, "readinto", bufferediobase_readinto);
        iobase_method(&mut bd, "readinto1", bufferediobase_readinto1);
        // The abstract `read`/`read1`/`write`/`detach` (CPython
        // `_io.BufferedIOBase`): default bodies raise `io.UnsupportedOperation`
        // and are overridden by every concrete `Buffered*`/`BytesIO`. They must
        // exist on the ABC so a subclass can alias them (`test_optional_abilities`
        // does `write = self.BufferedIOBase.write` to make a read-only stream).
        iobase_method(&mut bd, "read", bufferediobase_read_unsup);
        iobase_method(&mut bd, "read1", bufferediobase_read1_unsup);
        iobase_method(&mut bd, "write", bufferediobase_write_unsup);
        iobase_method(&mut bd, "detach", bufferediobase_detach_unsup);
        TypeObject::new_with_flags("BufferedIOBase", vec![iobase.clone()], bd, flags())
            .expect("io child type must linearise")
    };
    let text = child("TextIOBase", &iobase);
    let fileio = child("FileIO", &raw);
    install_fileio_ctor(&fileio);
    // The concrete, instantiable stream classes, built once and memoised on the
    // family so the module export and `class_of` share one identity each
    // (`type(open(p,'rb')) is io.BufferedReader`). Their MRO descends from the
    // matching ABC (`BufferedReader → BufferedIOBase → IOBase → object`, etc.),
    // which is what makes `isinstance` against the ABCs hold via ordinary MRO.
    let buffered_reader = make_buffered("BufferedReader", buffered.clone());
    let buffered_writer = make_buffered("BufferedWriter", buffered.clone());
    let buffered_random = make_buffered("BufferedRandom", buffered.clone());
    let text_io_wrapper = make_text_io_wrapper(text.clone());
    let bytes_io = make_memory_stream(
        "BytesIO",
        buffered.clone(),
        bytesio_new,
        "BytesIO.__new__",
        bytesio_init,
        "BytesIO.__init__",
    );
    let string_io = make_memory_stream(
        "StringIO",
        text.clone(),
        stringio_new,
        "StringIO.__new__",
        stringio_init,
        "StringIO.__init__",
    );
    // CPython reports the io ABCs as living in `io` and the concrete C
    // accelerator types in `_io` (`io.IOBase.__module__ == 'io'`,
    // `io.BytesIO.__module__ == '_io'`). This drives `type(x).__module__`,
    // the class repr (`<class '_io.BytesIO'>`), and — load-bearing — lets
    // `pickle` find `_io.BytesIO` by reference so `BytesIO`/`StringIO`
    // instances round-trip.
    for abc in [&iobase, &raw, &buffered, &text] {
        set_type_module(abc, "io");
    }
    for conc in [
        &fileio,
        &buffered_reader,
        &buffered_writer,
        &buffered_random,
        &text_io_wrapper,
        &bytes_io,
        &string_io,
    ] {
        set_type_module(conc, "_io");
    }
    IoFamily {
        iobase,
        raw,
        buffered,
        text,
        fileio,
        buffered_reader,
        buffered_writer,
        buffered_random,
        text_io_wrapper,
        bytes_io,
        string_io,
    }
}

/// Tag a type with its CPython `__module__` (`io` for the ABCs, `_io` for the
/// concrete accelerator classes). Stored as a string dict entry, which is what
/// `qualified_display_name`, `type.__module__`, and `pickle`'s global lookup
/// all read.
pub(crate) fn set_type_module(ty: &Rc<crate::types::TypeObject>, module: &'static str) {
    ty.dict.borrow_mut().insert(
        DictKey(Object::from_static("__module__")),
        Object::from_static(module),
    );
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
    iobase_method(dict, "__del__", iobase_del);
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

/// `RawIOBase.read(size=-1)` — CPython's default: a negative size delegates to
/// `readall()`; otherwise allocate a `bytearray(size)`, fill it with one
/// `self.readinto(b)`, and return `bytes(b[:n])` (or `None` for a non-blocking
/// `readinto` that returned `None`).
fn rawiobase_read(args: &[Object]) -> Result<Object, RuntimeError> {
    let me = iobase_self(args)?;
    let size = match args.get(1) {
        Some(Object::Int(n)) => *n,
        Some(Object::None) | None => -1,
        Some(Object::Bool(b)) => i64::from(*b),
        Some(_) => return Err(type_error("read() argument must be an integer")),
    };
    if size < 0 {
        return py_call(&me, "readall", &[]);
    }
    let buf = Object::new_bytearray(vec![0u8; size as usize]);
    let n = py_call(&me, "readinto", std::slice::from_ref(&buf))?;
    let n_i = match n {
        Object::None => return Ok(Object::None),
        Object::Int(n) => n,
        Object::Bool(b) => i64::from(b),
        _ => return Err(type_error("readinto() should return a non-negative integer")),
    };
    // gh-129830: a `readinto` whose return falls outside `[0, len(buffer)]` is a
    // `ValueError`, not a silent buffer overrun or a `TypeError`
    // (`test_io.test_RawIOBase_read_bounds_checking` covers a too-large *and*
    // negative return). CPython's C `RawIOBase.read` bounds-checks
    // `n < 0 || n > size`.
    if n_i < 0 || n_i > size {
        return Err(value_error(format!(
            "readinto returned {n_i} outside buffer size {size}"
        )));
    }
    let n = n_i as usize;
    let bytes = buf.as_bytes_view().unwrap_or_default();
    Ok(Object::new_bytes(bytes[..n.min(bytes.len())].to_vec()))
}

/// `RawIOBase.readall()` — read and return all bytes until EOF by repeatedly
/// calling `self.read(DEFAULT_BUFFER_SIZE)` (CPython's `_pyio.RawIOBase.readall`).
fn rawiobase_readall(args: &[Object]) -> Result<Object, RuntimeError> {
    let me = iobase_self(args)?;
    let mut res: Vec<u8> = Vec::new();
    let mut got_any = false;
    loop {
        let data = py_call(&me, "read", &[Object::Int(8192)])?;
        if matches!(data, Object::None) {
            if got_any {
                break;
            }
            return Ok(Object::None);
        }
        let bytes = data.as_bytes_view().unwrap_or_default();
        if bytes.is_empty() {
            break;
        }
        got_any = true;
        res.extend_from_slice(&bytes);
    }
    Ok(Object::new_bytes(res))
}

fn iobase_readlines(args: &[Object]) -> Result<Object, RuntimeError> {
    let me = iobase_self(args)?;
    let hint = match args.get(1) {
        Some(Object::Int(n)) => *n,
        Some(Object::Bool(b)) => i64::from(*b),
        Some(Object::None) | None => -1,
        Some(other) => {
            return Err(type_error(format!(
                "'{}' object cannot be interpreted as an integer",
                other.type_name()
            )))
        }
    };
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| value_error("no running interpreter for stream delegation"))?;
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    // Mirror CPython's `IOBase.readlines`: iterate `self` through the iterator
    // protocol (`for line in self`) rather than calling `readline` directly, so
    // a subclass overriding `__next__` is honoured, and accumulate `len(line)`
    // — which raises `TypeError` for a line with no `__len__`
    // (`test_next_nonsizeable` returns `None` from `__next__`).
    let iter = interp.iter_object(me.clone())?;
    let mut lines: Vec<Object> = Vec::new();
    let mut total: i64 = 0;
    while let Some(line) = interp.iter_next_object(iter.clone())? {
        if hint > 0 {
            total += match &line {
                Object::Str(s) => s.chars().count() as i64,
                Object::Bytes(b) => b.len() as i64,
                Object::ByteArray(b) => b.borrow().len() as i64,
                other => {
                    let lenf = interp.load_attr_public(other, "__len__").map_err(|_| {
                        type_error(format!("object of type '{}' has no len()", other.type_name()))
                    })?;
                    match interp.call_object(lenf, &[], &[])? {
                        Object::Int(n) => n,
                        _ => return Err(type_error("__len__() should return >= 0")),
                    }
                }
            };
        }
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
        Some(Object::MemoryView(mv)) => memoryview_writable_buffer(mv),
        Some(other) => instance_writable_buffer(other).unwrap_or_else(|| {
            Err(type_error(
                "readinto() argument must be a writable bytes-like object",
            ))
        }),
        None => Err(type_error(
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
    // CPython's `BufferedIOBase._readinto` assigns `b[:n] = data`; a `read()`
    // that overruns the destination buffer is a `ValueError` (the memoryview
    // slice-assignment size mismatch), not a silent truncation
    // (`test_io.test_readinto_buffer_overflow`).
    if bytes.len() > cap {
        return Err(value_error(format!(
            "{read_method}() returned too much data: {} bytes requested, {} returned",
            cap,
            bytes.len()
        )));
    }
    let n = bytes.len();
    dst.borrow_mut()[start..start + n].copy_from_slice(&bytes[..n]);
    Ok(Object::Int(n as i64))
}

fn bufferediobase_readinto(args: &[Object]) -> Result<Object, RuntimeError> {
    bufferediobase_readinto_via(args, "read")
}

fn bufferediobase_readinto1(args: &[Object]) -> Result<Object, RuntimeError> {
    bufferediobase_readinto_via(args, "read1")
}

// The abstract `BufferedIOBase` operations: each raises `io.UnsupportedOperation`
// (CPython's default bodies), overridden by every concrete buffered stream.
fn bufferediobase_read_unsup(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(unsupported_op("read"))
}

fn bufferediobase_read1_unsup(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(unsupported_op("read1"))
}

fn bufferediobase_write_unsup(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(unsupported_op("write"))
}

fn bufferediobase_detach_unsup(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(unsupported_op("detach"))
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
        // CPython's `IOBase.close` flushes, then marks the stream closed
        // *whether or not* the flush succeeded, then re-raises the flush
        // error (`_io__IOBase_close_impl` fetches/chains the exception around
        // setting `__IOBase_closed`). `test_io.test_close_assert` relies on a
        // failing `flush()` propagating out of `close()`.
        let flush_res = py_call(&me, "flush", &[]);
        tw_set(i, "_iobase_closed", Object::Bool(true));
        flush_res?;
    }
    Ok(Object::None)
}

fn iobase_false(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(false))
}

/// `IOBase.__del__` — CPython's `_PyIOBase_finalize` / `_pyio.IOBase.__del__`:
/// if the stream isn't already closed, close it (which flushes). The call is
/// virtually dispatched so a subclass `close`/`flush` override runs
/// (`test_override_destructor`), and a failure from `close()` propagates to the
/// finalizer driver, which reports it via `sys.unraisablehook`
/// (`test_error_through_destructor`). Exposing it here also makes every io
/// object a GC finalization candidate, so an abandoned `BufferedWriter` flushes
/// its buffer to disk when collected (`test_garbage_collection`).
fn iobase_del(args: &[Object]) -> Result<Object, RuntimeError> {
    let me = iobase_self(args)?;
    // If reading `closed` fails the object is in an unusable, half-built state
    // (CPython swallows the `AttributeError` and returns).
    let closed = match py_get_attr(&me, "closed") {
        Ok(c) => c.is_truthy(),
        Err(_) => return Ok(Object::None),
    };
    if closed {
        return Ok(Object::None);
    }
    py_call(&me, "close", &[])?;
    Ok(Object::None)
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
        let ty = crate::types::TypeObject::new_with_flags(
            "UnsupportedOperation",
            vec![bt.os_error.clone(), bt.value_error.clone()],
            DictData::new(),
            crate::types::TypeFlags {
                is_exception: true,
                is_builtin: true,
            },
        )
        .expect("UnsupportedOperation must linearise");
        // CPython creates this as `io.UnsupportedOperation` (via
        // `PyType_Type("io.UnsupportedOperation", …)`), so `__module__` is
        // `"io"` — `test_io.test___all__` builds the expected export set from
        // `dir(io)` filtered by `__module__ in ("io", "_io")`.
        set_type_module(&ty, "io");
        ty
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

/// Resolve a *buffered layer's* immediate raw stream. Unlike [`raw_target`],
/// this never reaches *through* an intermediate wrapper object: a native
/// `Object::File` is addressed directly, but any `Object::Instance` (a Python
/// `RawIOBase`, or even another `Buffered*`) keeps its own
/// `read`/`readinto`/`write` — CPython's C buffered object calls the raw it was
/// handed, honouring monkeypatched methods (`test_bad_readinto_value`/`type`).
fn bw_raw_target(obj: &Object) -> Result<RawTarget, RuntimeError> {
    match obj {
        Object::File(f) => Ok(RawTarget::Native(f.clone())),
        Object::Instance(_) => Ok(RawTarget::Py(obj.clone())),
        _ => raw_target(obj),
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

/// The configured buffer size (`DEFAULT_BUFFER_SIZE` when not stored), used by
/// the faithful `_pyio`-style buffered read/write algorithms.
fn bw_buffer_size(inst: &crate::types::PyInstance) -> usize {
    match tw_get(inst, "_bufsize") {
        Some(Object::Int(n)) if n > 0 => n as usize,
        _ => BW_CHUNK,
    }
}

/// Pending write-buffer bytes for `BufferedWriter`/`BufferedRandom`.
fn wrbuf_get(inst: &crate::types::PyInstance) -> Vec<u8> {
    match tw_get(inst, "_wrbuf") {
        Some(o) => o.as_bytes_view().unwrap_or_default(),
        None => Vec::new(),
    }
}

fn wrbuf_set(inst: &crate::types::PyInstance, v: Vec<u8>) {
    tw_set(inst, "_wrbuf", Object::new_bytes(v));
}

/// One read from the underlying raw stream.
///
/// Returns `Ok(None)` to signal "would block / no data available yet" (a raw
/// `read()` returning `None`), mirroring CPython's `nodata_val` handling.
/// `n == None` means "read all currently available" (`raw.read()` with no
/// argument); `Some(k)` requests at most `k` bytes.
fn raw_read(target: &RawTarget, n: Option<usize>) -> Result<Option<Vec<u8>>, RuntimeError> {
    match target {
        RawTarget::Native(raw) => Ok(Some(raw.read_bytes(n)?)),
        RawTarget::Py(raw) => {
            let res = match n {
                None => py_call(raw, "read", &[])?,
                Some(k) => py_call(raw, "read", &[Object::Int(k as i64)])?,
            };
            if matches!(res, Object::None) {
                Ok(None)
            } else {
                let bytes = res.as_bytes_view().unwrap_or_default();
                // A raw stream that returns more than requested is misbehaving
                // (`test_misbehaved_io`): CPython raises OSError.
                if let Some(req) = n {
                    if bytes.len() > req {
                        return Err(crate::error::os_error(format!(
                            "raw read() returned invalid length {} (should have been between 0 and {})",
                            bytes.len(),
                            req
                        )));
                    }
                }
                Ok(Some(bytes))
            }
        }
    }
}

/// One `readinto` into `dst`, returning the number of bytes read, or `None`
/// for "would block". Prefers the raw's native `readinto` (CPython parity);
/// falls back to `read` for Py raws lacking `readinto`.
fn raw_readinto(target: &RawTarget, dst: &mut [u8]) -> Result<Option<usize>, RuntimeError> {
    match target {
        RawTarget::Native(raw) => {
            let data = raw.read_bytes(Some(dst.len()))?;
            let n = data.len().min(dst.len());
            dst[..n].copy_from_slice(&data[..n]);
            Ok(Some(n))
        }
        RawTarget::Py(raw) => {
            let mv = Object::new_bytearray(vec![0u8; dst.len()]);
            let res = py_call(raw, "readinto", &[mv.clone()])?;
            if matches!(res, Object::None) {
                return Ok(None);
            }
            // CPython's `_bufferedreader_raw_read` coerces the result via
            // `__index__`; a non-integer return (`test_bad_readinto_type`)
            // surfaces as `OSError` chained (`__cause__`) from the `TypeError`.
            let n = match crate::builtins::coerce_index_i64(&res) {
                Ok(v) => v,
                Err(te) => {
                    return Err(chain_cause(
                        crate::error::os_error("raw readinto() failed"),
                        te,
                    ));
                }
            };
            // A negative or over-long count (`test_bad_readinto_value`,
            // `test_misbehaved_io_read`) is `OSError` with no `__cause__`.
            if n < 0 || n as usize > dst.len() {
                return Err(crate::error::os_error(format!(
                    "raw readinto() returned invalid length {} (should have been between 0 and {})",
                    n,
                    dst.len()
                )));
            }
            let n = n as usize;
            if let Object::ByteArray(b) = &mv {
                dst[..n].copy_from_slice(&b.borrow()[..n]);
            }
            Ok(Some(n))
        }
    }
}

/// Chain `cause` as the explicit `__cause__` of `primary` (CPython's
/// `_PyErr_FormatFromCause`). Used when a misbehaving raw `readinto` return
/// value cannot be coerced to an integer.
fn chain_cause(primary: RuntimeError, cause: RuntimeError) -> RuntimeError {
    match (primary, cause) {
        (RuntimeError::PyException(mut p), RuntimeError::PyException(c)) => {
            if let Object::Instance(inst) = &p.instance {
                let mut dict = inst.dict.borrow_mut();
                dict.insert(
                    DictKey(Object::from_static("__cause__")),
                    c.instance.clone(),
                );
                // Explicit cause suppresses implicit `__context__` rendering.
                dict.insert(
                    DictKey(Object::from_static("__suppress_context__")),
                    Object::Bool(true),
                );
            }
            p.cause = Some(Box::new(c));
            RuntimeError::PyException(p)
        }
        (p, _) => p,
    }
}

/// One `readinto`-based refill of up to `to_read` bytes, returning the bytes
/// actually read (`None` ⇒ the raw read would block). CPython's C buffered
/// reader *always* refills via `raw.readinto` (never `raw.read`), so misbehaving
/// raw streams are caught here (`test_misbehaved_io_read`, `test_bad_readinto_*`).
fn bw_fill_raw(target: &RawTarget, to_read: usize) -> Result<Option<Vec<u8>>, RuntimeError> {
    if to_read == 0 {
        return Ok(Some(Vec::new()));
    }
    let mut tmp = vec![0u8; to_read];
    match raw_readinto(target, &mut tmp)? {
        None => Ok(None),
        Some(k) => {
            tmp.truncate(k);
            Ok(Some(tmp))
        }
    }
}

/// One write to the underlying raw stream, returning the count written or
/// `None` for "would block".
fn raw_write(target: &RawTarget, data: &[u8]) -> Result<Option<usize>, RuntimeError> {
    match target {
        RawTarget::Native(raw) => Ok(Some(raw.write_bytes(data)?)),
        RawTarget::Py(raw) => {
            let res = py_call(raw, "write", &[Object::new_bytes(data.to_vec())])?;
            if matches!(res, Object::None) {
                return Ok(None);
            }
            // `_bufferedwriter_raw_write`: a non-integer return is `OSError`
            // chained from the `TypeError`; a count outside `[0, len]`
            // (a misbehaving raw doubling its byte count, `test_misbehaved_io`)
            // is a bare `OSError`.
            let n = match crate::builtins::coerce_index_i64(&res) {
                Ok(v) => v,
                Err(te) => {
                    return Err(chain_cause(
                        crate::error::os_error("raw write() failed"),
                        te,
                    ));
                }
            };
            if n < 0 || n as usize > data.len() {
                return Err(crate::error::os_error(format!(
                    "raw write() returned invalid length {} (should have been between 0 and {})",
                    n,
                    data.len()
                )));
            }
            Ok(Some(n as usize))
        }
    }
}

/// For a BOM-prefixing encoding (`utf-16`, `utf-32`, `utf-8-sig` with no
/// explicit byte order), return the **continuation** encoding to use after the
/// first write — the BOM-less variant. CPython's incremental encoders emit the
/// BOM exactly once at the start of the stream, then switch to the native
/// byte-order codec; we reproduce that with a `_start_of_stream` flag plus this
/// mapping. Returns `None` for encodings that never write a BOM (their writes
/// are stateless).
fn bom_continuation(encoding: &str) -> Option<&'static str> {
    match encoding_key_for(encoding).as_str() {
        // WeavePy encodes byte-order-less utf-16/utf-32 as little-endian
        // (matching its x86_64/aarch64 targets), so the continuation is the
        // little-endian codec.
        "utf16" => Some("utf-16-le"),
        "utf32" => Some("utf-32-le"),
        "utf8sig" => Some("utf-8"),
        _ => None,
    }
}

/// Normalise an encoding name to the compact key `bom_continuation` matches on
/// (lower-case, `-`/`_`/spaces removed), mirroring `codecs_mod::encoding_key`.
fn encoding_key_for(encoding: &str) -> String {
    encoding
        .chars()
        .filter(|c| !matches!(c, '-' | '_' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

/// Read the underlying buffer's current byte offset, if it is seekable. Used at
/// construction to decide whether a BOM-prefixing encoder is genuinely at the
/// start of the stream (CPython's `encoding_start_of_stream` init logic): a
/// `TextIOWrapper` opened in append mode, or over a non-empty seekable buffer
/// already positioned past byte 0, must *not* re-emit the BOM.
fn tw_buffer_tell(inst: &crate::types::PyInstance) -> Option<i64> {
    match tw_buffer_target(inst).ok()? {
        RawTarget::Native(file) => file.seek(0, 1).ok().map(|p| p as i64),
        RawTarget::Py(buffer) => {
            let seekable = py_call(&buffer, "seekable", &[])
                .map(|r| r.is_truthy())
                .unwrap_or(false);
            if !seekable {
                return None;
            }
            match py_call(&buffer, "tell", &[]) {
                Ok(Object::Int(n)) => Some(n),
                _ => None,
            }
        }
    }
}

fn tw_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    // CPython clears `self->ok` at the top of `__init__` and only sets it once
    // initialisation fully succeeds. A failed re-init (`test_initialization`:
    // `__init__(..., newline='xyzzy')`) therefore leaves the object
    // *uninitialised*, so a subsequent `read()` raises `ValueError`.
    tw_set(&inst, "_initialized", Object::Bool(false));
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
        Some(Object::Str(s)) => {
            // An embedded NUL can't name a codec (`test_constructor`:
            // `encoding='utf-8\0'` → ValueError).
            if s.contains('\0') {
                return Err(value_error("embedded null character"));
            }
            resolve_locale_encoding(s.as_ref())
        }
        Some(Object::None) | None => "utf-8".to_owned(),
        // The C `TextIOWrapper` raises `TypeError` for a non-str encoding
        // (`test_constructor`: `encoding=42`).
        Some(other) => {
            return Err(type_error(format!(
                "TextIOWrapper() argument 'encoding' must be str or None, not {}",
                other.type_name()
            )))
        }
    };
    // CPython resolves the codec in `TextIOWrapper.__init__`, so an unknown
    // encoding raises `LookupError` at construction (e.g.
    // `SpooledTemporaryFile(mode='w+', encoding='bad-encoding')`). A codec that
    // is not a *text* encoding (`hex`, `base64`) raises `LookupError("… is not a
    // text encoding")` (`test_non_text_encoding_codecs_are_rejected`).
    crate::stdlib::io_full::validate_text_encoding(&encoding)?;
    let errors = positional.get(2).cloned().or_else(|| kw("errors"));
    let errors = match &errors {
        Some(Object::Str(s)) => {
            if s.contains('\0') {
                return Err(value_error("embedded null character"));
            }
            s.to_string()
        }
        Some(Object::None) | None => "strict".to_owned(),
        Some(other) => {
            return Err(type_error(format!(
                "TextIOWrapper() argument 'errors' must be str or None, not {}",
                other.type_name()
            )))
        }
    };
    let newline = positional.get(3).cloned().or_else(|| kw("newline"));
    // Validate `newline=` (CPython rejects anything other than the five legal
    // values at construction). A non-str, non-None `newline` is a `TypeError`
    // (`test_constructor`: `newline=42`).
    match &newline {
        Some(Object::Str(s)) => {
            if !matches!(s.as_ref(), "" | "\n" | "\r" | "\r\n") {
                return Err(value_error(format!("illegal newline value: {}", s.as_ref())));
            }
        }
        Some(Object::None) | None => {}
        Some(other) => {
            return Err(type_error(format!(
                "TextIOWrapper() argument 'newline' must be str or None, not {}",
                other.type_name()
            )))
        }
    }
    let line_buffering = positional
        .get(4)
        .cloned()
        .or_else(|| kw("line_buffering"))
        .map(|v| v.is_truthy())
        .unwrap_or(false);
    let write_through = positional
        .get(5)
        .cloned()
        .or_else(|| kw("write_through"))
        .map(|v| v.is_truthy())
        .unwrap_or(false);
    tw_set(&inst, "buffer", buffer);
    tw_set(&inst, "encoding", Object::from_str(encoding.clone()));
    tw_set(&inst, "errors", Object::from_str(errors));
    tw_set(&inst, "newline", newline.unwrap_or(Object::None));
    // `newlines` starts `None` and is populated as terminators are read.
    tw_set(&inst, "newlines", Object::None);
    tw_set(&inst, "_detached", Object::Bool(false));
    tw_set(&inst, "line_buffering", Object::Bool(line_buffering));
    tw_set(&inst, "write_through", Object::Bool(write_through));
    // CPython's default text-layer read chunk (`_CHUNK_SIZE`); user-settable
    // and read back by `test_io.test_internal_buffer_size`.
    tw_set(&inst, "_CHUNK_SIZE", Object::Int(8192));
    // BOM-once bookkeeping: a fresh stream emits the BOM on the first write,
    // but an encoder constructed over a buffer already positioned past byte 0
    // (append mode, or `r+` after a prior write) must not (CPython's
    // `encoding_start_of_stream`).
    let start_of_stream = if bom_continuation(&encoding).is_some() {
        tw_buffer_tell(&inst).map(|p| p == 0).unwrap_or(true)
    } else {
        true
    };
    tw_set(&inst, "_start_of_stream", Object::Bool(start_of_stream));
    // Fresh stream: no pending text-layer write bytes yet.
    tw_pending_set(&inst, Vec::new());
    // Initialisation fully succeeded — the object is now usable.
    tw_set(&inst, "_initialized", Object::Bool(true));
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
/// Drain all bytes from a Python buffer object. CPython's `TextIOWrapper`
/// fills its decode chunk via `self.buffer.read1(self._CHUNK_SIZE)` when the
/// buffer exposes `read1` (`_has_read1`), only falling back to `read()`
/// otherwise. A mock whose `read`/`read1` *requires* a size argument
/// (`test_io.MemviewBytesIO`) therefore must be driven with `read1(CHUNK_SIZE)`
/// — calling a bare `read()` would raise `TypeError`.
fn tw_read_all_from_py(
    inst: &crate::types::PyInstance,
    buffer: &Object,
) -> Result<Vec<u8>, RuntimeError> {
    let chunk = match tw_get(inst, "_CHUNK_SIZE") {
        Some(Object::Int(n)) if n > 0 => n,
        _ => 8192,
    };
    let has_read1 = crate::vm_singletons::current_interpreter_ptr()
        .map(|ptr| {
            // SAFETY: published by the enclosing VM frame on this thread.
            let interp = unsafe { &mut *ptr };
            interp.load_attr_public(buffer, "read1").is_ok()
        })
        .unwrap_or(false);
    // CPython's `_read_chunk` always passes the chunk size (`read1(CHUNK_SIZE)`
    // or `read(CHUNK_SIZE)`) and loops until EOF — a raw stream (`MockRawIO`)
    // returns one queued block per call, so a single `read(CHUNK_SIZE)` is *not*
    // the whole stream (`test_rawio`). Loop the sized read until it yields no
    // more bytes.
    let method = if has_read1 { "read1" } else { "read" };
    let mut out: Vec<u8> = Vec::new();
    loop {
        let part_obj = py_call(buffer, method, &[Object::Int(chunk)])?;
        // A buffer whose `read` yields a non-bytes object (e.g. wrapping a
        // `StringIO` in a `TextIOWrapper`) is a `TypeError`, not a silent
        // empty read (`test_io.test_read_nonbytes`).
        let part = match &part_obj {
            Object::None => break,
            _ => match part_obj.as_bytes_view() {
                Some(b) => b,
                None => {
                    return Err(type_error(format!(
                        "underlying read() should have returned a bytes-like object, not '{}'",
                        part_obj.type_name()
                    )))
                }
            },
        };
        if part.is_empty() {
            break;
        }
        out.extend_from_slice(&part);
    }
    Ok(out)
}

fn tw_ensure_decoded(inst: &Rc<crate::types::PyInstance>) -> Result<(), RuntimeError> {
    if matches!(tw_get(inst, "_dec_done"), Some(Object::Bool(true))) {
        return Ok(());
    }
    // Flush any pending text-layer writes before reading back through the buffer
    // (CPython's read path runs `_writeflush` first on an r+ stream).
    tw_writeflush(inst)?;
    // Record the underlying byte position the snapshot starts at, so `tell()`
    // can map a character offset back to a byte cookie (`_dec_start +
    // encoded-length-of-consumed-chars`) even though we drain the whole buffer
    // eagerly (`test_io.test_issue1395_5`).
    let (raw, start) = match tw_buffer_target(inst)? {
        RawTarget::Native(file) => {
            // A buffer that yields *text* (a `StringIO`/text stream) instead of
            // bytes is a `TypeError` — `TextIOWrapper` decodes bytes, so
            // `TextIOWrapper(StringIO('a'))` must raise rather than re-encode
            // (`test_io.test_read_nonbytes`).
            if matches!(
                file.io_kind.get(),
                crate::object::IoKind::Text | crate::object::IoKind::StringIO
            ) {
                return Err(type_error(
                    "underlying read() should have returned a bytes-like object, not 'str'",
                ));
            }
            let start = file.tell().map(|p| p as i64).unwrap_or(0);
            (file.read_bytes(None)?, start)
        }
        RawTarget::Py(buffer) => {
            let start = match py_call(&buffer, "tell", &[]) {
                Ok(Object::Int(n)) => n,
                _ => 0,
            };
            (tw_read_all_from_py(inst, &buffer)?, start)
        }
    };
    let encoding = tw_encoding(inst);
    let errors = tw_errors(inst);
    let text = crate::stdlib::codecs_mod::decode_bytes(&raw, &encoding, &errors)?;
    tw_set(inst, "_dec_buf", Object::from_str(text));
    tw_set(inst, "_dec_pos", Object::Int(0));
    tw_set(inst, "_dec_start", Object::Int(start));
    tw_set(inst, "_dec_done", Object::Bool(true));
    Ok(())
}

/// Drop the decoded-snapshot cache so the next read re-materialises from the
/// (newly repositioned) underlying buffer.
fn tw_reset_decoded(inst: &crate::types::PyInstance) {
    let mut d = inst.dict.borrow_mut();
    for k in ["_dec_buf", "_dec_pos", "_dec_done", "_dec_start"] {
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

/// The logical byte position of the text stream — the cookie `tell()` reports.
/// Assumes pending writes have already been flushed. With an active decode
/// snapshot the underlying buffer has been drained to EOF, so the position is
/// reconstructed as `snapshot_start + len(encode(consumed chars))` (exact for
/// stateless, non-BOM codecs); BOM codecs re-emit a BOM on re-encode, so they
/// fall back to the buffer's own offset (`test_io.test_seek_bom`). Without a
/// snapshot it is simply the buffer's current offset.
fn tw_byte_position(inst: &crate::types::PyInstance) -> Result<i64, RuntimeError> {
    if matches!(tw_get(inst, "_dec_done"), Some(Object::Bool(true))) {
        let encoding = tw_encoding(inst);
        if crate::stdlib::codecs_mod::bom_continuation(&encoding).is_none() {
            let (buf, pos) = tw_dec_state(inst);
            let start = match tw_get(inst, "_dec_start") {
                Some(Object::Int(n)) => n.max(0),
                _ => 0,
            };
            let errors = tw_errors(inst);
            let consumed =
                crate::stdlib::codecs_mod::encode_str(&buf[..pos], &encoding, &errors)?.len();
            return Ok(start + consumed as i64);
        }
    }
    match tw_buffer_target(inst)? {
        RawTarget::Native(file) => Ok(file.seek(0, 1)? as i64),
        RawTarget::Py(buffer) => match py_call(&buffer, "tell", &[])? {
            Object::Int(n) => Ok(n),
            _ => Ok(0),
        },
    }
}

/// CPython's `_io_TextIOWrapper_write_impl` discards the decode snapshot and
/// resets the decoder when a write follows a read, so the write lands at the
/// logical position and a later `tell()` reflects the buffer's true offset.
/// WeavePy drains the buffer eagerly during a read (leaving it at EOF), so it
/// must *additionally* reposition the buffer to the logical byte cookie before
/// the write — otherwise an interleaved `read(); write()` (no seek) appends at
/// EOF and `tell()` keeps reporting the stale read cookie
/// (`test_tempfile.test_text_newline_and_encoding`'s rollover check).
fn tw_sync_for_write(inst: &crate::types::PyInstance) -> Result<(), RuntimeError> {
    if !matches!(tw_get(inst, "_dec_done"), Some(Object::Bool(true))) {
        return Ok(());
    }
    let target = tw_byte_position(inst)?;
    tw_reset_decoded(inst);
    match tw_buffer_target(inst)? {
        RawTarget::Native(file) => {
            file.seek(target as isize, 0)?;
        }
        RawTarget::Py(buffer) => {
            py_call(&buffer, "seek", &[Object::Int(target), Object::Int(0)])?;
        }
    }
    Ok(())
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
    tw_require_init(&inst)?;
    tw_check_open(&inst, "write to closed file")?;
    // A write following a read drops the decode snapshot and repositions the
    // buffer to the logical byte offset (CPython resets the decoder here); this
    // keeps the write landing at the right place and `tell()` advancing.
    tw_sync_for_write(&inst)?;
    // Newline translation on write: only `\r`/`\r\n` modes rewrite `\n`.
    // `None` maps `\n`→os.linesep, which is `\n` on WeavePy's Unix targets.
    let has_newline = text.contains('\n') || text.contains('\r');
    let translated = match tw_newline_mode(&inst) {
        NewlineMode::Cr => text.replace('\n', "\r"),
        NewlineMode::CrLf => text.replace('\n', "\r\n"),
        _ => text.clone(),
    };
    let encoding = tw_encoding(&inst);
    let errors = tw_errors(&inst);
    // BOM-once: the first write to a stream positioned at the start emits the
    // BOM (for `utf-16`/`utf-32`/`utf-8-sig`); subsequent writes — and any
    // write to a stream not at byte 0 — use the BOM-less continuation codec.
    let effective_encoding = match bom_continuation(&encoding) {
        Some(cont) => {
            let start = matches!(tw_get(&inst, "_start_of_stream"), Some(Object::Bool(true)));
            if start {
                encoding.clone()
            } else {
                cont.to_owned()
            }
        }
        None => encoding.clone(),
    };
    let bytes = crate::stdlib::codecs_mod::encode_str(&translated, &effective_encoding, &errors)?;
    // Any successful write leaves the stream past its start.
    tw_set(&inst, "_start_of_stream", Object::Bool(false));
    // CPython's C `TextIOWrapper` does not push every encoded write straight to
    // the buffer: it accumulates them in an internal `pending_bytes` buffer and
    // only flushes to `self.buffer` once the pending count reaches
    // `_CHUNK_SIZE`, on `line_buffering`/`write_through`, or an explicit
    // `flush()` (`_io_TextIOWrapper_write_impl`). `test_internal_buffer_size`
    // and `test_issue119506` pin this exactly.
    let line_buffering = matches!(tw_get(&inst, "line_buffering"), Some(Object::Bool(true)));
    let write_through = matches!(tw_get(&inst, "write_through"), Some(Object::Bool(true)));
    // `needflush` (a true buffer-level flush) fires for line buffering once the
    // text contains a recognised terminator.
    let needflush = line_buffering && has_newline;
    let bytes_len = bytes.len();
    let chunk = tw_chunk_size(&inst);
    let pending = tw_pending_get(&inst);
    // CPython caps a single concatenation at `chunk_size`: if appending the new
    // bytes would exceed it, flush the existing pending first and start a fresh
    // pending buffer with the new bytes (`_io_TextIOWrapper_write_impl`).
    let count = if pending.is_empty() {
        tw_pending_set(&inst, bytes);
        bytes_len
    } else if pending.len() + bytes_len > chunk {
        tw_pending_set(&inst, pending);
        tw_writeflush(&inst)?;
        tw_pending_set(&inst, bytes);
        bytes_len
    } else {
        let mut p = pending;
        p.extend_from_slice(&bytes);
        let c = p.len();
        tw_pending_set(&inst, p);
        c
    };
    if count >= chunk || needflush || write_through {
        tw_writeflush(&inst)?;
    }
    if needflush {
        match tw_buffer_target(&inst)? {
            RawTarget::Native(file) => file.flush()?,
            RawTarget::Py(buffer) => {
                py_call(&buffer, "flush", &[])?;
            }
        }
    }
    // TextIOWrapper.write returns the number of characters written (the
    // length of the *original* argument, before newline translation).
    Ok(Object::Int(text.chars().count() as i64))
}

/// The encoded `pending_bytes` not yet pushed to `self.buffer`.
fn tw_pending_get(inst: &crate::types::PyInstance) -> Vec<u8> {
    match tw_get(inst, "_pending") {
        Some(o) => o.as_bytes_view().unwrap_or_default(),
        None => Vec::new(),
    }
}

fn tw_pending_set(inst: &crate::types::PyInstance, v: Vec<u8>) {
    tw_set(inst, "_pending", Object::new_bytes(v));
}

/// `_CHUNK_SIZE` — the text layer's write-flush threshold and read-chunk size.
fn tw_chunk_size(inst: &crate::types::PyInstance) -> usize {
    match tw_get(inst, "_CHUNK_SIZE") {
        Some(Object::Int(n)) if n > 0 => n as usize,
        _ => 8192,
    }
}

/// Push the buffered `pending_bytes` to `self.buffer` in a single `write`
/// (CPython's `_textiowrapper_writeflush`). The pending buffer is cleared
/// *before* the write so a reentrant `write` (a buffer whose `write` calls back
/// into the text layer) starts from an empty pending buffer.
fn tw_writeflush(inst: &crate::types::PyInstance) -> Result<(), RuntimeError> {
    let pending = tw_pending_get(inst);
    if pending.is_empty() {
        return Ok(());
    }
    tw_pending_set(inst, Vec::new());
    match tw_buffer_target(inst)? {
        RawTarget::Native(file) => {
            file.write_bytes(&pending)?;
        }
        RawTarget::Py(buffer) => {
            py_call(&buffer, "write", &[Object::new_bytes(pending)])?;
        }
    }
    Ok(())
}

fn tw_read(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    tw_require_init(&inst)?;
    tw_check_readable(&inst)?;
    // The codec/newline are locked once a read has occurred (`reconfigure`).
    tw_set(&inst, "_read_started", Object::Bool(true));
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
    tw_require_init(&inst)?;
    tw_check_readable(&inst)?;
    tw_set(&inst, "_read_started", Object::Bool(true));
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
    // Push any buffered text-layer `pending_bytes` to the buffer, then flush the
    // buffer itself. Flush errors propagate (CPython parity —
    // `test_flush_error_on_close`).
    tw_writeflush(&inst)?;
    match tw_buffer_target(&inst) {
        Ok(RawTarget::Native(file)) => {
            file.flush()?;
        }
        Ok(RawTarget::Py(buffer)) => {
            py_call(&buffer, "flush", &[])?;
        }
        Err(_) => {}
    }
    Ok(Object::None)
}

#[allow(dead_code)]
fn tw_flush_noop(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

fn tw_close(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    let me = args[0].clone();
    // CPython `TextIOWrapper.close()` (`_io_TextIOWrapper_close_impl`): if the
    // buffer is already closed it's a no-op; otherwise flush the text layer
    // (`self.flush()`, virtual so a monkeypatched `flush` runs), then close the
    // buffer (`self.buffer.close()`, also virtual). If *both* raise, the close
    // error is raised with the flush error chained as its `__context__`; if only
    // the flush raised, the flush error propagates after the buffer is closed
    // (`test_close_error_on_close`/`test_flush_error_on_close`).
    let buffer = match tw_get(&inst, "buffer") {
        Some(b) => b,
        None => return Ok(Object::None),
    };
    if py_get_attr(&buffer, "closed")
        .map(|c| c.is_truthy())
        .unwrap_or(false)
    {
        return Ok(Object::None);
    }
    let flush_err = tw_virtual_flush(&me).err();
    let close_res = py_call(&buffer, "close", &[]);
    match (close_res, flush_err) {
        (Err(close_e), Some(flush_e)) => Err(chain_context(close_e, flush_e)),
        (Err(close_e), None) => Err(close_e),
        (Ok(_), Some(flush_e)) => Err(flush_e),
        (Ok(_), None) => Ok(Object::None),
    }
}

fn tw_seek(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    // Flush pending writes, then repositioning the underlying buffer
    // invalidates the decoded snapshot.
    tw_writeflush(&inst)?;
    tw_reset_decoded(&inst);
    let offset = match args.get(1) {
        Some(Object::Int(n)) => *n,
        _ => 0,
    };
    let whence = match args.get(2) {
        Some(Object::Int(n)) => *n as i32,
        _ => 0,
    };
    let pos = match tw_buffer_target(&inst)? {
        RawTarget::Native(file) => {
            let pos = file.seek(offset as isize, whence)?;
            Object::Int(pos as i64)
        }
        RawTarget::Py(buffer) => py_call(
            &buffer,
            "seek",
            &[Object::Int(offset), Object::Int(i64::from(whence))],
        )?,
    };
    // BOM-once: seeking back to byte 0 puts a BOM-prefixing encoder at the
    // start of the stream again (so the next write re-emits the BOM); any
    // other position suppresses it (CPython's seek `encoding_start_of_stream`).
    let at_start = matches!(&pos, Object::Int(0));
    tw_set(&inst, "_start_of_stream", Object::Bool(at_start));
    Ok(pos)
}

fn tw_tell(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    tw_writeflush(&inst)?;
    // With an active decoded snapshot the underlying buffer has been drained to
    // EOF, so its raw position no longer reflects the logical text position;
    // `tw_byte_position` reconstructs the cookie from the snapshot (exact for
    // stateless, non-BOM codecs) and otherwise reads the buffer's offset.
    Ok(Object::Int(tw_byte_position(&inst)?))
}

fn tw_fileno(args: &[Object]) -> Result<Object, RuntimeError> {
    // `TextIOWrapper.fileno()` forwards to the wrapped buffer (CPython
    // `textiowrapper_fileno`): a real OS file returns its descriptor; an
    // in-memory buffer raises `io.UnsupportedOperation` (OSError+ValueError).
    let inst = tw_self(args)?;
    match tw_buffer_target(&inst)? {
        RawTarget::Native(file) => match file.fileno() {
            Some(fd) => Ok(Object::Int(fd)),
            None => Err(unsupported_op("fileno")),
        },
        RawTarget::Py(buffer) => py_call(&buffer, "fileno", &[]),
    }
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
    // CPython flushes the text layer before handing the buffer back.
    tw_flush(args)?;
    let buffer = tw_get(&inst, "buffer")
        .ok_or_else(|| value_error("underlying buffer has been detached"))?;
    tw_set(&inst, "_detached", Object::Bool(true));
    inst.dict
        .borrow_mut()
        .shift_remove(&DictKey(Object::from_static("buffer")));
    Ok(buffer)
}

/// `TextIOWrapper.truncate([pos])` — flush the text layer, then truncate the
/// underlying buffer (CPython delegates to `buffer.truncate`, defaulting to the
/// current position).
fn tw_truncate(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    tw_flush(args)?;
    let pos = args.get(1).cloned();
    match tw_buffer_target(&inst)? {
        RawTarget::Native(file) => {
            let cur = file.seek(0, 1)? as i64;
            let at = match &pos {
                Some(Object::Int(n)) => *n,
                _ => cur,
            };
            file.flush()?;
            Ok(Object::Int(file.truncate(Some(at as u64))? as i64))
        }
        RawTarget::Py(buffer) => match pos {
            Some(p) if !matches!(p, Object::None) => py_call(&buffer, "truncate", &[p]),
            _ => py_call(&buffer, "truncate", &[]),
        },
    }
}

thread_local! {
    /// `Py_ReprEnter`-style reentrancy guard for `tw_repr` (separate id set from
    /// `bw_repr` is unnecessary — ids are object pointers, never colliding).
    static TW_REPR_ACTIVE: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// `<_io.TextIOWrapper name=… mode=… encoding='utf-8'>` — mirrors CPython's
/// `textiowrapper_repr`: the optional `name`/`mode` are probed via `getattr`
/// (delegating to the buffer) and silently skipped if absent; a self-
/// referential `name` re-enters and raises `RuntimeError`
/// (`test_recursive_repr`).
fn tw_repr(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    tw_require_init(&inst)?;
    let typename = format!("_io.{}", inst.cls().name);
    let id = match &args[0] {
        Object::Instance(i) => Rc::as_ptr(i) as *const () as usize,
        _ => 0,
    };
    if TW_REPR_ACTIVE.with(|s| s.borrow().contains(&id)) {
        return Err(crate::error::runtime_error(format!(
            "reentrant call inside {typename}.__repr__"
        )));
    }
    TW_REPR_ACTIVE.with(|s| s.borrow_mut().push(id));
    let render = || -> Result<String, RuntimeError> {
        let mut out = format!("<{typename}");
        // `name` delegates to the buffer (CPython's `name` getset); an
        // AttributeError (no such attribute) is swallowed, but a nested repr's
        // RuntimeError propagates. A bare descriptor object leaking back from
        // an in-memory buffer's attribute lookup is treated as "absent".
        if let Ok(name) = py_get_attr(&args[0], "name") {
            if !is_descriptor_object(&name) {
                out.push_str(&format!(" name={}", py_repr(&name)?));
            }
        }
        // `mode` is a plain (user-settable) instance attribute in CPython, not a
        // delegating property — `t.mode = 'r'` stores it and `repr` reflects it.
        if let Some(mode) = tw_get(&inst, "mode") {
            out.push_str(&format!(" mode={}", py_repr(&mode)?));
        }
        let encoding = tw_encoding(&inst);
        out.push_str(&format!(
            " encoding={}>",
            py_repr(&Object::from_str(encoding))?
        ));
        Ok(out)
    };
    let result = render();
    TW_REPR_ACTIVE.with(|s| s.borrow_mut().retain(|&x| x != id));
    Ok(Object::from_str(result?))
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
    let me = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("__enter__ requires a stream instance"))?;
    // CPython's `IOBase.__enter__` calls `self._checkClosed()`, so re-entering a
    // closed stream as a context manager raises `ValueError`
    // (`test_context_manager`). Shared by `Buffered*` and `TextIOWrapper`, both
    // of which expose a delegating `closed` property.
    if py_get_attr(&me, "closed")
        .map(|c| c.is_truthy())
        .unwrap_or(false)
    {
        return Err(value_error("I/O operation on closed file."));
    }
    Ok(me)
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
    tw_reconfigure_kw(args, &[])
}

/// `TextIOWrapper.reconfigure(*, encoding, errors, newline, line_buffering,
/// write_through)` — re-apply the text parameters, mirroring CPython's
/// `_io_TextIOWrapper_reconfigure_impl`:
///   * the codec/newline cannot be changed once the stream has been read from
///     (`UnsupportedOperation`);
///   * `encoding=None` keeps the current encoding (and keeps `errors` too unless
///     `errors` is given); a *new* encoding with `errors=None` resets errors to
///     `"strict"`;
///   * `line_buffering`/`write_through` accept any `__index__`-able value and
///     `None` keeps the current setting;
///   * an implicit `flush()` runs before the new settings take effect.
fn tw_reconfigure_kw(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let inst = tw_self(args)?;
    let kwget = |name: &str| {
        kwargs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let encoding = kwget("encoding");
    let errors = kwget("errors");
    let newline = kwget("newline");
    let line_buffering = kwget("line_buffering");
    let write_through = kwget("write_through");

    // --- validate types up front (CPython validates before applying) ---
    let new_encoding: Option<String> = match &encoding {
        None | Some(Object::None) => None,
        Some(Object::Str(s)) => Some(resolve_locale_encoding(s.as_ref())),
        Some(other) => {
            return Err(type_error(format!(
                "reconfigure() argument 'encoding' must be str or None, not {}",
                other.type_name()
            )))
        }
    };
    let new_errors: Option<String> = match &errors {
        None | Some(Object::None) => None,
        Some(Object::Str(s)) => Some(s.to_string()),
        Some(other) => {
            return Err(type_error(format!(
                "reconfigure() argument 'errors' must be str or None, not {}",
                other.type_name()
            )))
        }
    };
    // `newline`: `None`(not given) vs `Some(None)`(explicit None) vs `Some(Some(s))`.
    let new_newline: Option<Option<String>> = match &newline {
        None => None,
        Some(Object::None) => Some(None),
        Some(Object::Str(s)) => {
            if !matches!(s.as_ref(), "" | "\n" | "\r" | "\r\n") {
                return Err(value_error(format!("illegal newline value: {}", s.as_ref())));
            }
            Some(Some(s.to_string()))
        }
        Some(other) => {
            return Err(type_error(format!(
                "reconfigure() argument 'newline' must be str or None, not {}",
                other.type_name()
            )))
        }
    };
    // `line_buffering`/`write_through`: `None` keeps current; anything else is
    // coerced through `__index__` (so a bad `__index__` surfaces its own error,
    // and an out-of-range int raises `OverflowError`) and tested for truth.
    let new_lb: Option<bool> = match &line_buffering {
        None | Some(Object::None) => None,
        Some(v) => Some(crate::builtins::coerce_index_i64(v)? != 0),
    };
    let new_wt: Option<bool> = match &write_through {
        None | Some(Object::None) => None,
        Some(v) => Some(crate::builtins::coerce_index_i64(v)? != 0),
    };

    // Changing the codec or newline after the first read is forbidden.
    let read_started = matches!(tw_get(&inst, "_read_started"), Some(Object::Bool(true)));
    let codec_change = new_encoding.is_some() || new_newline.is_some();
    if read_started && codec_change {
        return Err(unsupported_op(
            "It is not possible to set the encoding or newline of stream after the first read",
        ));
    }

    // Implicit flush before applying (CPython flushes the text+buffer layers).
    tw_flush(args)?;

    // --- apply ---
    if let Some(enc) = new_encoding {
        crate::stdlib::io_full::validate_text_encoding(&enc)?;
        tw_set(&inst, "encoding", Object::from_str(enc.clone()));
        // A new codec restarts BOM bookkeeping (CPython rebuilds the encoder).
        // CPython's `_textiowrapper_fix_encoder_state` only *suppresses* the
        // restart-of-stream (and hence the BOM) when the stream is seekable and
        // positioned past byte 0; a non-seekable stream keeps the encoder at the
        // start, so a `utf-8-sig`/`utf-16` reconfigure re-emits the BOM
        // (`test_io.test_reconfigure_write_non_seekable`). Resolve `seekable`
        // through the buffer object so a monkeypatched `seekable`/`seek` (the
        // test disables both) is honoured.
        let seekable = tw_get(&inst, "buffer")
            .map(|b| {
                py_call(&b, "seekable", &[])
                    .map(|r| r.is_truthy())
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        let at_start = if seekable {
            tw_buffer_tell(&inst).map(|p| p == 0).unwrap_or(true)
        } else {
            true
        };
        tw_set(&inst, "_start_of_stream", Object::Bool(at_start));
        // `errors` defaults to "strict" when a new encoding is set without one.
        tw_set(
            &inst,
            "errors",
            Object::from_str(new_errors.clone().unwrap_or_else(|| "strict".to_owned())),
        );
    } else if let Some(err) = &new_errors {
        // Encoding unchanged: only update `errors` if explicitly given.
        tw_set(&inst, "errors", Object::from_str(err.clone()));
    }
    if let Some(nl) = new_newline {
        tw_set(
            &inst,
            "newline",
            nl.map_or(Object::None, Object::from_str),
        );
    }
    if let Some(lb) = new_lb {
        tw_set(&inst, "line_buffering", Object::Bool(lb));
    }
    if let Some(wt) = new_wt {
        tw_set(&inst, "write_through", Object::Bool(wt));
    }
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
    method("readinto1", bw_readinto1);
    method("readline", bw_readline);
    method("readlines", iobase_readlines);
    method("writelines", iobase_writelines);
    // `peek` is a reader-side method only: CPython's `BufferedWriter` genuinely
    // lacks it (`test_io.test_io_after_close` probes `hasattr(f, "peek")` on a
    // write-only buffered stream and must see `False`).
    if bw_is_reader(name) {
        method("peek", bw_peek);
    }
    method("flush", bw_flush);
    method("close", bw_close);
    method("seek", bw_seek);
    method("tell", bw_tell);
    method("truncate", bw_truncate);
    method("readable", bw_readable);
    method("writable", bw_writable);
    method("seekable", bw_seekable);
    method("isatty", bw_isatty);
    method("fileno", bw_fileno);
    method("detach", bw_detach);
    method("__iter__", tw_iter);
    method("__next__", bw_next);
    method("__enter__", tw_enter);
    method("__exit__", bw_exit);
    method("__repr__", bw_repr);
    // `__new__` only allocates (CPython `buffered_new`); `__init__` binds the
    // raw stream. Keeping them separate means `tp.__new__(tp)` yields an
    // uninitialized object whose methods raise `ValueError` (`test_uninitialized`)
    // rather than eagerly running `__init__`.
    dict.insert(
        DictKey(Object::from_static("__new__")),
        Object::StaticMethod(crate::object::MethodWrapper::new(Object::Builtin(Rc::new(
            BuiltinFn {
                name: "Buffered.__new__",
                binds_instance: false,
                call: Box::new(bw_new),
                call_kw: Some(Box::new(|args, _kw| bw_new(args))),
            },
        )))),
    );
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
    // `Buffered*.raw` is a *read-only* member descriptor in CPython
    // (`buffered_raw`), so `buf.raw = x` raises `AttributeError`
    // (`test_readonly_attributes`). `BufferedRWPair` has no `.raw` (it wraps a
    // reader+writer), so skip it there.
    if name != "BufferedRWPair" {
        install_delegating_attr(&ty, "raw", "The underlying raw stream", bw_raw_get);
    }
    ty
}

/// `Buffered*.raw` getter — returns the stored raw stream (or `None` once
/// detached). Installed as a read-only property so assignment raises
/// `AttributeError`, matching CPython's `buffered_raw` member descriptor.
fn bw_raw_get(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    Ok(tw_get(&inst, "raw").unwrap_or(Object::None))
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

/// The buffered wrapper's concrete type name, used to gate read/write semantics
/// (`BufferedReader` has no `write`, `BufferedWriter` no `read`, etc.). A user
/// *subclass* (e.g. `tarfile.ExFileObject(io.BufferedReader)`) carries its own
/// class name, so the reader/writer flavour is resolved by walking the MRO for
/// the built-in buffered base — otherwise `readable()`/`read()` on a subclass
/// instance would wrongly report write-only (`test_tarfile` extractfile path).
fn bw_typename(inst: &crate::types::PyInstance) -> String {
    let cls = inst.cls();
    if is_buffered_flavour(&cls.name) {
        return cls.name.clone();
    }
    for base in cls.mro.borrow().iter() {
        if is_buffered_flavour(&base.name) {
            return base.name.clone();
        }
    }
    cls.name.clone()
}

/// Whether `name` is one of the four built-in buffered type names.
fn is_buffered_flavour(name: &str) -> bool {
    matches!(
        name,
        "BufferedReader" | "BufferedWriter" | "BufferedRandom" | "BufferedRWPair"
    )
}

fn bw_is_reader(name: &str) -> bool {
    matches!(name, "BufferedReader" | "BufferedRandom" | "BufferedRWPair")
}

fn bw_is_writer(name: &str) -> bool {
    matches!(name, "BufferedWriter" | "BufferedRandom" | "BufferedRWPair")
}

/// `CHECK_INITIALIZED`: a buffered object created via `__new__` without a
/// successful `__init__` raises `ValueError("I/O operation on uninitialized
/// object.")` on every method (`test_uninitialized`/`test_initialization`).
fn bw_check_init(inst: &crate::types::PyInstance) -> Result<(), RuntimeError> {
    if tw_get(inst, "_ok").map(|o| o.is_truthy()).unwrap_or(false) {
        Ok(())
    } else {
        Err(value_error("I/O operation on uninitialized object."))
    }
}

/// Is the wrapped raw stream (reader side) closed?
fn bw_raw_closed(target: &RawTarget) -> bool {
    match target {
        RawTarget::Native(raw) => *raw.closed.borrow(),
        RawTarget::Py(raw) => py_get_attr(raw, "closed")
            .map(|c| c.is_truthy())
            .unwrap_or(false),
    }
}

/// CHECK_INITIALIZED + resolve `self.raw`, distinguishing an uninitialized
/// object (`__new__` only) from a detached one (`detach()` removed the raw).
fn bw_target(inst: &crate::types::PyInstance) -> Result<RawTarget, RuntimeError> {
    bw_check_init(inst)?;
    let raw = tw_get(inst, "raw").ok_or_else(|| value_error("raw stream has been detached"))?;
    bw_raw_target(&raw)
}

/// CHECK_INITIALIZED + CHECK_CLOSED: the common guard for I/O methods. Mirrors
/// CPython's `Buffered*` which raise `ValueError("I/O operation on closed
/// file.")` once the wrapper (or its raw) is closed.
fn bw_check_open(inst: &crate::types::PyInstance) -> Result<(), RuntimeError> {
    let target = bw_target(inst)?;
    if bw_raw_closed(&target) {
        return Err(value_error("I/O operation on closed file."));
    }
    Ok(())
}

const BW_CHUNK: usize = 8192;

/// Call `obj.<name>()` and return its truthiness; absent method → treat as
/// `false` for the capability probes during construction.
fn bw_raw_capable(raw: &Object, name: &str) -> bool {
    match raw_target(raw) {
        Ok(RawTarget::Native(_)) => true,
        Ok(RawTarget::Py(o)) => py_call(&o, name, &[]).map(|r| r.is_truthy()).unwrap_or(false),
        Err(_) => false,
    }
}

/// `Buffered*.__new__(cls, ...)` — allocate an *uninitialized* shell, mirroring
/// CPython's `buffered_new`/`bufferedrwpair_new`, which only allocate; the raw
/// stream and read/write buffers are bound later by `__init__`. Until then every
/// method raises `ValueError("I/O operation on uninitialized object.")`
/// (`test_uninitialized`). Extra positional/keyword arguments are ignored here
/// (they belong to `__init__`).
fn bw_new(args: &[Object]) -> Result<Object, RuntimeError> {
    let cls = match args.first() {
        Some(Object::Type(t)) => t.clone(),
        _ => return Err(type_error("Buffered.__new__(X): X is not a type object")),
    };
    let inst = Object::Instance(Rc::new(crate::types::PyInstance::new(cls)));
    crate::gc_trace::track(inst.clone());
    Ok(inst)
}

fn bw_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let clsname = bw_typename(&inst);
    let is_rwpair = clsname == "BufferedRWPair";
    // A failed (re)initialization leaves the object marked uninitialized, so a
    // subsequent `read`/`write` raises `ValueError` (`test_initialization`).
    tw_set(&inst, "_ok", Object::Bool(false));
    let positional = &args[1..];
    let kw = |name: &str| {
        kwargs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let raw = positional
        .first()
        .cloned()
        .or_else(|| kw("raw").or_else(|| kw("reader")))
        .ok_or_else(|| type_error("a raw stream is required"))?;

    // Resolve `buffer_size` and (for RWPair) the writer side.
    let (writer, bufsize) = if is_rwpair {
        let writer = positional
            .get(1)
            .cloned()
            .or_else(|| kw("writer"))
            .ok_or_else(|| type_error("a writer stream is required"))?;
        if positional.len() > 3 {
            return Err(type_error("BufferedRWPair() takes at most 3 arguments"));
        }
        (Some(writer), positional.get(2).cloned().or_else(|| kw("buffer_size")))
    } else {
        if positional.len() > 2 {
            return Err(type_error(format!(
                "{clsname}() takes at most 2 arguments"
            )));
        }
        (None, positional.get(1).cloned().or_else(|| kw("buffer_size")))
    };
    if let Some(ref bs) = bufsize {
        let n = crate::builtins::coerce_index_i64(bs)?;
        if n <= 0 {
            return Err(value_error("buffer size must be strictly positive"));
        }
        tw_set(&inst, "_bufsize", Object::Int(n));
    } else {
        tw_set(&inst, "_bufsize", Object::Int(BW_CHUNK as i64));
    }

    // Capability validation, matching CPython's `__init__` (`OSError`).
    if is_rwpair {
        if !bw_raw_capable(&raw, "readable") {
            return Err(crate::error::os_error(
                "\"reader\" argument must be readable.",
            ));
        }
        let w = writer.as_ref().unwrap();
        if !bw_raw_capable(w, "writable") {
            return Err(crate::error::os_error(
                "\"writer\" argument must be writable.",
            ));
        }
        tw_set(&inst, "_writer", writer.unwrap());
    } else {
        if bw_is_reader(&clsname) && !bw_raw_capable(&raw, "readable") {
            return Err(crate::error::os_error(
                "\"raw\" argument must be readable.",
            ));
        }
        if bw_is_writer(&clsname) && !bw_raw_capable(&raw, "writable") {
            return Err(crate::error::os_error(
                "\"raw\" argument must be writable.",
            ));
        }
    }
    tw_set(&inst, "raw", raw);
    rdbuf_set(&inst, Vec::new());
    wrbuf_set(&inst, Vec::new());
    tw_set(&inst, "_ok", Object::Bool(true));
    Ok(Object::None)
}

fn bw_bytes_arg(arg: Option<&Object>) -> Result<Vec<u8>, RuntimeError> {
    match arg {
        // `as_bytes_view` covers bytes/bytearray/memoryview fast; fall back to
        // the PEP 688 buffer protocol (`array.array`, `mmap`, …) so a buffered
        // stream accepts any bytes-like object, like CPython's `BufferedWriter`.
        Some(o) => o
            .as_bytes_view()
            .map_or_else(|| crate::builtins::bytes_argview(o), Ok),
        None => Err(type_error("write() takes exactly one argument")),
    }
}

/// The write side of the wrapper: the `_writer` stream for `BufferedRWPair`,
/// otherwise `self.raw`.
fn bw_writer_target(inst: &crate::types::PyInstance) -> Result<RawTarget, RuntimeError> {
    bw_check_init(inst)?;
    if bw_typename(inst) == "BufferedRWPair" {
        let w =
            tw_get(inst, "_writer").ok_or_else(|| value_error("raw stream has been detached"))?;
        bw_raw_target(&w)
    } else {
        bw_target(inst)
    }
}

/// Flush the pending write buffer to the raw stream, looping over partial
/// writes (`_pyio.BufferedWriter._flush_unlocked`).
fn bw_flush_unlocked(
    inst: &crate::types::PyInstance,
    target: &RawTarget,
) -> Result<(), RuntimeError> {
    let mut wbuf = wrbuf_get(inst);
    while !wbuf.is_empty() {
        match raw_write(target, &wbuf)? {
            None => {
                // Raw signalled "would block" (a non-blocking `RawIOBase.write`
                // returned `None`). Persist the remainder and raise the
                // canonical `BlockingIOError(EAGAIN, …, 0)`; `bw_write` may
                // catch it and re-raise with the real `characters_written`
                // (`_pyio.BufferedWriter._flush_unlocked`).
                wrbuf_set(inst, wbuf);
                return Err(crate::error::blocking_io_error_written(
                    eagain(),
                    "write could not complete without blocking",
                    0,
                ));
            }
            Some(n) if n > wbuf.len() => {
                wrbuf_set(inst, wbuf);
                return Err(crate::error::os_error(
                    "write() returned incorrect number of bytes",
                ));
            }
            Some(n) => {
                wbuf.drain(..n);
            }
        }
    }
    wrbuf_set(inst, Vec::new());
    Ok(())
}

/// `errno.EAGAIN` for the running platform (CPython hard-codes `EAGAIN` in the
/// buffered-writer would-block path).
fn eagain() -> i32 {
    #[cfg(unix)]
    {
        libc::EAGAIN
    }
    #[cfg(not(unix))]
    {
        11
    }
}

/// True when `err` is a `BlockingIOError` instance (so `bw_write` can mirror
/// `_pyio.BufferedWriter.write`'s `except BlockingIOError` accounting).
fn is_blocking_io_error(err: &RuntimeError) -> bool {
    if let RuntimeError::PyException(pe) = err {
        if let Object::Instance(inst) = &pe.instance {
            return inst
                .cls()
                .is_subclass_of(&crate::builtin_types::builtin_types().blocking_io_error);
        }
    }
    false
}

/// Read `e.errno` / `e.strerror` off a `BlockingIOError` so the re-raised
/// exception preserves them (`raise BlockingIOError(e.errno, e.strerror, …)`).
fn blocking_errno_strerror(err: &RuntimeError) -> (i32, String) {
    if let RuntimeError::PyException(pe) = err {
        if let Object::Instance(inst) = &pe.instance {
            let dict = inst.dict.borrow();
            let errno = dict
                .get(&DictKey(Object::from_static("errno")))
                .and_then(Object::as_i64)
                .unwrap_or_else(|| i64::from(eagain())) as i32;
            let strerror = match dict.get(&DictKey(Object::from_static("strerror"))) {
                Some(Object::Str(s)) => s.to_string(),
                _ => "write could not complete without blocking".to_owned(),
            };
            return (errno, strerror);
        }
    }
    (eagain(), "write could not complete without blocking".to_owned())
}

/// `BufferedRandom.write` first undoes any read-ahead so the raw position
/// reflects the logical cursor before switching to write mode.
fn bw_random_undo_readahead(inst: &crate::types::PyInstance) -> Result<(), RuntimeError> {
    let buffered = rdbuf_get(inst);
    if buffered.is_empty() {
        return Ok(());
    }
    if let Ok(target) = bw_target(inst) {
        match &target {
            RawTarget::Native(raw) => {
                raw.seek(-(buffered.len() as isize), 1)?;
            }
            RawTarget::Py(raw) => {
                py_call(raw, "seek", &[Object::Int(-(buffered.len() as i64)), Object::Int(1)])?;
            }
        }
    }
    rdbuf_set(inst, Vec::new());
    Ok(())
}

fn bw_write(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let clsname = bw_typename(&inst);
    bw_check_init(&inst)?;
    if !bw_is_writer(&clsname) {
        return Err(unsupported_op("write"));
    }
    if matches!(args.get(1), Some(Object::Str(_))) {
        return Err(type_error("can't write str to binary stream"));
    }
    let data = bw_bytes_arg(args.get(1))?;
    if clsname == "BufferedRandom" {
        bw_random_undo_readahead(&inst)?;
    }
    let target = bw_writer_target(&inst)?;
    if bw_raw_closed(&target) {
        return Err(value_error("write to closed file"));
    }
    let bufsize = bw_buffer_size(&inst);
    // `_pyio.BufferedWriter.write`: a pre-flush when already over the buffer
    // size may raise `BlockingIOError` with `characters_written == 0` — that
    // propagates unchanged (nothing of `b` has been accepted yet).
    if wrbuf_get(&inst).len() > bufsize {
        bw_flush_unlocked(&inst, &target)?;
    }
    let before = wrbuf_get(&inst).len();
    let mut wbuf = wrbuf_get(&inst);
    wbuf.extend_from_slice(&data);
    wrbuf_set(&inst, wbuf);
    let mut written = (wrbuf_get(&inst).len() - before) as i64;
    if wrbuf_get(&inst).len() > bufsize {
        if let Err(e) = bw_flush_unlocked(&inst, &target) {
            if is_blocking_io_error(&e) {
                // Partial non-blocking write: accept what fits in the buffer,
                // discard the overage, and report `characters_written`
                // (`_pyio.BufferedWriter.write`'s `except BlockingIOError`).
                let cur = wrbuf_get(&inst);
                if cur.len() > bufsize {
                    let overage = (cur.len() - bufsize) as i64;
                    written -= overage;
                    wrbuf_set(&inst, cur[..bufsize].to_vec());
                    let (errno, strerror) = blocking_errno_strerror(&e);
                    return Err(crate::error::blocking_io_error_written(
                        errno, &strerror, written,
                    ));
                }
            } else {
                return Err(e);
            }
        }
    }
    Ok(Object::Int(written))
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

/// `_pyio.BufferedReader._peek_unlocked`: do at most one raw read to satisfy a
/// minimal want, never returning more than `buffer_size`. Leaves data in the
/// read buffer (`rdbuf`).
fn bw_peek_unlocked(
    inst: &crate::types::PyInstance,
    target: &RawTarget,
    n: usize,
) -> Result<Vec<u8>, RuntimeError> {
    let bufsize = bw_buffer_size(inst);
    let mut buf = rdbuf_get(inst);
    let want = n.min(bufsize);
    let have = buf.len();
    if have < want || have == 0 {
        let to_read = bufsize.saturating_sub(have);
        if to_read > 0 {
            if let Some(current) = bw_fill_raw(target, to_read)? {
                if !current.is_empty() {
                    buf.extend_from_slice(&current);
                    rdbuf_set(inst, buf.clone());
                }
            }
        }
    }
    Ok(buf)
}

/// `_pyio.BufferedReader._read_unlocked`: return up to `n` bytes (or all, when
/// `n` is `None`), filling from the read buffer then the raw stream. Returns
/// `None` only when the very first raw read would block and nothing is buffered.
fn bw_read_unlocked(
    inst: &crate::types::PyInstance,
    target: &RawTarget,
    n: Option<usize>,
) -> Result<Option<Vec<u8>>, RuntimeError> {
    let bufsize = bw_buffer_size(inst);
    let buf = rdbuf_get(inst);

    // Read-all path.
    let Some(n) = n else {
        rdbuf_set(inst, Vec::new());
        let mut chunks = buf;
        let mut last_none = false;
        loop {
            match raw_read(target, None)? {
                None => {
                    last_none = true;
                    break;
                }
                Some(c) if c.is_empty() => break,
                Some(c) => chunks.extend_from_slice(&c),
            }
        }
        if chunks.is_empty() && last_none {
            return Ok(None);
        }
        return Ok(Some(chunks));
    };

    let avail = buf.len();
    if n <= avail {
        let out = buf[..n].to_vec();
        rdbuf_set(inst, buf[n..].to_vec());
        return Ok(Some(out));
    }
    rdbuf_set(inst, Vec::new());
    let mut chunks = buf;
    let mut got = chunks.len();
    let wanted = bufsize.max(n);
    let mut last_none = false;
    while got < n {
        match bw_fill_raw(target, wanted)? {
            None => {
                last_none = true;
                break;
            }
            Some(c) if c.is_empty() => break,
            Some(c) => {
                got += c.len();
                chunks.extend_from_slice(&c);
            }
        }
    }
    let take = n.min(chunks.len());
    let out = chunks[..take].to_vec();
    rdbuf_set(inst, chunks[take..].to_vec());
    if chunks.is_empty() {
        return Ok(if last_none { None } else { Some(Vec::new()) });
    }
    Ok(Some(out))
}

/// `BufferedRandom`/`BufferedReader.read`. `BufferedRandom` flushes the write
/// buffer first (mode switch from write→read).
fn bw_read(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let clsname = bw_typename(&inst);
    bw_check_init(&inst)?;
    if !bw_is_reader(&clsname) {
        return Err(unsupported_op("read"));
    }
    let size_opt = bw_size_arg(args.get(1))?;
    if let Some(n) = size_opt {
        if n < -1 {
            return Err(value_error("invalid number of bytes to read"));
        }
    }
    bw_check_open(&inst)?;
    if clsname == "BufferedRandom" {
        let wt = bw_writer_target(&inst)?;
        bw_flush_unlocked(&inst, &wt)?;
    }
    let target = bw_target(&inst)?;
    let n = size_opt.filter(|n| *n >= 0).map(|n| n as usize);
    match bw_read_unlocked(&inst, &target, n)? {
        Some(v) => Ok(Object::new_bytes(v)),
        None => Ok(Object::None),
    }
}

/// `read1(size=-1)` — at most one underlying read.
fn bw_read1(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let clsname = bw_typename(&inst);
    bw_check_init(&inst)?;
    if !bw_is_reader(&clsname) {
        return Err(unsupported_op("read1"));
    }
    bw_check_open(&inst)?;
    let mut size = match bw_size_arg(args.get(1))? {
        Some(n) if n >= 0 => n as usize,
        _ => bw_buffer_size(&inst),
    };
    if size == 0 {
        return Ok(Object::new_bytes(Vec::new()));
    }
    if clsname == "BufferedRandom" {
        let wt = bw_writer_target(&inst)?;
        bw_flush_unlocked(&inst, &wt)?;
    }
    let target = bw_target(&inst)?;
    bw_peek_unlocked(&inst, &target, 1)?;
    let buffered = rdbuf_get(&inst).len();
    size = size.min(buffered);
    match bw_read_unlocked(&inst, &target, Some(size))? {
        Some(v) => Ok(Object::new_bytes(v)),
        None => Ok(Object::new_bytes(Vec::new())),
    }
}

fn bw_peek(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let clsname = bw_typename(&inst);
    bw_check_init(&inst)?;
    if !bw_is_reader(&clsname) {
        return Err(unsupported_op("peek"));
    }
    bw_check_open(&inst)?;
    let n = match bw_size_arg(args.get(1))? {
        Some(n) if n >= 0 => n as usize,
        _ => 0,
    };
    if clsname == "BufferedRandom" {
        let wt = bw_writer_target(&inst)?;
        bw_flush_unlocked(&inst, &wt)?;
    }
    let target = bw_target(&inst)?;
    let buf = bw_peek_unlocked(&inst, &target, n)?;
    Ok(Object::new_bytes(buf))
}

/// Resolve a writable destination buffer for `readinto`, returning the backing
/// bytearray cell, its window start, and capacity.
fn bw_writable_dst(
    arg: Option<&Object>,
) -> Result<(Rc<RefCell<Vec<u8>>>, usize, usize), RuntimeError> {
    match arg {
        Some(Object::ByteArray(dst)) => {
            let cap = dst.borrow().len();
            Ok((dst.clone(), 0, cap))
        }
        Some(Object::MemoryView(mv)) => memoryview_writable_buffer(mv),
        Some(other) => instance_writable_buffer(other).unwrap_or_else(|| {
            Err(type_error(
                "readinto() argument must be a writable bytes-like object",
            ))
        }),
        None => Err(type_error(
            "readinto() argument must be a writable bytes-like object",
        )),
    }
}

/// Extract the backing writable `bytearray` storage from a memoryview, erroring
/// if it is released, read-only, non-contiguous, or not bytearray-backed.
fn memoryview_writable_buffer(
    mv: &crate::object::PyMemoryView,
) -> Result<(Rc<RefCell<Vec<u8>>>, usize, usize), RuntimeError> {
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
        _ => Err(type_error(
            "readinto() argument must be a writable bytes-like object",
        )),
    }
}

/// PEP 688: acquire a *writable* buffer from an object exposing `__buffer__`
/// (e.g. `array.array`). Reenters the VM to call `__buffer__(PyBUF_WRITABLE)`
/// and extracts the backing `bytearray` so `readinto` writes through to it.
/// Returns `None` only when the object has no `__buffer__` at all (so the
/// caller can raise its own "not a writable bytes-like object" message).
fn instance_writable_buffer(
    arg: &Object,
) -> Option<Result<(Rc<RefCell<Vec<u8>>>, usize, usize), RuntimeError>> {
    let method = crate::instance_method(arg, "__buffer__")?;
    let ptr = crate::vm_singletons::current_interpreter_ptr()?;
    // SAFETY: published by an enclosing VM frame still live on this thread; the
    // GIL keeps the access exclusive.
    let interp = unsafe { &mut *ptr };
    let globals = interp.builtins_dict();
    // PyBUF_WRITABLE = 0x1.
    let r = match interp.call_object_with_globals(&method, &[Object::Int(1)], &[], &globals) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    match r {
        Object::MemoryView(mv) => Some(memoryview_writable_buffer(&mv)),
        Object::ByteArray(b) => {
            let cap = b.borrow().len();
            Some(Ok((b, 0, cap)))
        }
        _ => Some(Err(type_error(
            "readinto() argument must be a writable bytes-like object",
        ))),
    }
}

fn bw_readinto_impl(args: &[Object], read1: bool) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let clsname = bw_typename(&inst);
    bw_check_init(&inst)?;
    if !bw_is_reader(&clsname) {
        return Err(unsupported_op("readinto"));
    }
    bw_check_open(&inst)?;
    let arg = args.get(1).cloned();
    let (dst, start, cap) = bw_writable_dst(arg.as_ref())?;
    if cap == 0 {
        return Ok(Object::Int(0));
    }
    if clsname == "BufferedRandom" {
        let wt = bw_writer_target(&inst)?;
        bw_flush_unlocked(&inst, &wt)?;
    }
    let target = bw_target(&inst)?;
    let mut written = 0usize;
    while written < cap {
        // Drain the read buffer first.
        let buf = rdbuf_get(&inst);
        let avail = buf.len().min(cap - written);
        if avail > 0 {
            dst.borrow_mut()[start + written..start + written + avail]
                .copy_from_slice(&buf[..avail]);
            rdbuf_set(&inst, buf[avail..].to_vec());
            written += avail;
            if written == cap {
                break;
            }
        }
        let bufsize = bw_buffer_size(&inst);
        if cap - written > bufsize {
            // Read directly into the caller's buffer.
            let mut tmp = vec![0u8; cap - written];
            match raw_readinto(&target, &mut tmp)? {
                None | Some(0) => break,
                Some(k) => {
                    dst.borrow_mut()[start + written..start + written + k]
                        .copy_from_slice(&tmp[..k]);
                    written += k;
                }
            }
        } else if !(read1 && written > 0) {
            if bw_peek_unlocked(&inst, &target, 1)?.is_empty() {
                break;
            }
        }
        if read1 && written > 0 {
            break;
        }
    }
    Ok(Object::Int(written as i64))
}

fn bw_readinto(args: &[Object]) -> Result<Object, RuntimeError> {
    bw_readinto_impl(args, false)
}

fn bw_readinto1(args: &[Object]) -> Result<Object, RuntimeError> {
    bw_readinto_impl(args, true)
}

fn bw_readline(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let clsname = bw_typename(&inst);
    bw_check_init(&inst)?;
    if !bw_is_reader(&clsname) {
        return Err(unsupported_op("readline"));
    }
    bw_check_open(&inst)?;
    let limit = match args.get(1) {
        Some(Object::Int(n)) if *n >= 0 => Some(*n as usize),
        _ => None,
    };
    if clsname == "BufferedRandom" {
        let wt = bw_writer_target(&inst)?;
        bw_flush_unlocked(&inst, &wt)?;
    }
    let target = bw_target(&inst)?;
    // `_pyio.IOBase.readline`: use `peek(1)` to find the next newline, then
    // read exactly that many bytes from the buffer.
    let mut res: Vec<u8> = Vec::new();
    loop {
        if let Some(l) = limit {
            if res.len() >= l {
                break;
            }
        }
        let readahead = bw_peek_unlocked(&inst, &target, 1)?;
        let mut n = if readahead.is_empty() {
            1
        } else {
            readahead
                .iter()
                .position(|&c| c == b'\n')
                .map(|i| i + 1)
                .unwrap_or(readahead.len())
        };
        if let Some(l) = limit {
            n = n.min(l - res.len());
        }
        let b = bw_read_unlocked(&inst, &target, Some(n))?.unwrap_or_default();
        if b.is_empty() {
            break;
        }
        res.extend_from_slice(&b);
        if res.ends_with(b"\n") {
            break;
        }
    }
    Ok(Object::new_bytes(res))
}

fn bw_next(args: &[Object]) -> Result<Object, RuntimeError> {
    let line = bw_readline(args)?;
    if obj_is_empty(&line) {
        return Err(crate::error::stop_iteration());
    }
    Ok(line)
}

/// Absolute position of the underlying raw stream.
fn bw_raw_tell(target: &RawTarget) -> Result<i64, RuntimeError> {
    let pos = match target {
        RawTarget::Native(raw) => raw.seek(0, 1)? as i64,
        RawTarget::Py(raw) => py_call(raw, "tell", &[])?.as_i64().unwrap_or(-1),
    };
    // `_buffered_raw_tell`: a raw stream reporting a negative position is
    // misbehaving (`test_misbehaved_io`).
    if pos < 0 {
        return Err(crate::error::os_error(
            "Raw stream returned invalid position -1".to_owned(),
        ));
    }
    Ok(pos)
}

/// Seek the underlying raw stream, returning the new absolute position.
fn bw_raw_seek(target: &RawTarget, offset: i64, whence: i32) -> Result<Object, RuntimeError> {
    let pos = match target {
        RawTarget::Native(raw) => raw.seek(offset as isize, whence)? as i64,
        RawTarget::Py(raw) => py_call(
            raw,
            "seek",
            &[Object::Int(offset), Object::Int(i64::from(whence))],
        )?
        .as_i64()
        .unwrap_or(-1),
    };
    // `_buffered_raw_seek`: negative result means the raw stream lied
    // (`test_misbehaved_io`).
    if pos < 0 {
        return Err(crate::error::os_error(format!(
            "Raw stream returned invalid position {pos}"
        )));
    }
    Ok(Object::Int(pos))
}

fn bw_raw_flush(target: &RawTarget) -> Result<(), RuntimeError> {
    match target {
        RawTarget::Native(raw) => raw.flush(),
        RawTarget::Py(raw) => py_call(raw, "flush", &[]).map(|_| ()),
    }
}

fn bw_flush(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let clsname = bw_typename(&inst);
    bw_check_init(&inst)?;
    if bw_is_writer(&clsname) {
        let target = bw_writer_target(&inst)?;
        if bw_raw_closed(&target) {
            return Err(value_error("flush on closed file"));
        }
        bw_flush_unlocked(&inst, &target)?;
        bw_raw_flush(&target)?;
    } else {
        // Reader: delegate flush to the raw stream (`_BufferedIOMixin.flush`).
        match bw_target(&inst) {
            Ok(t) => {
                if bw_raw_closed(&t) {
                    return Err(value_error("flush on closed file"));
                }
                bw_raw_flush(&t)?;
            }
            Err(_) => {}
        }
    }
    Ok(Object::None)
}

fn bw_raw_close(target: &RawTarget) -> Result<(), RuntimeError> {
    match target {
        RawTarget::Native(raw) => {
            raw.close();
            Ok(())
        }
        RawTarget::Py(raw) => py_call(raw, "close", &[]).map(|_| ()),
    }
}

/// Chain `context` as the `__context__` of `primary` (implicit exception
/// chaining), used when a close error supersedes a flush error. The link is
/// mirrored straight onto the instance dict (not just the `PyException`) so
/// Python's `except` sees `e.__context__` even though the close error already
/// carries a traceback from the raw stream's Python `close` frame
/// (`test_close_error_on_close`).
fn chain_context(primary: RuntimeError, context: RuntimeError) -> RuntimeError {
    match (primary, context) {
        (RuntimeError::PyException(mut p), RuntimeError::PyException(c)) => {
            if p.context.is_none() {
                if let Object::Instance(inst) = &p.instance {
                    inst.dict.borrow_mut().insert(
                        DictKey(Object::from_static("__context__")),
                        c.instance.clone(),
                    );
                }
                p.context = Some(Box::new(c));
                p.context_settled = true;
            }
            RuntimeError::PyException(p)
        }
        (p, _) => p,
    }
}

/// `BufferedRWPair`'s writer side (`self.writer.close()`): flush the pending
/// write buffer, then close the writer raw, chaining a flush error as the
/// `__context__` of a close error.
fn bw_close_writer_side(inst: &crate::types::PyInstance) -> Result<(), RuntimeError> {
    let wt = bw_writer_target(inst)?;
    if bw_raw_closed(&wt) {
        return Ok(());
    }
    let flush_err = bw_flush_unlocked(inst, &wt).err();
    let close_err = bw_raw_close(&wt).err();
    match (close_err, flush_err) {
        (Some(ce), Some(fe)) => Err(chain_context(ce, fe)),
        (Some(ce), None) => Err(ce),
        (None, Some(fe)) => Err(fe),
        (None, None) => Ok(()),
    }
}

/// `BufferedRWPair`'s reader side (`self.reader.close()`): drop the read buffer
/// and close the reader raw.
fn bw_close_reader_side(inst: &crate::types::PyInstance) -> Result<(), RuntimeError> {
    let rt = bw_target(inst)?;
    rdbuf_set(inst, Vec::new());
    if bw_raw_closed(&rt) {
        return Ok(());
    }
    bw_raw_close(&rt)
}

fn bw_close(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    // Idempotent; an uninitialized object's close is a no-op.
    if bw_check_init(&inst).is_err() {
        return Ok(Object::None);
    }
    if bw_typename(&inst) == "BufferedRWPair" {
        // `BufferedRWPair.close`: `try: writer.close() finally: reader.close()`.
        // A reader-close error supersedes a writer-close error, taking it as its
        // `__context__` (`test_reader_writer_close_error_on_close`).
        let writer_err = bw_close_writer_side(&inst).err();
        let reader_err = bw_close_reader_side(&inst).err();
        return match (reader_err, writer_err) {
            (Some(re), Some(we)) => Err(chain_context(re, we)),
            (Some(re), None) => Err(re),
            (None, Some(we)) => Err(we),
            (None, None) => Ok(Object::None),
        };
    }
    let reader = match bw_target(&inst) {
        Ok(t) => t,
        Err(_) => return Ok(Object::None),
    };
    if bw_raw_closed(&reader) {
        return Ok(Object::None);
    }
    // 1. `self.flush()` — honour a monkeypatched/overridden flush, and capture
    //    any error so it can be re-raised (or chained) after the raw is closed.
    let flush_err = py_call(&args[0], "flush", &[]).err();
    // 2. Close the underlying raw stream.
    let close_err = bw_raw_close(&reader).err();
    rdbuf_set(&inst, Vec::new());
    match (close_err, flush_err) {
        (Some(ce), Some(fe)) => Err(chain_context(ce, fe)),
        (Some(ce), None) => Err(ce),
        (None, Some(fe)) => Err(fe),
        (None, None) => Ok(Object::None),
    }
}

fn bw_seek(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let clsname = bw_typename(&inst);
    bw_check_open(&inst)?;
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
    // Flush pending writes before repositioning.
    if bw_is_writer(&clsname) {
        let wt = bw_writer_target(&inst)?;
        bw_flush_unlocked(&inst, &wt)?;
    }
    let target = bw_target(&inst)?;
    let buffered = rdbuf_get(&inst).len() as i64;
    rdbuf_set(&inst, Vec::new());
    let pos = if whence == 1 {
        let cur = bw_raw_tell(&target)?;
        bw_raw_seek(&target, cur - buffered + offset, 0)?
    } else {
        bw_raw_seek(&target, offset, whence)?
    };
    Ok(pos)
}

fn bw_tell(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    bw_check_open(&inst)?;
    let target = bw_target(&inst)?;
    let cur = bw_raw_tell(&target)?;
    let rd = rdbuf_get(&inst).len() as i64;
    let wr = wrbuf_get(&inst).len() as i64;
    // Reader: subtract unconsumed read-ahead. Writer: add pending writes.
    Ok(Object::Int((cur - rd + wr).max(0)))
}

/// `BufferedWriter/Random.truncate(pos=None)` — flush the buffer, then
/// resize the underlying raw stream (default: current position). CPython
/// returns the new size and leaves the stream position unchanged.
fn bw_truncate(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let clsname = bw_typename(&inst);
    bw_check_open(&inst)?;
    // `_BufferedIOMixin.truncate` calls `_checkWritable()`; a read-only
    // `BufferedReader` therefore raises `UnsupportedOperation`
    // (`test_truncate_on_read_only`).
    if !bw_is_writer(&clsname) {
        return Err(unsupported_op("truncate"));
    }
    if bw_is_writer(&clsname) {
        let wt = bw_writer_target(&inst)?;
        bw_flush_unlocked(&inst, &wt)?;
    }
    let size = args.get(1).cloned();
    let target = bw_target(&inst)?;
    // Resolve the truncation position. CPython's `_BufferedIOMixin.truncate`
    // uses `self.tell()` — the *logical* position, which subtracts unconsumed
    // read-ahead — when `pos is None`, not the raw stream's physical offset.
    // After a `read()` fills the buffer, the raw is positioned past the
    // logical cursor, so truncating at the raw offset would keep too much
    // (`test_truncate_after_read_or_write`).
    let pos: i64 = match size.as_ref() {
        None | Some(Object::None) => {
            let cur = bw_raw_tell(&target)?;
            let rd = rdbuf_get(&inst).len() as i64;
            let wr = wrbuf_get(&inst).len() as i64;
            (cur - rd + wr).max(0)
        }
        Some(o) => crate::builtins::coerce_index_i64(o)?.max(0),
    };
    match target {
        RawTarget::Native(raw) => {
            raw.flush()?;
            Ok(Object::Int(raw.truncate(Some(pos as u64))? as i64))
        }
        RawTarget::Py(raw) => {
            let _ = py_call(&raw, "flush", &[]);
            let arg = Object::Int(pos);
            py_call(&raw, "truncate", std::slice::from_ref(&arg))
        }
    }
}

/// Delegate a capability query to a resolved raw target. Native files answer
/// from their open mode; Python raws are asked directly (and any exception —
/// e.g. a stream-mode `tarfile._FileInFile.seekable()` reaching an absent
/// `_Stream.seekable` — propagates, per `test_extractfile_attrs`).
fn bw_delegate_cap(target: &RawTarget, name: &str) -> Result<bool, RuntimeError> {
    match target {
        RawTarget::Native(raw) => Ok(match name {
            "readable" => raw.readable(),
            "writable" => raw.writable(),
            _ => raw.seekable(),
        }),
        RawTarget::Py(raw) => Ok(py_call(raw, name, &[])?.is_truthy()),
    }
}

/// `readable()` — `True` only for the reader flavours, delegating to the read
/// side (CPython: `BufferedReader.readable` → `raw.readable()`, while
/// `BufferedWriter` inherits `IOBase.readable() == False`).
fn bw_readable(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    if !bw_is_reader(&bw_typename(&inst)) {
        return Ok(Object::Bool(false));
    }
    match bw_target(&inst) {
        Ok(t) => Ok(Object::Bool(bw_delegate_cap(&t, "readable")?)),
        Err(_) => Ok(Object::Bool(false)),
    }
}

/// `writable()` — mirror of [`bw_readable`] for the writer flavours
/// (`BufferedReader.writable() == False`, `test_truncate_on_read_only`).
fn bw_writable(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    if !bw_is_writer(&bw_typename(&inst)) {
        return Ok(Object::Bool(false));
    }
    match bw_writer_target(&inst) {
        Ok(t) => Ok(Object::Bool(bw_delegate_cap(&t, "writable")?)),
        Err(_) => Ok(Object::Bool(false)),
    }
}

/// `seekable()` — `self.raw.seekable()` for the single-stream wrappers
/// (`_BufferedIOMixin.seekable`); `BufferedRWPair` does not define it and so
/// inherits `IOBase.seekable() == False`.
fn bw_seekable(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    if bw_typename(&inst) == "BufferedRWPair" {
        return Ok(Object::Bool(false));
    }
    match bw_target(&inst) {
        Ok(t) => Ok(Object::Bool(bw_delegate_cap(&t, "seekable")?)),
        Err(_) => Ok(Object::Bool(false)),
    }
}

fn bw_fileno(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    match bw_target(&inst)? {
        // A buffered wrapper over a real OS file forwards its descriptor; over
        // an in-memory `BytesIO` it raises `io.UnsupportedOperation` (an
        // `OSError`+`ValueError` subclass), like CPython — `test_io`'s
        // `test_optional_abilities` does `assertRaises(OSError, obj.fileno)`.
        RawTarget::Native(file) => match file.fileno() {
            Some(fd) => Ok(Object::Int(fd)),
            None => Err(unsupported_op("fileno")),
        },
        RawTarget::Py(raw) => py_call(&raw, "fileno", &[]),
    }
}

/// `isatty()` — delegates to the raw stream(s). `BufferedRWPair` returns
/// `reader.isatty() or writer.isatty()` (`test_isatty`).
fn bw_isatty_target(target: &RawTarget) -> Result<bool, RuntimeError> {
    match target {
        RawTarget::Native(_) => Ok(false),
        RawTarget::Py(raw) => Ok(py_call(raw, "isatty", &[])?.is_truthy()),
    }
}

fn bw_isatty(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    bw_check_open(&inst)?;
    if bw_typename(&inst) == "BufferedRWPair" {
        if bw_isatty_target(&bw_target(&inst)?)? {
            return Ok(Object::Bool(true));
        }
        return Ok(Object::Bool(bw_isatty_target(&bw_writer_target(&inst)?)?));
    }
    Ok(Object::Bool(bw_isatty_target(&bw_target(&inst)?)?))
}

fn bw_detach(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    bw_check_init(&inst)?;
    // `BufferedRWPair` has no `detach` (inherits `BufferedIOBase.detach`, which
    // raises `UnsupportedOperation`) — `test_detach`.
    if bw_typename(&inst) == "BufferedRWPair" {
        return Err(unsupported_op("detach"));
    }
    let raw = tw_get(&inst, "raw").ok_or_else(|| value_error("raw stream has been detached"))?;
    // `_BufferedIOMixin.detach` flushes the pending write buffer first.
    if bw_is_writer(&bw_typename(&inst)) {
        if let Ok(wt) = bw_writer_target(&inst) {
            bw_flush_unlocked(&inst, &wt)?;
        }
    }
    inst.dict
        .borrow_mut()
        .shift_remove(&DictKey(Object::from_static("raw")));
    Ok(raw)
}

thread_local! {
    /// `Py_ReprEnter`-style reentrancy guard for `bw_repr`: object ids whose
    /// `__repr__` is currently being rendered. A self-referential `name`
    /// (`test_recursive_repr`) re-enters and must raise `RuntimeError`.
    static BW_REPR_ACTIVE: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// `<_io.BufferedReader>` / `<_io.BufferedReader name='dummy'>` — mirrors
/// CPython's `buffered_repr` (the `name` is taken from the raw stream).
fn bw_repr(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = bw_self(args)?;
    let typename = format!("_io.{}", inst.cls().name);
    let name_obj = if bw_check_init(&inst).is_ok() {
        tw_get(&inst, "raw").and_then(|raw| py_get_attr(&raw, "name").ok())
    } else {
        None
    };
    let Some(n) = name_obj else {
        return Ok(Object::from_str(format!("<{typename}>")));
    };
    let id = match &args[0] {
        Object::Instance(i) => Rc::as_ptr(i) as *const () as usize,
        _ => 0,
    };
    if BW_REPR_ACTIVE.with(|s| s.borrow().contains(&id)) {
        return Err(crate::error::runtime_error(format!(
            "reentrant call inside {typename}.__repr__"
        )));
    }
    BW_REPR_ACTIVE.with(|s| s.borrow_mut().push(id));
    // Fallible repr so the nested reentrant `RuntimeError` actually propagates
    // out (an infallible `Object::repr` would swallow it and the recursion guard
    // would be pointless — `test_recursive_repr`).
    let rendered = py_repr(&n);
    BW_REPR_ACTIVE.with(|s| s.borrow_mut().retain(|&x| x != id));
    Ok(Object::from_str(format!("<{typename} name={}>", rendered?)))
}

/// Fallible `repr(obj)` through the running interpreter (dispatches `__repr__`),
/// so errors raised by a nested repr propagate instead of being swallowed.
fn py_repr(obj: &Object) -> Result<String, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| value_error("no running interpreter for stream delegation"))?;
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    interp.repr_object(obj)
}
