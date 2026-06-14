//! `_lzma` — XZ/LZMA compress/decompress (RFC 0019).
//!
//! Backed by the `xz2` crate (libxz binding via xz2-sys, statically
//! built where possible). The frozen `lzma.py` builds the
//! `LZMAFile` class on top of this.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::raw::c_void;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};

use xz2::read::XzDecoder;
use xz2::stream::{Action, Check, Status, Stream};
use xz2::write::XzEncoder;

use crate::error::{type_error, value_error, PyException, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::types::{PyInstance, TypeFlags, TypeObject};

pub const FORMAT_AUTO: i64 = 0;
pub const FORMAT_XZ: i64 = 1;
pub const FORMAT_ALONE: i64 = 2;
pub const FORMAT_RAW: i64 = 3;

pub const CHECK_NONE: i64 = 0;
pub const CHECK_CRC32: i64 = 1;
pub const CHECK_CRC64: i64 = 4;
pub const CHECK_SHA256: i64 = 10;

// Filter IDs (liblzma `LZMA_FILTER_*`), matching CPython's `lzma.FILTER_*`.
pub const FILTER_LZMA1: i64 = 0x4000_0000_0000_0001;
pub const FILTER_LZMA2: i64 = 0x21;
pub const FILTER_DELTA: i64 = 0x03;
pub const FILTER_X86: i64 = 0x04;
pub const FILTER_POWERPC: i64 = 0x05;
pub const FILTER_IA64: i64 = 0x06;
pub const FILTER_ARM: i64 = 0x07;
pub const FILTER_ARMTHUMB: i64 = 0x08;
pub const FILTER_SPARC: i64 = 0x09;

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
        d.insert(
            DictKey(Object::from_static("LZMACompressor")),
            Object::Type(compressor_class()),
        );
        d.insert(
            DictKey(Object::from_static("LZMADecompressor")),
            Object::Type(decompressor_class()),
        );
        d.insert(
            DictKey(Object::from_static("LZMAError")),
            Object::Type(lzma_error_class()),
        );
        register(&mut d, "is_check_supported", l_is_check_supported);
        register(
            &mut d,
            "_encode_filter_properties",
            b_encode_filter_properties,
        );
        register(
            &mut d,
            "_decode_filter_properties",
            b_decode_filter_properties,
        );
    }
    Rc::new(PyModule {
        name: "_lzma".to_owned(),
        filename: None,
        dict,
    })
}

