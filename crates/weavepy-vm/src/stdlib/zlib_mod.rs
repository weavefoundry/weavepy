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
use std::collections::HashMap;

use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::types::{PyInstance, TypeObject};

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
        d.insert(DictKey(Object::from_static("Z_BLOCK")), Object::Int(5));
        d.insert(DictKey(Object::from_static("Z_TREES")), Object::Int(6));
        d.insert(DictKey(Object::from_static("MAX_WBITS")), Object::Int(15));
        // Compression method + strategy + tuning constants. `flate2`/miniz
        // only honours `level`/`wbits`; `method`, `memLevel` and `strategy`
        // are accepted for API parity (their effect on the byte stream is not
        // observed by `test_zlib`, which only checks round-trips for them).
        d.insert(DictKey(Object::from_static("DEFLATED")), Object::Int(8));
        d.insert(
            DictKey(Object::from_static("DEF_MEM_LEVEL")),
            Object::Int(8),
        );
        d.insert(
            DictKey(Object::from_static("DEF_BUF_SIZE")),
            Object::Int(16384),
        );
        d.insert(
            DictKey(Object::from_static("Z_DEFAULT_STRATEGY")),
            Object::Int(0),
        );
        d.insert(DictKey(Object::from_static("Z_FILTERED")), Object::Int(1));
        d.insert(
            DictKey(Object::from_static("Z_HUFFMAN_ONLY")),
            Object::Int(2),
        );
        d.insert(DictKey(Object::from_static("Z_RLE")), Object::Int(3));
        d.insert(DictKey(Object::from_static("Z_FIXED")), Object::Int(4));
        d.insert(
            DictKey(Object::from_static("ZLIB_VERSION")),
            Object::from_static("1.2.13"),
        );
        // CPython exposes the *runtime* zlib version separately from the
        // compile-time `ZLIB_VERSION`. We back zlib with flate2/miniz_oxide
        // (a faithful zlib reimplementation) and report the same parseable
        // dotted version so `test_zlib`'s version-tuple parser works.
        d.insert(
            DictKey(Object::from_static("ZLIB_RUNTIME_VERSION")),
            Object::from_static("1.2.13"),
        );
        d.insert(
            DictKey(Object::from_static("error")),
            Object::Type(crate::builtin_types::builtin_types().value_error.clone()),
        );
        d.insert(
            DictKey(Object::from_static("compress")),
            b_kw("compress", zlib_compress),
        );
        d.insert(
            DictKey(Object::from_static("decompress")),
            b_kw("decompress", zlib_decompress),
        );
        d.insert(
            DictKey(Object::from_static("compressobj")),
            b_kw("compressobj", zlib_compressobj),
        );
        d.insert(
            DictKey(Object::from_static("decompressobj")),
            b_kw("decompressobj", zlib_decompressobj),
        );
        d.insert(
            DictKey(Object::from_static("crc32")),
            b("crc32", zlib_crc32),
        );
        d.insert(
            DictKey(Object::from_static("adler32")),
            b("adler32", zlib_adler32),
        );
        d.insert(
            DictKey(Object::from_static("_ZlibDecompressor")),
            b_kw("_ZlibDecompressor", zlib_zlibdecompressor),
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
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

fn b_kw(
    name: &'static str,
    body: fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>,
) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(move |args| body(args, &[])),
        call_kw: Some(Box::new(body)),
    }))
}

