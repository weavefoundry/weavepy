//! `_gzip` — gzip-format compress/decompress (RFC 0019).
//!
//! Backed by `flate2`'s gzip codec. The frozen `gzip.py` wrapper
//! builds the user-facing `GzipFile` class on top of this surface.
//!
//! Surface:
//! * `compress(data, compresslevel=9)` — gzip the bytes.
//! * `decompress(data)` — gunzip the bytes.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::io::{Read, Write};

use flate2::read::{GzDecoder, MultiGzDecoder};
use flate2::write::GzEncoder;
use flate2::Compression;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_gzip"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("gzip-format compress/decompress (RFC 0019 core)."),
        );
        register(&mut d, "compress", b_compress);
        register(&mut d, "decompress", b_decompress);
    }
    Rc::new(PyModule {
        name: "_gzip".to_owned(),
        filename: None,
        dict,
    })
}

fn register(
    d: &mut DictData,
    name: &'static str,
    body: impl Fn(&[Object]) -> Result<Object, RuntimeError> + Send + Sync + 'static,
) {
    let bf = BuiltinFn {
        name,
        call: Box::new(body),
        call_kw: None,
    };
    d.insert(
        DictKey(Object::from_static(name)),
        Object::Builtin(Rc::new(bf)),
    );
}

fn b_compress(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = args
        .first()
        .and_then(|o| o.as_bytes_view())
        .ok_or_else(|| type_error("compress requires bytes-like"))?;
    let level = args
        .get(1)
        .and_then(|o| o.as_i64())
        .unwrap_or(9)
        .clamp(0, 9) as u32;
    let mut enc = GzEncoder::new(Vec::new(), Compression::new(level));
    enc.write_all(&data)
        .map_err(|e| value_error(format!("gzip compress: {e}")))?;
    let bytes = enc
        .finish()
        .map_err(|e| value_error(format!("gzip compress: {e}")))?;
    Ok(Object::new_bytes(bytes))
}

fn b_decompress(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = args
        .first()
        .and_then(|o| o.as_bytes_view())
        .ok_or_else(|| type_error("decompress requires bytes-like"))?;
    let mut dec = MultiGzDecoder::new(&data[..]);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)
        .map_err(|e| value_error(format!("gzip decompress: {e}")))?;
    Ok(Object::new_bytes(out))
}

/// Helper used by other parts of the stdlib (e.g. zipfile).
pub fn gzip_compress(data: &[u8], level: u32) -> Result<Vec<u8>, RuntimeError> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::new(level.min(9)));
    enc.write_all(data)
        .map_err(|e| value_error(format!("gzip compress: {e}")))?;
    enc.finish()
        .map_err(|e| value_error(format!("gzip compress: {e}")))
}

pub fn gzip_decompress(data: &[u8]) -> Result<Vec<u8>, RuntimeError> {
    let mut dec = GzDecoder::new(data);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)
        .map_err(|e| value_error(format!("gzip decompress: {e}")))?;
    Ok(out)
}
