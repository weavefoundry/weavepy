//! The `hashlib` built-in module.
//!
//! Backed by the RustCrypto family: `sha2`, `sha1`, `md-5`. Each
//! hasher is exposed as a tiny dict carrying its incremental state
//! plus `update`, `digest`, `hexdigest`, `copy`, `name`,
//! `digest_size`, `block_size`.
//!
//! Coverage: `md5`, `sha1`, `sha224`, `sha256`, `sha384`, `sha512`, the
//! SHA-3 family (`sha3_224/256/384/512`), the SHAKE XOFs
//! (`shake_128/256`, with the `digest(length)`/`hexdigest(length)`
//! signature), and `blake2b`/`blake2s` (with `digest_size`/`key`/`salt`/
//! `person` and the tree parameters). `usedforsecurity=` is accepted and
//! ignored on every constructor (we don't link OpenSSL, so FIPS mode is
//! moot). `pbkdf2_hmac` is implemented for HMAC-SHA{1,256,512}.
//!
//! Deferred: `scrypt` (needs the memory-hard KDF).

use crate::sync::Rc;
use crate::sync::RefCell;
use std::collections::HashMap;

use digest::Digest;
use md5::Md5;
use sha1::Sha1;
use sha2::{Sha224, Sha256, Sha384, Sha512};
use sha3::digest::{ExtendableOutput, XofReader};
use sha3::{Sha3_224, Sha3_256, Sha3_384, Sha3_512, Shake128, Shake256};

