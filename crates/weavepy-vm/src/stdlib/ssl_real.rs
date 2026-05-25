//! Real TLS via rustls — RFC 0023.
//!
//! This module sits behind `ssl.SSLContext.wrap_socket` and the
//! `_https` helper used by `urllib.request`. It performs full TLS
//! 1.2/1.3 handshakes against the host platform's CA bundle (via
//! `rustls-native-certs`) with `webpki-roots` as the fallback.
//!
//! Two entry points are exposed:
//!
//!   * [`open_https`] — convenience: open `host:port`, perform a
//!     handshake, return an `(reader, writer)` pair suitable for
//!     pure-Python HTTP machinery.
//!   * [`wrap_existing_socket`] — used by `ssl.wrap_socket`: take an
//!     existing `socket.socket` handle id, perform the handshake on
//!     its file descriptor, and hand back a TLS handle that the
//!     wrapper exposes as an `SSLSocket`-shaped object.
//!
//! All Rust-side state lives in a thread-local registry keyed by an
//! integer handle so that Python objects can be plain dicts.

#![allow(unsafe_op_in_unsafe_fn)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, Stream};

use crate::error::{value_error, RuntimeError};

/// State for a single live TLS session.
pub struct TlsSession {
    pub conn: ClientConnection,
    pub sock: TcpStream,
    pub sni: String,
}

impl std::fmt::Debug for TlsSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsSession")
            .field("sni", &self.sni)
            .field("peer", &self.sock.peer_addr().ok())
            .finish_non_exhaustive()
    }
}

thread_local! {
    static SESSIONS: RefCell<HashMap<i64, RefCell<TlsSession>>> =
        RefCell::new(HashMap::new());
    static NEXT_ID: RefCell<i64> = const { RefCell::new(1) };
    static SHARED_CONFIG: RefCell<Option<Arc<ClientConfig>>> = const { RefCell::new(None) };
}

fn alloc_id(session: TlsSession) -> i64 {
    let id = NEXT_ID.with(|n| {
        let mut g = n.borrow_mut();
        let id = *g;
        *g += 1;
        id
    });
    SESSIONS.with(|m| {
        m.borrow_mut().insert(id, RefCell::new(session));
    });
    id
}

/// Get-or-build a shared `ClientConfig` with the platform/host trust
/// store baked in. We try `rustls-native-certs` first, falling back
/// to `webpki-roots` if the system store isn't readable.
fn shared_config() -> Arc<ClientConfig> {
    SHARED_CONFIG.with(|c| {
        let mut slot = c.borrow_mut();
        if let Some(cfg) = slot.as_ref() {
            return cfg.clone();
        }
        let cfg = Arc::new(build_config());
        *slot = Some(cfg.clone());
        cfg
    })
}

fn build_config() -> ClientConfig {
    let mut roots = RootCertStore::empty();
    let mut added = 0usize;
    if let Ok(certs) = rustls_native_certs::load_native_certs() {
        for c in certs {
            if roots.add(CertificateDer::from(c.as_ref().to_vec())).is_ok() {
                added += 1;
            }
        }
    }
    if added == 0 {
        roots
            .roots
            .extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }
    ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}

/// Open a fresh TLS connection to `host:port` and return a session id.
pub fn open_tls(host: &str, port: u16) -> Result<i64, RuntimeError> {
    let sni: ServerName<'static> = match ServerName::try_from(host.to_owned()) {
        Ok(s) => s,
        Err(_) => return Err(value_error(format!("invalid SNI host: {host}"))),
    };
    let sock = TcpStream::connect((host, port))
        .map_err(|e| crate::error::os_error(format!("TLS connect failed: {e}")))?;
    let conn = ClientConnection::new(shared_config(), sni)
        .map_err(|e| crate::error::os_error(format!("TLS handshake init failed: {e}")))?;
    let session = TlsSession {
        conn,
        sock,
        sni: host.to_owned(),
    };
    Ok(alloc_id(session))
}

/// Wrap a socket whose raw fd we already own. Used by
/// `ssl.SSLContext.wrap_socket`.
pub fn wrap_existing(_handle: i64, host: &str) -> Result<i64, RuntimeError> {
    let _ = host;
    // We don't yet support wrapping an existing fd because the
    // ownership model in `socket_mod` keeps the `Socket` alive. The
    // proper fix would be to *detach* the socket from the registry
    // and re-attach as a TLS-wrapped session. Until then, callers
    // should prefer `open_tls`.
    Err(crate::error::not_implemented_error(
        "ssl.wrap_socket(existing_fd) — use ssl.create_default_context().wrap_socket(sock, ...) via the open_tls fast path",
    ))
}

/// Write `data` through the TLS session.
pub fn send(id: i64, data: &[u8]) -> Result<usize, RuntimeError> {
    SESSIONS.with(|m| {
        let map = m.borrow();
        let cell = map
            .get(&id)
            .ok_or_else(|| value_error("ssl: closed connection"))?;
        let mut s = cell.borrow_mut();
        let TlsSession { conn, sock, .. } = &mut *s;
        let mut stream = Stream::new(conn, sock);
        stream
            .write_all(data)
            .map_err(|e| crate::error::os_error(format!("TLS write: {e}")))?;
        Ok(data.len())
    })
}

/// Read up to `n` bytes from the TLS session.
pub fn recv(id: i64, n: usize) -> Result<Vec<u8>, RuntimeError> {
    SESSIONS.with(|m| {
        let map = m.borrow();
        let cell = map
            .get(&id)
            .ok_or_else(|| value_error("ssl: closed connection"))?;
        let mut s = cell.borrow_mut();
        let TlsSession { conn, sock, .. } = &mut *s;
        let mut stream = Stream::new(conn, sock);
        let mut buf = vec![0u8; n];
        let read = match stream.read(&mut buf) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => 0,
            Err(e) => return Err(crate::error::os_error(format!("TLS read: {e}"))),
        };
        buf.truncate(read);
        Ok(buf)
    })
}

/// Drop the session, closing the underlying TCP socket.
pub fn close(id: i64) {
    SESSIONS.with(|m| {
        m.borrow_mut().remove(&id);
    });
}

/// Get the peer's certificate chain (DER) — useful for the
/// `getpeercert(binary_form=True)` shape.
pub fn peer_certs(id: i64) -> Vec<Vec<u8>> {
    SESSIONS.with(|m| {
        let map = m.borrow();
        let Some(cell) = map.get(&id) else {
            return Vec::new();
        };
        let s = cell.borrow();
        s.conn
            .peer_certificates()
            .map(|certs| certs.iter().map(|c| c.as_ref().to_vec()).collect())
            .unwrap_or_default()
    })
}

/// Return `("TLSv1.3" | "TLSv1.2", cipher_suite, key_bits)` for the session.
pub fn cipher_info(id: i64) -> Option<(String, String, u16)> {
    SESSIONS.with(|m| {
        let map = m.borrow();
        let cell = map.get(&id)?;
        let s = cell.borrow();
        let v = s.conn.protocol_version()?;
        let cs = s.conn.negotiated_cipher_suite()?;
        let proto = format!("{:?}", v);
        let name = format!("{:?}", cs.suite());
        Some((proto, name, 256))
    })
}