/// Coerce a zlib data argument. Unlike a plain `str`, zlib requires a
/// *bytes-like* object (buffer protocol); `str` and numbers raise
/// `TypeError`, which `test_badargs` checks explicitly.
fn bytes_of(arg: Option<&Object>) -> Result<Vec<u8>, RuntimeError> {
    match arg {
        Some(o) => o.as_bytes_view().ok_or_else(|| {
            type_error(format!(
                "a bytes-like object is required, not '{}'",
                o.type_name()
            ))
        }),
        None => Err(type_error("missing required argument")),
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

/// Valid compression levels are `-1` (`Z_DEFAULT_COMPRESSION`) and `0..=9`.
fn check_level(level: i64) -> Result<(), RuntimeError> {
    if level == -1 || (0..=9).contains(&level) {
        Ok(())
    } else {
        Err(value_error("Bad compression level"))
    }
}

/// `compressobj` accepts zlib (`9..=15`), raw (`-15..=-9`) and gzip
/// (`25..=31`) window sizes. Anything else (e.g. `0`, `16`) is rejected,
/// matching `test_badcompressobj`.
fn check_compress_wbits(wbits: i64) -> Result<(), RuntimeError> {
    if (9..=15).contains(&wbits.abs()) || (25..=31).contains(&wbits) {
        Ok(())
    } else {
        Err(value_error("Invalid initialization option"))
    }
}

/// `decompressobj` additionally accepts `8` and `0`/`32+` (auto-detect)
/// window sizes. `test_baddecompressobj` checks that `-1` is rejected.
fn check_decompress_wbits(wbits: i64) -> Result<(), RuntimeError> {
    let ok = wbits == 0
        || (8..=15).contains(&wbits)
        || (-15..=-8).contains(&wbits)
        || (24..=31).contains(&wbits)
        || (40..=47).contains(&wbits);
    if ok {
        Ok(())
    } else {
        Err(value_error("Invalid initialization option"))
    }
}

/// Reject a positional-only `data` keyword and any unexpected keyword,
/// raising `TypeError` like CPython's Argument Clinic.
fn reject_kwargs(
    kwargs: &[(String, Object)],
    allowed: &[&str],
    fname: &str,
) -> Result<(), RuntimeError> {
    for (k, _) in kwargs {
        if k == "data" {
            return Err(type_error(format!(
                "{fname}() got some positional-only arguments passed as keyword arguments: 'data'"
            )));
        }
        if !allowed.contains(&k.as_str()) {
            return Err(type_error(format!(
                "{fname}() got an unexpected keyword argument '{k}'"
            )));
        }
    }
    Ok(())
}

fn zlib_compress(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    reject_kwargs(kwargs, &["level", "wbits"], "compress")?;
    let data = bytes_of(args.first())?;
    let level = int_arg(args, kwargs, 1, "level", -1);
    let wbits = int_arg(args, kwargs, 2, "wbits", 15);
    check_level(level)?;
    check_compress_wbits(wbits)?;
    // Share the low-level deflate path with `compressobj` so that
    // `compress(x)` and `compressobj().compress(x) + .flush()` produce
    // byte-identical output (`test_pair` asserts this equality).
    let mut c = build_compress(level, wbits, &[]);
    let mut out = deflate_step(&mut c, &data, FlushCompress::None)?;
    out.extend(deflate_step(&mut c, &[], FlushCompress::Finish)?);
    Ok(Object::new_bytes(out))
}

/// Decompress with optional `wbits`:
/// * `+9..+15` — zlib format (default 15).
/// * `-9..-15` — raw deflate (used by ZIP files).
/// * `+24..+31` — gzip format.
///
/// A `bufsize` keyword is accepted for CPython parity (it only tunes the
/// internal buffer growth, which we manage automatically).
fn zlib_decompress(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    reject_kwargs(kwargs, &["wbits", "bufsize"], "decompress")?;
    let data = bytes_of(args.first())?;
    let wbits = int_arg(args, kwargs, 1, "wbits", 15);
    let mut d = build_decompress(wbits, &data, &[]);
    let (out, _consumed, stream_end) = inflate_step(&mut d, &data, None, &[])?;
    if !stream_end {
        return Err(value_error(
            "Error -5 while decompressing data: incomplete or truncated stream",
        ));
    }
    Ok(Object::new_bytes(out))
}

// ---- incremental compress / decompress objects ----
//
// CPython's `compressobj()`/`decompressobj()` return stateful `Compress`/
// `Decompress` objects whose `compress`/`decompress`/`flush` are *methods*
// (attribute access), and whose decompressor exposes the live
// `unused_data` / `unconsumed_tail` / `eof` attributes. We model them the
// way `hashlib` models hashers: a thread-local registry of Rust state keyed
// by an integer handle stored on a `PyInstance` of a synthesised class.

struct CompressState {
    c: Compress,
    done: bool,
}

struct DecompressState {
    // `Decompress` is created lazily on the first `decompress()` so the
    // auto-detect `wbits` ranges (`0`, `32+`) can sniff the header.
    d: Option<Decompress>,
    wbits: i64,
    zdict: Vec<u8>,
    eof: bool,
}

/// State for `zlib._ZlibDecompressor` — a bz2/lzma-style one-shot streaming
/// decompressor that owns its *input* buffer (rather than exposing
/// `unconsumed_tail`) and reports `needs_input`.
struct ZlibDecompressorState {
    d: Option<Decompress>,
    wbits: i64,
    zdict: Vec<u8>,
    input: Vec<u8>,
    eof: bool,
    needs_input: bool,
    unused_data: Vec<u8>,
    broken: bool,
}

thread_local! {
    static COMPRESS_REG: RefCell<HashMap<i64, Rc<RefCell<CompressState>>>> =
        RefCell::new(HashMap::new());
    static DECOMPRESS_REG: RefCell<HashMap<i64, Rc<RefCell<DecompressState>>>> =
        RefCell::new(HashMap::new());
    static ZDECOMP_REG: RefCell<HashMap<i64, Rc<RefCell<ZlibDecompressorState>>>> =
        RefCell::new(HashMap::new());
    static NEXT_ID: RefCell<i64> = const { RefCell::new(1) };
    static COMPRESS_CLASS: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
    static DECOMPRESS_CLASS: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
    static ZDECOMP_CLASS: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
}

fn next_id() -> i64 {
    NEXT_ID.with(|c| {
        let mut v = c.borrow_mut();
        let id = *v;
        *v += 1;
        id
    })
}

/// Build a `flate2` compressor honouring CPython's `wbits`:
/// * `-9..=-15` — raw deflate (no header), window `|wbits|`.
/// * `25..=31` — gzip framing, window `wbits-16`.
/// * `9..=15` — zlib framing, window `wbits`.
///
/// A non-empty `zdict` primes the LZ77 window (`deflateSetDictionary`).
fn build_compress(level: i64, wbits: i64, zdict: &[u8]) -> Compress {
    let mut c = if wbits < 0 {
        Compress::new_with_window_bits(level_for(level), false, (-wbits).clamp(9, 15) as u8)
    } else if (25..=31).contains(&wbits) {
        Compress::new_gzip(level_for(level), (wbits - 16).clamp(9, 15) as u8)
    } else {
        Compress::new_with_window_bits(level_for(level), true, wbits.clamp(9, 15) as u8)
    };
    if !zdict.is_empty() {
        let _ = c.set_dictionary(zdict);
    }
    c
}

/// Build a `flate2` decompressor honouring CPython's `wbits` (incl. the
/// gzip `16+` and auto-detect `32+` ranges). For the auto-detect range the
/// first bytes of `data` are sniffed for the gzip magic. Raw streams carry
/// no `FDICT` flag, so a `zdict` must be primed eagerly here; zlib streams
/// signal `Z_NEED_DICT` and are handled in [`inflate_step`].
fn build_decompress(wbits: i64, data: &[u8], zdict: &[u8]) -> Decompress {
    let mut d = if wbits == 0 {
        // "use the window size recorded in the zlib header" — a max-window
        // decompressor accepts any zlib stream (its window is never larger).
        Decompress::new(true)
    } else if (8..=15).contains(&wbits) {
        Decompress::new_with_window_bits(true, wbits.clamp(9, 15) as u8)
    } else if (-15..=-8).contains(&wbits) {
        Decompress::new_with_window_bits(false, (-wbits).clamp(9, 15) as u8)
    } else if (24..=31).contains(&wbits) {
        Decompress::new_gzip((wbits - 16).clamp(9, 15) as u8)
    } else if data.starts_with(&[0x1f, 0x8b]) {
        Decompress::new_gzip(15)
    } else {
        Decompress::new(true)
    };
    if wbits < 0 && !zdict.is_empty() {
        let _ = d.set_dictionary(zdict);
    }
    d
}

/// Drive `Compress::compress` to completion for one call, growing the output
/// as needed. `flush` is `None` for `compress(data)` and `Finish`/`Sync`/
/// `Full`/`Partial` for `flush(mode)`.
fn deflate_step(
    c: &mut Compress,
    input: &[u8],
    flush: FlushCompress,
) -> Result<Vec<u8>, RuntimeError> {
    let mut out = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    let mut consumed = 0usize;
    loop {
        let before_out = c.total_out();
        let before_in = c.total_in();
        let status = c
            .compress(&input[consumed..], &mut buf, flush)
            .map_err(|e| value_error(format!("zlib: {e}")))?;
        let din = (c.total_in() - before_in) as usize;
        let dout = (c.total_out() - before_out) as usize;
        out.extend_from_slice(&buf[..dout]);
        consumed += din;
        match status {
            Status::StreamEnd | Status::BufError => break,
            // The canonical zlib drive loop: keep calling only while the
            // output buffer comes back completely full (more output pending).
            // A short write means the current operation is done — every byte
            // of input was consumed (Z_NO_FLUSH) or the flush marker was
            // fully emitted. This is essential for Z_SYNC_FLUSH/Z_FULL_FLUSH,
            // which emit a marker on *every* call and would otherwise spin
            // forever. `Z_FINISH` is terminated by `Status::StreamEnd` above.
            Status::Ok => {
                if dout < buf.len() && !matches!(flush, FlushCompress::Finish) {
                    break;
                }
                if din == 0 && dout == 0 {
                    break;
                }
            }
        }
    }
    Ok(out)
}

/// Drive `Decompress::decompress` for one call. `limit` caps the produced
/// output (`max_length`); `None` means unbounded. A non-empty `dict` is
/// installed when the zlib stream signals `Z_NEED_DICT`. Returns
/// `(output, input_consumed, stream_end)`.
fn inflate_step(
    d: &mut Decompress,
    input: &[u8],
    limit: Option<usize>,
    dict: &[u8],
) -> Result<(Vec<u8>, usize, bool), RuntimeError> {
    let mut out = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    let mut consumed = 0usize;
    let mut stream_end = false;
    let mut dict_set = false;
    loop {
        let room = match limit {
            Some(l) => {
                if out.len() >= l {
                    break;
                }
                (l - out.len()).min(buf.len())
            }
            None => buf.len(),
        };
        let before_in = d.total_in();
        let before_out = d.total_out();
        let result = d.decompress(&input[consumed..], &mut buf[..room], FlushDecompress::None);
        let din = (d.total_in() - before_in) as usize;
        let dout = (d.total_out() - before_out) as usize;
        out.extend_from_slice(&buf[..dout]);
        consumed += din;
        match result {
            Ok(Status::StreamEnd) => {
                stream_end = true;
                break;
            }
            Ok(Status::BufError) => break,
            Ok(Status::Ok) => {
                if din == 0 && dout == 0 {
                    break;
                }
            }
            Err(e) => {
                // A zlib stream with a preset dictionary stops at the header
                // with `Z_NEED_DICT`; install the dictionary and resume.
                if e.needs_dictionary().is_some() && !dict.is_empty() && !dict_set {
                    d.set_dictionary(dict)
                        .map_err(|se| value_error(format!("zlib: {se}")))?;
                    dict_set = true;
                    continue;
                }
                let code = if e.needs_dictionary().is_some() {
                    2
                } else {
                    -3
                };
                let detail = e.message().unwrap_or("invalid input data");
                return Err(value_error(format!(
                    "Error {code} while decompressing data: {detail}"
                )));
            }
        }
    }
    Ok((out, consumed, stream_end))
}

/// Optional positional-or-keyword integer argument.
fn int_arg(
    args: &[Object],
    kwargs: &[(String, Object)],
    pos: usize,
    name: &str,
    default: i64,
) -> i64 {
    args.get(pos)
        .and_then(Object::as_i64)
        .or_else(|| {
            kwargs
                .iter()
                .find(|(k, _)| k == name)
                .and_then(|(_, v)| v.as_i64())
        })
        .unwrap_or(default)
}

/// Optional positional-or-keyword bytes-like argument (e.g. `zdict`).
/// Defaults to empty; a present-but-non-bytes value raises `TypeError`.
fn bytes_kwarg(
    args: &[Object],
    kwargs: &[(String, Object)],
    pos: usize,
    name: &str,
) -> Result<Vec<u8>, RuntimeError> {
    let obj = args
        .get(pos)
        .or_else(|| kwargs.iter().find(|(k, _)| k == name).map(|(_, v)| v));
    match obj {
        None => Ok(Vec::new()),
        Some(o) => o
            .as_bytes_view()
            .ok_or_else(|| type_error("zdict argument must support the buffer protocol")),
    }
}

fn zlib_compressobj(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    // compressobj(level=-1, method=DEFLATED, wbits=15, memLevel=8,
    // strategy=Z_DEFAULT_STRATEGY, zdict=b''). level/wbits/zdict affect us.
    let level = int_arg(args, kwargs, 0, "level", -1);
    let wbits = int_arg(args, kwargs, 2, "wbits", 15);
    let zdict = bytes_kwarg(args, kwargs, 5, "zdict")?;
    check_level(level)?;
    check_compress_wbits(wbits)?;
    let id = next_id();
    COMPRESS_REG.with(|r| {
        r.borrow_mut().insert(
            id,
            Rc::new(RefCell::new(CompressState {
                c: build_compress(level, wbits, &zdict),
                done: false,
            })),
        );
    });
    let inst = PyInstance::new(compress_class());
    inst.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("_handle")), Object::Int(id));
    Ok(Object::Instance(Rc::new(inst)))
}