use crate::error::{blocking_io_error, type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::types::{PyInstance, TypeObject};

/// Erased dispatch table. Each variant carries its own digest type; we
/// keep them under one enum so the hasher dict can own a single handle.
/// SHA-3/SHAKE come from `sha3`; BLAKE2 from `blake2{b,s}_simd` (which,
/// unlike RustCrypto's `blake2`, exposes the `salt`/`person`/tree
/// parameters CPython's `_blake2` surfaces). The trailing `usize` on the
/// BLAKE2 variants is the configured digest length.
enum AnyHasher {
    Md5(Md5),
    Sha1(Sha1),
    Sha224(Sha224),
    Sha256(Sha256),
    Sha384(Sha384),
    Sha512(Sha512),
    Sha3_224(Sha3_224),
    Sha3_256(Sha3_256),
    Sha3_384(Sha3_384),
    Sha3_512(Sha3_512),
    Shake128(Shake128),
    Shake256(Shake256),
    Blake2b(Box<blake2b_simd::State>, usize),
    Blake2s(Box<blake2s_simd::State>, usize),
}

impl AnyHasher {
    fn update(&mut self, data: &[u8]) {
        use digest::Update;
        match self {
            Self::Md5(h) => Update::update(h, data),
            Self::Sha1(h) => Update::update(h, data),
            Self::Sha224(h) => Update::update(h, data),
            Self::Sha256(h) => Update::update(h, data),
            Self::Sha384(h) => Update::update(h, data),
            Self::Sha512(h) => Update::update(h, data),
            Self::Sha3_224(h) => Update::update(h, data),
            Self::Sha3_256(h) => Update::update(h, data),
            Self::Sha3_384(h) => Update::update(h, data),
            Self::Sha3_512(h) => Update::update(h, data),
            Self::Shake128(h) => Update::update(h, data),
            Self::Shake256(h) => Update::update(h, data),
            Self::Blake2b(s, _) => {
                s.update(data);
            }
            Self::Blake2s(s, _) => {
                s.update(data);
            }
        }
    }

    fn is_xof(&self) -> bool {
        matches!(self, Self::Shake128(_) | Self::Shake256(_))
    }

    /// Produce the digest. `length` is required (and honoured) only for the
    /// SHAKE XOFs; fixed-size digests ignore it.
    fn digest(&self, length: Option<usize>) -> Vec<u8> {
        match self {
            Self::Md5(h) => h.clone().finalize().to_vec(),
            Self::Sha1(h) => h.clone().finalize().to_vec(),
            Self::Sha224(h) => h.clone().finalize().to_vec(),
            Self::Sha256(h) => h.clone().finalize().to_vec(),
            Self::Sha384(h) => h.clone().finalize().to_vec(),
            Self::Sha512(h) => h.clone().finalize().to_vec(),
            Self::Sha3_224(h) => h.clone().finalize().to_vec(),
            Self::Sha3_256(h) => h.clone().finalize().to_vec(),
            Self::Sha3_384(h) => h.clone().finalize().to_vec(),
            Self::Sha3_512(h) => h.clone().finalize().to_vec(),
            Self::Shake128(h) => {
                let mut buf = vec![0u8; length.unwrap_or(0)];
                h.clone().finalize_xof().read(&mut buf);
                buf
            }
            Self::Shake256(h) => {
                let mut buf = vec![0u8; length.unwrap_or(0)];
                h.clone().finalize_xof().read(&mut buf);
                buf
            }
            Self::Blake2b(s, _) => s.clone().finalize().as_bytes().to_vec(),
            Self::Blake2s(s, _) => s.clone().finalize().as_bytes().to_vec(),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Md5(_) => "md5",
            Self::Sha1(_) => "sha1",
            Self::Sha224(_) => "sha224",
            Self::Sha256(_) => "sha256",
            Self::Sha384(_) => "sha384",
            Self::Sha512(_) => "sha512",
            Self::Sha3_224(_) => "sha3_224",
            Self::Sha3_256(_) => "sha3_256",
            Self::Sha3_384(_) => "sha3_384",
            Self::Sha3_512(_) => "sha3_512",
            Self::Shake128(_) => "shake_128",
            Self::Shake256(_) => "shake_256",
            Self::Blake2b(..) => "blake2b",
            Self::Blake2s(..) => "blake2s",
        }
    }

    fn digest_size(&self) -> i64 {
        match self {
            Self::Md5(_) => 16,
            Self::Sha1(_) => 20,
            Self::Sha224(_) | Self::Sha3_224(_) => 28,
            Self::Sha256(_) | Self::Sha3_256(_) => 32,
            Self::Sha384(_) | Self::Sha3_384(_) => 48,
            Self::Sha512(_) | Self::Sha3_512(_) => 64,
            // SHAKE digests are variable-length: CPython reports 0.
            Self::Shake128(_) | Self::Shake256(_) => 0,
            Self::Blake2b(_, n) | Self::Blake2s(_, n) => *n as i64,
        }
    }

    fn block_size(&self) -> i64 {
        match self {
            Self::Md5(_) | Self::Sha1(_) | Self::Sha224(_) | Self::Sha256(_) => 64,
            Self::Sha384(_) | Self::Sha512(_) => 128,
            // SHA-3 / SHAKE block size is the sponge rate (bytes).
            Self::Sha3_224(_) => 144,
            Self::Sha3_256(_) => 136,
            Self::Sha3_384(_) => 104,
            Self::Sha3_512(_) => 72,
            Self::Shake128(_) => 168,
            Self::Shake256(_) => 136,
            Self::Blake2b(..) => 128,
            Self::Blake2s(..) => 64,
        }
    }

    /// SHA-3/SHAKE expose `_capacity_bits`/`_rate_bits`/`_suffix` (the
    /// sponge geometry + domain-separation byte). `capacity + rate` is
    /// always 1600. Returns `None` for the non-Keccak digests.
    fn sha3_params(&self) -> Option<(i64, i64, u8)> {
        Some(match self {
            Self::Sha3_224(_) => (448, 1152, 0x06),
            Self::Sha3_256(_) => (512, 1088, 0x06),
            Self::Sha3_384(_) => (768, 832, 0x06),
            Self::Sha3_512(_) => (1024, 576, 0x06),
            Self::Shake128(_) => (256, 1344, 0x1f),
            Self::Shake256(_) => (512, 1088, 0x1f),
            _ => return None,
        })
    }

    fn clone_state(&self) -> Self {
        match self {
            Self::Md5(h) => Self::Md5(h.clone()),
            Self::Sha1(h) => Self::Sha1(h.clone()),
            Self::Sha224(h) => Self::Sha224(h.clone()),
            Self::Sha256(h) => Self::Sha256(h.clone()),
            Self::Sha384(h) => Self::Sha384(h.clone()),
            Self::Sha512(h) => Self::Sha512(h.clone()),
            Self::Sha3_224(h) => Self::Sha3_224(h.clone()),
            Self::Sha3_256(h) => Self::Sha3_256(h.clone()),
            Self::Sha3_384(h) => Self::Sha3_384(h.clone()),
            Self::Sha3_512(h) => Self::Sha3_512(h.clone()),
            Self::Shake128(h) => Self::Shake128(h.clone()),
            Self::Shake256(h) => Self::Shake256(h.clone()),
            Self::Blake2b(s, n) => Self::Blake2b(s.clone(), *n),
            Self::Blake2s(s, n) => Self::Blake2s(s.clone(), *n),
        }
    }
}

thread_local! {
    static HASHER_REGISTRY: RefCell<HashMap<i64, Rc<RefCell<AnyHasher>>>> =
        RefCell::new(HashMap::new());
    static HASHER_NEXT_ID: RefCell<i64> = const { RefCell::new(1) };
    static HASHER_CLASS: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
    /// CPython's pure-Python `hashlib` exposes a `__builtin_constructor_cache`
    /// dict that `new(name)` consults before its builtin table; `hmac`'s
    /// string-name fallback and `test_hmac.test_with_fallback` rely on it.
    /// We share one `dict` object between the module attribute and `new`.
    static CTOR_CACHE: RefCell<Option<Rc<RefCell<DictData>>>> = const { RefCell::new(None) };
}

fn register_hasher(state: AnyHasher) -> i64 {
    let id = HASHER_NEXT_ID.with(|c| {
        let mut v = c.borrow_mut();
        let id = *v;
        *v += 1;
        id
    });
    HASHER_REGISTRY.with(|r| {
        r.borrow_mut().insert(id, Rc::new(RefCell::new(state)));
    });
    id
}

fn lookup_hasher(id: i64) -> Result<Rc<RefCell<AnyHasher>>, RuntimeError> {
    HASHER_REGISTRY.with(|r| {
        r.borrow()
            .get(&id)
            .cloned()
            .ok_or_else(|| value_error("hashlib: stale hasher handle"))
    })
}

fn instance_handle(args: &[Object]) -> Result<i64, RuntimeError> {
    let inst = match args.first() {
        Some(Object::Instance(i)) => i.clone(),
        _ => return Err(type_error("expected hasher instance")),
    };
    let handle_obj = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_handle")))
        .cloned()
        .ok_or_else(|| value_error("hasher missing _handle"))?;
    match handle_obj {
        Object::Int(v) => Ok(v),
        _ => Err(type_error("hasher _handle must be int")),
    }
}

fn make_hasher_instance(state: AnyHasher) -> Object {
    let (name, digest_size, block_size) = (state.name(), state.digest_size(), state.block_size());
    let sha3 = state.sha3_params();
    let id = register_hasher(state);
    let cls = hasher_class();
    let inst = PyInstance::new(cls);
    {
        let mut d = inst.dict.borrow_mut();
        d.insert(DictKey(Object::from_static("_handle")), Object::Int(id));
        d.insert(
            DictKey(Object::from_static("name")),
            Object::from_static(name),
        );
        d.insert(
            DictKey(Object::from_static("digest_size")),
            Object::Int(digest_size),
        );
        d.insert(
            DictKey(Object::from_static("block_size")),
            Object::Int(block_size),
        );
        if let Some((capacity, rate, suffix)) = sha3 {
            d.insert(
                DictKey(Object::from_static("_capacity_bits")),
                Object::Int(capacity),
            );
            d.insert(
                DictKey(Object::from_static("_rate_bits")),
                Object::Int(rate),
            );
            d.insert(
                DictKey(Object::from_static("_suffix")),
                Object::new_bytes(vec![suffix]),
            );
        }
    }
    Object::Instance(Rc::new(inst))
}

fn hasher_class() -> Rc<TypeObject> {
    HASHER_CLASS.with(|slot| {
        if let Some(c) = slot.borrow().as_ref() {
            return c.clone();
        }
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::new();
        macro_rules! method {
            ($name:literal, $body:expr) => {
                dict.insert(
                    DictKey(Object::from_static($name)),
                    Object::Builtin(Rc::new(BuiltinFn {
                        name: $name,
                        binds_instance: true,
                        call: Box::new($body),
                        call_kw: None,
                    })),
                );
            };
        }
        method!("__repr__", hasher_repr);
        method!("update", hasher_update);
        method!("digest", hasher_digest);
        method!("hexdigest", hasher_hexdigest);
        method!("copy", hasher_copy);
        let cls = TypeObject::new_user("hashlib._Hasher", vec![bt.object_.clone()], dict)
            .expect("hasher class must linearise");
        *slot.borrow_mut() = Some(cls.clone());
        cls
    })
}

fn hasher_repr(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = instance_handle(args)?;
    let state = lookup_hasher(id)?;
    let name = state.borrow().name();
    // Mirror CPython's `<md5 _hashlib.HASH object @ 0x…>` shape (the
    // address is cosmetic; `test_blocksize_and_name` only asserts the
    // algorithm name appears in the repr).
    Ok(Object::from_str(format!(
        "<{name} _hashlib.HASH object @ 0x{id:012x}>"
    )))
}

fn hasher_update(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = instance_handle(args)?;
    let data = args
        .get(1)
        .ok_or_else(|| type_error("update: missing data"))?;
    let bytes = bytes_of(data)?;
    let state = lookup_hasher(id)?;
    state.borrow_mut().update(&bytes);
    Ok(Object::None)
}

/// SHAKE digests take a mandatory `length`; fixed-size digests take none.
/// CPython's `_sha3` caps the SHAKE output at `2**29` bytes and rejects
/// negatives, both as `ValueError` (`test_digest_length_overflow`).
fn digest_length(state: &AnyHasher, args: &[Object]) -> Result<Option<usize>, RuntimeError> {
    if !state.is_xof() {
        return Ok(None);
    }
    let arg = args
        .get(1)
        .ok_or_else(|| type_error("digest() missing required argument 'length' (pos 1)"))?;
    if !matches!(arg, Object::Int(_) | Object::Long(_) | Object::Bool(_)) {
        return Err(type_error("'length' must be an integer"));
    }
    if matches!(arg.as_i64(), Some(i) if i < 0) {
        return Err(value_error("length must be a non-negative integer"));
    }
    match arg.as_usize() {
        Some(n) if n < (1 << 29) => Ok(Some(n)),
        _ => Err(value_error("length is too large")),
    }
}

fn hasher_digest(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = instance_handle(args)?;
    let state = lookup_hasher(id)?;
    let st = state.borrow();
    let length = digest_length(&st, args)?;
    Ok(Object::new_bytes(st.digest(length)))
}

fn hasher_hexdigest(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = instance_handle(args)?;
    let state = lookup_hasher(id)?;
    let st = state.borrow();
    let length = digest_length(&st, args)?;
    let bytes = st.digest(length);
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        write!(out, "{b:02x}").unwrap();
    }
    Ok(Object::from_str(out))
}