fn l_is_check_supported(args: &[Object]) -> Result<Object, RuntimeError> {
    let check = args.first().and_then(Object::as_i64).unwrap_or(-1);
    // xz2 always builds with NONE/CRC32/CRC64/SHA256.
    Ok(Object::Bool(matches!(
        check,
        CHECK_NONE | CHECK_CRC32 | CHECK_CRC64 | CHECK_SHA256
    )))
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

// ---------------------------------------------------------------------------
// FORMAT_RAW filter chains via direct liblzma FFI.
//
// xz2's high-level `Stream` covers the XZ/ALONE container formats but exposes
// no raw encoder/decoder and no delta filter. liblzma itself (linked through
// `lzma-sys`) provides the full surface: `lzma_raw_{encoder,decoder}`,
// `lzma_stream_encoder`, `lzma_alone_encoder`, and the
// `lzma_properties_{size,encode,decode}` filter-property codec. We drive those
// directly here to support `lzma.FORMAT_RAW`, custom filter chains for the XZ /
// ALONE encoders, and `_{en,de}code_filter_properties`.
// ---------------------------------------------------------------------------

/// FFI addition liblzma ships but `lzma-sys` 0.1 doesn't re-export:
/// `lzma_options_delta` — `type` + `dist` followed by reserved fields, every
/// member a C `uint32_t`/enum (4 bytes), 10 in total (layout from liblzma's
/// `delta.h`).
mod raw_ffi {
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(super) struct LzmaOptionsDelta {
        pub(super) type_: u32,
        pub(super) dist: u32,
        pub(super) reserved_int1: u32,
        pub(super) reserved_int2: u32,
        pub(super) reserved_int3: u32,
        pub(super) reserved_int4: u32,
        pub(super) reserved_enum1: u32,
        pub(super) reserved_enum2: u32,
        pub(super) reserved_enum3: u32,
        pub(super) reserved_enum4: u32,
    }
}

/// One parsed filter with liblzma-ready, owned options. The boxed options keep
/// a stable address while a `lzma_filter` array points at them across the
/// encoder/decoder/property init call (liblzma copies what it needs).
enum FilterOpts {
    Lzma(Box<lzma_sys::lzma_options_lzma>),
    Delta(Box<raw_ffi::LzmaOptionsDelta>),
    Bcj(Box<lzma_sys::lzma_options_bcj>),
    BcjNone,
}

struct OwnedFilter {
    id: u64,
    opts: FilterOpts,
}

/// Build the null-terminated `lzma_filter` array liblzma expects, pointing at
/// the owned option boxes.
fn to_lzma_filters(owned: &mut [OwnedFilter]) -> Vec<lzma_sys::lzma_filter> {
    let mut v = Vec::with_capacity(owned.len() + 1);
    for f in owned.iter_mut() {
        let options: *mut c_void = match &mut f.opts {
            FilterOpts::Lzma(b) => std::ptr::from_mut(b.as_mut()).cast(),
            FilterOpts::Delta(b) => std::ptr::from_mut(b.as_mut()).cast(),
            FilterOpts::Bcj(b) => std::ptr::from_mut(b.as_mut()).cast(),
            FilterOpts::BcjNone => std::ptr::null_mut(),
        };
        v.push(lzma_sys::lzma_filter { id: f.id, options });
    }
    v.push(lzma_sys::lzma_filter {
        id: lzma_sys::LZMA_VLI_UNKNOWN,
        options: std::ptr::null_mut(),
    });
    v
}

fn dict_get(dict: &DictData, key: &str) -> Option<Object> {
    dict.get(&DictKey(Object::from_str(key))).cloned()
}

/// Reject any dict key not in `allowed` (CPython rejects unknown filter
/// options with `ValueError`).
fn check_filter_keys(dict: &DictData, allowed: &[&str]) -> Result<(), RuntimeError> {
    for (k, _) in dict.iter() {
        match &k.0 {
            Object::Str(s) if allowed.contains(&s.as_ref()) => {}
            _ => return Err(value_error("Invalid filter specifier")),
        }
    }
    Ok(())
}

/// Read an unsigned 32-bit filter option through `__index__`, rejecting
/// negatives / overflow.
fn opt_u32(dict: &DictData, key: &str) -> Result<Option<u32>, RuntimeError> {
    match dict_get(dict, key) {
        None => Ok(None),
        Some(o) => {
            let v = crate::builtins::coerce_index_i64(&o)?;
            if v < 0 || v > i64::from(u32::MAX) {
                return Err(overflow_error("Filter option out of range"));
            }
            Ok(Some(v as u32))
        }
    }
}

/// Parse a single Python filter spec (a `{"id": …, …}` dict) into an owned,
/// liblzma-ready filter, with CPython's error taxonomy
/// (`TypeError`/`ValueError`).
fn parse_filter_spec(spec: &Object) -> Result<OwnedFilter, RuntimeError> {
    let dref = match spec {
        Object::Dict(d) => d.clone(),
        _ => {
            return Err(type_error(
                "Filter specifier must be a dict or dict-like object",
            ))
        }
    };
    let dict = dref.borrow();
    let id = match dict_get(&dict, "id") {
        Some(o) => crate::builtins::coerce_index_i64(&o)?,
        None => return Err(value_error("Filter specifier must have an \"id\" entry")),
    };
    match id {
        FILTER_LZMA1 | FILTER_LZMA2 => {
            check_filter_keys(
                &dict,
                &[
                    "id",
                    "preset",
                    "dict_size",
                    "lc",
                    "lp",
                    "pb",
                    "mode",
                    "nice_len",
                    "mf",
                    "depth",
                ],
            )?;
            let preset = opt_u32(&dict, "preset")?.unwrap_or(lzma_sys::LZMA_PRESET_DEFAULT);
            let mut opts: lzma_sys::lzma_options_lzma = unsafe { std::mem::zeroed() };
            if unsafe { lzma_sys::lzma_lzma_preset(std::ptr::from_mut(&mut opts), preset) } != 0 {
                return Err(lzma_error("Invalid compression preset"));
            }
            if let Some(v) = opt_u32(&dict, "dict_size")? {
                opts.dict_size = v;
            }
            if let Some(v) = opt_u32(&dict, "lc")? {
                opts.lc = v;
            }
            if let Some(v) = opt_u32(&dict, "lp")? {
                opts.lp = v;
            }
            if let Some(v) = opt_u32(&dict, "pb")? {
                opts.pb = v;
            }
            if let Some(v) = opt_u32(&dict, "mode")? {
                opts.mode = v;
            }
            if let Some(v) = opt_u32(&dict, "nice_len")? {
                opts.nice_len = v;
            }
            if let Some(v) = opt_u32(&dict, "mf")? {
                opts.mf = v;
            }
            if let Some(v) = opt_u32(&dict, "depth")? {
                opts.depth = v;
            }
            Ok(OwnedFilter {
                id: id as u64,
                opts: FilterOpts::Lzma(Box::new(opts)),
            })
        }
        FILTER_DELTA => {
            check_filter_keys(&dict, &["id", "dist"])?;
            let mut opts: raw_ffi::LzmaOptionsDelta = unsafe { std::mem::zeroed() };
            opts.dist = opt_u32(&dict, "dist")?.unwrap_or(1);
            Ok(OwnedFilter {
                id: id as u64,
                opts: FilterOpts::Delta(Box::new(opts)),
            })
        }
        FILTER_X86 | FILTER_POWERPC | FILTER_IA64 | FILTER_ARM | FILTER_ARMTHUMB | FILTER_SPARC => {
            check_filter_keys(&dict, &["id", "start_offset"])?;
            match opt_u32(&dict, "start_offset")? {
                Some(v) => Ok(OwnedFilter {
                    id: id as u64,
                    opts: FilterOpts::Bcj(Box::new(lzma_sys::lzma_options_bcj { start_offset: v })),
                }),
                None => Ok(OwnedFilter {
                    id: id as u64,
                    opts: FilterOpts::BcjNone,
                }),
            }
        }
        _ => Err(value_error("Invalid filter ID")),
    }
}

/// Parse a Python `filters=` sequence (list/tuple of dicts) into owned
/// filters. `TypeError` if it isn't a sequence.
fn parse_filters(arg: &Object) -> Result<Vec<OwnedFilter>, RuntimeError> {
    let items: Vec<Object> = match arg {
        Object::List(l) => l.borrow().clone(),
        Object::Tuple(t) => t.to_vec(),
        _ => return Err(type_error("Filters must be a sequence of dicts")),
    };
    items.iter().map(parse_filter_spec).collect()
}

/// Map a CPython check id to liblzma's `lzma_check`, rejecting unsupported ids.
fn check_to_lzma_id(check: i64) -> Result<lzma_sys::lzma_check, RuntimeError> {
    match check {
        CHECK_NONE => Ok(lzma_sys::LZMA_CHECK_NONE),
        CHECK_CRC32 => Ok(lzma_sys::LZMA_CHECK_CRC32),
        CHECK_CRC64 => Ok(lzma_sys::LZMA_CHECK_CRC64),
        CHECK_SHA256 => Ok(lzma_sys::LZMA_CHECK_SHA256),
        _ => Err(lzma_error("Invalid or unsupported integrity check")),
    }
}

/// A liblzma coder owning a raw `lzma_stream`, driven through `lzma_code`.
/// Used for the FORMAT_RAW / filter-chain paths xz2 can't express. Access is
/// serialised by the GIL, so it's sound to move between threads.
struct RawCoder {
    strm: Box<lzma_sys::lzma_stream>,
}

// SAFETY: the underlying `lzma_stream` is only ever touched while the owning
// state's `GilCell`/`Mutex` is held (one thread at a time), exactly like
// xz2's own `unsafe impl Send for Stream`.
unsafe impl Send for RawCoder {}

impl RawCoder {
    fn from_init(
        init: impl FnOnce(*mut lzma_sys::lzma_stream) -> lzma_sys::lzma_ret,
    ) -> Result<Self, RuntimeError> {
        let mut strm: Box<lzma_sys::lzma_stream> = Box::new(unsafe { std::mem::zeroed() });
        let ret = init(std::ptr::from_mut(strm.as_mut()));
        if ret != lzma_sys::LZMA_OK {
            return Err(lzma_error(&format!(
                "Failed to initialize filter chain (lzma_ret {ret})"
            )));
        }
        Ok(RawCoder { strm })
    }

    fn new_raw_encoder(filters: &[lzma_sys::lzma_filter]) -> Result<Self, RuntimeError> {
        Self::from_init(|s| unsafe { lzma_sys::lzma_raw_encoder(s, filters.as_ptr()) })
    }

    fn new_raw_decoder(filters: &[lzma_sys::lzma_filter]) -> Result<Self, RuntimeError> {
        Self::from_init(|s| unsafe { lzma_sys::lzma_raw_decoder(s, filters.as_ptr()) })
    }

    fn process(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        finish: bool,
    ) -> Result<(usize, usize, bool), RuntimeError> {
        unsafe {
            let s = &mut *self.strm;
            s.next_in = input.as_ptr();
            s.avail_in = input.len();
            s.next_out = output.as_mut_ptr();
            s.avail_out = output.len();
            let action = if finish {
                lzma_sys::LZMA_FINISH
            } else {
                lzma_sys::LZMA_RUN
            };
            let ret = lzma_sys::lzma_code(s, action);
            let consumed = input.len() - s.avail_in;
            let produced = output.len() - s.avail_out;
            match ret {
                // BUF_ERROR simply means "no further progress without more
                // input/output room" — the driver loops treat it as a clean
                // pause, like xz2's `Status::Ok`.
                lzma_sys::LZMA_OK | lzma_sys::LZMA_BUF_ERROR => Ok((consumed, produced, false)),
                lzma_sys::LZMA_STREAM_END => Ok((consumed, produced, true)),
                e => Err(lzma_error(&format!("Internal error in filter chain ({e})"))),
            }
        }
    }
}

impl Drop for RawCoder {
    fn drop(&mut self) {
        unsafe { lzma_sys::lzma_end(std::ptr::from_mut(self.strm.as_mut())) };
    }
}

/// Either an xz2-backed container coder (XZ/ALONE via preset) or a raw
/// liblzma coder (FORMAT_RAW / custom filter chains).
enum Coder {
    Xz(Stream),
    Raw(RawCoder),
}

impl Coder {
    /// Drive one `process` step, returning `(input_consumed, output_produced,
    /// stream_ended)` with a uniform interface across both backends.
    fn run(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        action: Action,
    ) -> Result<(usize, usize, bool), RuntimeError> {
        match self {
            Coder::Xz(s) => {
                let before_in = s.total_in();
                let before_out = s.total_out();
                let status = s
                    .process(input, output, action)
                    .map_err(|e| lzma_error(&format!("{e:?}")))?;
                Ok((
                    (s.total_in() - before_in) as usize,
                    (s.total_out() - before_out) as usize,
                    matches!(status, Status::StreamEnd),
                ))
            }
            Coder::Raw(r) => r.process(input, output, matches!(action, Action::Finish)),
        }
    }
}

/// `_lzma._encode_filter_properties(filterspec) -> bytes`.
fn b_encode_filter_properties(args: &[Object]) -> Result<Object, RuntimeError> {
    let spec = args
        .first()
        .ok_or_else(|| type_error("_encode_filter_properties expected a filter spec"))?;
    let mut owned = [parse_filter_spec(spec)?];
    let filters = to_lzma_filters(&mut owned);
    let filter = &filters[0];
    let mut size: u32 = 0;
    if unsafe { lzma_sys::lzma_properties_size(std::ptr::from_mut(&mut size), filter) }
        != lzma_sys::LZMA_OK
    {
        return Err(lzma_error("Invalid or unsupported filter chain"));
    }
    let mut props = vec![0u8; size as usize];
    if unsafe { lzma_sys::lzma_properties_encode(filter, props.as_mut_ptr()) } != lzma_sys::LZMA_OK
    {
        return Err(lzma_error("Invalid or unsupported filter chain"));
    }
    Ok(Object::new_bytes(props))
}

/// `_lzma._decode_filter_properties(filter_id, encoded_props) -> dict`.
fn b_decode_filter_properties(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = crate::builtins::coerce_index_i64(
        args.first()
            .ok_or_else(|| type_error("_decode_filter_properties expected a filter id"))?,
    )?;
    let props = args
        .get(1)
        .and_then(Object::as_bytes_view)
        .ok_or_else(|| type_error("a bytes-like object is required"))?;
    let mut filter = lzma_sys::lzma_filter {
        id: id as u64,
        options: std::ptr::null_mut(),
    };
    let ret = unsafe {
        lzma_sys::lzma_properties_decode(
            std::ptr::from_mut(&mut filter),
            std::ptr::null(),
            props.as_ptr(),
            props.len(),
        )
    };
    if ret != lzma_sys::LZMA_OK {
        return Err(lzma_error("Invalid or unsupported filter properties"));
    }
    let result = build_filter_spec(id, filter.options);
    if !filter.options.is_null() {
        // liblzma allocated the options with the C allocator (we passed a
        // null allocator); free it through the matching libc `free`.
        unsafe { libc::free(filter.options) };
    }
    result
}

/// Build a `{"id": …, …}` dict from a decoded filter's options.
fn build_filter_spec(id: i64, options: *mut c_void) -> Result<Object, RuntimeError> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(DictKey(Object::from_static("id")), Object::Int(id));
        match id {
            FILTER_LZMA1 | FILTER_LZMA2 if !options.is_null() => {
                let o = unsafe { &*(options as *const lzma_sys::lzma_options_lzma) };
                d.insert(
                    DictKey(Object::from_static("dict_size")),
                    Object::Int(i64::from(o.dict_size)),
                );
                d.insert(
                    DictKey(Object::from_static("lc")),
                    Object::Int(i64::from(o.lc)),
                );
                d.insert(
                    DictKey(Object::from_static("lp")),
                    Object::Int(i64::from(o.lp)),
                );
                d.insert(
                    DictKey(Object::from_static("pb")),
                    Object::Int(i64::from(o.pb)),
                );
            }
            FILTER_DELTA if !options.is_null() => {
                let o = unsafe { &*(options as *const raw_ffi::LzmaOptionsDelta) };
                d.insert(
                    DictKey(Object::from_static("dist")),
                    Object::Int(i64::from(o.dist)),
                );
            }
            _ if !options.is_null() => {
                // BCJ filters: a non-null options block carries `start_offset`.
                let o = unsafe { &*(options as *const lzma_sys::lzma_options_bcj) };
                d.insert(
                    DictKey(Object::from_static("start_offset")),
                    Object::Int(i64::from(o.start_offset)),
                );
            }
            _ => {}
        }
    }
    Ok(Object::Dict(dict))
}

