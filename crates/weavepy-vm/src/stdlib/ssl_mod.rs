//! The `ssl` built-in module — surface only.
//!
//! WeavePy currently does **not** link a TLS engine. We ship the
//! surface so that `import ssl` works, `SSLContext()` returns a
//! useful-looking object, and code that branches on
//! `hasattr(ssl, 'create_default_context')` finds the entry points.
//! Actually establishing a TLS handshake raises
//! `NotImplementedError` — explicit failure beats silent fallback to
//! plaintext.
//!
//! When the rustls integration lands (tracked in RFC 0017 Future
//! Work) the `wrap_socket` path will gain real handshake / read /
//! write code without changing the surface seen by user code.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("ssl"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("TLS / SSL wrapper for socket objects."),
        );
        d.insert(
            DictKey(Object::from_static("OPENSSL_VERSION")),
            Object::from_static("rustls (placeholder)"),
        );
        d.insert(
            DictKey(Object::from_static("OPENSSL_VERSION_INFO")),
            Object::new_tuple(vec![
                Object::Int(0),
                Object::Int(0),
                Object::Int(0),
                Object::Int(0),
                Object::Int(0),
            ]),
        );
        d.insert(
            DictKey(Object::from_static("OPENSSL_VERSION_NUMBER")),
            Object::Int(0),
        );
        d.insert(
            DictKey(Object::from_static("HAS_TLSv1_3")),
            Object::Bool(true),
        );
        d.insert(DictKey(Object::from_static("HAS_SNI")), Object::Bool(true));
        d.insert(DictKey(Object::from_static("HAS_ALPN")), Object::Bool(true));
        d.insert(DictKey(Object::from_static("PROTOCOL_TLS")), Object::Int(2));
        d.insert(
            DictKey(Object::from_static("PROTOCOL_TLS_CLIENT")),
            Object::Int(16),
        );
        d.insert(
            DictKey(Object::from_static("PROTOCOL_TLS_SERVER")),
            Object::Int(17),
        );
        d.insert(DictKey(Object::from_static("CERT_NONE")), Object::Int(0));
        d.insert(
            DictKey(Object::from_static("CERT_OPTIONAL")),
            Object::Int(1),
        );
        d.insert(
            DictKey(Object::from_static("CERT_REQUIRED")),
            Object::Int(2),
        );
        d.insert(
            DictKey(Object::from_static("VerifyMode")),
            Object::from_static("VerifyMode"),
        );
        d.insert(DictKey(Object::from_static("Purpose")), purpose_dict());
        d.insert(
            DictKey(Object::from_static("SSLError")),
            Object::Type(crate::builtin_types::builtin_types().os_error.clone()),
        );
        d.insert(
            DictKey(Object::from_static("CertificateError")),
            Object::Type(crate::builtin_types::builtin_types().value_error.clone()),
        );
        d.insert(
            DictKey(Object::from_static("create_default_context")),
            b("create_default_context", create_default_context),
        );
        d.insert(
            DictKey(Object::from_static("_create_default_https_context")),
            b("_create_default_https_context", create_default_context),
        );
        d.insert(
            DictKey(Object::from_static("SSLContext")),
            b("SSLContext", create_default_context),
        );
        d.insert(
            DictKey(Object::from_static("get_default_verify_paths")),
            b("get_default_verify_paths", get_default_verify_paths),
        );
        d.insert(
            DictKey(Object::from_static("match_hostname")),
            b("match_hostname", match_hostname_stub),
        );
    }
    Rc::new(PyModule {
        name: "ssl".to_owned(),
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

fn purpose_dict() -> Object {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("SERVER_AUTH")),
            Object::from_static("server_auth"),
        );
        d.insert(
            DictKey(Object::from_static("CLIENT_AUTH")),
            Object::from_static("client_auth"),
        );
    }
    Object::Dict(dict)
}

fn create_default_context(_args: &[Object]) -> Result<Object, RuntimeError> {
    // Return an SSLContext-shaped dict carrying the toggles user
    // code mutates. The `wrap_socket` callable below raises
    // `NotImplementedError`.
    let ctx = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = ctx.borrow_mut();
        d.insert(DictKey(Object::from_static("protocol")), Object::Int(2));
        d.insert(DictKey(Object::from_static("verify_mode")), Object::Int(2));
        d.insert(
            DictKey(Object::from_static("check_hostname")),
            Object::Bool(true),
        );
        d.insert(DictKey(Object::from_static("options")), Object::Int(0));
        d.insert(
            DictKey(Object::from_static("wrap_socket")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "wrap_socket",
                binds_instance: false,
                call: Box::new(wrap_socket_stub),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("load_default_certs")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "load_default_certs",
                binds_instance: false,
                call: Box::new(noop),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("load_cert_chain")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "load_cert_chain",
                binds_instance: false,
                call: Box::new(noop),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("load_verify_locations")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "load_verify_locations",
                binds_instance: false,
                call: Box::new(noop),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("set_alpn_protocols")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "set_alpn_protocols",
                binds_instance: false,
                call: Box::new(noop),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("set_ciphers")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "set_ciphers",
                binds_instance: false,
                call: Box::new(noop),
                call_kw: None,
            })),
        );
    }
    Ok(Object::Dict(ctx))
}

fn noop(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::None)
}

fn wrap_socket_stub(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(RuntimeError::PyException(
        crate::error::PyException::from_builtin(
            "NotImplementedError",
            "ssl.SSLContext.wrap_socket: WeavePy ships the surface but not the TLS engine (RFC 0017 future work)",
        ),
    ))
}

fn get_default_verify_paths(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::new_tuple(vec![
        Object::from_static("SSL_CERT_FILE"),
        Object::None,
        Object::from_static("SSL_CERT_DIR"),
        Object::None,
    ]))
}

fn match_hostname_stub(_args: &[Object]) -> Result<Object, RuntimeError> {
    // No-op — we don't validate certificates because we don't establish them.
    Ok(Object::None)
}

fn _unused() {
    let _ = type_error("");
}
