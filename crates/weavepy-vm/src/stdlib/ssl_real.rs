//! Real TLS via rustls — the `_ssl` core (RFC 0023 + RFC 0042).
//!
//! This is the native primitive that the frozen `ssl.py`
//! (`SSLContext`/`SSLSocket`/`SSLObject`) and the `_https` accelerator
//! sit on. It grew from RFC 0023's "open my own client stream" into a
//! faithful `_ssl`-shaped core:
//!
//!   * an `SSLContext`-like *config registry* (`new_context`,
//!     `load_cert_chain`, `load_verify_locations`, verify-mode, ALPN),
//!     built up from Python then materialized into a rustls
//!     `ClientConfig`/`ServerConfig` at wrap time;
//!   * an `SSLSocket`-like *session registry* that wraps an **existing**
//!     socket fd (POSIX: the fd *is* the socket handle) for **client**
//!     and **server** roles by `dup(2)`-ing it into a `TcpStream` —
//!     leaving the original `socket.socket` owned by `socket_mod`;
//!   * blocking `do_handshake`/`read`/`write`/`pending`, plus
//!     `getpeercert` (DER), `cipher`, `version`, `selected_alpn`.
//!
//! All Rust-side state lives in thread-local registries keyed by an
//! integer id, so the Python objects can stay plain wrappers.

#![allow(unsafe_op_in_unsafe_fn)]

use crate::sync::Rc;
use crate::sync::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{
    AlertDescription, CertificateError, ClientConfig, ClientConnection, Connection,
    DigitallySignedStruct, RootCertStore, ServerConfig, ServerConnection, SignatureScheme,
};

use crate::error::{os_error, timeout_error, type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

// ---------------------------------------------------------------------------
// Context configs (the SSLContext payload)
// ---------------------------------------------------------------------------

/// Mutable config accumulated by the Python `SSLContext` before a wrap.
pub struct CtxConfig {
    /// CPython protocol constant (PROTOCOL_TLS_CLIENT / _SERVER / TLS).
    pub protocol: i64,
    pub verify_mode: i64, // 0 NONE, 1 OPTIONAL, 2 REQUIRED
    pub check_hostname: bool,
    pub use_native_roots: bool,
    pub extra_ca: Vec<CertificateDer<'static>>,
    pub cert_chain: Option<Vec<CertificateDer<'static>>>,
    pub private_key: Option<PrivateKeyDer<'static>>,
    pub alpn: Vec<Vec<u8>>,
}

impl std::fmt::Debug for CtxConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CtxConfig")
            .field("protocol", &self.protocol)
            .field("verify_mode", &self.verify_mode)
            .field("check_hostname", &self.check_hostname)
            .field("use_native_roots", &self.use_native_roots)
            .field("extra_ca", &self.extra_ca.len())
            .field("has_cert_chain", &self.cert_chain.is_some())
            .field("has_private_key", &self.private_key.is_some())
            .field("alpn", &self.alpn.len())
            .finish()
    }
}

impl Default for CtxConfig {
    fn default() -> Self {
        CtxConfig {
            protocol: 2, // PROTOCOL_TLS
            verify_mode: 0,
            check_hostname: false,
            use_native_roots: true,
            extra_ca: Vec::new(),
            cert_chain: None,
            private_key: None,
            alpn: Vec::new(),
        }
    }
}

/// A live TLS session: a rustls connection driven over a `dup`'d fd.
pub struct TlsSession {
    pub conn: Connection,
    pub sock: TcpStream,
    pub server_side: bool,
    pub sni: String,
    /// Cross-call state for [`RecordReader`] so a record split over several
    /// `read_tls` calls is still never read past its boundary.
    pub rec: RecordState,
}

/// Per-session bookkeeping for [`RecordReader`].
#[derive(Debug, Default)]
pub struct RecordState {
    /// Body bytes still owed for the record currently being read (0 at a
    /// record boundary, where the next 5 bytes are a fresh header).
    left: usize,
    /// Header bytes accumulated so far (0..=5) while at a boundary.
    hdr: [u8; 5],
    hdr_have: usize,
}

/// A `Read` adapter that hands rustls TLS bytes **one record at a time**, never
/// reading past a record boundary from the kernel.
///
/// rustls' own `read_tls` greedily drains whatever the kernel has buffered into
/// its internal deframer, which empties the socket receive buffer even though
/// decrypted plaintext is still pending. That defeats `select()`/`poll()`-based
/// event loops (asyncore in the test_ftplib/test_imaplib TLS servers): they
/// watch the *raw fd*, see it go quiet, and stop calling `recv` — stranding the
/// already-decrypted bytes until the peer's FIN finally wakes the loop (a
/// multi-second stall, or an outright truncation). OpenSSL avoids this by
/// reading exactly one record's worth at a time; this adapter does the same, so
/// the kernel always still holds the *next* record and the fd stays readable
/// until the stream (including the peer's `close_notify`) is fully drained.
struct RecordReader<'a> {
    sock: &'a mut TcpStream,
    st: &'a mut RecordState,
}

impl Read for RecordReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.st.left == 0 {
            // At a record boundary: read only enough to complete the 5-byte
            // record header, so we learn the body length before touching it.
            let want = (5 - self.st.hdr_have).min(buf.len());
            let n = self.sock.read(&mut buf[..want])?;
            if n == 0 {
                return Ok(0);
            }
            self.st.hdr[self.st.hdr_have..self.st.hdr_have + n].copy_from_slice(&buf[..n]);
            self.st.hdr_have += n;
            if self.st.hdr_have == 5 {
                self.st.left = u16::from_be_bytes([self.st.hdr[3], self.st.hdr[4]]) as usize;
                self.st.hdr_have = 0;
            }
            return Ok(n);
        }
        // Mid-record: never read more than the bytes left in this record.
        let want = self.st.left.min(buf.len());
        let n = self.sock.read(&mut buf[..want])?;
        self.st.left -= n;
        Ok(n)
    }
}

impl std::fmt::Debug for TlsSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsSession")
            .field("server_side", &self.server_side)
            .field("sni", &self.sni)
            .finish_non_exhaustive()
    }
}

// Process-global registries (shared across all OS threads), *not*
// thread-local: a TLS context/session created on one Python thread is
// routinely used from another (server-accept thread vs. client thread,
// asyncio executors, etc.) — a thread-local registry made such handles
// resolve to "invalid" off their creating thread. `Rc`/`RefCell` alias
// `Arc`/`GilCell` (RFC 0025), so the stored handles are `Send + Sync`.
// Each session lives behind its own `Rc<RefCell<_>>` cell so we can drop the
// registry lock *before* the blocking handshake/read/write — distinct
// sessions then never serialize against (or deadlock) each other.
fn contexts() -> &'static parking_lot::Mutex<HashMap<i64, Rc<RefCell<CtxConfig>>>> {
    static R: std::sync::OnceLock<parking_lot::Mutex<HashMap<i64, Rc<RefCell<CtxConfig>>>>> =
        std::sync::OnceLock::new();
    R.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

fn sessions() -> &'static parking_lot::Mutex<HashMap<i64, Rc<RefCell<TlsSession>>>> {
    static R: std::sync::OnceLock<parking_lot::Mutex<HashMap<i64, Rc<RefCell<TlsSession>>>>> =
        std::sync::OnceLock::new();
    R.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

fn shared_client_slot() -> &'static parking_lot::Mutex<Option<Arc<ClientConfig>>> {
    static R: std::sync::OnceLock<parking_lot::Mutex<Option<Arc<ClientConfig>>>> =
        std::sync::OnceLock::new();
    R.get_or_init(|| parking_lot::Mutex::new(None))
}

fn next_id() -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    static NEXT: AtomicI64 = AtomicI64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn alloc_ctx(cfg: CtxConfig) -> i64 {
    let id = next_id();
    contexts().lock().insert(id, Rc::new(RefCell::new(cfg)));
    id
}

fn ctx_cell(id: i64) -> Option<Rc<RefCell<CtxConfig>>> {
    contexts().lock().get(&id).cloned()
}

fn with_ctx<R>(id: i64, f: impl FnOnce(&mut CtxConfig) -> R) -> Result<R, RuntimeError> {
    let cell = ctx_cell(id).ok_or_else(|| value_error("ssl: invalid SSLContext"))?;
    let mut guard = cell.borrow_mut();
    let r = f(&mut guard);
    drop(guard);
    Ok(r)
}

fn session_cell(id: i64) -> Option<Rc<RefCell<TlsSession>>> {
    sessions().lock().get(&id).cloned()
}