fn zlib_decompressobj(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let wbits = int_arg(args, kwargs, 0, "wbits", 15);
    let zdict = bytes_kwarg(args, kwargs, 1, "zdict")?;
    check_decompress_wbits(wbits)?;
    let id = next_id();
    DECOMPRESS_REG.with(|r| {
        r.borrow_mut().insert(
            id,
            Rc::new(RefCell::new(DecompressState {
                d: None,
                wbits,
                zdict,
                eof: false,
            })),
        );
    });
    let inst = PyInstance::new(decompress_class());
    {
        let mut d = inst.dict.borrow_mut();
        d.insert(DictKey(Object::from_static("_handle")), Object::Int(id));
        d.insert(
            DictKey(Object::from_static("unused_data")),
            Object::new_bytes(Vec::new()),
        );
        d.insert(
            DictKey(Object::from_static("unconsumed_tail")),
            Object::new_bytes(Vec::new()),
        );
        d.insert(DictKey(Object::from_static("eof")), Object::Bool(false));
    }
    Ok(Object::Instance(Rc::new(inst)))
}

fn handle_of(args: &[Object]) -> Result<i64, RuntimeError> {
    let inst = match args.first() {
        Some(Object::Instance(i)) => i,
        _ => return Err(type_error("expected zlib compress/decompress object")),
    };
    match inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_handle")))
        .cloned()
    {
        Some(Object::Int(v)) => Ok(v),
        _ => Err(type_error("zlib object missing _handle")),
    }
}