fn hasher_copy(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = instance_handle(args)?;
    let state = lookup_hasher(id)?;
    let cloned = state.borrow().clone_state();
    Ok(make_hasher_instance(cloned))
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("hashlib"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Secure hash and message digest algorithms."),
        );
        d.insert(
            DictKey(Object::from_static("algorithms_guaranteed")),
            algos_set(),
        );
        d.insert(
            DictKey(Object::from_static("algorithms_available")),
            algos_set(),
        );
        let cache = Rc::new(RefCell::new(DictData::new()));
        CTOR_CACHE.with(|c| *c.borrow_mut() = Some(cache.clone()));
        d.insert(
            DictKey(Object::from_static("__builtin_constructor_cache")),
            Object::Dict(cache),
        );
        d.insert(DictKey(Object::from_static("new")), b_kw("new", hash_new));
        d.insert(DictKey(Object::from_static("md5")), b_kw("md5", make_md5));
        d.insert(
            DictKey(Object::from_static("sha1")),
            b_kw("sha1", make_sha1),
        );
        d.insert(
            DictKey(Object::from_static("sha224")),
            b_kw("sha224", make_sha224),
        );
        d.insert(
            DictKey(Object::from_static("sha256")),
            b_kw("sha256", make_sha256),
        );
        d.insert(
            DictKey(Object::from_static("sha384")),
            b_kw("sha384", make_sha384),
        );
        d.insert(
            DictKey(Object::from_static("sha512")),
            b_kw("sha512", make_sha512),
        );
        d.insert(
            DictKey(Object::from_static("sha3_224")),
            b_kw("sha3_224", make_sha3_224),
        );
        d.insert(
            DictKey(Object::from_static("sha3_256")),
            b_kw("sha3_256", make_sha3_256),
        );
        d.insert(
            DictKey(Object::from_static("sha3_384")),
            b_kw("sha3_384", make_sha3_384),
        );
        d.insert(
            DictKey(Object::from_static("sha3_512")),
            b_kw("sha3_512", make_sha3_512),
        );
        d.insert(
            DictKey(Object::from_static("shake_128")),
            b_kw("shake_128", make_shake_128),
        );
        d.insert(
            DictKey(Object::from_static("shake_256")),
            b_kw("shake_256", make_shake_256),
        );
        d.insert(
            DictKey(Object::from_static("blake2b")),
            b_kw("blake2b", make_blake2b),
        );
        d.insert(
            DictKey(Object::from_static("blake2s")),
            b_kw("blake2s", make_blake2s),
        );
        d.insert(
            DictKey(Object::from_static("pbkdf2_hmac")),
            b("pbkdf2_hmac", hash_pbkdf2),
        );
        d.insert(
            DictKey(Object::from_static("file_digest")),
            b_kw("file_digest", file_digest),
        );
    }
    Rc::new(PyModule {
        name: "hashlib".to_owned(),
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

/// Register a module-level constructor that tolerates the keyword
/// arguments CPython's hash constructors accept — chiefly
/// `usedforsecurity=` (a FIPS hint we don't need) and the `data`/`string`
/// digest seed.
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

/// The digest seed passed to a constructor: positional index 0, or the
/// `data=`/`string=` keyword. `usedforsecurity=` and other keywords are
/// ignored (we never link OpenSSL, so FIPS mode is moot).
fn seed_arg(args: &[Object], kwargs: &[(String, Object)]) -> Result<Option<Vec<u8>>, RuntimeError> {
    let obj = args.first().or_else(|| {
        kwargs
            .iter()
            .find(|(k, _)| k == "data" || k == "string")
            .map(|(_, v)| v)
    });
    match obj {
        None | Some(Object::None) => Ok(None),
        Some(o) => Ok(Some(bytes_of(o)?)),
    }
}

fn algos_set() -> Object {
    Object::new_set_from(
        [
            "md5",
            "sha1",
            "sha224",
            "sha256",
            "sha384",
            "sha512",
            "sha3_224",
            "sha3_256",
            "sha3_384",
            "sha3_512",
            "shake_128",
            "shake_256",
            "blake2b",
            "blake2s",
        ]
        .iter()
        .map(|s| Object::from_static(s)),
    )
}

fn hash_new(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let name = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => match kwargs.iter().find(|(k, _)| k == "name") {
            Some((_, Object::Str(s))) => s.to_string(),
            _ => return Err(type_error("new() missing required argument 'name' (pos 1)")),
        },
    };
    // `new(name, data)` takes the seed at position 1, or via `data=`/`string=`.
    let seed = args.get(1).cloned().or_else(|| {
        kwargs
            .iter()
            .find(|(k, _)| k == "data" || k == "string")
            .map(|(_, v)| v.clone())
    });
    let seed = seed.filter(|d| !matches!(d, Object::None));
    // A name registered in `__builtin_constructor_cache` shadows the builtin
    // table (CPython's `__get_builtin_constructor` checks the cache first).
    if let Some(ctor) = cache_lookup(&name) {
        if let Object::Builtin(bf) = &ctor {
            return match &seed {
                Some(d) => (bf.call)(std::slice::from_ref(d)),
                None => (bf.call)(&[]),
            };
        }
    }
    let mut hasher = make_by_name(&name)?;
    if let Some(data) = seed {
        hasher.update(&bytes_of(&data)?);
    }
    Ok(make_hasher_instance(hasher))
}

/// Look a digest name up in the shared `__builtin_constructor_cache`.
fn cache_lookup(name: &str) -> Option<Object> {
    CTOR_CACHE.with(|c| {
        c.borrow().as_ref().and_then(|cache| {
            cache
                .borrow()
                .get(&DictKey(Object::from_str(name.to_owned())))
                .cloned()
        })
    })
}

fn make_by_name(name: &str) -> Result<AnyHasher, RuntimeError> {
    Ok(match name.to_ascii_lowercase().as_str() {
        "md5" => AnyHasher::Md5(Md5::new()),
        "sha1" => AnyHasher::Sha1(Sha1::new()),
        "sha224" => AnyHasher::Sha224(Sha224::new()),
        "sha256" => AnyHasher::Sha256(Sha256::new()),
        "sha384" => AnyHasher::Sha384(Sha384::new()),
        "sha512" => AnyHasher::Sha512(Sha512::new()),
        "sha3_224" => AnyHasher::Sha3_224(Sha3_224::default()),
        "sha3_256" => AnyHasher::Sha3_256(Sha3_256::default()),
        "sha3_384" => AnyHasher::Sha3_384(Sha3_384::default()),
        "sha3_512" => AnyHasher::Sha3_512(Sha3_512::default()),
        "shake_128" => AnyHasher::Shake128(Shake128::default()),
        "shake_256" => AnyHasher::Shake256(Shake256::default()),
        "blake2b" => AnyHasher::Blake2b(Box::new(blake2b_simd::Params::new().to_state()), 64),
        "blake2s" => AnyHasher::Blake2s(Box::new(blake2s_simd::Params::new().to_state()), 32),
        other => return Err(value_error(format!("unsupported hash type: {other}"))),
    })
}

/// Parsed BLAKE2 keyword parameters (shared by `blake2b`/`blake2s`), with
/// the per-variant maxima CPython enforces (`b`: 64/16/16, `s`: 32/8/8).
struct Blake2Params {
    digest_size: usize,
    key: Vec<u8>,
    salt: Vec<u8>,
    person: Vec<u8>,
    fanout: u8,
    depth: u8,
    leaf_size: u32,
    node_offset: u64,
    node_depth: u8,
    inner_size: usize,
    last_node: bool,
    data: Option<Vec<u8>>,
}

fn kw<'a>(kwargs: &'a [(String, Object)], name: &str) -> Option<&'a Object> {
    kwargs.iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