fn alloc_session(s: TlsSession) -> i64 {
    let id = next_id();
    sessions().lock().insert(id, Rc::new(RefCell::new(s)));
    id
}

// ---------------------------------------------------------------------------
// Memory BIO path (the `_ssl` `MemoryBIO`/`SSLObject` / `wrap_bio` surface)
//
// rustls is *natively* a memory-BIO API: a `Connection` is driven by feeding it
// ciphertext via `read_tls`/`write_tls` and exchanging plaintext via
// `reader()`/`writer()`. So the BIO path needs no socket at all — it drives the
// very same `Connection` over two in-memory byte queues. This is deliberately a
// *separate* registry and a separate set of `_ssl` entry points so the proven,
// fd-backed `TlsSession` hot path (the five passing protocol-client suites) is
// left completely untouched.
//
// CPython's `MemoryBIO`/`SSLObject` is inherently non-blocking: when the
// connection needs more ciphertext than the incoming BIO holds, the operation
// raises `SSLWantReadError`; the asyncio TLS transport pumps the BIOs across
// event-loop turns. We mirror that exactly (`want_read_error()`).
// ---------------------------------------------------------------------------

/// An in-memory byte buffer with a write-side EOF marker — the `_ssl.MemoryBIO`
/// payload. `write_eof` records that no more ciphertext will ever be appended
/// (the peer's transport closed); `eof` (drained && `write_eof`) is what
/// `MemoryBIO.eof` reports.
#[derive(Default, Debug)]
pub struct MemBio {
    buf: std::collections::VecDeque<u8>,
    write_eof: bool,
}

/// A live TLS session driven purely over two [`MemBio`]s (no socket).
pub struct BioSession {
    conn: Connection,
    /// BIO we *read* ciphertext from (network → us).
    incoming: i64,
    /// BIO we *write* ciphertext to (us → network).
    outgoing: i64,
    #[allow(dead_code)]
    server_side: bool,
    #[allow(dead_code)]
    sni: String,
    /// Whether our `close_notify` has already been emitted (so `unwrap()` is
    /// idempotent and doesn't queue a second alert).
    close_sent: bool,
}

impl std::fmt::Debug for BioSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BioSession")
            .field("server_side", &self.server_side)
            .field("sni", &self.sni)
            .finish_non_exhaustive()
    }
}

fn bios() -> &'static parking_lot::Mutex<HashMap<i64, Rc<RefCell<MemBio>>>> {
    static R: std::sync::OnceLock<parking_lot::Mutex<HashMap<i64, Rc<RefCell<MemBio>>>>> =
        std::sync::OnceLock::new();
    R.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

fn bio_sessions() -> &'static parking_lot::Mutex<HashMap<i64, Rc<RefCell<BioSession>>>> {
    static R: std::sync::OnceLock<parking_lot::Mutex<HashMap<i64, Rc<RefCell<BioSession>>>>> =
        std::sync::OnceLock::new();
    R.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

fn bio_cell(id: i64) -> Option<Rc<RefCell<MemBio>>> {
    bios().lock().get(&id).cloned()
}

fn alloc_bio() -> i64 {
    let id = next_id();
    bios()
        .lock()
        .insert(id, Rc::new(RefCell::new(MemBio::default())));
    id
}

fn bio_session_cell(id: i64) -> Option<Rc<RefCell<BioSession>>> {
    bio_sessions().lock().get(&id).cloned()
}

/// `Write` adapter that appends rustls ciphertext to an outgoing [`MemBio`]
/// (never blocks — it's an in-memory queue).
struct BioWriter<'a> {
    bio: &'a mut MemBio,
}

impl Write for BioWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bio.buf.extend(buf.iter().copied());
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `Read` adapter that hands rustls ciphertext from an incoming [`MemBio`].
/// An empty buffer reports `WouldBlock` (→ `SSL_ERROR_WANT_READ`) unless the
/// write side was closed, in which case it reports a clean EOF (`Ok(0)`).
struct BioReader<'a> {
    bio: &'a mut MemBio,
}

impl Read for BioReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.bio.buf.is_empty() {
            if self.bio.write_eof {
                return Ok(0);
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "memory BIO empty",
            ));
        }
        let n = buf.len().min(self.bio.buf.len());
        for slot in buf.iter_mut().take(n) {
            *slot = self.bio.buf.pop_front().unwrap();
        }
        Ok(n)
    }
}

/// Run `f` with mutable access to a BIO session and both of its memory BIOs.
/// The session and the two BIOs each live in their own `RefCell`, so borrowing
/// all three at once is sound (the incoming/outgoing BIOs are always distinct).
fn with_bio_session<R>(
    sess_id: i64,
    f: impl FnOnce(&mut BioSession, &mut MemBio, &mut MemBio) -> Result<R, RuntimeError>,
) -> Result<R, RuntimeError> {
    let scell =
        bio_session_cell(sess_id).ok_or_else(|| value_error("ssl: closed BIO connection"))?;
    let mut s = scell.borrow_mut();
    let (in_id, out_id) = (s.incoming, s.outgoing);
    let icell = bio_cell(in_id).ok_or_else(|| value_error("ssl: invalid incoming BIO"))?;
    let ocell = bio_cell(out_id).ok_or_else(|| value_error("ssl: invalid outgoing BIO"))?;
    let mut inb = icell.borrow_mut();
    let mut outb = ocell.borrow_mut();
    f(&mut s, &mut inb, &mut outb)
    // (`s`, `inb`, `outb` are `RefMut`s; `&mut RefMut<T>` coerces to `&mut T`
    // at the call boundary via `DerefMut`.)
}

/// Flush every queued TLS record into the outgoing BIO (in-memory; never blocks).
fn bio_flush_out(conn: &mut Connection, outb: &mut MemBio) {
    while conn.wants_write() {
        // Writing to a `MemBio` is infallible.
        let _ = conn.write_tls(&mut BioWriter { bio: outb });
    }
}

// ---------------------------------------------------------------------------
// "accept anything" verifier for CERT_NONE clients
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct NoVerifier;

impl ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

// ---------------------------------------------------------------------------
// "verify chain, skip hostname" verifier
//
// CPython lets a context keep `verify_mode == CERT_REQUIRED` while turning
// `check_hostname` off: the certificate chain is still validated against the
// trust store, but the SNI/hostname is not checked (e.g. connecting to a host
// by IP, or `test_httplib.test_local_bad_hostname`'s `check_hostname = False`
// leg). rustls couples both checks inside `WebPkiServerVerifier`, so we wrap it
// and downgrade *only* the name-mismatch error to success; every other
// certificate failure (bad signature, unknown issuer, expiry) stays fatal.
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ChainOnlyVerifier {
    inner: Arc<WebPkiServerVerifier>,
}

fn is_name_mismatch(ce: &CertificateError) -> bool {
    matches!(
        ce,
        CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. }
    )
}

impl ServerCertVerifier for ChainOnlyVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        ) {
            Ok(v) => Ok(v),
            Err(rustls::Error::InvalidCertificate(ce)) if is_name_mismatch(&ce) => {
                Ok(ServerCertVerified::assertion())
            }
            Err(e) => Err(e),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

// ---------------------------------------------------------------------------
// Config materialization
// ---------------------------------------------------------------------------

fn native_root_store() -> RootCertStore {
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
    roots
}

fn build_client_config(cfg: &CtxConfig) -> Result<Arc<ClientConfig>, RuntimeError> {
    // Build a root store from native roots (+ any explicit CA the context
    // loaded). With CERT_NONE we instead install the accept-all verifier.
    let verify = cfg.verify_mode != 0 || cfg.check_hostname;
    let builder = ClientConfig::builder();
    let builder = if verify {
        let mut roots = if cfg.use_native_roots {
            native_root_store()
        } else {
            RootCertStore::empty()
        };
        for c in &cfg.extra_ca {
            let _ = roots.add(c.clone());
        }
        if cfg.check_hostname {
            // Chain + hostname (rustls' default `WebPkiServerVerifier`).
            builder.with_root_certificates(roots)
        } else {
            // Chain only: validate against the trust store but ignore the
            // hostname, matching CPython's `check_hostname = False` while
            // `verify_mode` stays `CERT_REQUIRED`.
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let inner = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider)
                .build()
                .map_err(|e| ssl_error_rt(format!("verifier: {e}")))?;
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(ChainOnlyVerifier { inner }))
        }
    } else {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
    };
    let mut config = match (&cfg.cert_chain, &cfg.private_key) {
        (Some(chain), Some(key)) => builder
            .with_client_auth_cert(chain.clone(), key.clone_key())
            .map_err(|e| ssl_error_rt(format!("client cert: {e}")))?,
        _ => builder.with_no_client_auth(),
    };
    config.alpn_protocols = cfg.alpn.clone();
    Ok(Arc::new(config))
}