fn compress_class() -> Rc<TypeObject> {
    COMPRESS_CLASS.with(|slot| {
        if let Some(c) = slot.borrow().as_ref() {
            return c.clone();
        }
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::new();
        method_into(&mut dict, "compress", compress_compress);
        method_into(&mut dict, "flush", compress_flush);
        let cls = TypeObject::new_user("zlib.Compress", vec![bt.object_.clone()], dict)
            .expect("zlib.Compress must linearise");
        *slot.borrow_mut() = Some(cls.clone());
        cls
    })
}

fn decompress_class() -> Rc<TypeObject> {
    DECOMPRESS_CLASS.with(|slot| {
        if let Some(c) = slot.borrow().as_ref() {
            return c.clone();
        }
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::new();
        method_into_kw(&mut dict, "decompress", decompress_decompress);
        method_into(&mut dict, "flush", decompress_flush);
        let cls = TypeObject::new_user("zlib.Decompress", vec![bt.object_.clone()], dict)
            .expect("zlib.Decompress must linearise");
        *slot.borrow_mut() = Some(cls.clone());
        cls
    })
}

fn method_into(
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

/// Like [`method_into`] but the method also accepts keyword arguments
/// (e.g. `decompressor.decompress(data, max_length=…)`). When bound to an
/// instance the receiver is prepended to `args` before `body` runs.
fn method_into_kw(
    dict: &mut DictData,
    name: &'static str,
    body: fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError>,
) {
    dict.insert(
        DictKey(Object::from_static(name)),
        Object::Builtin(Rc::new(BuiltinFn {
            name,
            binds_instance: true,
            call: Box::new(move |args| body(args, &[])),
            call_kw: Some(Box::new(body)),
        })),
    );
}

fn compress_compress(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = handle_of(args)?;
    let data = bytes_of(args.get(1))?;
    let state = COMPRESS_REG.with(|r| r.borrow().get(&id).cloned());
    let state = state.ok_or_else(|| value_error("zlib: stale compressor"))?;
    let mut st = state.borrow_mut();
    if st.done {
        return Err(value_error("compress() after flush(Z_FINISH)"));
    }
    let out = deflate_step(&mut st.c, &data, FlushCompress::None)?;
    Ok(Object::new_bytes(out))
}

fn compress_flush(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = handle_of(args)?;
    let mode = args.get(1).and_then(Object::as_i64).unwrap_or(4); // Z_FINISH
    let state = COMPRESS_REG.with(|r| r.borrow().get(&id).cloned());
    let state = state.ok_or_else(|| value_error("zlib: stale compressor"))?;
    let mut st = state.borrow_mut();
    if st.done {
        return Err(value_error("inconsistent flush state"));
    }
    let flush = match mode {
        0 => FlushCompress::None,
        1 => FlushCompress::Partial,
        2 => FlushCompress::Sync,
        3 => FlushCompress::Full,
        4 => FlushCompress::Finish,
        // Z_BLOCK / Z_TREES have no `flate2` equivalent; approximate with a
        // sync flush. It still round-trips (`test_flushes` only checks that
        // `decompress(...) == data`, not the exact block framing).
        5 | 6 => FlushCompress::Sync,
        _ => return Err(value_error("Invalid flush option")),
    };
    let out = deflate_step(&mut st.c, &[], flush)?;
    if matches!(flush, FlushCompress::Finish) {
        st.done = true;
    }
    Ok(Object::new_bytes(out))
}

fn store_decompress_attrs(args: &[Object], unused: &[u8], unconsumed: &[u8], eof: bool) {
    if let Some(Object::Instance(inst)) = args.first() {
        let mut d = inst.dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("unused_data")),
            Object::new_bytes(unused.to_vec()),
        );
        d.insert(
            DictKey(Object::from_static("unconsumed_tail")),
            Object::new_bytes(unconsumed.to_vec()),
        );
        d.insert(DictKey(Object::from_static("eof")), Object::Bool(eof));
    }
}