fn parse_blake2_params(
    args: &[Object],
    kwargs: &[(String, Object)],
    max_digest: usize,
    max_key_salt_person: (usize, usize, usize),
) -> Result<Blake2Params, RuntimeError> {
    let (max_key, max_salt, max_person) = max_key_salt_person;
    let int_kw = |name: &str, default: i64| -> i64 {
        kw(kwargs, name).and_then(Object::as_i64).unwrap_or(default)
    };
    let bytes_kw = |name: &str| -> Result<Vec<u8>, RuntimeError> {
        match kw(kwargs, name) {
            None => Ok(Vec::new()),
            Some(o) => bytes_of(o),
        }
    };

    let digest_size = int_kw("digest_size", max_digest as i64);
    if digest_size < 1 || digest_size as usize > max_digest {
        return Err(value_error(format!(
            "digest_size for {} must be between 1 and {max_digest} bytes",
            if max_digest == 64 {
                "blake2b"
            } else {
                "blake2s"
            }
        )));
    }
    let key = bytes_kw("key")?;
    if key.len() > max_key {
        return Err(value_error(format!(
            "maximum key length is {max_key} bytes"
        )));
    }
    let salt = bytes_kw("salt")?;
    if salt.len() > max_salt {
        return Err(value_error(format!(
            "maximum salt length is {max_salt} bytes"
        )));
    }
    let person = bytes_kw("person")?;
    if person.len() > max_person {
        return Err(value_error(format!(
            "maximum person length is {max_person} bytes"
        )));
    }
    let inner_size = int_kw("inner_size", 0);
    if inner_size < 0 || inner_size as usize > max_digest {
        return Err(value_error(format!(
            "inner_size must be between 0 and {max_digest}"
        )));
    }
    // The seed (`data`) is positional index 0 or the `data`/`string` keyword.
    let data = seed_arg(args, kwargs)?;
    Ok(Blake2Params {
        digest_size: digest_size as usize,
        key,
        salt,
        person,
        fanout: int_kw("fanout", 1).clamp(0, 255) as u8,
        depth: int_kw("depth", 1).clamp(1, 255) as u8,
        leaf_size: int_kw("leaf_size", 0) as u32,
        node_offset: int_kw("node_offset", 0) as u64,
        node_depth: int_kw("node_depth", 0).clamp(0, 255) as u8,
        inner_size: inner_size as usize,
        last_node: kw(kwargs, "last_node")
            .map(Object::is_truthy)
            .unwrap_or(false),
        data,
    })
}