// ---------------------------------------------------------------------------
// Incremental streaming objects — `LZMACompressor` / `LZMADecompressor`.
//
// Mirrors the `bz2` implementation: state lives in a process-global registry
// keyed by an integer handle stored on the instance (so a stream object is
// reachable from any thread under the GIL).
// ---------------------------------------------------------------------------

struct LzmaCompState {
    s: Coder,
    done: bool,
}

struct LzmaDecompState {
    s: Coder,
    format: i64,
    input: Vec<u8>,
    eof: bool,
    needs_input: bool,
    unused_data: Vec<u8>,
    /// Integrity-check id, discovered from the stream header on first input.
    check: i64,
    check_known: bool,
}

type CompReg = Mutex<HashMap<i64, Rc<RefCell<LzmaCompState>>>>;
type DecompReg = Mutex<HashMap<i64, Rc<RefCell<LzmaDecompState>>>>;

fn comp_reg() -> &'static CompReg {
    static REG: OnceLock<CompReg> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn decomp_reg() -> &'static DecompReg {
    static REG: OnceLock<DecompReg> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lzma_next_id() -> i64 {
    static NEXT: AtomicI64 = AtomicI64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn comp_state(id: i64) -> Option<Rc<RefCell<LzmaCompState>>> {
    comp_reg().lock().ok()?.get(&id).cloned()
}

fn decomp_state(id: i64) -> Option<Rc<RefCell<LzmaDecompState>>> {
    decomp_reg().lock().ok()?.get(&id).cloned()
}

/// The `_lzma.LZMAError` exception type (a plain `Exception` subclass, as in
/// CPython). Created once and shared via the module dict.
fn lzma_error_class() -> Rc<TypeObject> {
    static CLS: OnceLock<Rc<TypeObject>> = OnceLock::new();
    CLS.get_or_init(|| {
        let bt = crate::builtin_types::builtin_types();
        TypeObject::new_with_flags(
            "LZMAError",
            vec![bt.exception.clone()],
            DictData::new(),
            TypeFlags {
                is_exception: true,
                is_builtin: true,
            },
        )
        .expect("LZMAError must linearise")
    })
    .clone()
}

fn lzma_error(msg: &str) -> RuntimeError {
    let inst = crate::builtin_types::make_exception_with_class(lzma_error_class(), msg);
    RuntimeError::PyException(PyException::new(inst))
}

fn eof_error(msg: &str) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("EOFError", msg))
}