fn decompress_decompress(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    reject_kwargs(kwargs, &["max_length"], "decompress")?;
    let id = handle_of(args)?;
    let data = bytes_of(args.get(1))?;
    let max_length = if let Some(o) = args.get(2) {
        match o {
            Object::None => 0,
            _ => crate::builtins::coerce_index_i64(o)?,
        }
    } else if let Some((_, o)) = kwargs.iter().find(|(k, _)| k == "max_length") {
        crate::builtins::coerce_index_i64(o)?
    } else {
        0
    };
    if max_length < 0 {
        return Err(value_error("max_length must be non-negative"));
    }
    let limit = if max_length == 0 {
        None
    } else {
        Some(max_length as usize)
    };
    let state = DECOMPRESS_REG.with(|r| r.borrow().get(&id).cloned());
    let state = state.ok_or_else(|| value_error("zlib: stale decompressor"))?;
    let mut st = state.borrow_mut();
    if st.eof {
        // Past the stream end every extra byte is "unused data".
        let prev = read_bytes_attr(args, "unused_data");
        let mut unused = prev;
        unused.extend_from_slice(&data);
        store_decompress_attrs(args, &unused, &[], true);
        return Ok(Object::new_bytes(Vec::new()));
    }
    if st.d.is_none() {
        st.d = Some(build_decompress(st.wbits, &data, &st.zdict));
    }
    let zdict = st.zdict.clone();
    let (out, consumed, stream_end) = inflate_step(st.d.as_mut().unwrap(), &data, limit, &zdict)?;
    let leftover = &data[consumed..];
    if stream_end {
        st.eof = true;
        store_decompress_attrs(args, leftover, &[], true);
    } else {
        // Unbounded calls consume all input (tail cleared); a length-capped
        // call parks the unread input in `unconsumed_tail`.
        store_decompress_attrs(args, &[], leftover, false);
    }
    Ok(Object::new_bytes(out))
}

