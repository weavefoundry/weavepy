//! The `hmac` built-in module.
//!
//! Surface: `hmac.new(key, msg=None, digestmod="sha256")` returns a
//! hasher with `update`/`digest`/`hexdigest`/`copy` (matching the
//! `hashlib` hasher shape). `hmac.compare_digest(a, b)` is the
//! constant-time comparison user code reaches for.
//!
//! Backed by the `hmac` crate over RustCrypto digests (sha2 / sha1 /
//! md-5). Algorithms outside that set (blake2, shake) raise
//! `ValueError`, matching CPython on systems without the corresponding
//! OpenSSL backend.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::collections::HashMap;

use ::hmac::{Hmac, Mac};
use md5::Md5;
use sha1::Sha1;
use sha2::{Sha224, Sha256, Sha384, Sha512};

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::types::{PyInstance, TypeObject};

enum AnyMac {
    Md5(Hmac<Md5>),
    Sha1(Hmac<Sha1>),
    Sha224(Hmac<Sha224>),
    Sha256(Hmac<Sha256>),
    Sha384(Hmac<Sha384>),
    Sha512(Hmac<Sha512>),
}

impl AnyMac {
    fn update(&mut self, data: &[u8]) {
        match self {
            Self::Md5(m) => m.update(data),
            Self::Sha1(m) => m.update(data),
            Self::Sha224(m) => m.update(data),
            Self::Sha256(m) => m.update(data),
            Self::Sha384(m) => m.update(data),
            Self::Sha512(m) => m.update(data),
        }
    }

    fn digest(&self) -> Vec<u8> {
        match self {
            Self::Md5(m) => m.clone().finalize().into_bytes().to_vec(),
            Self::Sha1(m) => m.clone().finalize().into_bytes().to_vec(),
            Self::Sha224(m) => m.clone().finalize().into_bytes().to_vec(),
            Self::Sha256(m) => m.clone().finalize().into_bytes().to_vec(),
            Self::Sha384(m) => m.clone().finalize().into_bytes().to_vec(),
            Self::Sha512(m) => m.clone().finalize().into_bytes().to_vec(),
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
            Self::Md5(m) => Self::Md5(m.clone()),
            Self::Sha1(m) => Self::Sha1(m.clone()),
            Self::Sha224(m) => Self::Sha224(m.clone()),
            Self::Sha256(m) => Self::Sha256(m.clone()),
            Self::Sha384(m) => Self::Sha384(m.clone()),
            Self::Sha512(m) => Self::Sha512(m.clone()),
        }
    }
}

struct MacRegEntry {
    state: Rc<RefCell<AnyMac>>,
    key: Vec<u8>,
    digest_name: String,
}

thread_local! {
    static MAC_REGISTRY: RefCell<HashMap<i64, Rc<MacRegEntry>>> =
        RefCell::new(HashMap::new());
    static MAC_NEXT_ID: RefCell<i64> = const { RefCell::new(1) };
    static MAC_CLASS: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
}

fn register_mac(state: AnyMac, key: Vec<u8>, digest_name: String) -> i64 {
    let id = MAC_NEXT_ID.with(|c| {
        let mut v = c.borrow_mut();
        let id = *v;
        *v += 1;
        id
    });
    MAC_REGISTRY.with(|r| {
        r.borrow_mut().insert(
            id,
            Rc::new(MacRegEntry {
                state: Rc::new(RefCell::new(state)),
                key,
                digest_name,
            }),
        );
    });
    id
}

fn lookup_mac(id: i64) -> Result<Rc<MacRegEntry>, RuntimeError> {
    MAC_REGISTRY.with(|r| {
        r.borrow()
            .get(&id)
            .cloned()
            .ok_or_else(|| value_error("hmac: stale mac handle"))
    })
}

fn instance_handle(args: &[Object]) -> Result<i64, RuntimeError> {
    let inst = match args.first() {
        Some(Object::Instance(i)) => i.clone(),
        _ => return Err(type_error("expected hmac instance")),
    };
    let handle_obj = inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_handle")))
        .cloned()
        .ok_or_else(|| value_error("hmac missing _handle"))?;
    match handle_obj {
        Object::Int(v) => Ok(v),
        _ => Err(type_error("hmac _handle must be int")),
    }
}

fn make_mac_instance(state: AnyMac, key: Vec<u8>, digest_name: String) -> Object {
    let (name, digest_size, block_size) = (state.name(), state.digest_size(), state.block_size());
    let id = register_mac(state, key, digest_name);
    let cls = hmac_class();
    let inst = PyInstance::new(cls);
    {
        let mut d = inst.dict.borrow_mut();
        d.insert(DictKey(Object::from_static("_handle")), Object::Int(id));
        d.insert(
            DictKey(Object::from_static("name")),
            Object::from_str(format!("hmac-{name}")),
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

fn hmac_class() -> Rc<TypeObject> {
    MAC_CLASS.with(|slot| {
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
                        call: Box::new($body),
                    })),
                );
            };
        }
        method!("update", mac_update);
        method!("digest", mac_digest);
        method!("hexdigest", mac_hexdigest);
        method!("copy", mac_copy);
        let cls = TypeObject::new_user("hmac.HMAC", vec![bt.object_.clone()], dict)
            .expect("hmac class must linearise");
        *slot.borrow_mut() = Some(cls.clone());
        cls
    })
}