fn bytes_arg(arg: Option<&Object>) -> Result<Vec<u8>, RuntimeError> {
    arg.and_then(Object::as_bytes_view)
        .ok_or_else(|| type_error("a bytes-like object is required"))
}

fn handle_of(args: &[Object]) -> Result<i64, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) => match i
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("_handle")))
            .cloned()
        {
            Some(Object::Int(v)) => Ok(v),
            _ => Err(type_error("lzma object missing _handle")),
        },
        _ => Err(type_error("expected lzma compressor/decompressor object")),
    }
}

fn self_instance(args: &[Object]) -> Result<Rc<PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) => Ok(i.clone()),
        _ => Err(type_error("method requires an lzma object instance")),
    }
}

/// Drive a coder to completion for one encode call.
fn lzma_compress_step(
    s: &mut Coder,
    input: &[u8],
    action: Action,
) -> Result<Vec<u8>, RuntimeError> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut consumed = 0usize;
    loop {
        let (din, dout, end) = s.run(&input[consumed..], &mut buf, action)?;
        out.extend_from_slice(&buf[..dout]);
        consumed += din;
        if end {
            break;
        }
        let finishing = matches!(action, Action::Finish);
        if !finishing && dout < buf.len() {
            break;
        }
        if din == 0 && dout == 0 {
            break;
        }
    }
    Ok(out)
}