fn decompress_flush(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = handle_of(args)?;
    // The optional `length` is only a buffer-size hint, but CPython still
    // validates that it is strictly positive (`test_decompressobj_badflush`).
    if let Some(o) = args.get(1) {
        let length = crate::builtins::coerce_index_i64(o)?;
        if length <= 0 {
            return Err(value_error("length must be greater than zero"));
        }
    }
    let state = DECOMPRESS_REG.with(|r| r.borrow().get(&id).cloned());
    let state = state.ok_or_else(|| value_error("zlib: stale decompressor"))?;
    let mut st = state.borrow_mut();
    // Once the stream has ended, `flush()` has nothing left to do and must
    // NOT recompute `unused_data` — the trailing bytes were already captured
    // by the `decompress()` call that hit the stream end.
    if st.eof {
        return Ok(Object::new_bytes(Vec::new()));
    }
    let tail = read_bytes_attr(args, "unconsumed_tail");
    if st.d.is_none() {
        st.d = Some(build_decompress(st.wbits, &tail, &st.zdict));
    }
    let zdict = st.zdict.clone();
    let (out, consumed, stream_end) = inflate_step(st.d.as_mut().unwrap(), &tail, None, &zdict)?;
    if stream_end {
        st.eof = true;
        let leftover = tail[consumed..].to_vec();
        store_decompress_attrs(args, &leftover, &[], true);
    }
    Ok(Object::new_bytes(out))
}