fn build_server_config(cfg: &CtxConfig) -> Result<Arc<ServerConfig>, RuntimeError> {
    let chain = cfg
        .cert_chain
        .clone()
        .ok_or_else(|| ssl_error_rt("server side requires a certificate (load_cert_chain)"))?;
    let key = cfg
        .private_key
        .as_ref()
        .ok_or_else(|| ssl_error_rt("server side requires a private key (load_cert_chain)"))?
        .clone_key();
    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(|e| ssl_error_rt(format!("server cert: {e}")))?;
    config.alpn_protocols = cfg.alpn.clone();
    Ok(Arc::new(config))
}

fn shared_client_default() -> Arc<ClientConfig> {
    let mut slot = shared_client_slot().lock();
    if let Some(cfg) = slot.as_ref() {
        return cfg.clone();
    }
    let cfg = Arc::new(
        ClientConfig::builder()
            .with_root_certificates(native_root_store())
            .with_no_client_auth(),
    );
    *slot = Some(cfg.clone());
    cfg
}

// ---------------------------------------------------------------------------
// fd → TcpStream (dup so socket_mod keeps ownership of the original)
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn tcp_from_fd(fd: i64) -> Result<TcpStream, RuntimeError> {
    use std::os::unix::io::FromRawFd;
    if fd < 0 {
        return Err(value_error("ssl: invalid file descriptor"));
    }
    let dup = unsafe { libc::dup(fd as libc::c_int) };
    if dup < 0 {
        return Err(os_error(format!(
            "ssl: dup failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(unsafe { TcpStream::from_raw_fd(dup) })
}

#[cfg(not(unix))]
fn tcp_from_fd(_fd: i64) -> Result<TcpStream, RuntimeError> {
    Err(crate::error::not_implemented_error(
        "ssl.wrap_socket: only POSIX fds are supported",
    ))
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

fn ssl_error_rt(msg: impl Into<String>) -> RuntimeError {
    // Surfaced to Python as ssl.SSLError (ssl.py installs the mapping via the
    // OSError subclass it raises). Native side keeps it an OSError-shaped
    // error carrying an "[SSL] " marker so ssl.py can classify it.
    os_error(format!("[SSL] {}", msg.into()))
}

/// SSL "operation would block" markers for a **non-blocking** socket. OpenSSL/
/// CPython report `SSL_ERROR_WANT_READ`/`SSL_ERROR_WANT_WRITE`; `ssl.py`'s
/// `_wrap_ssl_error` recognises these markers and raises
/// `SSLWantReadError`/`SSLWantWriteError`, which asyncore-style non-blocking
/// drivers (the `test_ftplib` TLS server's `_do_ssl_handshake`, the data-channel
/// `recv`/`send`) catch to retry on the next event-loop turn.
fn want_read_error() -> RuntimeError {
    ssl_error_rt("WANT_READ: The operation did not complete (read)")
}

fn want_write_error() -> RuntimeError {
    ssl_error_rt("WANT_WRITE: The operation did not complete (write)")
}

/// A clean stream EOF during a TLS handshake/teardown. Phrased so `ssl.py` maps
/// it to `SSLEOFError`/`SSL_ERROR_EOF`; asyncore TLS servers (`_do_ssl_handshake`)
/// treat that as "peer went away" and `handle_close()` rather than crashing.
fn eof_error() -> RuntimeError {
    ssl_error_rt("EOF occurred in violation of protocol")
}

/// OpenSSL's symbolic name for a TLS alert (what CPython surfaces in
/// `SSLError.args[1]`, e.g. `"SSLV3_ALERT_BAD_CERTIFICATE"`). asyncore TLS
/// servers branch on these substrings, so a rustls `AlertReceived` has to be
/// rendered the OpenSSL way.
fn alert_openssl_token(desc: AlertDescription) -> &'static str {
    match desc {
        AlertDescription::UnexpectedMessage => "SSLV3_ALERT_UNEXPECTED_MESSAGE",
        AlertDescription::BadRecordMac => "SSLV3_ALERT_BAD_RECORD_MAC",
        AlertDescription::DecompressionFailure => "SSLV3_ALERT_DECOMPRESSION_FAILURE",
        AlertDescription::HandshakeFailure => "SSLV3_ALERT_HANDSHAKE_FAILURE",
        AlertDescription::NoCertificate => "SSLV3_ALERT_NO_CERTIFICATE",
        AlertDescription::BadCertificate => "SSLV3_ALERT_BAD_CERTIFICATE",
        AlertDescription::UnsupportedCertificate => "SSLV3_ALERT_UNSUPPORTED_CERTIFICATE",
        AlertDescription::CertificateRevoked => "SSLV3_ALERT_CERTIFICATE_REVOKED",
        AlertDescription::CertificateExpired => "SSLV3_ALERT_CERTIFICATE_EXPIRED",
        AlertDescription::CertificateUnknown => "SSLV3_ALERT_CERTIFICATE_UNKNOWN",
        AlertDescription::IllegalParameter => "SSLV3_ALERT_ILLEGAL_PARAMETER",
        AlertDescription::UnknownCA => "TLSV1_ALERT_UNKNOWN_CA",
        AlertDescription::AccessDenied => "TLSV1_ALERT_ACCESS_DENIED",
        AlertDescription::DecodeError => "TLSV1_ALERT_DECODE_ERROR",
        AlertDescription::DecryptError => "TLSV1_ALERT_DECRYPT_ERROR",
        AlertDescription::ProtocolVersion => "TLSV1_ALERT_PROTOCOL_VERSION",
        AlertDescription::InsufficientSecurity => "TLSV1_ALERT_INSUFFICIENT_SECURITY",
        AlertDescription::InternalError => "TLSV1_ALERT_INTERNAL_ERROR",
        AlertDescription::UserCanceled => "TLSV1_ALERT_USER_CANCELLED",
        AlertDescription::NoRenegotiation => "TLSV1_ALERT_NO_RENEGOTIATION",
        AlertDescription::UnsupportedExtension => "TLSV1_ALERT_UNSUPPORTED_EXTENSION",
        _ => "TLSV1_ALERT_INTERNAL_ERROR",
    }
}

/// Map a `process_new_packets` failure to a Python-facing error. A received
/// fatal alert is rendered OpenSSL-style (`[SSL: <TOKEN>] <human> (_ssl.c)`) so
/// `ssl.py` keeps it a plain `SSLError` whose `args[1]` carries the alert name.
fn tls_process_error(e: &rustls::Error) -> RuntimeError {
    if let rustls::Error::AlertReceived(desc) = e {
        let token = alert_openssl_token(*desc);
        let human = token.to_ascii_lowercase().replace('_', " ");
        return ssl_error_rt(format!("[SSL: {token}] {human} (_ssl.c)"));
    }
    ssl_error_rt(format!("tls: {e}"))
}

/// Is the (dup'd) session socket in non-blocking mode? `socket.settimeout(0)` /
/// `setblocking(False)` arm `O_NONBLOCK` on the shared open-file description,
/// which the `dup(2)` inherits; a *positive* timeout instead arms
/// `SO_RCVTIMEO`/`SO_SNDTIMEO` (no `O_NONBLOCK`). So `O_NONBLOCK` distinguishes
/// "raise WANT_READ/WANT_WRITE immediately" (non-blocking) from "block up to the
/// deadline, then `socket.timeout`" (timeout mode) and "block forever"
/// (blocking).
#[cfg(unix)]
fn sock_is_nonblocking(sock: &TcpStream) -> bool {
    use std::os::unix::io::AsRawFd;
    let flags = unsafe { libc::fcntl(sock.as_raw_fd(), libc::F_GETFL) };
    flags >= 0 && (flags & libc::O_NONBLOCK) != 0
}

#[cfg(not(unix))]
fn sock_is_nonblocking(_sock: &TcpStream) -> bool {
    false
}

// ---------------------------------------------------------------------------
// Public primitives reused by `_https` (RFC 0023 fast path)
// ---------------------------------------------------------------------------

/// Open a fresh client TLS connection to `host:port` (the `_https` fast path).
pub fn open_tls(host: &str, port: u16) -> Result<i64, RuntimeError> {
    let sni: ServerName<'static> = ServerName::try_from(host.to_owned())
        .map_err(|_| value_error(format!("invalid SNI host: {host}")))?;
    let sock = TcpStream::connect((host, port))
        .map_err(|e| os_error(format!("TLS connect failed: {e}")))?;
    let mut conn = ClientConnection::new(shared_client_default(), sni)
        .map_err(|e| os_error(format!("TLS handshake init failed: {e}")))?;
    let mut sock2 = sock;
    crate::gil::allow_threads_then(|| conn.complete_io(&mut sock2))
        .map_err(|e| ssl_error_rt(format!("handshake: {e}")))?;
    Ok(alloc_session(TlsSession {
        conn: Connection::Client(conn),
        sock: sock2,
        server_side: false,
        sni: host.to_owned(),
        rec: RecordState::default(),
    }))
}

/// Write all of `data` through the TLS session (blocking).
pub fn send(id: i64, data: &[u8]) -> Result<usize, RuntimeError> {
    write_all(id, data)?;
    Ok(data.len())
}

/// Read up to `n` bytes (blocking); empty vec on clean EOF.
pub fn recv(id: i64, n: usize) -> Result<Vec<u8>, RuntimeError> {
    read_n(id, n)
}

/// Drop the session, closing the dup'd socket fd.
pub fn close(id: i64) {
    if std::env::var("WEAVE_SSL_DEBUG").is_ok() {
        eprintln!("[close id={id}]");
    }
    sessions().lock().remove(&id);
}

/// Peer DER cert chain (for `getpeercert(binary_form=True)`).
pub fn peer_certs(id: i64) -> Vec<Vec<u8>> {
    let Some(cell) = session_cell(id) else {
        return Vec::new();
    };
    let s = cell.borrow();
    s.conn
        .peer_certificates()
        .map(|certs| certs.iter().map(|c| c.as_ref().to_vec()).collect())
        .unwrap_or_default()
}

/// `(protocol, cipher_suite, key_bits)` for the session.
pub fn cipher_info(id: i64) -> Option<(String, String, u16)> {
    let cell = session_cell(id)?;
    let s = cell.borrow();
    let v = s.conn.protocol_version()?;
    let cs = s.conn.negotiated_cipher_suite()?;
    Some((tls_version_str(v), cipher_name(cs.suite()), 256))
}

// ---------------------------------------------------------------------------
// Core blocking I/O over a session
// ---------------------------------------------------------------------------

/// Write `data` through the TLS session, returning the number of plaintext
/// bytes accepted. Blocking sockets drain every queued record to the transport
/// (GIL released); non-blocking sockets flush as much as the socket buffer
/// accepts and surface `WANT_WRITE` when it would block.
fn write_all(id: i64, data: &[u8]) -> Result<usize, RuntimeError> {
    // The `Connection` enum derefs to `CommonState` (not `ConnectionCommon<S>`),
    // so `rustls::Stream` can't wrap it; drive the inherent reader/writer API.
    let cell = session_cell(id).ok_or_else(|| value_error("ssl: closed connection"))?;
    let mut s = cell.borrow_mut();
    if std::env::var("WEAVE_SSL_DEBUG").is_ok() {
        eprintln!(
            "[write_all id={id} nb={}] data.len()={}",
            sock_is_nonblocking(&s.sock),
            data.len()
        );
    }
    if sock_is_nonblocking(&s.sock) {
        // Drain TLS records still queued from a previous partial flush *before*
        // accepting new plaintext — OpenSSL's moving-write-buffer contract has
        // the caller retry the same bytes on `WANT_WRITE`.
        {
            let TlsSession { conn, sock, .. } = &mut *s;
            while conn.wants_write() {
                match conn.write_tls(sock) {
                    Ok(_) => {}
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        return Err(want_write_error());
                    }
                    Err(e) => return Err(ssl_error_rt(format!("write_tls: {e}"))),
                }
            }
        }
        let n = s
            .conn
            .writer()
            .write(data)
            .map_err(|e| ssl_error_rt(format!("write: {e}")))?;
        let TlsSession { conn, sock, .. } = &mut *s;
        while conn.wants_write() {
            match conn.write_tls(sock) {
                Ok(_) => {}
                // Records remain buffered; they flush on the next `write_tls`.
                // Report the plaintext accepted so the caller advances.
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(ssl_error_rt(format!("write_tls: {e}"))),
            }
        }
        let _ = sock.flush();
        return Ok(n);
    }
    s.conn
        .writer()
        .write_all(data)
        .map_err(|e| ssl_error_rt(format!("write: {e}")))?;
    // Flush queued TLS records to the transport with the GIL released so peer
    // threads (e.g. the loopback server) can run while we block on the socket.
    let res = {
        let TlsSession { conn, sock, .. } = &mut *s;
        crate::gil::allow_threads_then(|| -> std::io::Result<()> {
            while conn.wants_write() {
                conn.write_tls(sock)?;
            }
            sock.flush()
        })
    };
    res.map_err(|e| {
        if e.kind() == std::io::ErrorKind::WouldBlock {
            timeout_error("The write operation timed out")
        } else {
            ssl_error_rt(format!("write_tls: {e}"))
        }
    })?;
    Ok(data.len())
}

fn read_n(id: i64, n: usize) -> Result<Vec<u8>, RuntimeError> {
    if n == 0 {
        return Ok(Vec::new());
    }
    let cell = session_cell(id).ok_or_else(|| value_error("ssl: closed connection"))?;
    let mut s = cell.borrow_mut();
    let nonblocking = sock_is_nonblocking(&s.sock);
    let dbg = std::env::var("WEAVE_SSL_DEBUG").is_ok();
    if dbg {
        eprintln!("[read_n id={id} nb={nonblocking}] ENTER n={n}");
    }
    let mut buf = vec![0u8; n];
    loop {
        // Hand back any plaintext rustls has already decrypted.
        match s.conn.reader().read(&mut buf) {
            Ok(0) => {
                if dbg {
                    eprintln!(
                        "[read_n id={id} nb={nonblocking}] reader Ok(0) EOF wants_read={}",
                        s.conn.wants_read()
                    );
                }
                buf.truncate(0);
                return Ok(buf);
            }
            Ok(r) => {
                if dbg {
                    eprintln!("[read_n id={id}] reader Ok({r})");
                }
                buf.truncate(r);
                return Ok(buf);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(ssl_error_rt(format!("read: {e}"))),
        }
        if !s.conn.wants_read() {
            if dbg {
                eprintln!("[read_n id={id} nb={nonblocking}] !wants_read -> empty");
            }
            buf.truncate(0);
            return Ok(buf);
        }
        // Pull the *next single record* off the transport (GIL released) and
        // process it. Reading one record at a time (rather than letting rustls
        // drain the whole socket buffer) keeps the raw fd readable for the
        // peer's `select()`/`poll()` loop while decrypted bytes are still
        // pending — see [`RecordReader`].
        let rd = {
            let TlsSession {
                conn, sock, rec, ..
            } = &mut *s;
            crate::gil::allow_threads_then(|| conn.read_tls(&mut RecordReader { sock, st: rec }))
        };
        match rd {
            Ok(0) => {
                if dbg {
                    eprintln!("[read_n id={id}] read_tls Ok(0) EOF");
                }
                buf.truncate(0);
                return Ok(buf);
            }
            Ok(k) => {
                if dbg {
                    eprintln!("[read_n id={id}] read_tls Ok({k})");
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // No TLS bytes available yet: a non-blocking socket reports this
                // as `SSL_ERROR_WANT_READ`; a timeout-mode socket (an expired
                // `SO_RCVTIMEO`) reports `socket.timeout`.
                if dbg {
                    eprintln!("[read_n id={id} nb={nonblocking}] read_tls WouldBlock");
                }
                if nonblocking {
                    return Err(want_read_error());
                }
                return Err(timeout_error("The read operation timed out"));
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::UnexpectedEof
                    || e.kind() == std::io::ErrorKind::ConnectionAborted
                    || e.kind() == std::io::ErrorKind::ConnectionReset =>
            {
                // A peer that closes (or RST-drops) the transport is a stream
                // EOF, not an SSL fault: CPython's asyncore maps ECONNRESET/
                // ECONNABORTED to an empty `recv`, so the server's data handler
                // sees a clean close instead of crashing its event-loop thread
                // (test_poplib STLS, where the client tears the socket down
                // without a TLS close_notify).
                buf.truncate(0);
                return Ok(buf);
            }
            Err(e) => return Err(ssl_error_rt(format!("read_tls: {e}"))),
        }
        s.conn
            .process_new_packets()
            .map_err(|e| tls_process_error(&e))?;
    }
}

fn tls_version_str(v: rustls::ProtocolVersion) -> String {
    match v {
        rustls::ProtocolVersion::TLSv1_3 => "TLSv1.3".to_owned(),
        rustls::ProtocolVersion::TLSv1_2 => "TLSv1.2".to_owned(),
        rustls::ProtocolVersion::TLSv1_1 => "TLSv1.1".to_owned(),
        rustls::ProtocolVersion::TLSv1_0 => "TLSv1".to_owned(),
        other => format!("{other:?}"),
    }
}

/// Map a rustls cipher suite to an OpenSSL-style name (best effort — the
/// common TLS 1.3 / ECDHE suites the loopback tests negotiate).
fn cipher_name(suite: rustls::CipherSuite) -> String {
    use rustls::CipherSuite as Cs;
    let name = match suite {
        Cs::TLS13_AES_256_GCM_SHA384 => "TLS_AES_256_GCM_SHA384",
        Cs::TLS13_AES_128_GCM_SHA256 => "TLS_AES_128_GCM_SHA256",
        Cs::TLS13_CHACHA20_POLY1305_SHA256 => "TLS_CHACHA20_POLY1305_SHA256",
        Cs::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384 => "ECDHE-ECDSA-AES256-GCM-SHA384",
        Cs::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384 => "ECDHE-RSA-AES256-GCM-SHA384",
        Cs::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256 => "ECDHE-ECDSA-AES128-GCM-SHA256",
        Cs::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256 => "ECDHE-RSA-AES128-GCM-SHA256",
        Cs::TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256 => "ECDHE-ECDSA-CHACHA20-POLY1305",
        Cs::TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256 => "ECDHE-RSA-CHACHA20-POLY1305",
        other => return format!("{other:?}"),
    };
    name.to_owned()
}

// ---------------------------------------------------------------------------
// Native `_ssl` module
// ---------------------------------------------------------------------------

fn arg_str(args: &[Object], i: usize, what: &str) -> Result<String, RuntimeError> {
    match args.get(i) {
        Some(Object::Str(s)) => Ok(s.to_string()),
        _ => Err(type_error(format!("_ssl: {what} must be str"))),
    }
}

fn arg_int(args: &[Object], i: usize, what: &str) -> Result<i64, RuntimeError> {
    match args.get(i) {
        Some(Object::Int(n)) => Ok(*n),
        Some(Object::Bool(b)) => Ok(i64::from(*b)),
        _ => Err(type_error(format!("_ssl: {what} must be int"))),
    }
}

fn arg_bool(args: &[Object], i: usize) -> bool {
    matches!(args.get(i), Some(Object::Bool(true)) | Some(Object::Int(1)))
}

fn read_pem_file(path: &str) -> Result<Vec<u8>, RuntimeError> {
    std::fs::read(path).map_err(|e| ssl_error_rt(format!("cannot read {path}: {e}")))
}

fn parse_cert_chain(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, RuntimeError> {
    let mut rd = std::io::BufReader::new(pem);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut rd)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| ssl_error_rt(format!("PEM cert parse: {e}")))?;
    if certs.is_empty() {
        return Err(ssl_error_rt("no certificate found in PEM"));
    }
    Ok(certs)
}

fn parse_private_key(pem: &[u8]) -> Result<PrivateKeyDer<'static>, RuntimeError> {
    let mut rd = std::io::BufReader::new(pem);
    let key = rustls_pemfile::private_key(&mut rd)
        .map_err(|e| ssl_error_rt(format!("PEM key parse: {e}")))?;
    key.ok_or_else(|| ssl_error_rt("no private key found in PEM"))
}

fn ns_new_context(args: &[Object]) -> Result<Object, RuntimeError> {
    let protocol = arg_int(args, 0, "protocol").unwrap_or(2);
    let mut cfg = CtxConfig {
        protocol,
        ..Default::default()
    };
    // PROTOCOL_TLS_CLIENT defaults to verify+check_hostname (CPython).
    if protocol == 16 {
        cfg.verify_mode = 2;
        cfg.check_hostname = true;
    }
    Ok(Object::Int(alloc_ctx(cfg)))
}

fn ns_load_cert_chain(args: &[Object]) -> Result<Object, RuntimeError> {
    let ctx = arg_int(args, 0, "ctx")?;
    let certfile = arg_str(args, 1, "certfile")?;
    let keyfile = match args.get(2) {
        Some(Object::Str(s)) => s.to_string(),
        _ => certfile.clone(),
    };
    let cert_pem = read_pem_file(&certfile)?;
    let chain = parse_cert_chain(&cert_pem)?;
    let key_pem = read_pem_file(&keyfile)?;
    let key = parse_private_key(&key_pem)?;
    with_ctx(ctx, |c| {
        c.cert_chain = Some(chain);
        c.private_key = Some(key);
    })?;
    Ok(Object::None)
}

fn ns_load_verify_locations(args: &[Object]) -> Result<Object, RuntimeError> {
    let ctx = arg_int(args, 0, "ctx")?;
    // (cafile, capath, cadata) — we honour cafile and cadata(bytes/str PEM).
    let mut pem: Vec<u8> = Vec::new();
    if let Some(Object::Str(p)) = args.get(1) {
        pem.extend_from_slice(&read_pem_file(p)?);
    }
    match args.get(3) {
        Some(Object::Str(s)) => pem.extend_from_slice(s.to_string().as_bytes()),
        Some(Object::Bytes(b)) => pem.extend_from_slice(b),
        _ => {}
    }
    if pem.is_empty() {
        return Ok(Object::None);
    }
    let certs = parse_cert_chain(&pem)?;
    with_ctx(ctx, |c| c.extra_ca.extend(certs))?;
    Ok(Object::None)
}

fn ns_set_verify_mode(args: &[Object]) -> Result<Object, RuntimeError> {
    let ctx = arg_int(args, 0, "ctx")?;
    let mode = arg_int(args, 1, "mode")?;
    with_ctx(ctx, |c| c.verify_mode = mode)?;
    Ok(Object::None)
}

fn ns_get_verify_mode(args: &[Object]) -> Result<Object, RuntimeError> {
    let ctx = arg_int(args, 0, "ctx")?;
    Ok(Object::Int(with_ctx(ctx, |c| c.verify_mode)?))
}

fn ns_set_check_hostname(args: &[Object]) -> Result<Object, RuntimeError> {
    let ctx = arg_int(args, 0, "ctx")?;
    let on = arg_bool(args, 1);
    with_ctx(ctx, |c| c.check_hostname = on)?;
    Ok(Object::None)
}

fn ns_get_check_hostname(args: &[Object]) -> Result<Object, RuntimeError> {
    let ctx = arg_int(args, 0, "ctx")?;
    Ok(Object::Bool(with_ctx(ctx, |c| c.check_hostname)?))
}

fn ns_set_alpn_protocols(args: &[Object]) -> Result<Object, RuntimeError> {
    let ctx = arg_int(args, 0, "ctx")?;
    let mut protos: Vec<Vec<u8>> = Vec::new();
    match args.get(1) {
        Some(Object::List(l)) => {
            for it in l.borrow().iter() {
                if let Object::Str(s) = it {
                    protos.push(s.to_string().into_bytes());
                }
            }
        }
        Some(Object::Tuple(t)) => {
            for it in t.iter() {
                if let Object::Str(s) = it {
                    protos.push(s.to_string().into_bytes());
                }
            }
        }
        _ => {}
    }
    with_ctx(ctx, |c| c.alpn = protos)?;
    Ok(Object::None)
}

fn ns_wrap_socket(args: &[Object]) -> Result<Object, RuntimeError> {
    // (ctx, fd, server_side, server_hostname)
    let ctx = arg_int(args, 0, "ctx")?;
    let fd = arg_int(args, 1, "fd")?;
    let server_side = arg_bool(args, 2);
    let server_hostname = match args.get(3) {
        Some(Object::Str(s)) => s.to_string(),
        _ => String::new(),
    };
    let sock = tcp_from_fd(fd)?;
    // Materialize the rustls config straight from the registered context
    // (CtxConfig isn't `Clone` — `PrivateKeyDer` isn't — so build in place).
    let conn = with_ctx(ctx, |c| -> Result<Connection, RuntimeError> {
        if server_side {
            let scfg = build_server_config(c)?;
            Ok(Connection::Server(
                ServerConnection::new(scfg)
                    .map_err(|e| ssl_error_rt(format!("server init: {e}")))?,
            ))
        } else {
            let ccfg = build_client_config(c)?;
            let name_str = if server_hostname.is_empty() {
                "localhost".to_owned()
            } else {
                server_hostname.clone()
            };
            let sni: ServerName<'static> = ServerName::try_from(name_str.clone())
                .map_err(|_| value_error(format!("invalid server_hostname: {name_str}")))?;
            Ok(Connection::Client(
                ClientConnection::new(ccfg, sni)
                    .map_err(|e| ssl_error_rt(format!("client init: {e}")))?,
            ))
        }
    })??;
    Ok(Object::Int(alloc_session(TlsSession {
        conn,
        sock,
        server_side,
        sni: server_hostname,
        rec: RecordState::default(),
    })))
}

fn ns_do_handshake(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    let cell = session_cell(id).ok_or_else(|| value_error("ssl: closed connection"))?;
    let mut s = cell.borrow_mut();
    // A non-blocking socket drives the handshake one step at a time, raising
    // WANT_READ/WANT_WRITE so an asyncore-style event loop can pump it across
    // turns (the `test_ftplib` TLS server uses `do_handshake_on_connect=False`).
    if sock_is_nonblocking(&s.sock) {
        let TlsSession {
            conn, sock, rec, ..
        } = &mut *s;
        return drive_handshake_nonblocking(conn, sock, rec).map(|()| Object::None);
    }
    // The handshake blocks on the socket; release the GIL so the peer thread
    // (loopback server/client) can make progress instead of deadlocking.
    let res = {
        let TlsSession { conn, sock, .. } = &mut *s;
        crate::gil::allow_threads_then(|| conn.complete_io(sock))
    };
    res.map_err(|e| handshake_io_error(&e))?;
    Ok(Object::None)
}

/// Single-shot, non-blocking handshake step. Flushes any handshake output the
/// peer is waiting on, then reads/processes the next flight; if a socket op
/// would block it returns `WANT_WRITE`/`WANT_READ` (rather than blocking), and
/// returns `Ok(())` once `is_handshaking()` clears. rustls keeps the in-progress
/// handshake state on `conn`, so re-invoking this resumes where it left off.
fn drive_handshake_nonblocking(
    conn: &mut Connection,
    sock: &mut TcpStream,
    rec: &mut RecordState,
) -> Result<(), RuntimeError> {
    loop {
        while conn.wants_write() {
            match conn.write_tls(sock) {
                Ok(_) => {}
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    return Err(want_write_error());
                }
                Err(e) => return Err(ssl_error_rt(format!("write_tls: {e}"))),
            }
        }
        if !conn.is_handshaking() {
            return Ok(());
        }
        // Read one record at a time so any application data coalesced behind the
        // final handshake flight stays in the kernel buffer (visible to the
        // server's `select()` loop) instead of being swallowed here.
        match conn.read_tls(&mut RecordReader { sock, st: rec }) {
            Ok(0) => return Err(eof_error()),
            Ok(_) => {
                conn.process_new_packets()
                    .map_err(|e| tls_process_error(&e))?;
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(want_read_error());
            }
            Err(ref e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::UnexpectedEof
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::ConnectionReset
                ) =>
            {
                // The peer dropped the transport mid-handshake (e.g. it rejected
                // our certificate and bailed). Surface a clean EOF so the server's
                // event loop closes the channel instead of dying on the error.
                return Err(eof_error());
            }
            Err(e) => return Err(ssl_error_rt(format!("read_tls: {e}"))),
        }
    }
}

/// Map a handshake I/O failure to the right Python exception. A socket in
/// timeout mode (`settimeout(d>0)`) reports an expired deadline as
/// EAGAIN/EWOULDBLOCK (`WouldBlock`) or `TimedOut`; CPython raises
/// `socket.timeout` (`TimeoutError`) for the TLS handshake in that case
/// (`test_imaplib`/`test_ssl` timeout tests), not a generic `SSLError`.
fn handshake_io_error(e: &std::io::Error) -> RuntimeError {
    use std::io::ErrorKind::{TimedOut, WouldBlock};
    if matches!(e.kind(), WouldBlock | TimedOut) {
        timeout_error("_ssl.c: The handshake operation timed out")
    } else {
        ssl_error_rt(format!("handshake: {e}"))
    }
}

fn ns_write(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    let data = match args.get(1) {
        Some(Object::Bytes(b)) => b.to_vec(),
        Some(Object::ByteArray(b)) => b.borrow().clone(),
        Some(Object::MemoryView(mv)) => mv.to_bytes(),
        Some(Object::Str(s)) => s.to_string().into_bytes(),
        _ => return Err(type_error("_ssl.write: data must be bytes-like")),
    };
    let written = write_all(id, &data)?;
    Ok(Object::Int(written as i64))
}

fn ns_read(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    let n = arg_int(args, 1, "len").unwrap_or(4096).max(0) as usize;
    let buf = read_n(id, n)?;
    Ok(Object::new_bytes(buf))
}

fn ns_pending(args: &[Object]) -> Result<Object, RuntimeError> {
    // rustls exposes no "decrypted bytes buffered" count; the buffered-reader
    // makefile() path the clients use never relies on pending(), so report 0.
    let _id = arg_int(args, 0, "session")?;
    Ok(Object::Int(0))
}

fn ns_peer_cert_der(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    let certs = peer_certs(id);
    match certs.into_iter().next() {
        Some(der) => Ok(Object::new_bytes(der)),
        None => Ok(Object::None),
    }
}

fn ns_cipher(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    match cipher_info(id) {
        Some((proto, name, bits)) => Ok(Object::new_tuple(vec![
            Object::from_str(name),
            Object::from_str(proto),
            Object::Int(i64::from(bits)),
        ])),
        None => Ok(Object::None),
    }
}

fn ns_version(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    let v = session_cell(id)
        .and_then(|cell| cell.borrow().conn.protocol_version().map(tls_version_str));
    match v {
        Some(s) => Ok(Object::from_str(s)),
        None => Ok(Object::None),
    }
}

fn ns_selected_alpn(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    let p = session_cell(id).and_then(|cell| {
        cell.borrow()
            .conn
            .alpn_protocol()
            .map(|b| String::from_utf8_lossy(b).into_owned())
    });
    match p {
        Some(s) => Ok(Object::from_str(s)),
        None => Ok(Object::None),
    }
}

fn ns_shutdown(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    let dbg = std::env::var("WEAVE_SSL_DEBUG").is_ok();
    // A faithful, OpenSSL-style bidirectional TLS shutdown: send our
    // `close_notify`, then *drain* everything the peer has queued — TLS 1.3
    // `NewSessionTicket` records, the peer's own `close_notify`, and any
    // trailing application data — before we drop the fd.
    //
    // This drain matters: rustls servers send session tickets right after the
    // handshake, and a one-way uploader (ftplib `STOR` over TLS) never reads
    // them. If we close the dup'd fd with those bytes still unread in the
    // kernel receive buffer, the OS answers the peer's next write with an RST,
    // which *truncates* data the peer hadn't consumed yet (the asyncore data
    // server in test_ftplib then sees `ECONNRESET` and reports only a prefix of
    // the upload). `Connection::complete_io` is no help here: once the
    // handshake is done it returns as soon as it has flushed, without reading.
    if let Some(cell) = session_cell(id) {
        let mut s = cell.borrow_mut();
        let nonblocking = sock_is_nonblocking(&s.sock);
        s.conn.send_close_notify();
        let TlsSession { conn, sock, .. } = &mut *s;
        let res = crate::gil::allow_threads_then(|| -> std::io::Result<()> {
            // 1) Flush our close_notify (and any records still queued).
            while conn.wants_write() {
                match conn.write_tls(sock) {
                    Ok(_) => {}
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e),
                }
            }
            let _ = sock.flush();
            // 2) Drain inbound records until the peer closes (clean
            //    `close_notify`/EOF) or the transport would block. A blocking
            //    socket waits for the peer's close_notify (bounded by any
            //    SO_RCVTIMEO); a non-blocking socket (the asyncore TLS server)
            //    only sweeps what is already buffered and bails on WouldBlock,
            //    so it never stalls its event loop.
            let mut scratch = [0u8; 16 * 1024];
            loop {
                // Toss any plaintext rustls has already decrypted.
                loop {
                    match conn.reader().read(&mut scratch) {
                        Ok(0) => break,
                        Ok(_) => continue,
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(_) => break,
                    }
                }
                match conn.read_tls(sock) {
                    Ok(0) => break, // EOF: peer closed the transport.
                    Ok(_) => {}
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => break,
                }
                match conn.process_new_packets() {
                    Ok(io) => {
                        if io.peer_has_closed() {
                            // Flush whatever plaintext that close surfaced, then stop.
                            while let Ok(n) = conn.reader().read(&mut scratch) {
                                if n == 0 {
                                    break;
                                }
                            }
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            Ok(())
        });
        if dbg {
            eprintln!("[shutdown id={id} nb={nonblocking}] -> {res:?}");
        }
        let _ = res;
    }
    close(id);
    Ok(Object::None)
}

fn ns_close(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    close(id);
    Ok(Object::None)
}

// ---------------------------------------------------------------------------
// `_ssl` MemoryBIO / wrap_bio entry points
// ---------------------------------------------------------------------------

fn arg_bytes_like(args: &[Object], i: usize, what: &str) -> Result<Vec<u8>, RuntimeError> {
    match args.get(i) {
        Some(Object::Bytes(b)) => Ok(b.to_vec()),
        Some(Object::ByteArray(b)) => Ok(b.borrow().clone()),
        Some(Object::MemoryView(mv)) => Ok(mv.to_bytes()),
        _ => Err(type_error(format!("_ssl: {what} must be bytes-like"))),
    }
}

fn ns_memory_bio_new(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(alloc_bio()))
}

fn ns_memory_bio_write(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "bio")?;
    let data = arg_bytes_like(args, 1, "data")?;
    let cell = bio_cell(id).ok_or_else(|| value_error("ssl: invalid MemoryBIO"))?;
    let mut b = cell.borrow_mut();
    b.buf.extend(data.iter().copied());
    Ok(Object::Int(data.len() as i64))
}

fn ns_memory_bio_read(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "bio")?;
    // A negative `size` (the `MemoryBIO.read()` default) drains everything.
    let want = arg_int(args, 1, "size").unwrap_or(-1);
    let cell = bio_cell(id).ok_or_else(|| value_error("ssl: invalid MemoryBIO"))?;
    let mut b = cell.borrow_mut();
    let n = if want < 0 {
        b.buf.len()
    } else {
        (want as usize).min(b.buf.len())
    };
    let out: Vec<u8> = b.buf.drain(..n).collect();
    Ok(Object::new_bytes(out))
}

fn ns_memory_bio_pending(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "bio")?;
    let cell = bio_cell(id).ok_or_else(|| value_error("ssl: invalid MemoryBIO"))?;
    let len = cell.borrow().buf.len();
    Ok(Object::Int(len as i64))
}

fn ns_memory_bio_eof(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "bio")?;
    let cell = bio_cell(id).ok_or_else(|| value_error("ssl: invalid MemoryBIO"))?;
    let b = cell.borrow();
    Ok(Object::Bool(b.write_eof && b.buf.is_empty()))
}

fn ns_memory_bio_set_eof(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "bio")?;
    let cell = bio_cell(id).ok_or_else(|| value_error("ssl: invalid MemoryBIO"))?;
    cell.borrow_mut().write_eof = true;
    Ok(Object::None)
}