/// Drive `Stream::process` for one decode call. `limit` caps the output.
/// Returns `(output, input_consumed, stream_end, output_capped)` where
/// `output_capped` means decoding stopped because the `max_length` limit was
/// reached (so more output may be available without feeding more input).
fn lzma_decompress_step(
    s: &mut Coder,
    input: &[u8],
    limit: Option<usize>,
) -> Result<(Vec<u8>, usize, bool, bool), RuntimeError> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut consumed = 0usize;
    let mut eof = false;
    let mut capped = false;
    loop {
        let room = match limit {
            Some(l) => {
                if out.len() >= l {
                    capped = true;
                    break;
                }
                (l - out.len()).min(buf.len())
            }
            None => buf.len(),
        };
        let (din, dout, end) = s.run(&input[consumed..], &mut buf[..room], Action::Run)?;
        out.extend_from_slice(&buf[..dout]);
        consumed += din;
        if end {
            eof = true;
            break;
        }
        if din == 0 && dout == 0 {
            break;
        }
    }
    Ok((out, consumed, eof, capped))
}

/// XZ stream header: 6-byte magic, then 2 Stream Flags bytes whose low nibble
/// of the second byte encodes the integrity-check id. Returns `None` when the
/// data is not (yet) a recognisable XZ header.
fn xz_check_id(data: &[u8]) -> Option<i64> {
    const MAGIC: [u8; 6] = [0xFD, b'7', b'z', b'X', b'Z', 0x00];
    if data.len() >= 8 && data[..6] == MAGIC {
        Some(i64::from(data[7] & 0x0F))
    } else {
        None
    }
}

