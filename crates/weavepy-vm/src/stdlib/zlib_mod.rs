//! The `zlib` built-in module.
//!
//! Compression / decompression backed by `flate2` (pure-Rust
//! `miniz_oxide` backend). Surface matches CPython's:
//!
//! * `compress(data, level=-1)` / `decompress(data, wbits=15)`
//! * `compressobj(level=-1, ...)` / `decompressobj(wbits=15)` —
//!   incremental stateful API.
//! * `crc32(data, value=0)` — same polynomial as `binascii.crc32`.
//! * `adler32(data, value=1)`.
//! * Constants: `Z_BEST_SPEED`, `Z_BEST_COMPRESSION`,
//!   `Z_DEFAULT_COMPRESSION`, `Z_NO_FLUSH`, `Z_PARTIAL_FLUSH`,
//!   `Z_SYNC_FLUSH`, `Z_FULL_FLUSH`, `Z_FINISH`.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::io::Write;

use flate2::write::{DeflateDecoder, GzDecoder, ZlibDecoder, ZlibEncoder};
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
            Object::from_static("zlib"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("DEFLATE compression and decompression."),
        );
        d.insert(DictKey(Object::from_static("Z_BEST_SPEED")), Object::Int(1));
        d.insert(
            DictKey(Object::from_static("Z_BEST_COMPRESSION")),
            Object::Int(9),
        );
        d.insert(
            DictKey(Object::from_static("Z_DEFAULT_COMPRESSION")),
            Object::Int(-1),
        );
        d.insert(DictKey(Object::from_static("Z_NO_FLUSH")), Object::Int(0));
        d.insert(
            DictKey(Object::from_static("Z_PARTIAL_FLUSH")),
            Object::Int(1),
        );
        d.insert(DictKey(Object::from_static("Z_SYNC_FLUSH")), Object::Int(2));
        d.insert(DictKey(Object::from_static("Z_FULL_FLUSH")), Object::Int(3));
        d.insert(DictKey(Object::from_static("Z_FINISH")), Object::Int(4));
        d.insert(DictKey(Object::from_static("MAX_WBITS")), Object::Int(15));
        d.insert(
            DictKey(Object::from_static("ZLIB_VERSION")),
            Object::from_static("1.2.13 (flate2/miniz_oxide)"),
        );
        d.insert(
            DictKey(Object::from_static("error")),
            Object::Type(crate::builtin_types::builtin_types().value_error.clone()),
        );
        d.insert(
            DictKey(Object::from_static("compress")),
            b("compress", zlib_compress),
        );
        d.insert(
            DictKey(Object::from_static("decompress")),
            b("decompress", zlib_decompress),
        );
        d.insert(
            DictKey(Object::from_static("compressobj")),
            b("compressobj", zlib_compressobj),
        );
        d.insert(
            DictKey(Object::from_static("decompressobj")),
            b("decompressobj", zlib_decompressobj),
        );
        d.insert(
            DictKey(Object::from_static("crc32")),
            b("crc32", zlib_crc32),
        );
        d.insert(
            DictKey(Object::from_static("adler32")),
            b("adler32", zlib_adler32),
        );
    }
    Rc::new(PyModule {
        name: "zlib".to_owned(),
        filename: None,
        dict,
    })
}

fn b(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(body),
    }))
}

fn bytes_of(arg: Option<&Object>) -> Result<Vec<u8>, RuntimeError> {
    match arg {
        Some(Object::Bytes(b)) => Ok(b.to_vec()),
        Some(Object::ByteArray(b)) => Ok(b.borrow().clone()),
        Some(Object::Str(s)) => Ok(s.as_bytes().to_vec()),
        _ => Err(type_error("expected bytes-like object")),
    }
}

fn level_for(value: i64) -> Compression {
    match value {
        0 => Compression::none(),
        9 => Compression::best(),
        n if n > 0 && n <= 9 => Compression::new(n as u32),
        _ => Compression::default(),
    }
}

fn zlib_compress(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = bytes_of(args.first())?;
    let level = match args.get(1) {
        Some(Object::Int(n)) => *n,
        _ => -1,
    };
    // CPython accepts an optional wbits argument too, but only via
    // `compressobj`; the top-level `compress` always emits zlib.
    let mut encoder = ZlibEncoder::new(Vec::new(), level_for(level));
    encoder
        .write_all(&data)
        .map_err(|e| value_error(e.to_string()))?;
    let compressed = encoder.finish().map_err(|e| value_error(e.to_string()))?;
    Ok(Object::new_bytes(compressed))
}