fn ns_memory_bio_free(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "bio")?;
    bios().lock().remove(&id);
    Ok(Object::None)
}

fn ns_wrap_bio(args: &[Object]) -> Result<Object, RuntimeError> {
    // (ctx, incoming_bio, outgoing_bio, server_side, server_hostname)
    let ctx = arg_int(args, 0, "ctx")?;
    let incoming = arg_int(args, 1, "incoming")?;
    let outgoing = arg_int(args, 2, "outgoing")?;
    let server_side = arg_bool(args, 3);
    let server_hostname = match args.get(4) {
        Some(Object::Str(s)) => s.to_string(),
        _ => String::new(),
    };
    if bio_cell(incoming).is_none() || bio_cell(outgoing).is_none() {
        return Err(value_error(
            "ssl: wrap_bio needs two valid MemoryBIO objects",
        ));
    }
    let conn = with_ctx(ctx, |c| -> Result<Connection, RuntimeError> {
        if server_side {
            let scfg = build_server_config(c)?;
            Ok(Connection::Server(
                ServerConnection::new(scfg)
                    .map_err(|e| ssl_error_rt(format!("server init: {e}")))?,
            ))
        } else {
            let ccfg = build_client_config(c)?;
            let name_str = if server_hostname.is_empty() {
                "localhost".to_owned()
            } else {
                server_hostname.clone()
            };
            let sni: ServerName<'static> = ServerName::try_from(name_str.clone())
                .map_err(|_| value_error(format!("invalid server_hostname: {name_str}")))?;
            Ok(Connection::Client(
                ClientConnection::new(ccfg, sni)
                    .map_err(|e| ssl_error_rt(format!("client init: {e}")))?,
            ))
        }
    })??;
    let id = next_id();
    bio_sessions().lock().insert(
        id,
        Rc::new(RefCell::new(BioSession {
            conn,
            incoming,
            outgoing,
            server_side,
            sni: server_hostname,
            close_sent: false,
        })),
    );
    Ok(Object::Int(id))
}