/// Coerce an optional scalar argument through the `__index__` protocol,
/// yielding `TypeError` for non-integers (floats, str, bytes, …). `None`
/// (the Python value) and an absent argument both map to `Ok(None)`.
fn opt_index(arg: Option<&Object>) -> Result<Option<i64>, RuntimeError> {
    match arg {
        None | Some(Object::None) => Ok(None),
        Some(o) => Ok(Some(crate::builtins::coerce_index_i64(o)?)),
    }
}

/// Validate the `filters` argument shape and report whether one was given.
/// `filters` may only be `None` or a list/tuple (of dicts); anything else is a
/// `TypeError` (matching CPython). The element-level parsing/validation and
/// the actual chain construction happen later via [`parse_filters`].
fn filters_is_seq(arg: Option<&Object>) -> Result<bool, RuntimeError> {
    match arg {
        None | Some(Object::None) => Ok(false),
        Some(Object::List(_)) | Some(Object::Tuple(_)) => Ok(true),
        Some(_) => Err(type_error("Filters must be a sequence of dicts")),
    }
}

fn class_method(
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

fn class_method_kw(
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

fn no_pickle(args: &[Object]) -> Result<Object, RuntimeError> {
    let name = match args.first() {
        Some(Object::Instance(i)) => i.cls().name.clone(),
        _ => "lzma object".to_owned(),
    };
    Err(type_error(format!("cannot pickle '{name}' object")))
}

fn compressor_class() -> Rc<TypeObject> {
    static CLS: OnceLock<Rc<TypeObject>> = OnceLock::new();
    CLS.get_or_init(|| {
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::new();
        class_method_kw(&mut dict, "__init__", compressor_init);
        class_method(&mut dict, "compress", compressor_compress);
        class_method(&mut dict, "flush", compressor_flush);
        class_method(&mut dict, "__reduce__", no_pickle);
        class_method(&mut dict, "__reduce_ex__", no_pickle);
        class_method(&mut dict, "__getstate__", no_pickle);
        TypeObject::new_with_flags(
            "LZMACompressor",
            vec![bt.object_.clone()],
            dict,
            TypeFlags {
                is_exception: false,
                is_builtin: true,
            },
        )
        .expect("LZMACompressor must linearise")
    })
    .clone()
}

fn decompressor_class() -> Rc<TypeObject> {
    static CLS: OnceLock<Rc<TypeObject>> = OnceLock::new();
    CLS.get_or_init(|| {
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::new();
        class_method_kw(&mut dict, "__init__", decompressor_init);
        class_method_kw(&mut dict, "decompress", decompressor_decompress);
        class_method(&mut dict, "__reduce__", no_pickle);
        class_method(&mut dict, "__reduce_ex__", no_pickle);
        class_method(&mut dict, "__getstate__", no_pickle);
        TypeObject::new_with_flags(
            "LZMADecompressor",
            vec![bt.object_.clone()],
            dict,
            TypeFlags {
                is_exception: false,
                is_builtin: true,
            },
        )
        .expect("LZMADecompressor must linearise")
    })
    .clone()
}

fn overflow_error(msg: &str) -> RuntimeError {
    RuntimeError::PyException(PyException::from_builtin("OverflowError", msg))
}

/// Map a CPython check id to xz2's `Check`, rejecting ids liblzma cannot
/// honour (CHECK_UNKNOWN / out-of-range) with `LZMAError`, like CPython.
fn check_to_xz_checked(check: i64) -> Result<Check, RuntimeError> {
    match check {
        CHECK_NONE => Ok(Check::None),
        CHECK_CRC32 => Ok(Check::Crc32),
        CHECK_CRC64 => Ok(Check::Crc64),
        CHECK_SHA256 => Ok(Check::Sha256),
        _ => Err(lzma_error("Invalid or unsupported integrity check")),
    }
}

fn compressor_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args)?;
    let kw = |name: &str| kwargs.iter().find(|(k, _)| k == name).map(|(_, v)| v);

    let format = opt_index(args.get(1).or_else(|| kw("format")))?.unwrap_or(FORMAT_XZ);
    let check_arg = opt_index(args.get(2).or_else(|| kw("check")))?;
    let preset_given =
        matches!(args.get(3).or_else(|| kw("preset")), Some(o) if !matches!(o, Object::None));
    let preset_val = opt_index(args.get(3).or_else(|| kw("preset")))?;
    let has_filters = filters_is_seq(args.get(4).or_else(|| kw("filters")))?;

    if format == FORMAT_AUTO {
        return Err(value_error("FORMAT_AUTO cannot be used for compression"));
    }
    if !matches!(format, FORMAT_XZ | FORMAT_ALONE | FORMAT_RAW) {
        return Err(value_error("Invalid container format"));
    }
    if preset_given && has_filters {
        return Err(value_error("Cannot specify both preset and filter chain"));
    }
    if format == FORMAT_RAW && !has_filters {
        return Err(value_error("Must specify filters for FORMAT_RAW"));
    }
    // `check == -1` is the "unspecified" sentinel; only a *real* check id
    // other than CHECK_NONE is rejected for the checkless container formats.
    if format != FORMAT_XZ && matches!(check_arg, Some(c) if c > CHECK_NONE) {
        return Err(value_error(
            "Integrity checks are only supported by FORMAT_XZ",
        ));
    }

    let preset_u = match preset_val {
        Some(p) if p < 0 => return Err(overflow_error("can't convert negative int to unsigned")),
        Some(p) => p as u32,
        None => 6,
    };

    let check = check_arg.unwrap_or(-1);
    let coder = if has_filters {
        // A custom filter chain: parse + validate, then build the matching
        // liblzma encoder directly (xz2 can't express raw / delta chains).
        let filters_obj = args
            .get(4)
            .or_else(|| kw("filters"))
            .expect("has_filters implies a filters argument");
        let mut owned = parse_filters(filters_obj)?;
        let lzfilters = to_lzma_filters(&mut owned);
        match format {
            FORMAT_RAW => Coder::Raw(RawCoder::new_raw_encoder(&lzfilters)?),
            FORMAT_XZ => {
                let chk = if check < 0 {
                    lzma_sys::LZMA_CHECK_CRC64
                } else {
                    check_to_lzma_id(check)?
                };
                Coder::Raw(RawCoder::from_init(|s| unsafe {
                    lzma_sys::lzma_stream_encoder(s, lzfilters.as_ptr(), chk)
                })?)
            }
            _ => {
                // FORMAT_ALONE: a single LZMA1 filter's options drive the
                // legacy `.lzma` encoder.
                let opts = match owned.first() {
                    Some(OwnedFilter {
                        opts: FilterOpts::Lzma(b),
                        ..
                    }) if owned.len() == 1 => std::ptr::from_ref(b.as_ref()),
                    _ => return Err(lzma_error("Invalid filter chain for FORMAT_ALONE")),
                };
                Coder::Raw(RawCoder::from_init(|s| unsafe {
                    lzma_sys::lzma_alone_encoder(s, opts)
                })?)
            }
        }
    } else {
        match format {
            FORMAT_ALONE => {
                let opts = xz2::stream::LzmaOptions::new_preset(preset_u)
                    .map_err(|e| lzma_error(&format!("{e:?}")))?;
                Coder::Xz(
                    Stream::new_lzma_encoder(&opts).map_err(|e| lzma_error(&format!("{e:?}")))?,
                )
            }
            _ => {
                let chk = if check < 0 {
                    Check::Crc64
                } else {
                    check_to_xz_checked(check)?
                };
                Coder::Xz(
                    Stream::new_easy_encoder(preset_u, chk)
                        .map_err(|e| lzma_error(&format!("{e:?}")))?,
                )
            }
        }
    };
    let id = lzma_next_id();
    if let Ok(mut reg) = comp_reg().lock() {
        reg.insert(
            id,
            Rc::new(RefCell::new(LzmaCompState {
                s: coder,
                done: false,
            })),
        );
    }
    inst.dict
        .borrow_mut()
        .insert(DictKey(Object::from_static("_handle")), Object::Int(id));
    Ok(Object::None)
}