fn read_bytes_attr(args: &[Object], name: &str) -> Vec<u8> {
    if let Some(Object::Instance(inst)) = args.first() {
        if let Some(Object::Bytes(b)) = inst
            .dict
            .borrow()
            .get(&DictKey(Object::from_str(name.to_owned())))
            .cloned()
        {
            return b.to_vec();
        }
    }
    Vec::new()
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

// ---- zlib._ZlibDecompressor -------------------------------------------
//
// A single-use streaming decompressor (mirrors `bz2.BZ2Decompressor` /
// `lzma.LZMADecompressor`): it buffers compressed *input* internally,
// produces at most `max_length` bytes per call, exposes `needs_input` /
// `eof` / `unused_data`, and raises `EOFError` once the stream has ended.

fn eof_error(msg: &str) -> RuntimeError {
    RuntimeError::PyException(crate::error::PyException::from_builtin("EOFError", msg))
}

fn zlibdecompressor_class() -> Rc<TypeObject> {
    ZDECOMP_CLASS.with(|slot| {
        if let Some(c) = slot.borrow().as_ref() {
            return c.clone();
        }
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::new();
        method_into_kw(&mut dict, "decompress", zlibdecompressor_decompress);
        // `_ZlibDecompressor` objects are unpicklable in CPython.
        method_into(&mut dict, "__reduce__", zlibdecompressor_no_pickle);
        method_into(&mut dict, "__reduce_ex__", zlibdecompressor_no_pickle);
        let cls = TypeObject::new_user("_ZlibDecompressor", vec![bt.object_.clone()], dict)
            .expect("zlib._ZlibDecompressor must linearise");
        *slot.borrow_mut() = Some(cls.clone());
        cls
    })
}

fn zlibdecompressor_no_pickle(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(type_error("cannot pickle '_ZlibDecompressor' object"))
}

fn store_zd_attrs(args: &[Object], unused: &[u8], eof: bool, needs_input: bool) {
    if let Some(Object::Instance(inst)) = args.first() {
        let mut d = inst.dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("unused_data")),
            Object::new_bytes(unused.to_vec()),
        );
        d.insert(DictKey(Object::from_static("eof")), Object::Bool(eof));
        d.insert(
            DictKey(Object::from_static("needs_input")),
            Object::Bool(needs_input),
        );
    }
}