fn make_blake2b(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let p = parse_blake2_params(args, kwargs, 64, (64, 16, 16))?;
    let mut params = blake2b_simd::Params::new();
    params
        .hash_length(p.digest_size)
        .fanout(p.fanout)
        .max_depth(p.depth)
        .max_leaf_length(p.leaf_size)
        .node_offset(p.node_offset)
        .node_depth(p.node_depth)
        .inner_hash_length(p.inner_size)
        .last_node(p.last_node);
    if !p.key.is_empty() {
        params.key(&p.key);
    }
    if !p.salt.is_empty() {
        params.salt(&p.salt);
    }
    if !p.person.is_empty() {
        params.personal(&p.person);
    }
    let mut state = params.to_state();
    if let Some(d) = &p.data {
        state.update(d);
    }
    Ok(make_hasher_instance(AnyHasher::Blake2b(
        Box::new(state),
        p.digest_size,
    )))
}

fn make_blake2s(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let p = parse_blake2_params(args, kwargs, 32, (32, 8, 8))?;
    let mut params = blake2s_simd::Params::new();
    params
        .hash_length(p.digest_size)
        .fanout(p.fanout)
        .max_depth(p.depth)
        .max_leaf_length(p.leaf_size)
        .node_offset(p.node_offset)
        .node_depth(p.node_depth)
        .inner_hash_length(p.inner_size)
        .last_node(p.last_node);
    if !p.key.is_empty() {
        params.key(&p.key);
    }
    if !p.salt.is_empty() {
        params.salt(&p.salt);
    }
    if !p.person.is_empty() {
        params.personal(&p.person);
    }
    let mut state = params.to_state();
    if let Some(d) = &p.data {
        state.update(d);
    }
    Ok(make_hasher_instance(AnyHasher::Blake2s(
        Box::new(state),
        p.digest_size,
    )))
}

