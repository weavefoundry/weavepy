//! The `_https` accelerator module — RFC 0023.
//!
//! Provides a single high-level entry point that performs a complete
//! HTTPS round-trip against a real TLS endpoint:
//!
//! ```python
//! import _https
//! status, headers, body = _https.request("GET", "example.com", 443, "/", {}, b"")
//! ```
//!
//! The wrapper `urllib.request` (frozen Python) is updated to defer
//! to this module for `https://` URLs.

use std::cell::RefCell;
use std::rc::Rc;

use crate::error::{type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_https"),
        );
        d.insert(
            DictKey(Object::from_static("request")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "request",
                call: Box::new(https_request),
            })),
        );
        d.insert(
            DictKey(Object::from_static("open")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "open",
                call: Box::new(https_open),
            })),
        );
        d.insert(
            DictKey(Object::from_static("send")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "send",
                call: Box::new(https_send),
            })),
        );
        d.insert(
            DictKey(Object::from_static("recv")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "recv",
                call: Box::new(https_recv),
            })),
        );
        d.insert(
            DictKey(Object::from_static("close")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "close",
                call: Box::new(https_close),
            })),
        );
    }
    Rc::new(PyModule {
        name: "_https".to_owned(),
        filename: None,
        dict,
    })
}

fn https_open(args: &[Object]) -> Result<Object, RuntimeError> {
    let host = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("_https.open: host must be str")),
    };
    let port = match args.get(1) {
        Some(Object::Int(p)) => *p as u16,
        _ => 443,
    };
    let id = crate::stdlib::ssl_real::open_tls(&host, port)?;
    Ok(Object::Int(id))
}

fn https_send(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = match args.first() {
        Some(Object::Int(i)) => *i,
        _ => return Err(type_error("_https.send: id must be int")),
    };
    let data: Vec<u8> = match args.get(1) {
        Some(Object::Bytes(b)) => b.to_vec(),
        Some(Object::ByteArray(b)) => b.borrow().clone(),
        Some(Object::Str(s)) => s.as_bytes().to_vec(),
        _ => return Err(type_error("_https.send: data must be bytes-like")),
    };
    let n = crate::stdlib::ssl_real::send(id, &data)?;
    Ok(Object::Int(n as i64))
}

fn https_recv(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = match args.first() {
        Some(Object::Int(i)) => *i,
        _ => return Err(type_error("_https.recv: id must be int")),
    };
    let n = match args.get(1) {
        Some(Object::Int(i)) => *i as usize,
        _ => 8192,
    };
    let buf = crate::stdlib::ssl_real::recv(id, n)?;
    Ok(Object::new_bytes(buf))
}

fn https_close(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = match args.first() {
        Some(Object::Int(i)) => *i,
        _ => return Err(type_error("_https.close: id must be int")),
    };
    crate::stdlib::ssl_real::close(id);
    Ok(Object::None)
}

/// One-shot HTTPS request: open → send request line + headers + body →
/// read everything → close.
fn https_request(args: &[Object]) -> Result<Object, RuntimeError> {
    let method = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("_https.request: method must be str")),
    };
    let host = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("_https.request: host must be str")),
    };
    let port = match args.get(2) {
        Some(Object::Int(p)) => *p as u16,
        _ => 443,
    };
    let path = match args.get(3) {
        Some(Object::Str(s)) => s.to_string(),
        _ => "/".to_owned(),
    };
    let headers_obj = args.get(4).cloned().unwrap_or(Object::None);
    let body: Vec<u8> = match args.get(5) {
        Some(Object::Bytes(b)) => b.to_vec(),
        Some(Object::ByteArray(b)) => b.borrow().clone(),
        Some(Object::Str(s)) => s.as_bytes().to_vec(),
        Some(Object::None) | None => Vec::new(),
        _ => {
            return Err(type_error(
                "_https.request: body must be bytes-like or None",
            ))
        }
    };

    let id = crate::stdlib::ssl_real::open_tls(&host, port)?;
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    let mut has_content_length = false;
    if let Object::Dict(d) = &headers_obj {
        for (k, v) in d.borrow().iter() {
            let k_s = match &k.0 {
                Object::Str(s) => s.to_string(),
                _ => continue,
            };
            let v_s = match v {
                Object::Str(s) => s.to_string(),
                Object::Int(i) => i.to_string(),
                _ => continue,
            };
            if k_s.eq_ignore_ascii_case("content-length") {
                has_content_length = true;
            }
            req.push_str(&format!("{k_s}: {v_s}\r\n"));
        }
    }
    if !body.is_empty() && !has_content_length {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    crate::stdlib::ssl_real::send(id, req.as_bytes())?;
    if !body.is_empty() {
        crate::stdlib::ssl_real::send(id, &body)?;
    }

    let mut all = Vec::with_capacity(4096);
    loop {
        let chunk = crate::stdlib::ssl_real::recv(id, 8192)?;
        if chunk.is_empty() {
            break;
        }
        all.extend_from_slice(&chunk);
    }
    crate::stdlib::ssl_real::close(id);

    let (status, headers, body) = parse_response(&all);
    Ok(Object::new_tuple(vec![
        Object::Int(i64::from(status)),
        headers,
        Object::new_bytes(body),
    ]))
}

/// Parse an HTTP/1.x response into (status, headers-dict, body-bytes).
fn parse_response(buf: &[u8]) -> (u16, Object, Vec<u8>) {
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n");
    let (head, body) = match split {
        Some(i) => (&buf[..i], &buf[i + 4..]),
        None => (buf, &[][..]),
    };
    let head_s = String::from_utf8_lossy(head);
    let mut lines = head_s.split("\r\n");
    let status_line = lines.next().unwrap_or("HTTP/1.1 0 ");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        for line in lines {
            if let Some(colon) = line.find(':') {
                let k = line[..colon].trim().to_owned();
                let v = line[colon + 1..].trim().to_owned();
                d.insert(DictKey(Object::from_str(k)), Object::from_str(v));
            }
        }
    }
    let body = decode_body(&dict, body);
    (status, Object::Dict(dict), body)
}

fn decode_body(headers: &Rc<RefCell<DictData>>, raw: &[u8]) -> Vec<u8> {
    let is_chunked = headers.borrow().iter().any(|(k, v)| {
        matches!(&k.0, Object::Str(s) if s.eq_ignore_ascii_case("transfer-encoding"))
            && matches!(v, Object::Str(s) if s.to_ascii_lowercase().contains("chunked"))
    });
    if !is_chunked {
        return raw.to_vec();
    }
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0usize;
    while i < raw.len() {
        let mut j = i;
        while j + 1 < raw.len() && !(raw[j] == b'\r' && raw[j + 1] == b'\n') {
            j += 1;
        }
        if j + 1 >= raw.len() {
            break;
        }
        let size_hex = std::str::from_utf8(&raw[i..j]).unwrap_or("0");
        let size = usize::from_str_radix(size_hex.trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        i = j + 2;
        if i + size > raw.len() {
            break;
        }
        out.extend_from_slice(&raw[i..i + size]);
        i += size;
        // skip trailing CRLF
        if i + 1 < raw.len() && raw[i] == b'\r' && raw[i + 1] == b'\n' {
            i += 2;
        }
    }
    out
}
