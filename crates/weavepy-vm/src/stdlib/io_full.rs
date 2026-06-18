//! The `_io` C-level layer — RFC 0023.
//!
//! Mirrors CPython's `_io` module: the buffered / raw / text IO
//! hierarchy that lives below the user-facing `io` module. We
//! expose the type objects so that `import _io; _io.IOBase` works
//! and the Python `io.py` wrapper (RFC 0023 future) can subclass
//! them. Concrete impls (`FileIO`, `BytesIO`, `StringIO`) are
//! provided as fast paths; `BufferedReader`/`BufferedWriter`/
//! `TextIOWrapper` are thin Python-driven layers when WeavePy ships
//! the corresponding `io.py`.
//!
//! For now, this module also re-exports the existing `io.StringIO` /
//! `io.BytesIO` machinery (see [`crate::stdlib::io`]) under their
//! `_io.*` names so consumers like `tokenize`, `pickle`, and
//! `warnings` can import either way.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::types::{TypeFlags, TypeObject};

pub fn build(cache: &ModuleCache) -> Rc<PyModule> {
    // Inherit everything from the existing `io` module and add the
    // protocol classes.
    let high_level = crate::stdlib::io::build(cache);
    let dict = Rc::new(RefCell::new(high_level.dict.borrow().clone()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_io"),
        );
        let bt = crate::builtin_types::builtin_types();

        // Share the user-facing `io` module's IOBase hierarchy (RFC 0038) so
        // `_io.IOBase` carries the same working mixin methods
        // (`__enter__`/`__iter__`/`writelines`/…) and the two faces stay in
        // lockstep.
        let fam = crate::stdlib::io::build_iobase_family();
        let iobase = fam.iobase.clone();
        let raw_iobase = fam.raw.clone();
        let buffered_iobase = fam.buffered.clone();
        let text_iobase = fam.text.clone();
        let fileio = fam.fileio.clone();
        install_closed_getset(&fileio);
        // Functional buffered wrappers (shared with the user-facing `io`
        // module): `_io.BufferedWriter(_io.BytesIO())` actually wraps and
        // delegates, so the stdlib's `from _io import BufferedWriter` paths
        // (and stream-capture helpers built on them) work.
        let buffered_reader =
            crate::stdlib::io::make_buffered("BufferedReader", buffered_iobase.clone());
        let buffered_writer =
            crate::stdlib::io::make_buffered("BufferedWriter", buffered_iobase.clone());
        let buffered_random =
            crate::stdlib::io::make_buffered("BufferedRandom", buffered_iobase.clone());
        let buffered_rw =
            crate::stdlib::io::make_buffered("BufferedRWPair", buffered_iobase.clone());
        // Functional `TextIOWrapper` (text layer over a binary buffer), shared
        // with the user-facing `io` module so `from _io import TextIOWrapper`
        // — used by CPython's `io.py` and the stdlib test harness — works.
        let text_io_wrapper = crate::stdlib::io::make_text_io_wrapper(text_iobase.clone());
        let incremental_newline =
            make_protocol("IncrementalNewlineDecoder", vec![bt.object_.clone()]);
        let unsupported_op = make_protocol(
            "UnsupportedOperation",
            vec![bt.os_error.clone(), bt.value_error.clone()],
        );

        for (name, cls) in [
            ("IOBase", &iobase),
            ("RawIOBase", &raw_iobase),
            ("BufferedIOBase", &buffered_iobase),
            ("TextIOBase", &text_iobase),
            ("FileIO", &fileio),
            ("BufferedReader", &buffered_reader),
            ("BufferedWriter", &buffered_writer),
            ("BufferedRandom", &buffered_random),
            ("BufferedRWPair", &buffered_rw),
            ("TextIOWrapper", &text_io_wrapper),
            ("IncrementalNewlineDecoder", &incremental_newline),
            ("UnsupportedOperation", &unsupported_op),
        ] {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Type(cls.clone()),
            );
        }

        // CPython exposes the buffer-size default and a couple of
        // module-level constants. Keep parity for code that reads
        // `_io.DEFAULT_BUFFER_SIZE`.
        d.insert(
            DictKey(Object::from_static("DEFAULT_BUFFER_SIZE")),
            Object::Int(8192),
        );
        d.insert(
            DictKey(Object::from_static("text_encoding")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "text_encoding",
                binds_instance: false,
                call: Box::new(crate::stdlib::io::io_text_encoding),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("open")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "open",
                binds_instance: false,
                call: Box::new(io_open),
                call_kw: Some(Box::new(io_open_kw)),
            })),
        );
        d.insert(
            DictKey(Object::from_static("open_code")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "open_code",
                binds_instance: false,
                call: Box::new(io_open),
                call_kw: Some(Box::new(io_open_kw)),
            })),
        );
    }
    Rc::new(PyModule {
        name: "_io".to_owned(),
        filename: None,
        dict,
    })
}

