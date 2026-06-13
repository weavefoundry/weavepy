//! The `hashlib` built-in module.
//!
//! Backed by the RustCrypto family: `sha2`, `sha1`, `md-5`. Each
//! hasher is exposed as a tiny dict carrying its incremental state
//! plus `update`, `digest`, `hexdigest`, `copy`, `name`,
//! `digest_size`, `block_size`.
//!
//! Coverage: `md5`, `sha1`, `sha224`, `sha256`, `sha384`, `sha512`.
//! `pbkdf2_hmac` is implemented for HMAC-SHA{1,256,512}.
//!
//! Deferred: `blake2b`/`blake2s`, `shake_*`, `scrypt`, the
//! `usedforsecurity=False` parameter (accepted-and-ignored on
//! `new("md5", ...)`). FIPS-mode handling is not relevant — we don't
//! link OpenSSL.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::collections::HashMap;

use digest::Digest;
use md5::Md5;
use sha1::Sha1;
use sha2::{Sha224, Sha256, Sha384, Sha512};

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::types::{PyInstance, TypeObject};

/// Erased dispatch table. Each variant carries its own RustCrypto
/// digest type; we keep them under one enum so the hasher dict can
/// own a single trait-object handle.
enum AnyHasher {
    Md5(Md5),
    Sha1(Sha1),
    Sha224(Sha224),
    Sha256(Sha256),
    Sha384(Sha384),
    Sha512(Sha512),
}

impl AnyHasher {
    fn update(&mut self, data: &[u8]) {
        match self {
            Self::Md5(h) => h.update(data),
            Self::Sha1(h) => h.update(data),
            Self::Sha224(h) => h.update(data),
            Self::Sha256(h) => h.update(data),
            Self::Sha384(h) => h.update(data),
            Self::Sha512(h) => h.update(data),
        }
    }

    fn digest(&self) -> Vec<u8> {
        match self {
            Self::Md5(h) => h.clone().finalize().to_vec(),
            Self::Sha1(h) => h.clone().finalize().to_vec(),
            Self::Sha224(h) => h.clone().finalize().to_vec(),
            Self::Sha256(h) => h.clone().finalize().to_vec(),
            Self::Sha384(h) => h.clone().finalize().to_vec(),
            Self::Sha512(h) => h.clone().finalize().to_vec(),
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
        }
    }

    fn digest_size(&self) -> i64 {
        match self {
            Self::Md5(_) => 16,
            Self::Sha1(_) => 20,
            Self::Sha224(_) => 28,
            Self::Sha256(_) => 32,
            Self::Sha384(_) => 48,
            Self::Sha512(_) => 64,
        }
    }

    fn block_size(&self) -> i64 {
        match self {
            Self::Md5(_) | Self::Sha1(_) | Self::Sha224(_) | Self::Sha256(_) => 64,
            Self::Sha384(_) | Self::Sha512(_) => 128,
        }
    }

    fn clone_state(&self) -> Self {
        match self {
            Self::Md5(h) => Self::Md5(h.clone()),
            Self::Sha1(h) => Self::Sha1(h.clone()),
            Self::Sha224(h) => Self::Sha224(h.clone()),
            Self::Sha256(h) => Self::Sha256(h.clone()),
            Self::Sha384(h) => Self::Sha384(h.clone()),
            Self::Sha512(h) => Self::Sha512(h.clone()),
        }
    }
}

thread_local! {
    static HASHER_REGISTRY: RefCell<HashMap<i64, Rc<RefCell<AnyHasher>>>> =
        RefCell::new(HashMap::new());
    static HASHER_NEXT_ID: RefCell<i64> = const { RefCell::new(1) };
    static HASHER_CLASS: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
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

fn hasher_digest(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = instance_handle(args)?;
    let state = lookup_hasher(id)?;
    let bytes = state.borrow().digest();
    Ok(Object::new_bytes(bytes))
}

fn hasher_hexdigest(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = instance_handle(args)?;
    let state = lookup_hasher(id)?;
    let bytes = state.borrow().digest();
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
        d.insert(DictKey(Object::from_static("new")), b("new", hash_new));
        d.insert(DictKey(Object::from_static("md5")), b("md5", make_md5));
        d.insert(DictKey(Object::from_static("sha1")), b("sha1", make_sha1));
        d.insert(
            DictKey(Object::from_static("sha224")),
            b("sha224", make_sha224),
        );
        d.insert(
            DictKey(Object::from_static("sha256")),
            b("sha256", make_sha256),
        );
        d.insert(
            DictKey(Object::from_static("sha384")),
            b("sha384", make_sha384),
        );
        d.insert(
            DictKey(Object::from_static("sha512")),
            b("sha512", make_sha512),
        );
        d.insert(
            DictKey(Object::from_static("pbkdf2_hmac")),
            b("pbkdf2_hmac", hash_pbkdf2),
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

fn algos_set() -> Object {
    Object::new_list(
        ["md5", "sha1", "sha224", "sha256", "sha384", "sha512"]
            .iter()
            .map(|s| Object::from_static(s))
            .collect(),
    )
}

fn hash_new(args: &[Object]) -> Result<Object, RuntimeError> {
    let name = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("hashlib.new: name must be str")),
    };
    let mut hasher = make_by_name(&name)?;
    if let Some(data) = args.get(1) {
        let bytes = bytes_of(data)?;
        hasher.update(&bytes);
    }
    Ok(make_hasher_instance(hasher))
}

fn make_by_name(name: &str) -> Result<AnyHasher, RuntimeError> {
    Ok(match name.to_ascii_lowercase().as_str() {
        "md5" => AnyHasher::Md5(Md5::new()),
        "sha1" => AnyHasher::Sha1(Sha1::new()),
        "sha224" => AnyHasher::Sha224(Sha224::new()),
        "sha256" => AnyHasher::Sha256(Sha256::new()),
        "sha384" => AnyHasher::Sha384(Sha384::new()),
        "sha512" => AnyHasher::Sha512(Sha512::new()),
        other => return Err(value_error(format!("unsupported hash type: {other}"))),
    })
}

fn make_md5(args: &[Object]) -> Result<Object, RuntimeError> {
    seeded(AnyHasher::Md5(Md5::new()), args)
}

fn make_sha1(args: &[Object]) -> Result<Object, RuntimeError> {
    seeded(AnyHasher::Sha1(Sha1::new()), args)
}

fn make_sha224(args: &[Object]) -> Result<Object, RuntimeError> {
    seeded(AnyHasher::Sha224(Sha224::new()), args)
}

fn make_sha256(args: &[Object]) -> Result<Object, RuntimeError> {
    seeded(AnyHasher::Sha256(Sha256::new()), args)
}

fn make_sha384(args: &[Object]) -> Result<Object, RuntimeError> {
    seeded(AnyHasher::Sha384(Sha384::new()), args)
}

fn make_sha512(args: &[Object]) -> Result<Object, RuntimeError> {
    seeded(AnyHasher::Sha512(Sha512::new()), args)
}

fn seeded(mut hasher: AnyHasher, args: &[Object]) -> Result<Object, RuntimeError> {
    if let Some(data) = args.first() {
        let bytes = bytes_of(data)?;
        hasher.update(&bytes);
    }
    Ok(make_hasher_instance(hasher))
}

fn bytes_of(obj: &Object) -> Result<Vec<u8>, RuntimeError> {
    match obj {
        Object::Bytes(b) => Ok(b.to_vec()),
        Object::ByteArray(b) => Ok(b.borrow().clone()),
        Object::Str(s) => Ok(s.as_bytes().to_vec()),
        _ => Err(type_error("expected bytes-like object")),
    }
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
