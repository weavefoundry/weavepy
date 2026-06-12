//! `_lzma` — XZ/LZMA compress/decompress (RFC 0019).
//!
//! Backed by the `xz2` crate (libxz binding via xz2-sys, statically
//! built where possible). The frozen `lzma.py` builds the
//! `LZMAFile` class on top of this.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::io::{Read, Write};

use xz2::read::XzDecoder;
use xz2::write::XzEncoder;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub const FORMAT_AUTO: i64 = 0;
pub const FORMAT_XZ: i64 = 1;
pub const FORMAT_ALONE: i64 = 2;
pub const FORMAT_RAW: i64 = 3;

pub const CHECK_NONE: i64 = 0;
pub const CHECK_CRC32: i64 = 1;
pub const CHECK_CRC64: i64 = 4;
pub const CHECK_SHA256: i64 = 10;

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_lzma"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("XZ/LZMA compress/decompress (RFC 0019 core)."),
        );
        register(&mut d, "compress", b_compress);
        register(&mut d, "decompress", b_decompress);
        d.insert(
            DictKey(Object::from_static("FORMAT_AUTO")),
            Object::Int(FORMAT_AUTO),
        );
        d.insert(
            DictKey(Object::from_static("FORMAT_XZ")),
            Object::Int(FORMAT_XZ),
        );
        d.insert(
            DictKey(Object::from_static("FORMAT_ALONE")),
            Object::Int(FORMAT_ALONE),
        );
        d.insert(
            DictKey(Object::from_static("FORMAT_RAW")),
            Object::Int(FORMAT_RAW),
        );
        d.insert(
            DictKey(Object::from_static("CHECK_NONE")),
            Object::Int(CHECK_NONE),
        );
        d.insert(
            DictKey(Object::from_static("CHECK_CRC32")),
            Object::Int(CHECK_CRC32),
        );
        d.insert(
            DictKey(Object::from_static("CHECK_CRC64")),
            Object::Int(CHECK_CRC64),
        );
        d.insert(
            DictKey(Object::from_static("CHECK_SHA256")),
            Object::Int(CHECK_SHA256),
        );
        d.insert(
            DictKey(Object::from_static("PRESET_DEFAULT")),
            Object::Int(6),
        );
        d.insert(
            DictKey(Object::from_static("PRESET_EXTREME")),
            Object::Int(i64::from(0x8000_0000_u32) | 6),
        );
    }
    Rc::new(PyModule {
        name: "_lzma".to_owned(),
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
        binds_instance: false,
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
    let preset = args
        .get(1)
        .and_then(|o| o.as_i64())
        .unwrap_or(6)
        .clamp(0, 9) as u32;
    let mut enc = XzEncoder::new(Vec::new(), preset);
    enc.write_all(&data)
        .map_err(|e| value_error(format!("lzma compress: {e}")))?;
    let bytes = enc
        .finish()
        .map_err(|e| value_error(format!("lzma compress: {e}")))?;
    Ok(Object::new_bytes(bytes))
}

fn b_decompress(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = args
        .first()
        .and_then(|o| o.as_bytes_view())
        .ok_or_else(|| type_error("decompress requires bytes-like"))?;
    let mut dec = XzDecoder::new(&data[..]);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)
        .map_err(|e| value_error(format!("lzma decompress: {e}")))?;
    Ok(Object::new_bytes(out))
}