fn mac_update(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = instance_handle(args)?;
    let data = args
        .get(1)
        .ok_or_else(|| type_error("update: missing data"))?;
    let bytes = bytes_of(data)?;
    let entry = lookup_mac(id)?;
    entry.state.borrow_mut().update(&bytes);
    Ok(Object::None)
}

fn mac_digest(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = instance_handle(args)?;
    let entry = lookup_mac(id)?;
    let bytes = entry.state.borrow().digest();
    Ok(Object::new_bytes(bytes))
}

fn mac_hexdigest(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = instance_handle(args)?;
    let entry = lookup_mac(id)?;
    let bytes = entry.state.borrow().digest();
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        write!(out, "{b:02x}").unwrap();
    }
    Ok(Object::from_str(out))
}

fn mac_copy(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = instance_handle(args)?;
    let entry = lookup_mac(id)?;
    let cloned = entry.state.borrow().clone_state();
    Ok(make_mac_instance(
        cloned,
        entry.key.clone(),
        entry.digest_name.clone(),
    ))
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("hmac"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Keyed-Hashing for Message Authentication."),
        );
        d.insert(DictKey(Object::from_static("new")), b("new", hmac_new));
        d.insert(
            DictKey(Object::from_static("digest")),
            b("digest", hmac_digest),
        );
        d.insert(
            DictKey(Object::from_static("compare_digest")),
            b("compare_digest", hmac_compare),
        );
        d.insert(DictKey(Object::from_static("HMAC")), b("HMAC", hmac_new));
    }
    Rc::new(PyModule {
        name: "hmac".to_owned(),
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

fn bytes_of(obj: &Object) -> Result<Vec<u8>, RuntimeError> {
    match obj {
        Object::Bytes(b) => Ok(b.to_vec()),
        Object::ByteArray(b) => Ok(b.borrow().clone()),
        Object::Str(s) => Ok(s.as_bytes().to_vec()),
        _ => Err(type_error("expected bytes-like object")),
    }
}

fn hmac_new(args: &[Object]) -> Result<Object, RuntimeError> {
    let key = bytes_of(args.first().ok_or_else(|| type_error("missing key"))?)?;
    let msg = args.get(1).map(bytes_of).transpose()?;
    let name = match args.get(2) {
        Some(Object::Str(s)) => s.to_string(),
        Some(Object::None) | None => "sha256".to_string(),
        _ => return Err(type_error("digestmod must be str")),
    };
    let mut mac = build_mac(&name, &key)?;
    if let Some(m) = msg {
        mac.update(&m);
    }
    Ok(make_mac_instance(mac, key, name))
}

fn build_mac(name: &str, key: &[u8]) -> Result<AnyMac, RuntimeError> {
    Ok(match name.to_ascii_lowercase().as_str() {
        "md5" => AnyMac::Md5(
            <Hmac<Md5> as Mac>::new_from_slice(key).map_err(|e| value_error(e.to_string()))?,
        ),
        "sha1" => AnyMac::Sha1(
            <Hmac<Sha1> as Mac>::new_from_slice(key).map_err(|e| value_error(e.to_string()))?,
        ),
        "sha224" => AnyMac::Sha224(
            <Hmac<Sha224> as Mac>::new_from_slice(key).map_err(|e| value_error(e.to_string()))?,
        ),
        "sha256" => AnyMac::Sha256(
            <Hmac<Sha256> as Mac>::new_from_slice(key).map_err(|e| value_error(e.to_string()))?,
        ),
        "sha384" => AnyMac::Sha384(
            <Hmac<Sha384> as Mac>::new_from_slice(key).map_err(|e| value_error(e.to_string()))?,
        ),
        "sha512" => AnyMac::Sha512(
            <Hmac<Sha512> as Mac>::new_from_slice(key).map_err(|e| value_error(e.to_string()))?,
        ),
        other => return Err(value_error(format!("unsupported digest: {other}"))),
    })
}

fn hmac_digest(args: &[Object]) -> Result<Object, RuntimeError> {
    let key = bytes_of(args.first().ok_or_else(|| type_error("missing key"))?)?;
    let msg = bytes_of(args.get(1).ok_or_else(|| type_error("missing msg"))?)?;
    let name = match args.get(2) {
        Some(Object::Str(s)) => s.to_string(),
        _ => "sha256".to_string(),
    };
    let mut mac = build_mac(&name, &key)?;
    mac.update(&msg);
    Ok(Object::new_bytes(mac.digest()))
}

fn hmac_compare(args: &[Object]) -> Result<Object, RuntimeError> {
    let a = bytes_of(args.first().ok_or_else(|| type_error("missing a"))?)?;
    let b = bytes_of(args.get(1).ok_or_else(|| type_error("missing b"))?)?;
    if a.len() != b.len() {
        return Ok(Object::Bool(false));
    }
    // Constant-time comparison — XOR every byte and accumulate.
    let acc = a
        .iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y));
    Ok(Object::Bool(acc == 0))
}