fn ns_bio_do_handshake(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    with_bio_session(id, |s, inb, outb| {
        loop {
            // Emit whatever handshake flight rustls has ready for the peer.
            bio_flush_out(&mut s.conn, outb);
            if !s.conn.is_handshaking() {
                return Ok(Object::None);
            }
            // Need the peer's next flight. If the incoming BIO is dry, ask the
            // caller to pump more ciphertext (asyncio retries next turn).
            if inb.buf.is_empty() {
                if inb.write_eof {
                    return Err(eof_error());
                }
                return Err(want_read_error());
            }
            match s.conn.read_tls(&mut BioReader { bio: inb }) {
                Ok(0) => return Err(eof_error()),
                Ok(_) => {
                    s.conn
                        .process_new_packets()
                        .map_err(|e| tls_process_error(&e))?;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    return Err(want_read_error());
                }
                Err(e) => return Err(ssl_error_rt(format!("read_tls: {e}"))),
            }
        }
    })
}

fn ns_bio_write(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    let data = arg_bytes_like(args, 1, "data")?;
    with_bio_session(id, |s, _inb, outb| {
        let n = s
            .conn
            .writer()
            .write(&data)
            .map_err(|e| ssl_error_rt(format!("write: {e}")))?;
        bio_flush_out(&mut s.conn, outb);
        Ok(Object::Int(n as i64))
    })
}

