//! The `uuid` built-in module.
//!
//! Generates RFC 4122 UUIDs: `uuid4()` reads OS entropy via the same
//! `/dev/urandom` path as `secrets`; `uuid1()` uses node + clock
//! state; `uuid3` and `uuid5` are namespace-name UUIDs derived from
//! MD5 / SHA-1.
//!
//! The user-visible `UUID` class is intentionally small — a dict
//! exposing `bytes`, `hex`, `int`, `__str__`, `version`, and the
//! common `urn` shortcut. Code that needs the full CPython surface
//! (`fields`, `time_low`/`time_mid`/…) can reach into the byte
//! payload directly.

use crate::sync::Rc;
use crate::sync::RefCell;

use digest::Digest;
use md5::Md5;
use sha1::Sha1;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("uuid"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("UUID objects (universally unique identifiers)."),
        );
        d.insert(DictKey(Object::from_static("uuid1")), b("uuid1", uuid1));
        d.insert(DictKey(Object::from_static("uuid3")), b("uuid3", uuid3));
        d.insert(DictKey(Object::from_static("uuid4")), b("uuid4", uuid4));
        d.insert(DictKey(Object::from_static("uuid5")), b("uuid5", uuid5));
        d.insert(DictKey(Object::from_static("UUID")), b("UUID", uuid_ctor));

        // Common namespaces (RFC 4122 appendix C).
        d.insert(
            DictKey(Object::from_static("NAMESPACE_DNS")),
            uuid_from_bytes([
                0x6b, 0xa7, 0xb8, 0x10, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4,
                0x30, 0xc8,
            ]),
        );
        d.insert(
            DictKey(Object::from_static("NAMESPACE_URL")),
            uuid_from_bytes([
                0x6b, 0xa7, 0xb8, 0x11, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4,
                0x30, 0xc8,
            ]),
        );
        d.insert(
            DictKey(Object::from_static("NAMESPACE_OID")),
            uuid_from_bytes([
                0x6b, 0xa7, 0xb8, 0x12, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4,
                0x30, 0xc8,
            ]),
        );
        d.insert(
            DictKey(Object::from_static("NAMESPACE_X500")),
            uuid_from_bytes([
                0x6b, 0xa7, 0xb8, 0x14, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4,
                0x30, 0xc8,
            ]),
        );
    }
    Rc::new(PyModule {
        name: "uuid".to_owned(),
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

fn os_random_bytes(out: &mut [u8]) -> Result<(), RuntimeError> {
    #[cfg(unix)]
    {
        use std::fs::File;
        use std::io::Read;
        let mut f = File::open("/dev/urandom").map_err(|e| value_error(e.to_string()))?;
        f.read_exact(out).map_err(|e| value_error(e.to_string()))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0xDEAD_BEEF_FEED_FACE);
        let mut state = seed;
        for byte in out.iter_mut() {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *byte = (state >> 33) as u8;
        }
        Ok(())
    }
}

fn uuid_from_bytes(bytes: [u8; 16]) -> Object {
    let hex = format_uuid(&bytes);
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("bytes")),
            Object::new_bytes(bytes.to_vec()),
        );
        d.insert(
            DictKey(Object::from_static("hex")),
            Object::from_str(hex.replace('-', "")),
        );
        d.insert(
            DictKey(Object::from_static("urn")),
            Object::from_str(format!("urn:uuid:{hex}")),
        );
        d.insert(
            DictKey(Object::from_static("version")),
            Object::Int(i64::from((bytes[6] >> 4) & 0x0F)),
        );
        d.insert(
            DictKey(Object::from_static("__str__")),
            Object::from_str(hex.clone()),
        );
        d.insert(
            DictKey(Object::from_static("__repr__")),
            Object::from_str(hex),
        );
    }
    Object::Dict(dict)
}

fn format_uuid(bytes: &[u8; 16]) -> String {
    let mut s = String::with_capacity(36);
    for (i, b) in bytes.iter().enumerate() {
        use std::fmt::Write;
        write!(s, "{b:02x}").unwrap();
        if matches!(i, 3 | 5 | 7 | 9) {
            s.push('-');
        }
    }
    s
}

fn uuid4(_args: &[Object]) -> Result<Object, RuntimeError> {
    let mut bytes = [0u8; 16];
    os_random_bytes(&mut bytes)?;
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    Ok(uuid_from_bytes(bytes))
}

fn uuid1(_args: &[Object]) -> Result<Object, RuntimeError> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u128)
        .unwrap_or(0);
    // UUID v1: 60-bit timestamp in 100-ns intervals since 1582-10-15.
    let intervals_since_1582 = (nanos / 100).wrapping_add(0x01B2_1DD2_1381_4000u128) as u64;
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&(intervals_since_1582 as u32).to_be_bytes());
    bytes[4..6].copy_from_slice(&((intervals_since_1582 >> 32) as u16).to_be_bytes());
    let mid_hi = (((intervals_since_1582 >> 48) as u16) & 0x0FFF) | 0x1000;
    bytes[6..8].copy_from_slice(&mid_hi.to_be_bytes());
    // Random clock seq + node identifier.
    let mut tail = [0u8; 8];
    os_random_bytes(&mut tail)?;
    bytes[8..16].copy_from_slice(&tail);
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    Ok(uuid_from_bytes(bytes))
}

fn uuid3(args: &[Object]) -> Result<Object, RuntimeError> {
    let (ns_bytes, name) = parse_ns_name(args)?;
    let mut h = Md5::new();
    h.update(ns_bytes);
    h.update(name.as_bytes());
    let out = h.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&out);
    bytes[6] = (bytes[6] & 0x0F) | 0x30;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    Ok(uuid_from_bytes(bytes))
}

fn uuid5(args: &[Object]) -> Result<Object, RuntimeError> {
    let (ns_bytes, name) = parse_ns_name(args)?;
    let mut h = Sha1::new();
    h.update(ns_bytes);
    h.update(name.as_bytes());
    let out = h.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&out[..16]);
    bytes[6] = (bytes[6] & 0x0F) | 0x50;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    Ok(uuid_from_bytes(bytes))
}

fn parse_ns_name(args: &[Object]) -> Result<([u8; 16], String), RuntimeError> {
    let ns = args
        .first()
        .ok_or_else(|| type_error("missing namespace"))?;
    let name = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("name must be str")),
    };
    let ns_bytes = match ns {
        Object::Dict(d) => match d
            .borrow()
            .get(&DictKey(Object::from_static("bytes")))
            .cloned()
        {
            Some(Object::Bytes(b)) if b.len() == 16 => {
                let mut arr = [0u8; 16];
                arr.copy_from_slice(&b);
                arr
            }
            _ => return Err(type_error("namespace must be a UUID")),
        },
        _ => return Err(type_error("namespace must be a UUID")),
    };
    Ok((ns_bytes, name))
}

fn uuid_ctor(args: &[Object]) -> Result<Object, RuntimeError> {
    // Accept `hex=...` shape or first positional hex string.
    let hex = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("UUID() expects a hex string")),
    };
    let clean: String = hex.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if clean.len() != 32 {
        return Err(value_error("UUID hex must be 32 hex digits"));
    }
    let mut bytes = [0u8; 16];
    for i in 0..16 {
        bytes[i] = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16)
            .map_err(|_| value_error("invalid UUID hex"))?;
    }
    Ok(uuid_from_bytes(bytes))
}