/// Decompress with optional `wbits`:
/// * `+9..+15` — zlib format (default 15).
/// * `-9..-15` — raw deflate (used by ZIP files).
/// * `+25..+31` — gzip format.
fn zlib_decompress(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = bytes_of(args.first())?;
    let wbits = match args.get(1) {
        Some(Object::Int(n)) => *n,
        _ => 15,
    };
    let plain = if (-15..=-8).contains(&wbits) {
        let mut decoder = DeflateDecoder::new(Vec::new());
        decoder
            .write_all(&data)
            .map_err(|e| value_error(e.to_string()))?;
        decoder.finish().map_err(|e| value_error(e.to_string()))?
    } else if (24..=31).contains(&wbits) {
        let mut decoder = GzDecoder::new(Vec::new());
        decoder
            .write_all(&data)
            .map_err(|e| value_error(e.to_string()))?;
        decoder.finish().map_err(|e| value_error(e.to_string()))?
    } else {
        let mut decoder = ZlibDecoder::new(Vec::new());
        decoder
            .write_all(&data)
            .map_err(|e| value_error(e.to_string()))?;
        decoder.finish().map_err(|e| value_error(e.to_string()))?
    };
    Ok(Object::new_bytes(plain))
}

fn zlib_compressobj(args: &[Object]) -> Result<Object, RuntimeError> {
    let level = match args.first() {
        Some(Object::Int(n)) => *n,
        _ => -1,
    };
    let enc = Rc::new(RefCell::new(Some(ZlibEncoder::new(
        Vec::new(),
        level_for(level),
    ))));
    let dict = Rc::new(RefCell::new(DictData::new()));
    let enc_for_write = enc.clone();
    let compress = move |a: &[Object]| -> Result<Object, RuntimeError> {
        let data = bytes_of(a.first())?;
        let mut slot = enc_for_write.borrow_mut();
        let e = slot
            .as_mut()
            .ok_or_else(|| value_error("compressor closed"))?;
        e.write_all(&data)
            .map_err(|err| value_error(err.to_string()))?;
        Ok(Object::new_bytes(Vec::new()))
    };
    let enc_for_flush = enc;
    let flush = move |_a: &[Object]| -> Result<Object, RuntimeError> {
        let mut slot = enc_for_flush.borrow_mut();
        match slot.take() {
            Some(e) => {
                let bytes = e.finish().map_err(|err| value_error(err.to_string()))?;
                Ok(Object::new_bytes(bytes))
            }
            None => Ok(Object::new_bytes(Vec::new())),
        }
    };
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("compress")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "compress",
                call: Box::new(compress),
            })),
        );
        d.insert(
            DictKey(Object::from_static("flush")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "flush",
                call: Box::new(flush),
            })),
        );
    }
    Ok(Object::Dict(dict))
}

fn zlib_decompressobj(_args: &[Object]) -> Result<Object, RuntimeError> {
    let buf: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
    let dict = Rc::new(RefCell::new(DictData::new()));
    let buf_for_w = buf.clone();
    let decompress = move |a: &[Object]| -> Result<Object, RuntimeError> {
        let data = bytes_of(a.first())?;
        buf_for_w.borrow_mut().extend_from_slice(&data);
        Ok(Object::new_bytes(Vec::new()))
    };
    let buf_for_f = buf;
    let flush = move |_a: &[Object]| -> Result<Object, RuntimeError> {
        let bytes = std::mem::take(&mut *buf_for_f.borrow_mut());
        let mut decoder = ZlibDecoder::new(Vec::new());
        decoder
            .write_all(&bytes)
            .map_err(|e| value_error(e.to_string()))?;
        Ok(Object::new_bytes(
            decoder.finish().map_err(|e| value_error(e.to_string()))?,
        ))
    };
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("decompress")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "decompress",
                call: Box::new(decompress),
            })),
        );
        d.insert(
            DictKey(Object::from_static("flush")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "flush",
                call: Box::new(flush),
            })),
        );
    }
    Ok(Object::Dict(dict))
}

fn zlib_crc32(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = bytes_of(args.first())?;
    let init = match args.get(1) {
        Some(Object::Int(n)) => *n as u32,
        _ => 0,
    };
    let mut hasher = crc32fast::Hasher::new_with_initial(init);
    hasher.update(&data);
    Ok(Object::Int(i64::from(hasher.finalize())))
}

fn zlib_adler32(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = bytes_of(args.first())?;
    let init = match args.get(1) {
        Some(Object::Int(n)) => *n as u32,
        _ => 1,
    };
    // Classic Adler-32 from RFC 1950.
    let mut a = init & 0xFFFF;
    let mut b = (init >> 16) & 0xFFFF;
    const MOD_ADLER: u32 = 65521;
    for &byte in &data {
        a = (a + u32::from(byte)) % MOD_ADLER;
        b = (b + a) % MOD_ADLER;
    }
    Ok(Object::Int(i64::from((b << 16) | a)))
}