fn compressor_compress(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error("compress() takes exactly one argument"));
    }
    let id = handle_of(args)?;
    let data = bytes_arg(args.get(1))?;
    let state = comp_state(id).ok_or_else(|| value_error("stale LZMACompressor"))?;
    let mut st = state.borrow_mut();
    if st.done {
        return Err(value_error("Compressor has already finished"));
    }
    let out = lzma_compress_step(&mut st.s, &data, Action::Run)?;
    Ok(Object::new_bytes(out))
}

fn compressor_flush(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() > 1 {
        return Err(type_error("flush() takes no arguments"));
    }
    let id = handle_of(args)?;
    let state = comp_state(id).ok_or_else(|| value_error("stale LZMACompressor"))?;
    let mut st = state.borrow_mut();
    if st.done {
        return Err(value_error("Repeated call to flush()"));
    }
    let out = lzma_compress_step(&mut st.s, &[], Action::Finish)?;
    st.done = true;
    Ok(Object::new_bytes(out))
}

fn decompressor_init(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let inst = self_instance(args)?;
    let kw = |name: &str| kwargs.iter().find(|(k, _)| k == name).map(|(_, v)| v);

    let format = opt_index(args.get(1).or_else(|| kw("format")))?.unwrap_or(FORMAT_AUTO);
    let memlimit_arg = opt_index(args.get(2).or_else(|| kw("memlimit")))?;
    let has_filters = filters_is_seq(args.get(3).or_else(|| kw("filters")))?;

    if !matches!(format, FORMAT_AUTO | FORMAT_XZ | FORMAT_ALONE | FORMAT_RAW) {
        return Err(value_error("Invalid container format"));
    }
    if format == FORMAT_RAW {
        if !has_filters {
            return Err(value_error("Must specify filters for FORMAT_RAW"));
        }
        if memlimit_arg.is_some() {
            return Err(value_error("Cannot specify memory limit with FORMAT_RAW"));
        }
    } else if has_filters {
        return Err(value_error("Cannot specify filters except with FORMAT_RAW"));
    }

    let memlimit = memlimit_arg.map_or(u64::MAX, |v| v.max(0) as u64);
    let coder = if format == FORMAT_RAW {
        let filters_obj = args
            .get(3)
            .or_else(|| kw("filters"))
            .expect("FORMAT_RAW requires filters (checked above)");
        let mut owned = parse_filters(filters_obj)?;
        let lzfilters = to_lzma_filters(&mut owned);
        Coder::Raw(RawCoder::new_raw_decoder(&lzfilters)?)
    } else {
        Coder::Xz(
            match format {
                FORMAT_ALONE => Stream::new_lzma_decoder(memlimit),
                FORMAT_XZ => Stream::new_stream_decoder(memlimit, 0),
                _ => Stream::new_auto_decoder(memlimit, 0),
            }
            .map_err(|e| lzma_error(&format!("{e:?}")))?,
        )
    };
    let id = lzma_next_id();
    if let Ok(mut reg) = decomp_reg().lock() {
        reg.insert(
            id,
            Rc::new(RefCell::new(LzmaDecompState {
                s: coder,
                format,
                input: Vec::new(),
                eof: false,
                needs_input: true,
                unused_data: Vec::new(),
                check: CHECK_NONE,
                check_known: false,
            })),
        );
    }
    let mut d = inst.dict.borrow_mut();
    d.insert(DictKey(Object::from_static("_handle")), Object::Int(id));
    d.insert(DictKey(Object::from_static("eof")), Object::Bool(false));
    d.insert(
        DictKey(Object::from_static("needs_input")),
        Object::Bool(true),
    );
    d.insert(
        DictKey(Object::from_static("unused_data")),
        Object::new_bytes(Vec::new()),
    );
    d.insert(
        DictKey(Object::from_static("check")),
        Object::Int(CHECK_NONE),
    );
    Ok(Object::None)
}