fn make_sha3_224(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    seeded(AnyHasher::Sha3_224(Sha3_224::default()), args, kwargs)
}

fn make_sha3_256(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    seeded(AnyHasher::Sha3_256(Sha3_256::default()), args, kwargs)
}

fn make_sha3_384(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    seeded(AnyHasher::Sha3_384(Sha3_384::default()), args, kwargs)
}

fn make_sha3_512(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    seeded(AnyHasher::Sha3_512(Sha3_512::default()), args, kwargs)
}

fn make_shake_128(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    seeded(AnyHasher::Shake128(Shake128::default()), args, kwargs)
}

fn make_shake_256(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    seeded(AnyHasher::Shake256(Shake256::default()), args, kwargs)
}

fn make_md5(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    seeded(AnyHasher::Md5(Md5::new()), args, kwargs)
}

fn make_sha1(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    seeded(AnyHasher::Sha1(Sha1::new()), args, kwargs)
}

fn make_sha224(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    seeded(AnyHasher::Sha224(Sha224::new()), args, kwargs)
}

fn make_sha256(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    seeded(AnyHasher::Sha256(Sha256::new()), args, kwargs)
}

fn make_sha384(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    seeded(AnyHasher::Sha384(Sha384::new()), args, kwargs)
}

fn make_sha512(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    seeded(AnyHasher::Sha512(Sha512::new()), args, kwargs)
}

