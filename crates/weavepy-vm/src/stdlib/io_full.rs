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
        // Buffered wrappers and `TextIOWrapper`: take the *memoised* family
        // objects (the same ones `class_of` reports for a native `Object::File`
        // and the user-facing `io` module exports). This is load-bearing for
        // identity — `io.BufferedReader is _io.BufferedReader` and, crucially,
        // `type(open(p,'rb')) is io.BufferedReader` (the frozen `io.py`
        // re-exports these `_io` names, and a native file's `class_of` must land
        // on the very same object). `BufferedRWPair` has no native `Object::File`
        // mapping, so it is still minted fresh.
        let buffered_reader = fam.buffered_reader.clone();
        let buffered_writer = fam.buffered_writer.clone();
        let buffered_random = fam.buffered_random.clone();
        let buffered_rw =
            crate::stdlib::io::make_buffered("BufferedRWPair", buffered_iobase.clone());
        crate::stdlib::io::set_type_module(&buffered_rw, "_io");
        let text_io_wrapper = fam.text_io_wrapper.clone();
        // `BytesIO`/`StringIO` must be reachable as `_io.BytesIO`/`_io.StringIO`
        // (the same memoised objects `class_of` reports), so `pickle` — which
        // serialises an in-memory stream by reference to `_io.BytesIO` — can
        // resolve the class on load. `io.BytesIO is _io.BytesIO` then holds too.
        let bytes_io = fam.bytes_io.clone();
        let string_io = fam.string_io.clone();
        // A functional `IncrementalNewlineDecoder` (native port of
        // `_pyio.IncrementalNewlineDecoder`): `decode`/`getstate`/`setstate`/
        // `reset`/`newlines` all work, matching CPython's C accelerator so
        // `CIncrementalNewlineDecoderTest` passes alongside the `Py` twin.
        let incremental_newline = crate::stdlib::io::make_incremental_newline_decoder();
        // Use the *canonical* `io.UnsupportedOperation` type (the memoised one
        // raised by the native IO methods via `io::unsupported_op`), so that
        // `isinstance(exc, io.UnsupportedOperation)` holds for errors raised by
        // `FileIO`/`BytesIO`/`Buffered*` — gzip/bz2/lzma and `test_io` all do
        // `assertRaises(io.UnsupportedOperation, ...)`. Building a *fresh*
        // `make_protocol` class here would diverge from the raised type.
        let unsupported_op = crate::stdlib::io::unsupported_operation_class();

        for (name, cls) in [
            ("IOBase", &iobase),
            ("RawIOBase", &raw_iobase),
            ("BufferedIOBase", &buffered_iobase),
            ("TextIOBase", &text_iobase),
            ("FileIO", &fileio),
            ("BytesIO", &bytes_io),
            ("StringIO", &string_io),
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
                call: Box::new(io_open_code),
                call_kw: Some(Box::new(io_open_code_kw)),
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

// Retained for symmetry with the other `_io` ABC stubs even though every
// concrete `_io` type is now built with real methods; kept so re-adding a
// protocol-only class stays a one-liner.
#[allow(dead_code)]
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
        apply_buffering(&file, buffering_arg(slots[2].as_ref()), mode.contains('b'))?;
        return Ok(file);
    }

    let last = slots.iter().rposition(Option::is_some).map_or(0, |i| i + 1);
    let positional: Vec<Object> = slots[..last]
        .iter()
        .map(|s| s.clone().unwrap_or(Object::None))
        .collect();
    io_open(&positional)
}