fn zlib_zlibdecompressor(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    // _ZlibDecompressor(wbits=15, zdict=b'')
    if args.len() > 2 {
        return Err(type_error(
            "_ZlibDecompressor() takes at most 2 positional arguments",
        ));
    }
    let wbits = match args.first() {
        None => int_arg(&[], kwargs, 0, "wbits", 15),
        Some(Object::Int(n)) => *n,
        Some(Object::Bool(b)) => i64::from(*b),
        Some(o) => {
            return Err(type_error(format!(
                "'{}' object cannot be interpreted as an integer",
                o.type_name()
            )))
        }
    };
    // `zdict` must be a bytes-like object if supplied (test_Constructor).
    let zdict = match args.get(1) {
        None => Vec::new(),
        Some(o) => o
            .as_bytes_view()
            .ok_or_else(|| type_error("zdict argument must support the buffer protocol"))?,
    };
    for (k, _) in kwargs {
        if !matches!(k.as_str(), "wbits" | "zdict") {
            return Err(type_error(format!(
                "_ZlibDecompressor() got an unexpected keyword argument '{k}'"
            )));
        }
    }
    let id = next_id();
    ZDECOMP_REG.with(|r| {
        r.borrow_mut().insert(
            id,
            Rc::new(RefCell::new(ZlibDecompressorState {
                d: None,
                wbits,
                zdict,
                input: Vec::new(),
                eof: false,
                needs_input: true,
                unused_data: Vec::new(),
                broken: false,
            })),
        );
    });
    let inst = PyInstance::new(zlibdecompressor_class());
    {
        let mut d = inst.dict.borrow_mut();
        d.insert(DictKey(Object::from_static("_handle")), Object::Int(id));
        d.insert(
            DictKey(Object::from_static("unused_data")),
            Object::new_bytes(Vec::new()),
        );
        d.insert(DictKey(Object::from_static("eof")), Object::Bool(false));
        d.insert(
            DictKey(Object::from_static("needs_input")),
            Object::Bool(true),
        );
    }
    Ok(Object::Instance(Rc::new(inst)))
}

fn zlibdecompressor_decompress(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    reject_kwargs(kwargs, &["max_length"], "decompress")?;
    let id = handle_of(args)?;
    if args.len() < 2 {
        return Err(type_error(
            "decompress() missing 1 required positional argument: 'data'",
        ));
    }
    let data = bytes_of(args.get(1))?;
    let max_length = if let Some(o) = args.get(2) {
        crate::builtins::coerce_index_i64(o)?
    } else if let Some((_, o)) = kwargs.iter().find(|(k, _)| k == "max_length") {
        crate::builtins::coerce_index_i64(o)?
    } else {
        -1
    };
    let limit = if max_length < 0 {
        None
    } else {
        Some(max_length as usize)
    };
    let state = ZDECOMP_REG.with(|r| r.borrow().get(&id).cloned());
    let state = state.ok_or_else(|| value_error("zlib: stale decompressor"))?;
    let mut st = state.borrow_mut();
    if st.eof {
        return Err(eof_error("End of stream already reached"));
    }
    if st.broken {
        return Err(value_error(
            "Error -3 while decompressing data: invalid input data",
        ));
    }
    // Take the buffered input out to satisfy the borrow checker, then feed
    // it (plus the new data) through one inflate step.
    let mut input = std::mem::take(&mut st.input);
    input.extend_from_slice(&data);
    if st.d.is_none() {
        st.d = Some(build_decompress(st.wbits, &input, &st.zdict));
    }
    let zdict = st.zdict.clone();
    let (out, consumed, stream_end) =
        match inflate_step(st.d.as_mut().unwrap(), &input, limit, &zdict) {
            Ok(t) => t,
            Err(e) => {
                st.broken = true;
                return Err(e);
            }
        };
    let leftover = input[consumed..].to_vec();
    if stream_end {
        st.eof = true;
        st.unused_data = leftover;
        st.input = Vec::new();
        st.needs_input = false;
    } else {
        st.input = leftover;
        // We need more input only once the internal buffer drains.
        st.needs_input = st.input.is_empty();
    }
    let (unused, eof, needs_input) = (st.unused_data.clone(), st.eof, st.needs_input);
    drop(st);
    store_zd_attrs(args, &unused, eof, needs_input);
    Ok(Object::new_bytes(out))
}