fn seeded(
    mut hasher: AnyHasher,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    if let Some(bytes) = seed_arg(args, kwargs)? {
        hasher.update(&bytes);
    }
    Ok(make_hasher_instance(hasher))
}

fn bytes_of(obj: &Object) -> Result<Vec<u8>, RuntimeError> {
    // Hashers consume the buffer protocol only: `bytes`, `bytearray`,
    // `memoryview` (and their subclasses). A `str` is rejected with
    // CPython's exact message — `test_hmac` asserts `update("…")` raises.
    if let Some(v) = obj.as_bytes_view() {
        return Ok(v);
    }
    if let Some(v) = obj.native_value().as_ref().and_then(Object::as_bytes_view) {
        return Ok(v);
    }
    // Buffer-protocol fallback for objects that expose memory but aren't
    // bytes/bytearray/memoryview themselves (`array.array`, `mmap`, …):
    // snapshot `memoryview(obj)` through the running interpreter. `str`
    // never qualifies (CPython demands an explicit encode), so it is
    // rejected before the protocol is consulted.
    let is_str =
        matches!(obj, Object::Str(_)) || matches!(obj.native_value(), Some(Object::Str(_)));
    if !is_str {
        if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
            // SAFETY: published by the enclosing VM frame on this thread.
            let interp = unsafe { &mut *ptr };
            let mv_ctor = interp
                .builtins_dict()
                .borrow()
                .get(&DictKey(Object::from_static("memoryview")))
                .cloned();
            if let Some(mv_ctor) = mv_ctor {
                if let Ok(mv) = interp.call_object(mv_ctor, &[obj.clone()], &[]) {
                    if let Some(v) = mv.as_bytes_view() {
                        return Ok(v);
                    }
                }
            }
        }
    }
    Err(type_error("Strings must be encoded before hashing"))
}