fn ns_bio_read(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    let n = arg_int(args, 1, "len").unwrap_or(4096).max(0) as usize;
    if n == 0 {
        return Ok(Object::new_bytes(Vec::new()));
    }
    with_bio_session(id, |s, inb, outb| {
        let mut buf = vec![0u8; n];
        loop {
            match s.conn.reader().read(&mut buf) {
                Ok(0) => return Ok(Object::new_bytes(Vec::new())), // clean close_notify EOF
                Ok(r) => {
                    buf.truncate(r);
                    return Ok(Object::new_bytes(buf));
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(ssl_error_rt(format!("read: {e}"))),
            }
            if !s.conn.wants_read() {
                return Err(want_read_error());
            }
            if inb.buf.is_empty() {
                if inb.write_eof {
                    return Err(eof_error());
                }
                return Err(want_read_error());
            }
            match s.conn.read_tls(&mut BioReader { bio: inb }) {
                Ok(0) => return Ok(Object::new_bytes(Vec::new())),
                Ok(_) => {
                    s.conn
                        .process_new_packets()
                        .map_err(|e| tls_process_error(&e))?;
                    // Post-handshake messages / acks the peer may be waiting on.
                    bio_flush_out(&mut s.conn, outb);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    return Err(want_read_error());
                }
                Err(e) => return Err(ssl_error_rt(format!("read_tls: {e}"))),
            }
        }
    })
}

fn ns_bio_pending(args: &[Object]) -> Result<Object, RuntimeError> {
    let _id = arg_int(args, 0, "session")?;
    Ok(Object::Int(0))
}

fn ns_bio_peer_cert_der(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    let der = bio_session_cell(id).and_then(|cell| {
        cell.borrow()
            .conn
            .peer_certificates()
            .and_then(|c| c.first().map(|c| c.as_ref().to_vec()))
    });
    match der {
        Some(d) => Ok(Object::new_bytes(d)),
        None => Ok(Object::None),
    }
}

fn ns_bio_cipher(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    let info = bio_session_cell(id).and_then(|cell| {
        let s = cell.borrow();
        let v = s.conn.protocol_version()?;
        let cs = s.conn.negotiated_cipher_suite()?;
        Some((tls_version_str(v), cipher_name(cs.suite())))
    });
    match info {
        Some((proto, name)) => Ok(Object::new_tuple(vec![
            Object::from_str(name),
            Object::from_str(proto),
            Object::Int(256),
        ])),
        None => Ok(Object::None),
    }
}

fn ns_bio_version(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    let v = bio_session_cell(id)
        .and_then(|cell| cell.borrow().conn.protocol_version().map(tls_version_str));
    match v {
        Some(s) => Ok(Object::from_str(s)),
        None => Ok(Object::None),
    }
}

fn ns_bio_selected_alpn(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    let p = bio_session_cell(id).and_then(|cell| {
        cell.borrow()
            .conn
            .alpn_protocol()
            .map(|b| String::from_utf8_lossy(b).into_owned())
    });
    match p {
        Some(s) => Ok(Object::from_str(s)),
        None => Ok(Object::None),
    }
}

fn ns_bio_shutdown(args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython's `SSLObject.unwrap()` is a *bidirectional* TLS close: emit our
    // `close_notify` (once), then wait for the peer's. If the peer's hasn't
    // arrived yet, raise `SSL_ERROR_WANT_READ` so the caller pumps the BIOs and
    // retries (test_ssl `SSLObjectTests.test_unwrap`); once it has, return.
    let id = arg_int(args, 0, "session")?;
    with_bio_session(id, |s, inb, outb| {
        if !s.close_sent {
            s.conn.send_close_notify();
            s.close_sent = true;
            bio_flush_out(&mut s.conn, outb);
        }
        loop {
            if inb.buf.is_empty() {
                if inb.write_eof {
                    return Ok(Object::None); // transport gone — treat as closed
                }
                return Err(want_read_error());
            }
            match s.conn.read_tls(&mut BioReader { bio: inb }) {
                Ok(0) => return Ok(Object::None),
                Ok(_) => {
                    let io = s
                        .conn
                        .process_new_packets()
                        .map_err(|e| tls_process_error(&e))?;
                    bio_flush_out(&mut s.conn, outb);
                    if io.peer_has_closed() {
                        return Ok(Object::None);
                    }
                    // Otherwise keep draining (tickets / app data) until the
                    // peer's close_notify shows up or the BIO empties.
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    return Err(want_read_error());
                }
                Err(e) => return Err(ssl_error_rt(format!("read_tls: {e}"))),
            }
        }
    })
}

fn ns_bio_close(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    bio_sessions().lock().remove(&id);
    Ok(Object::None)
}

fn builtin(name: &'static str, f: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(f),
        call_kw: None,
    }))
}