/// Translate an `open()` mode string into the host platform's `os.open` flag
/// bits, matching the constants exposed by the `os` module. Used only to hand a
/// plausible `flags` value to a user `opener` callback.
fn open_flags_for_mode(mode: &str) -> i64 {
    crate::stdlib::os::open_flags_for_mode(mode)
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
        // CPython's `FileIO.__init__` `fstat()`s the descriptor and raises
        // `OSError(EBADF)` when it is not a live fd. This is how a custom
        // `opener` that returns a stale/closed descriptor is rejected
        // (`test_io.test_opener_invalid_fd`) instead of silently wrapping a
        // dead fd. Do this *before* `from_raw_fd` so we never take ownership
        // of (and later `close(2)`) a descriptor we don't own.
        {
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstat(fd, &mut st) } != 0 {
                return Err(crate::error::io_error_to_py(
                    &std::io::Error::last_os_error(),
                ));
            }
        }
        // SAFETY: ownership of the fd transfers to the new File; the
        // descriptor came from `os.open`/`opener` and is closed exactly
        // once when the PyFile drops.
        let f = unsafe { std::fs::File::from_raw_fd(fd) };
        let from_bare_fd = name.is_empty();
        let display = if from_bare_fd { fd.to_string() } else { name };
        let pyfile = PyFile::new(display, mode, FileBackend::Disk(f));
        // A file opened over a bare integer descriptor reports the fd *as an
        // int* for `f.name` (CPython's `FileIO.name` is the original argument):
        // `open(fd, ...).name == fd` (`test_io.test_attributes`). A path-opened
        // file keeps its string name.
        if from_bare_fd {
            pyfile.name_is_fd.set(true);
        }
        Ok(Object::File(Rc::new(pyfile)))
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
        // CPython's TextIOWrapper validates the error handler eagerly under
        // `-X dev` (the `_CHECK_ERRORS` path), so `open(..., errors='Boom')`
        // raises `LookupError` at construction time.
        crate::stdlib::codecs_mod::check_text_errors(err)?;
        f.set_errors(err);
    }
    if let Some(Object::Str(nl)) = newline {
        // CPython validates the newline argument eagerly in `open()`/the
        // `TextIOWrapper` constructor: only `''`, `'\n'`, `'\r'`, `'\r\n'`
        // (and `None`) are legal.
        if !matches!(nl.as_ref(), "" | "\n" | "\r" | "\r\n") {
            return Err(value_error(format!("illegal newline value: {}", nl.as_ref())));
        }
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
    let info = interp.call_object(lookup, &[Object::from_str(encoding)], &[])?;
    // CPython opens text streams through `_PyCodec_LookupTextEncoding`, which
    // rejects a codec whose `CodecInfo._is_text_encoding` is `False` (the
    // binary transforms `hex`/`base64`/`quopri`/`bz2`): `LookupError("<name> is
    // not a text encoding; use codecs.open() to handle arbitrary codecs")`
    // (`test_io.test_non_text_encoding_codecs_are_rejected`).
    if let Ok(flag) = interp.load_attr_public(&info, "_is_text_encoding") {
        if !flag.is_truthy() {
            return Err(crate::error::lookup_error(format!(
                "{encoding} is not a text encoding; use codecs.open() to handle arbitrary codecs"
            )));
        }
    }
    Ok(())
}

/// PEP 597: emit `EncodingWarning` when `open()` opens a *text* stream with no
/// explicit `encoding` and `-X warn_default_encoding` is active, mirroring the
/// C `_io.open`. `stacklevel = 1` points at the user's `open(...)` line because
/// the native builtin pushes no Python frame of its own. A no-op when the flag
/// is off. Call only on the text path (`!binary`) with an absent/`None`
/// `encoding` argument.
pub(crate) fn warn_open_default_encoding() -> Result<(), RuntimeError> {
    if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
        // SAFETY: published by the enclosing VM frame on this thread; the GIL
        // keeps the pointer exclusive.
        let interp = unsafe { &mut *ptr };
        return interp.warn_encoding_default_from_builtin(1);
    }
    Ok(())
}

/// `True` when `open()`'s text path took no explicit `encoding`, i.e. the slot
/// is absent or `None` (a `str` would be an explicit choice). Used to gate the
/// PEP 597 `EncodingWarning`.
fn encoding_arg_is_default(encoding: Option<&Object>) -> bool {
    matches!(encoding, None | Some(Object::None))
}

/// Parse the `buffering` argument (`open()` positional index 2). A missing
/// or non-integer slot is the default policy (`-1`), matching how the rest of
/// `io_open` tolerates absent/`None` slots.
pub(crate) fn buffering_arg(arg: Option<&Object>) -> i64 {
    match arg {
        Some(Object::Int(n)) => *n,
        Some(Object::Bool(b)) => i64::from(*b),
        _ => -1,
    }
}

/// Apply CPython's `buffering` selection to a freshly built stream:
/// `buffering == 0` downgrades to the raw `FileIO` layer (binary only — an
/// unbuffered text stream is a `ValueError`), and `buffering == 1` in binary
/// mode is *not* line buffering, so `open()` warns and falls back to the
/// default block buffer (`_pyio.open`, `test_subprocess`
/// `test_bufsize_equal_one_binary_mode` / `test_io_unbuffered_works`).
pub(crate) fn apply_buffering(
    file: &Object,
    buffering: i64,
    binary: bool,
) -> Result<(), RuntimeError> {
    if buffering == 0 {
        if !binary {
            return Err(value_error("can't have unbuffered text I/O"));
        }
        if let Object::File(f) = file {
            f.set_io_kind(crate::object::IoKind::Raw);
        }
    } else if buffering == 1 && binary {
        if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
            // SAFETY: published by the enclosing VM frame on this thread.
            let interp = unsafe { &mut *ptr };
            interp.warn_runtime_from_builtin(
                "line buffering (buffering=1) isn't supported in binary \
                 mode, the default buffer size will be used"
                    .to_owned(),
            )?;
        }
    } else if buffering > 1 {
        // An explicit buffer size: CPython sizes the `Buffered*` layer to
        // `buffering` bytes. Only the binary buffered-writer path actually
        // stages writes, but recording it keeps `tell()`/flush thresholds
        // faithful for any future buffered layer.
        if let Object::File(f) = file {
            f.buf_size.set(buffering as usize);
        }
    }
    Ok(())
}