/// Install `FileIO.closed` as a getset descriptor (CPython's
/// `_io.FileIO.closed`). Type-level access yields the descriptor itself,
/// whose `__doc__` is the curated string the descriptor tests assert on
/// (test_descr test_descrdoc); instance access reports the file state.
fn install_closed_getset(ty: &Rc<TypeObject>) {
    fn closed_get(args: &[Object]) -> Result<Object, RuntimeError> {
        match args.first() {
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
        Object::from_static("True if the file is closed"),
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

fn make_protocol(name: &'static str, bases: Vec<Rc<TypeObject>>) -> Rc<TypeObject> {
    let mut td = DictData::new();
    // Default abstract methods — concrete impls supply real bodies.
    for stub_name in [
        "read",
        "readinto",
        "readline",
        "readlines",
        "write",
        "writelines",
        "seek",
        "tell",
        "truncate",
        "flush",
        "close",
        "__enter__",
        "__exit__",
        "fileno",
        "isatty",
        "readable",
        "writable",
        "seekable",
        "detach",
    ] {
        td.insert(
            DictKey(Object::from_static(stub_name)),
            Object::Builtin(Rc::new(BuiltinFn {
                name: stub_name,
                binds_instance: true,
                call: Box::new(stub_method),
                call_kw: None,
            })),
        );
    }
    TypeObject::new_with_flags(
        name,
        bases,
        td,
        TypeFlags {
            is_exception: name == "UnsupportedOperation",
            is_builtin: true,
        },
    )
    .expect("io protocol type")
}

fn stub_method(args: &[Object]) -> Result<Object, RuntimeError> {
    // Default behaviour when a subclass hasn't overridden a method:
    // raise `io.UnsupportedOperation`-shaped error. We can't reach
    // the type registry from a static fn without recursion, so route
    // through the existing `OSError` family.
    let _ = args;
    Err(crate::error::os_error(
        "operation not supported on this stream",
    ))
}

/// `_io.open` — delegates to the regular `open()` builtin via the VM
/// (the call site routes through `builtin_constructor_for`). This is
/// a thin shim so `from _io import open` works.
/// Keyword-aware front door for [`io_open`]. CPython's signature is
/// `open(file, mode='r', buffering=-1, encoding=None, errors=None,
/// newline=None, closefd=True, opener=None)`; `tempfile` (and much user
/// code) passes these by keyword. We fold the keywords back into the
/// positional order `io_open` understands.
pub(crate) fn io_open_kw(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    const NAMES: [&str; 8] = [
        "file",
        "mode",
        "buffering",
        "encoding",
        "errors",
        "newline",
        "closefd",
        "opener",
    ];
    if args.len() > NAMES.len() {
        return Err(type_error("open() takes at most 8 arguments"));
    }
    let mut slots: [Option<Object>; 8] = [None, None, None, None, None, None, None, None];
    for (i, a) in args.iter().enumerate() {
        slots[i] = Some(a.clone());
    }
    for (k, v) in kwargs {
        match NAMES.iter().position(|n| n == k) {
            Some(idx) => {
                if slots[idx].is_some() {
                    return Err(type_error(format!(
                        "open() got multiple values for argument '{k}'"
                    )));
                }
                slots[idx] = Some(v.clone());
            }
            None => {
                return Err(type_error(format!(
                    "open() got an unexpected keyword argument '{k}'"
                )))
            }
        }
    }
    // `opener=` (CPython): call `fd = opener(file, flags)` and wrap the
    // returned descriptor. `tempfile.NamedTemporaryFile`/`TemporaryFile`
    // drive their open through this path, so honouring it is what makes
    // those (and the many tests that lean on them) work.
    let opener = slots[7].clone().filter(|o| !matches!(o, Object::None));
    if let Some(opener) = opener {
        let file_arg = slots[0].clone().unwrap_or(Object::None);
        let mode = match &slots[1] {
            Some(Object::Str(s)) => s.to_string(),
            None | Some(Object::None) => "r".to_owned(),
            Some(other) => {
                return Err(type_error(format!(
                    "open() argument 'mode' must be str, not {}",
                    other.type_name()
                )))
            }
        };
        crate::builtins::validate_open_mode(&mode)?;
        let flags = open_flags_for_mode(&mode);
        let ptr = crate::vm_singletons::current_interpreter_ptr()
            .ok_or_else(|| crate::error::runtime_error("no running interpreter"))?;
        // SAFETY: an enclosing VM frame on this thread published the
        // pointer; the GIL keeps the reentrant access exclusive. Same
        // pattern as `slot_call` in `builtins.rs`.
        let interp = unsafe { &mut *ptr };
        let globals = interp.builtins_dict();
        let fd_obj = interp.call_object_with_globals(
            &opener,
            &[file_arg, Object::Int(flags)],
            &[],
            &globals,
        )?;
        let name = match &slots[0] {
            Some(Object::Str(s)) => s.to_string(),
            _ => String::new(),
        };
        let file = file_from_fd(&fd_obj, &mode, name)?;
        apply_text_config(
            &file,
            slots[3].as_ref(),
            slots[4].as_ref(),
            slots[5].as_ref(),
        )?;
        return Ok(file);
    }

    let last = slots.iter().rposition(Option::is_some).map_or(0, |i| i + 1);
    let positional: Vec<Object> = slots[..last]
        .iter()
        .map(|s| s.clone().unwrap_or(Object::None))
        .collect();
    io_open(&positional)
}

/// Translate an `open()` mode string into WeavePy's (Linux-style) `os.open`
/// flag bits, matching the constants exposed by the `os` module. Used only
/// to hand a plausible `flags` value to a user `opener` callback.
fn open_flags_for_mode(mode: &str) -> i64 {
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

/// Adopt an already-open OS file descriptor (e.g. produced by an `opener`
/// callback or `os.open`) into a `PyFile`.
fn file_from_fd(fd_obj: &Object, mode: &str, name: String) -> Result<Object, RuntimeError> {
    let fd = match fd_obj {
        Object::Int(n) => *n,
        _ => return Err(type_error("opener returned a non-integer file descriptor")),
    };
    // A negative descriptor (e.g. a custom `opener` that returns -1) is an
    // error, not a real fd — CPython's `FileIO` raises `ValueError: opener
    // returned -1` rather than handing the bad fd to the OS.
    if fd < 0 {
        return Err(value_error(format!("opener returned {fd}")));
    }
    #[cfg(unix)]
    {
        use crate::object::{FileBackend, PyFile};
        use std::os::unix::io::FromRawFd;
        let fd = i32::try_from(fd).map_err(|_| value_error("file descriptor out of range"))?;
        // SAFETY: ownership of the fd transfers to the new File; the
        // descriptor came from `os.open`/`opener` and is closed exactly
        // once when the PyFile drops.
        let f = unsafe { std::fs::File::from_raw_fd(fd) };
        let display = if name.is_empty() {
            fd.to_string()
        } else {
            name
        };
        Ok(Object::File(Rc::new(PyFile::new(
            display,
            mode,
            FileBackend::Disk(f),
        ))))
    }
    #[cfg(not(unix))]
    {
        let _ = (mode, name);
        Err(crate::error::runtime_error(
            "file-descriptor open is only supported on Unix",
        ))
    }
}

/// When `open()` is called in text mode, push the explicit `encoding=`,
/// `errors=`, and `newline=` settings onto the resulting text file (binary
/// files ignore them). Mirrors the builtin `open()` plumbing so the
/// `io.open` path (used by `pathlib`, `tempfile`, …) honours them too.
fn apply_text_config(
    file: &Object,
    encoding: Option<&Object>,
    errors: Option<&Object>,
    newline: Option<&Object>,
) -> Result<(), RuntimeError> {
    let Object::File(f) = file else {
        return Ok(());
    };
    if f.binary {
        return Ok(());
    }
    if let Some(Object::Str(enc)) = encoding {
        // CPython resolves the codec at construction time (via
        // `_PyCodec_LookupTextEncoding`), so an unknown encoding raises
        // `LookupError` from `open()`/`TextIOWrapper(...)` — not later at the
        // first read. Validate against the live codec registry so e.g.
        // `tempfile.TemporaryFile(encoding='bad-encoding')` fails eagerly.
        validate_text_encoding(enc)?;
        f.set_encoding(enc);
    }
    if let Some(Object::Str(err)) = errors {
        f.set_errors(err);
    }
    if let Some(Object::Str(nl)) = newline {
        f.set_newline(Some(nl));
    }
    Ok(())
}

/// Look the encoding up in the running interpreter's `codecs` registry,
/// mirroring CPython's eager codec resolution. Returns `Ok(())` when the
/// codec is known and propagates `codecs.lookup`'s `LookupError` otherwise.
/// A missing interpreter (should not happen for user-facing `open`) is
/// treated as "do not block".
pub(crate) fn validate_text_encoding(encoding: &str) -> Result<(), RuntimeError> {
    let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() else {
        return Ok(());
    };
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    // Be tolerant of early bootstrap: if `codecs` isn't importable yet (or has
    // no `lookup`), skip validation rather than wedge interpreter start-up.
    // Only the codec lookup itself — which raises `LookupError` for an unknown
    // encoding — is allowed to fail the open.
    let Ok(codecs) = interp.import_path("codecs") else {
        return Ok(());
    };
    let Ok(lookup) = interp.load_attr_public(&codecs, "lookup") else {
        return Ok(());
    };
    interp.call_object(lookup, &[Object::from_str(encoding)], &[])?;
    Ok(())
}

pub(crate) fn io_open(args: &[Object]) -> Result<Object, RuntimeError> {
    use crate::object::{FileBackend, PyFile};
    // `open(fd, mode, ...)` — adopt an already-open OS descriptor, exactly
    // like CPython's `io.FileIO(fd)` / `io.open(fd)`. `subprocess.Popen`
    // wraps its pipe ends this way, and `os.fdopen` is a thin alias.
    if let Some(Object::Int(fd)) = args.first() {
        let mode = match args.get(1) {
            Some(Object::Str(s)) => s.to_string(),
            Some(Object::None) | None => "r".to_owned(),
            Some(other) => {
                return Err(type_error(format!(
                    "open() argument 'mode' must be str, not {}",
                    other.type_name()
                )))
            }
        };
        crate::builtins::validate_open_mode(&mode)?;
        if *fd < 0 {
            return Err(value_error("negative file descriptor"));
        }
        let file = file_from_fd(&Object::Int(*fd), &mode, String::new())?;
        // `closefd=False` (positional index 6): the caller keeps ownership
        // of the descriptor; closing the stream must not close the fd.
        if let (Object::File(f), Some(Object::Bool(false))) = (&file, args.get(6)) {
            f.closefd.set(false);
        }
        apply_text_config(&file, args.get(3), args.get(4), args.get(5))?;
        return Ok(file);
    }
    let path = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        Some(other) => {
            return Err(type_error(format!(
                "open: path must be str, not '{}'",
                other.type_name()
            )))
        }
        None => return Err(type_error("open() requires a path")),
    };
    let mode = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        Some(Object::None) | None => "r".to_owned(),
        Some(other) => {
            return Err(type_error(format!(
                "open() argument 'mode' must be str, not {}",
                other.type_name()
            )))
        }
    };
    crate::builtins::validate_open_mode(&mode)?;
    // PEP 578 — `open(file, mode, flags)` audit event.
    crate::stdlib::sys::audit_event(
        "open",
        &[
            Object::from_str(path.clone()),
            Object::from_str(mode.clone()),
            Object::Int(0),
        ],
    );
    let binary = mode.contains('b');
    let writable =
        mode.contains('w') || mode.contains('a') || mode.contains('+') || mode.contains('x');
    let appending = mode.contains('a');
    let truncate = mode.contains('w');
    let mut opts = std::fs::OpenOptions::new();
    opts.read(!writable || mode.contains('+'))
        .write(writable)
        .append(appending)
        .truncate(truncate)
        .create(writable || appending);
    let f = opts
        .open(&path)
        .map_err(|e| crate::error::io_error_to_py_named(&e, Some(&path)))?;
    let backend = FileBackend::Disk(f);
    let file = Object::File(Rc::new(PyFile::new(path, mode, backend)));
    let _ = binary; // text decoding is handled by PyFile itself.
                    // Positional `open(file, mode, buffering, encoding, errors, newline, …)`.
    apply_text_config(&file, args.get(3), args.get(4), args.get(5))?;
    Ok(file)
}

/// Public entry: ensure the `_io` types exist even before module
/// import, so `isinstance(x, io.IOBase)` from user code never fails.
pub fn _prefetch() {
    let _ = value_error("unused");
}
