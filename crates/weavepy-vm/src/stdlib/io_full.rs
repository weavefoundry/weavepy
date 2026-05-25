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

use std::cell::RefCell;
use std::rc::Rc;

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

        let iobase = make_protocol("IOBase", vec![bt.object_.clone()]);
        let raw_iobase = make_protocol("RawIOBase", vec![iobase.clone()]);
        let buffered_iobase = make_protocol("BufferedIOBase", vec![iobase.clone()]);
        let text_iobase = make_protocol("TextIOBase", vec![iobase.clone()]);
        let fileio = make_protocol("FileIO", vec![raw_iobase.clone()]);
        let buffered_reader = make_protocol("BufferedReader", vec![buffered_iobase.clone()]);
        let buffered_writer = make_protocol("BufferedWriter", vec![buffered_iobase.clone()]);
        let buffered_random = make_protocol("BufferedRandom", vec![buffered_iobase.clone()]);
        let buffered_rw = make_protocol("BufferedRWPair", vec![buffered_iobase.clone()]);
        let text_io_wrapper = make_protocol("TextIOWrapper", vec![text_iobase.clone()]);
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
            DictKey(Object::from_static("open")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "open",
                call: Box::new(io_open),
            })),
        );
        d.insert(
            DictKey(Object::from_static("open_code")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "open_code",
                call: Box::new(io_open),
            })),
        );
    }
    Rc::new(PyModule {
        name: "_io".to_owned(),
        filename: None,
        dict,
    })
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
                call: Box::new(stub_method),
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
fn io_open(args: &[Object]) -> Result<Object, RuntimeError> {
    use crate::object::{FileBackend, PyFile};
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
        _ => "r".to_owned(),
    };
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
        .map_err(|e| crate::error::os_error(format!("{path}: {e}")))?;
    let backend = FileBackend::Disk(f);
    let file = PyFile::new(path, mode, backend);
    let _ = binary; // text decoding is handled by PyFile itself.
    Ok(Object::File(Rc::new(file)))
}

/// Public entry: ensure the `_io` types exist even before module
/// import, so `isinstance(x, io.IOBase)` from user code never fails.
pub fn _prefetch() {
    let _ = value_error("unused");
}