/// Resolve an `open()` path argument to a filesystem `String`, mirroring
/// CPython's `os.fspath()` (str/bytes pass through, an `os.PathLike` has its
/// `__fspath__()` called, anything else is a `TypeError`, and `__fspath__`
/// raising propagates) plus the embedded-NUL guard CPython applies before
/// touching the OS (`ValueError: embedded null byte`). A `bytes` path is
/// decoded with the filesystem encoding (UTF-8 here).
fn coerce_open_path(obj: &Object) -> Result<String, RuntimeError> {
    let resolved = crate::stdlib::os::os_fspath(std::slice::from_ref(obj))?;
    let s = match resolved {
        Object::Str(s) => s.to_string(),
        Object::Bytes(b) => String::from_utf8_lossy(&b).into_owned(),
        // `os_fspath` only ever returns `str`/`bytes`.
        other => {
            return Err(type_error(format!(
                "expected str, bytes or os.PathLike object, not {}",
                other.type_name()
            )))
        }
    };
    if s.as_bytes().contains(&0) {
        return Err(value_error("embedded null byte"));
    }
    Ok(s)
}

/// `io.open_code(path)` — open a file for reading executable code (PEP
/// 578). CPython opens it unconditionally in binary mode (`open(path,
/// 'rb')`); WeavePy has no audit-hook layer, so this is exactly that.
/// Forcing `'rb'` is load-bearing: `runpy._get_code_from_file` /
/// `pkgutil.read_code` read a `.pyc`'s 16-byte header as raw bytes, and a
/// text-mode stream would choke decoding the non-UTF-8 magic
/// (`importlib.util.MAGIC_NUMBER`).
pub(crate) fn io_open_code(args: &[Object]) -> Result<Object, RuntimeError> {
    let path = match args.first() {
        Some(obj) => obj.clone(),
        None => {
            return Err(type_error(
                "open_code() missing required argument 'path' (pos 1)",
            ))
        }
    };
    io_open(&[path, Object::from_static("rb")])
}

/// Keyword face of [`io_open_code`]: accepts `open_code(path=...)`.
pub(crate) fn io_open_code_kw(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let mut path = args.first().cloned();
    for (k, v) in kwargs {
        if k == "path" {
            if path.is_some() {
                return Err(type_error(
                    "open_code() got multiple values for argument 'path'",
                ));
            }
            path = Some(v.clone());
        } else {
            return Err(type_error(format!(
                "open_code() got an unexpected keyword argument '{k}'"
            )));
        }
    }
    match path {
        Some(p) => io_open(&[p, Object::from_static("rb")]),
        None => Err(type_error(
            "open_code() missing required argument 'path' (pos 1)",
        )),
    }
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
        if !mode.contains('b') && encoding_arg_is_default(args.get(3)) {
            warn_open_default_encoding()?;
        }
        apply_text_config(&file, args.get(3), args.get(4), args.get(5))?;
        apply_buffering(&file, buffering_arg(args.get(2)), mode.contains('b'))?;
        return Ok(file);
    }
    let path = match args.first() {
        Some(obj) => coerce_open_path(obj)?,
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
    // `closefd=False` (positional slot 6) is only meaningful for the
    // `open(fd, …)` form handled above; with a *path* CPython's `FileIO`
    // raises `ValueError` before touching the filesystem (`test_io`'s
    // `test_closefd` / `test_no_closefd_with_filename`).
    let closefd = match args.get(6) {
        None | Some(Object::None) => true,
        Some(Object::Bool(b)) => *b,
        Some(Object::Int(n)) => *n != 0,
        _ => true,
    };
    if !closefd {
        return Err(value_error("Cannot use closefd=False with file name"));
    }
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
    // `'x'` is exclusive-create (O_CREAT|O_EXCL): it must fail with
    // `FileExistsError` when the path already exists. `create_new(true)`
    // expresses exactly that and supersedes `create`/`truncate`.
    let exclusive = mode.contains('x');
    let mut opts = std::fs::OpenOptions::new();
    opts.read(!writable || mode.contains('+'))
        .write(writable)
        .append(appending)
        .truncate(truncate && !exclusive)
        .create(!exclusive && (writable || appending))
        .create_new(exclusive);
    let f = opts
        .open(&path)
        .map_err(|e| crate::error::io_error_to_py_named(&e, Some(&path)))?;
    let backend = FileBackend::Disk(f);
    let file = Object::File(Rc::new(PyFile::new(path, mode, backend)));
    // CPython's `FileIO` explicitly seeks an append-mode stream to the end at
    // open (`fileio.c`), so `tell()` reports the file size immediately. This is
    // what makes a BOM-prefixing encoder over a *non-empty* file opened in `'a'`
    // skip re-emitting the BOM (`test_io.test_append_bom`): the seek sets the
    // text start-of-stream flag from the resulting non-zero position.
    if appending {
        if let Object::File(f) = &file {
            let _ = f.seek(0, 2);
        }
    }
    // Positional `open(file, mode, buffering, encoding, errors, newline, …)`.
    if !binary && encoding_arg_is_default(args.get(3)) {
        warn_open_default_encoding()?;
    }
    apply_text_config(&file, args.get(3), args.get(4), args.get(5))?;
    apply_buffering(&file, buffering_arg(args.get(2)), binary)?;
    Ok(file)
}

/// Public entry: ensure the `_io` types exist even before module
/// import, so `isinstance(x, io.IOBase)` from user code never fails.
pub fn _prefetch() {
    let _ = value_error("unused");
}