fn decompressor_decompress(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    if args.len() < 2 {
        return Err(type_error(
            "decompress() missing 1 required positional argument: 'data'",
        ));
    }
    let id = handle_of(args)?;
    let data = bytes_arg(args.get(1))?;
    let max_length = if let Some(o) = args.get(2) {
        crate::builtins::coerce_index_i64(o)?
    } else if let Some((_, o)) = kwargs.iter().find(|(k, _)| k == "max_length") {
        crate::builtins::coerce_index_i64(o)?
    } else {
        -1
    };
    let state = decomp_state(id).ok_or_else(|| value_error("stale LZMADecompressor"))?;
    let mut st = state.borrow_mut();
    if st.eof {
        return Err(eof_error("Already at end of stream"));
    }

    let mut combined = std::mem::take(&mut st.input);
    combined.extend_from_slice(&data);

    // Discover the integrity check id from the stream header once enough
    // bytes are buffered. ALONE/RAW streams carry no check (CHECK_NONE).
    if !st.check_known {
        if st.format == FORMAT_ALONE || st.format == FORMAT_RAW {
            st.check = CHECK_NONE;
            st.check_known = true;
        } else if let Some(c) = xz_check_id(&combined) {
            st.check = c;
            st.check_known = true;
        } else if combined.len() >= 8 {
            // 8+ bytes that are not an XZ header (e.g. ALONE via AUTO).
            st.check = CHECK_NONE;
            st.check_known = true;
        }
    }

    let limit = if max_length < 0 {
        None
    } else {
        Some(max_length as usize)
    };
    let (out, consumed, eof, capped) = lzma_decompress_step(&mut st.s, &combined, limit)?;
    let leftover = combined[consumed..].to_vec();
    if eof {
        st.eof = true;
        st.needs_input = false;
        st.unused_data = leftover;
        st.input = Vec::new();
    } else if capped {
        // Stopped because the output limit was hit: there may be more output
        // available without feeding more input, so `needs_input` stays False.
        st.needs_input = false;
        st.input = leftover;
    } else {
        st.needs_input = leftover.is_empty();
        st.input = leftover;
    }
    let check = st.check;
    if let Some(Object::Instance(inst)) = args.first() {
        let mut d = inst.dict.borrow_mut();
        d.insert(DictKey(Object::from_static("eof")), Object::Bool(st.eof));
        d.insert(
            DictKey(Object::from_static("needs_input")),
            Object::Bool(st.needs_input),
        );
        d.insert(
            DictKey(Object::from_static("unused_data")),
            Object::new_bytes(st.unused_data.clone()),
        );
        d.insert(DictKey(Object::from_static("check")), Object::Int(check));
    }
    Ok(Object::new_bytes(out))
}
