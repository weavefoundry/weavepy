//! The `io` built-in module.
//!
//! Ships in-memory text and byte streams plus a path-based `open()`
//! that mirrors the top-level builtin. Real file objects live in
//! [`crate::object::PyFile`]; this module just exposes the factory
//! callables that wrap them.

use std::cell::RefCell;
use std::rc::Rc;

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
            "TextIOWrapper",
            "IncrementalNewlineDecoder",
            "UnsupportedOperation",
        ] {
            let cls = make_io_protocol(name);
            d.insert(DictKey(Object::from_static(name)), Object::Type(cls));
        }
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
        call: Box::new(body),
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