/// Build the native `_ssl` module.
pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_ssl"),
        );
        macro_rules! func {
            ($n:literal, $f:expr) => {
                d.insert(DictKey(Object::from_static($n)), builtin($n, $f));
            };
        }
        func!("new_context", ns_new_context);
        func!("load_cert_chain", ns_load_cert_chain);
        func!("load_verify_locations", ns_load_verify_locations);
        func!("set_verify_mode", ns_set_verify_mode);
        func!("get_verify_mode", ns_get_verify_mode);
        func!("set_check_hostname", ns_set_check_hostname);
        func!("get_check_hostname", ns_get_check_hostname);
        func!("set_alpn_protocols", ns_set_alpn_protocols);
        func!("wrap_socket", ns_wrap_socket);
        func!("do_handshake", ns_do_handshake);
        func!("read", ns_read);
        func!("write", ns_write);
        func!("pending", ns_pending);
        func!("peer_cert_der", ns_peer_cert_der);
        func!("cipher", ns_cipher);
        func!("version", ns_version);
        func!("selected_alpn", ns_selected_alpn);
        func!("shutdown", ns_shutdown);
        func!("close", ns_close);
        // MemoryBIO / wrap_bio (SSLObject) — the socketless, in-memory path.
        func!("memory_bio_new", ns_memory_bio_new);
        func!("memory_bio_write", ns_memory_bio_write);
        func!("memory_bio_read", ns_memory_bio_read);
        func!("memory_bio_pending", ns_memory_bio_pending);
        func!("memory_bio_eof", ns_memory_bio_eof);
        func!("memory_bio_set_eof", ns_memory_bio_set_eof);
        func!("memory_bio_free", ns_memory_bio_free);
        func!("wrap_bio", ns_wrap_bio);
        func!("bio_do_handshake", ns_bio_do_handshake);
        func!("bio_read", ns_bio_read);
        func!("bio_write", ns_bio_write);
        func!("bio_pending", ns_bio_pending);
        func!("bio_peer_cert_der", ns_bio_peer_cert_der);
        func!("bio_cipher", ns_bio_cipher);
        func!("bio_version", ns_bio_version);
        func!("bio_selected_alpn", ns_bio_selected_alpn);
        func!("bio_shutdown", ns_bio_shutdown);
        func!("bio_close", ns_bio_close);

        macro_rules! konst {
            ($n:literal, $v:expr) => {
                d.insert(DictKey(Object::from_static($n)), Object::Int($v));
            };
        }
        // verify modes
        konst!("CERT_NONE", 0);
        konst!("CERT_OPTIONAL", 1);
        konst!("CERT_REQUIRED", 2);
        // protocols
        konst!("PROTOCOL_TLS", 2);
        konst!("PROTOCOL_TLS_CLIENT", 16);
        konst!("PROTOCOL_TLS_SERVER", 17);
        konst!("PROTOCOL_TLSv1", 3);
        konst!("PROTOCOL_TLSv1_1", 4);
        konst!("PROTOCOL_TLSv1_2", 5);
        // options (opaque bit flags; ssl.py ORs/masks them)
        konst!("OP_ALL", 0x8000_0050);
        konst!("OP_NO_SSLv2", 0x0100_0000);
        konst!("OP_NO_SSLv3", 0x0200_0000);
        konst!("OP_NO_TLSv1", 0x0400_0000);
        konst!("OP_NO_TLSv1_1", 0x1000_0000);
        konst!("OP_NO_TLSv1_2", 0x0800_0000);
        konst!("OP_NO_TLSv1_3", 0x2000_0000);
        konst!("OP_NO_COMPRESSION", 0x0002_0000);
        konst!("OP_CIPHER_SERVER_PREFERENCE", 0x0040_0000);
        konst!("OP_SINGLE_DH_USE", 0);
        konst!("OP_SINGLE_ECDH_USE", 0);
        konst!("OP_NO_TICKET", 0x0000_4000);
        konst!("OP_ENABLE_MIDDLEBOX_COMPAT", 0x0010_0000);
        // SSL error codes
        konst!("SSL_ERROR_NONE", 0);
        konst!("SSL_ERROR_SSL", 1);
        konst!("SSL_ERROR_WANT_READ", 2);
        konst!("SSL_ERROR_WANT_WRITE", 3);
        konst!("SSL_ERROR_WANT_X509_LOOKUP", 4);
        konst!("SSL_ERROR_SYSCALL", 5);
        konst!("SSL_ERROR_ZERO_RETURN", 6);
        konst!("SSL_ERROR_WANT_CONNECT", 7);
        konst!("SSL_ERROR_EOF", 8);
        // verify flags
        konst!("VERIFY_DEFAULT", 0);
        konst!("VERIFY_CRL_CHECK_LEAF", 0x4);
        konst!("VERIFY_CRL_CHECK_CHAIN", 0xC);
        konst!("VERIFY_X509_STRICT", 0x20);
        konst!("VERIFY_X509_TRUSTED_FIRST", 0x8000);
        konst!("HAS_SNI", 1);
        konst!("HAS_ECDH", 1);
        konst!("HAS_NPN", 0);
        konst!("HAS_ALPN", 1);
        konst!("HAS_TLSv1_3", 1);
        konst!("PROTO_VERSION_TLSv1_3", 0x0304);

        d.insert(
            DictKey(Object::from_static("OPENSSL_VERSION")),
            Object::from_static("rustls (WeavePy _ssl, ring)"),
        );
        d.insert(
            DictKey(Object::from_static("OPENSSL_VERSION_NUMBER")),
            Object::Int(0x3000_0000),
        );
        d.insert(
            DictKey(Object::from_static("OPENSSL_VERSION_INFO")),
            Object::new_tuple(vec![
                Object::Int(3),
                Object::Int(0),
                Object::Int(0),
                Object::Int(0),
                Object::Int(0),
            ]),
        );
        d.insert(
            DictKey(Object::from_static("_DEFAULT_CIPHERS")),
            Object::from_static("DEFAULT"),
        );
    }
    Rc::new(PyModule {
        name: "_ssl".to_owned(),
        filename: None,
        dict,
    })
}