/// `hashlib.file_digest(fileobj, digest, /, *, _bufsize=2**18)` — port of
/// CPython's helper. `digest` is a name (→ `new(name)`) or a zero-arg
/// constructor. Uses the `getbuffer()` fast path when available, otherwise
/// a `readinto()` loop. Needs the running interpreter to drive the file
/// object's Python-level methods.
fn file_digest(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| type_error("file_digest() requires a running interpreter"))?;
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };

    let fileobj = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("file_digest() missing required argument 'fileobj' (pos 1)"))?;
    let digest = args
        .get(1)
        .cloned()
        .ok_or_else(|| type_error("file_digest() missing required argument 'digest' (pos 2)"))?;

    let digestobj = match &digest {
        Object::Str(name) => make_hasher_instance(make_by_name(name)?),
        callable => interp.call_object(callable.clone(), &[], &[])?,
    };
    let update = interp.load_attr_public(&digestobj, "update")?;

    // BytesIO fast path: a single `update(getbuffer())`.
    if let Ok(getbuffer) = interp.load_attr_public(&fileobj, "getbuffer") {
        let buffer = interp.call_object(getbuffer, &[], &[])?;
        interp.call_object(update, &[buffer], &[])?;
        return Ok(digestobj);
    }

    // Only readable binary files implement `readinto()`. Text files lack
    // it; write-only files report `readable() == False`. CPython rejects
    // both (and `None`) with `ValueError` before reading.
    let readinto = interp.load_attr_public(&fileobj, "readinto").ok();
    let readable = match interp.load_attr_public(&fileobj, "readable") {
        Ok(r) => interp
            .call_object(r, &[], &[])
            .map(|v| v.is_truthy())
            .unwrap_or(false),
        Err(_) => false,
    };
    let Some(readinto) = readinto.filter(|_| readable) else {
        return Err(value_error("'fileobj' is not a readable binary file"));
    };

    let bufsize = kwargs
        .iter()
        .find(|(k, _)| k == "_bufsize")
        .and_then(|(_, v)| v.as_usize())
        .filter(|n| *n > 0)
        .unwrap_or(1 << 18);
    let buf = Object::new_bytearray(vec![0u8; bufsize]);
    loop {
        let size = interp.call_object(readinto.clone(), &[buf.clone()], &[])?;
        // `readinto()` returning `None` signals a non-blocking stream with
        // no data ready — CPython raises `BlockingIOError`.
        if matches!(size, Object::None) {
            return Err(blocking_io_error(
                "'fileobj.readinto()' returned None, file is in non-blocking mode",
            ));
        }
        let n = match size.as_usize() {
            Some(0) | None => break,
            Some(n) => n,
        };
        let chunk = {
            let all = buf.as_bytes_view().unwrap_or_default();
            Object::new_bytes(all[..n.min(all.len())].to_vec())
        };
        interp.call_object(update.clone(), &[chunk], &[])?;
    }
    Ok(digestobj)
}

fn hash_pbkdf2(args: &[Object]) -> Result<Object, RuntimeError> {
    use ::hmac::{Hmac, Mac};
    let name = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("pbkdf2_hmac: hash name must be str")),
    };
    let password = bytes_of(args.get(1).ok_or_else(|| type_error("missing password"))?)?;
    let salt = bytes_of(args.get(2).ok_or_else(|| type_error("missing salt"))?)?;
    let iterations = match args.get(3) {
        Some(Object::Int(n)) => *n as u32,
        _ => return Err(type_error("pbkdf2_hmac: iterations must be int")),
    };
    let dklen = match args.get(4) {
        Some(Object::Int(n)) => *n as usize,
        _ => 32,
    };

    // Per-algorithm specialisation keeps the trait bounds on `Hmac<D>`
    // satisfied locally — a generic helper trips the same `CoreWrapper`
    // bounds the upstream `hmac` crate uses internally.
    macro_rules! pbkdf2_for {
        ($digest:ty) => {{
            let hash_len = <$digest as Digest>::output_size();
            let blocks = dklen.div_ceil(hash_len);
            let mut out = vec![0u8; dklen];
            let mut t = vec![0u8; hash_len];
            let mut u = vec![0u8; hash_len];
            for i in 0..blocks {
                let block_index = (i as u32) + 1;
                let mut mac = <Hmac<$digest> as Mac>::new_from_slice(&password)
                    .expect("HMAC accepts any key length");
                mac.update(&salt);
                mac.update(&block_index.to_be_bytes());
                let first = mac.finalize().into_bytes();
                u.copy_from_slice(&first);
                t.copy_from_slice(&first);
                for _ in 1..iterations {
                    let mut mac = <Hmac<$digest> as Mac>::new_from_slice(&password)
                        .expect("HMAC accepts any key length");
                    mac.update(&u);
                    let next = mac.finalize().into_bytes();
                    u.copy_from_slice(&next);
                    for (acc, byte) in t.iter_mut().zip(u.iter()) {
                        *acc ^= *byte;
                    }
                }
                let start = i * hash_len;
                let end = (start + hash_len).min(out.len());
                out[start..end].copy_from_slice(&t[..end - start]);
            }
            out
        }};
    }

    let out = match name.to_ascii_lowercase().as_str() {
        "sha1" => pbkdf2_for!(Sha1),
        "sha256" => pbkdf2_for!(Sha256),
        "sha512" => pbkdf2_for!(Sha512),
        other => return Err(value_error(format!("unsupported hash: {other}"))),
    };
    Ok(Object::new_bytes(out))
}
