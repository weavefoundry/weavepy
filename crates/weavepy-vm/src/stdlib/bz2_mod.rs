//! `_bz2` — bzip2 compress/decompress (RFC 0019).
//!
//! Backed by the `bzip2` crate (statically-linked libbz2). The
//! frozen `bz2.py` builds the `BZ2File` class on top of this.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::io::{Read, Write};

use bzip2::read::BzDecoder;
use bzip2::write::BzEncoder;
use bzip2::Compression;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_bz2"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("bzip2 compress/decompress (RFC 0019 core)."),
        );
        register(&mut d, "compress", b_compress);
        register(&mut d, "decompress", b_decompress);
    }
    Rc::new(PyModule {
        name: "_bz2".to_owned(),
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
    let level = args
        .get(1)
        .and_then(|o| o.as_i64())
        .unwrap_or(9)
        .clamp(1, 9) as u32;
    let mut enc = BzEncoder::new(Vec::new(), Compression::new(level));
    enc.write_all(&data)
        .map_err(|e| value_error(format!("bz2 compress: {e}")))?;
    let bytes = enc
        .finish()
        .map_err(|e| value_error(format!("bz2 compress: {e}")))?;
    Ok(Object::new_bytes(bytes))
}

fn b_decompress(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = args
        .first()
        .and_then(|o| o.as_bytes_view())
        .ok_or_else(|| type_error("decompress requires bytes-like"))?;
    let mut dec = BzDecoder::new(&data[..]);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)
        .map_err(|e| value_error(format!("bz2 decompress: {e}")))?;
    Ok(Object::new_bytes(out))
}
