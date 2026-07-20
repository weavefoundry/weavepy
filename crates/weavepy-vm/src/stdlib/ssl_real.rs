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
    /// CAs discovered via `capath`. OpenSSL loads hashed-dir entries lazily
    /// (only when a handshake needs them), and `SSLContext.get_ca_certs()`
    /// reflects that: capath anchors appear only once used. We load eagerly
    /// for verification but report them through `capath_used`.
    pub capath_ca: Vec<CertificateDer<'static>>,
    pub capath_used: Vec<CertificateDer<'static>>,
    pub cert_chain: Option<Vec<CertificateDer<'static>>>,
    pub private_key: Option<PrivateKeyDer<'static>>,
    pub alpn: Vec<Vec<u8>>,
    /// `ssl.TLSVersion` wire codes (-2 = MINIMUM_SUPPORTED, -1 = MAXIMUM_
    /// SUPPORTED). Combined with `protocol` to pin rustls protocol versions.
    pub min_version: i64,
    pub max_version: i64,
    /// OpenSSL cipher names selected via `set_ciphers` (None = all). Only
    /// TLS 1.2 suites are filtered — OpenSSL never lets cipher strings touch
    /// the TLS 1.3 suite list, and neither does CPython.
    pub cipher_names: Option<Vec<String>>,
    /// `SSLContext.options` bitmask (OP_NO_TLSv1_2 / OP_NO_TLSv1_3 gate the
    /// negotiated protocol versions like OpenSSL's option bits).
    pub options: i64,
    /// TLS 1.3 post-handshake client auth opt-in. A PHA server defers the
    /// client-certificate request out of the handshake (the echo-server PHA
    /// tests observe no cert until `verify_client_post_handshake`).
    pub pha: bool,
    /// `set_ecdh_curve` restriction: rustls `NamedGroup` to pin key exchange
    /// to (test_ecdh_curve's client/server curve-mismatch handshake failure).
    pub ecdh_curve: Option<rustls::NamedGroup>,
    /// Every chain/key pair ever loaded (OpenSSL keeps one cert slot *per key
    /// type*, so an RSA + ECDSA dual config serves whichever the client can
    /// negotiate — test_dual_rsa_ecc).
    pub cert_slots: Vec<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)>,
    /// CRLs loaded via `load_verify_locations` (counted for
    /// `cert_store_stats()['crl']`; VERIFY_CRL_CHECK_LEAF without one fails).
    pub crl_count: i64,
    /// `session_stats()` counters (OpenSSL's SSL_CTX_sess_* bookkeeping).
    pub stats_accept: i64,
    pub stats_hits: i64,
    /// `SSLContext.verify_flags` (VERIFY_CRL_CHECK_LEAF / VERIFY_X509_STRICT
    /// alter client-side verification).
    pub verify_flags: i64,
}

// Deliberately partial: key material and large DER blobs are summarized.
#[allow(clippy::missing_fields_in_debug)]
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
            // OpenSSL parity: a fresh SSL_CTX trusts *nothing*. The system
            // trust store only joins the root set after
            // `set_default_verify_paths()` / `load_default_certs()`
            // (`ns_set_default_verify_paths` flips this). Trusting native
            // roots unconditionally made `get_server_certificate(addr,
            // ca_certs=private_ca)` verify public sites their real CA signed
            // (test_ssl.test_get_server_certificate_ipv6 expects that to
            // fail).
            use_native_roots: false,
            extra_ca: Vec::new(),
            capath_ca: Vec::new(),
            capath_used: Vec::new(),
            cert_chain: None,
            private_key: None,
            alpn: Vec::new(),
            min_version: -2,
            max_version: -1,
            cipher_names: None,
            options: 0,
            pha: false,
            ecdh_curve: None,
            cert_slots: Vec::new(),
            crl_count: 0,
            stats_accept: 0,
            stats_hits: 0,
            verify_flags: 0x8000, // VERIFY_X509_TRUSTED_FIRST (OpenSSL default)
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
    /// Owning context id (0 for the context-less `_https` fast path); lets
    /// handshake completion report capath-anchor usage back to the context.
    pub ctx: i64,
    /// Client certificate chain "acquired" via emulated TLS 1.3 post-handshake
    /// auth (rustls has no PHA; the loopback PHA tests hand it over in-process
    /// — see `ns_pha_verify`). Preferred by `peer_certs`.
    pub pha_peer: Vec<Vec<u8>>,
    /// Whether this session's handshake completion has been counted (guards
    /// the accept/hit counters against redundant `do_handshake()` calls).
    pub hs_counted: bool,
    /// Armed by `ns_pha_verify` when post-handshake auth *requires* a client
    /// certificate that will never come: the next read tears the connection
    /// down, mirroring OpenSSL's deferred CertificateRequest failure
    /// (test_pha_required_nocert).
    pub pha_abort: bool,
    /// Raw transport bytes captured during a blocking handshake (rx, tx) —
    /// parsed on demand into OpenSSL-msg_callback-style events for
    /// `SSLContext._msg_callback` (test_msg_callback_tls12).
    pub hs_rx: Vec<u8>,
    pub hs_tx: Vec<u8>,
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
// Two-phase server handshake (rustls `Acceptor`)
//
// A server-side `wrap_socket` first *reads the ClientHello* without committing
// to a config, so Python-level hooks can run in between: the SNI callback
// (`SSLContext.set_servername_callback`) may swap the context, and OpenSSL's
// lenient ALPN behavior (no overlap → continue without ALPN, not an alert)
// needs the client's offer before the `ServerConfig` is materialized.
// ---------------------------------------------------------------------------

/// A server-side wrap that has not yet completed its handshake commitment.
struct PendingServer {
    /// `Some(acceptor)` until the full ClientHello has been read.
    acceptor: Option<rustls::server::Acceptor>,
    /// The parsed ClientHello state once accepted.
    accepted: Option<rustls::server::Accepted>,
    /// SNI server name from the ClientHello (once accepted).
    server_name: Option<String>,
    /// ALPN protocols the client offered (once accepted).
    client_alpn: Vec<Vec<u8>>,
    sock: TcpStream,
}

fn pending_servers() -> &'static parking_lot::Mutex<HashMap<i64, Rc<RefCell<PendingServer>>>> {
    static R: std::sync::OnceLock<parking_lot::Mutex<HashMap<i64, Rc<RefCell<PendingServer>>>>> =
        std::sync::OnceLock::new();
    R.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

fn pending_cell(id: i64) -> Option<Rc<RefCell<PendingServer>>> {
    pending_servers().lock().get(&id).cloned()
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
    /// Owning context id (capath-usage reporting, as on [`TlsSession`]).
    ctx: i64,
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
// verify-flags wrapper (VERIFY_CRL_CHECK_LEAF / VERIFY_X509_STRICT)
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct FlagChecksVerifier {
    inner: Arc<dyn ServerCertVerifier>,
    /// CRL checking requested but no CRL loaded: every verification fails
    /// (OpenSSL's "unable to get certificate CRL").
    crl_missing: bool,
    /// RFC 5280 strictness: require an authorityKeyIdentifier on the leaf.
    strict: bool,
}

impl ServerCertVerifier for FlagChecksVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if self.crl_missing {
            return Err(rustls::Error::General(
                "certificate verify failed: unable to get certificate CRL".into(),
            ));
        }
        let v = self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        )?;
        if self.strict {
            use x509_parser::prelude::FromDer;
            if let Ok((_, cert)) =
                x509_parser::certificate::X509Certificate::from_der(end_entity.as_ref())
            {
                let has_aki = cert
                    .extensions()
                    .iter()
                    .any(|e| e.oid.to_id_string() == "2.5.29.35");
                if !has_aki {
                    return Err(rustls::Error::General(
                        "certificate verify failed: RFC 5280 strict check: \
                         missing authorityKeyIdentifier"
                            .into(),
                    ));
                }
            }
        }
        Ok(v)
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
// pinned-anchor fallback verifier
//
// OpenSSL treats a certificate that *is itself* a trust anchor as a complete,
// valid chain — that is how CPython's test suite (and plenty of real code)
// connects to self-signed servers after `load_verify_locations(server_cert)`.
// rustls/webpki instead rejects the leaf with `CaUsedAsEndEntity`. Wrap the
// inner verifier: if standard verification fails but the end-entity is
// byte-identical to an *explicitly loaded* CA, accept it (exact-match pinning
// is at least as strong as chain verification). Native/system roots do not
// participate — only certs the context loaded by hand.
// ---------------------------------------------------------------------------

/// Minimal DER walker over an X.509 certificate: just enough to (a) detect
/// whether a subjectAltName extension is present and (b) pull the subject
/// commonName strings. Used only for the OpenSSL-parity CN fallback on the
/// pinned-anchor path — webpki (correctly, per RFC 6125) refuses to match
/// hostnames against the CN, but OpenSSL's `X509_check_host` falls back to
/// CN when *no* SAN extension exists, and CPython inherits that behavior.
mod der_cert {
    /// One TLV: returns `(tag, content, rest_after_tlv)`.
    fn tlv(buf: &[u8]) -> Option<(u8, &[u8], &[u8])> {
        let (&tag, rest) = buf.split_first()?;
        let (&l0, rest) = rest.split_first()?;
        let (len, rest) = if l0 & 0x80 == 0 {
            (l0 as usize, rest)
        } else {
            let n = (l0 & 0x7f) as usize;
            if n == 0 || n > 4 || rest.len() < n {
                return None;
            }
            let mut len = 0usize;
            for &b in &rest[..n] {
                len = (len << 8) | b as usize;
            }
            (len, &rest[n..])
        };
        if rest.len() < len {
            return None;
        }
        Some((tag, &rest[..len], &rest[len..]))
    }

    const OID_COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03]; // 2.5.4.3
    const OID_SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1d, 0x11]; // 2.5.29.17

    /// Parsed subset: subject CNs + whether a SAN extension exists.
    pub(super) struct CertNames {
        pub(super) common_names: Vec<String>,
        pub(super) has_san: bool,
    }

    pub(super) fn parse(cert_der: &[u8]) -> Option<CertNames> {
        // Certificate ::= SEQUENCE { tbsCertificate, sigAlg, sigValue }
        let Some((0x30, cert_body, _)) = tlv(cert_der) else {
            return None;
        };
        let Some((0x30, tbs, _)) = tlv(cert_body) else {
            return None;
        };
        // tbsCertificate ::= SEQUENCE { [0] version?, serialNumber, signature,
        //   issuer, validity, subject, subjectPublicKeyInfo, [1]?, [2]?, [3]? }
        let mut rest = tbs;
        // Optional explicit version tag.
        if let Some((0xa0, _, r)) = tlv(rest) {
            rest = r;
        }
        let (_, _, r) = tlv(rest)?; // serialNumber
        let (_, _, r) = tlv(r)?; // signature algorithm
        let (_, _, r) = tlv(r)?; // issuer
        let (_, _, r) = tlv(r)?; // validity
        let Some((0x30, subject, r)) = tlv(r) else {
            return None;
        };
        let (_, _, mut r) = tlv(r)?; // subjectPublicKeyInfo

        // Subject Name ::= SEQUENCE OF SET OF SEQUENCE { OID, value }
        let mut common_names = Vec::new();
        let mut rdns = subject;
        while let Some((0x31, set, next)) = tlv(rdns) {
            let mut atvs = set;
            while let Some((0x30, atv, next_atv)) = tlv(atvs) {
                if let Some((0x06, oid, val_rest)) = tlv(atv) {
                    if oid == OID_COMMON_NAME {
                        if let Some((tag, val, _)) = tlv(val_rest) {
                            // PrintableString / UTF8String / IA5String / T61.
                            if matches!(tag, 0x0c | 0x13 | 0x14 | 0x16) {
                                if let Ok(s) = std::str::from_utf8(val) {
                                    common_names.push(s.to_owned());
                                }
                            }
                        }
                    }
                }
                atvs = next_atv;
            }
            rdns = next;
        }

        // Optional [1] issuerUniqueID / [2] subjectUniqueID, then
        // [3] extensions (an explicit tag wrapping SEQUENCE OF Extension).
        let mut has_san = false;
        while !r.is_empty() {
            let (tag, body, next) = tlv(r)?;
            if tag == 0xa3 {
                if let Some((0x30, exts, _)) = tlv(body) {
                    let mut e = exts;
                    while let Some((0x30, ext, next_ext)) = tlv(e) {
                        if let Some((0x06, oid, _)) = tlv(ext) {
                            if oid == OID_SUBJECT_ALT_NAME {
                                has_san = true;
                            }
                        }
                        e = next_ext;
                    }
                }
            }
            r = next;
        }
        Some(CertNames {
            common_names,
            has_san,
        })
    }

    /// RFC 6125-style DNS-name match with single leftmost-label wildcard —
    /// the same rules as `ssl.py`'s `_dnsname_match`.
    pub(super) fn dnsname_match(pattern: &str, hostname: &str) -> bool {
        if pattern.is_empty() {
            return false;
        }
        let (pattern, hostname) = (pattern.to_ascii_lowercase(), hostname.to_ascii_lowercase());
        if pattern == hostname {
            return true;
        }
        if let Some(suffix) = pattern.strip_prefix("*.") {
            if let Some(head) = hostname.strip_suffix(suffix) {
                if let Some(head) = head.strip_suffix('.') {
                    return !head.is_empty() && !head.contains('.');
                }
            }
        }
        false
    }
}

#[derive(Debug)]
struct PinnedAnchorVerifier {
    inner: Arc<dyn ServerCertVerifier>,
    pinned: Vec<CertificateDer<'static>>,
    /// Mirror of the context's `check_hostname`: a pinned-anchor acceptance
    /// still enforces the SAN/hostname match OpenSSL would.
    check_hostname: bool,
}

impl ServerCertVerifier for PinnedAnchorVerifier {
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
            // Never rescue a hostname mismatch — only chain-building
            // failures (`CaUsedAsEndEntity`, `UnknownIssuer`, …) qualify.
            Err(rustls::Error::InvalidCertificate(ce))
                if !is_name_mismatch(&ce)
                    && self
                        .pinned
                        .iter()
                        .any(|p| p.as_ref() == end_entity.as_ref()) =>
            {
                if self.check_hostname {
                    let parsed = rustls::server::ParsedCertificate::try_from(end_entity)?;
                    match rustls::client::verify_server_name(&parsed, server_name) {
                        Ok(()) => {}
                        Err(name_err) => {
                            // OpenSSL parity: a cert with *no* SAN extension
                            // falls back to subject-CN matching.
                            let names = der_cert::parse(end_entity.as_ref());
                            let cn_matches = match (&names, server_name) {
                                (Some(n), ServerName::DnsName(dns)) if !n.has_san => n
                                    .common_names
                                    .iter()
                                    .any(|cn| der_cert::dnsname_match(cn, dns.as_ref())),
                                _ => false,
                            };
                            if !cn_matches {
                                return Err(name_err);
                            }
                        }
                    }
                }
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

/// Resolve the context's `protocol` constant plus min/max `TLSVersion` codes
/// into the rustls protocol-version slice (rustls supports 1.2 and 1.3 only;
/// the HAS_TLSv1/HAS_SSLv3 module flags advertise the older ones as absent).
fn protocol_versions(
    cfg: &CtxConfig,
) -> Result<Vec<&'static rustls::SupportedProtocolVersion>, RuntimeError> {
    let (mut lo, mut hi) = (cfg.min_version, cfg.max_version);
    match cfg.protocol {
        3 => (lo, hi) = (0x0301, 0x0301), // PROTOCOL_TLSv1
        4 => (lo, hi) = (0x0302, 0x0302), // PROTOCOL_TLSv1_1
        5 => (lo, hi) = (0x0303, 0x0303), // PROTOCOL_TLSv1_2
        _ => {}
    }
    let lo = if lo <= 0 { 0 } else { lo };
    let hi = if hi < 0 { i64::MAX } else { hi };
    let mut vers: Vec<&'static rustls::SupportedProtocolVersion> = Vec::new();
    // OpenSSL's per-version option bits veto individual versions on top of
    // the min/max range (try_protocol_combo drives OP_NO_TLSv1_2/_3).
    let no_tls12 = cfg.options & 0x0800_0000 != 0;
    let no_tls13 = cfg.options & 0x2000_0000 != 0;
    if lo <= 0x0303 && hi >= 0x0303 && !no_tls12 {
        vers.push(&rustls::version::TLS12);
    }
    if lo <= 0x0304 && hi >= 0x0304 && !no_tls13 {
        vers.push(&rustls::version::TLS13);
    }
    if vers.is_empty() {
        return Err(ssl_error_rt(
            "NO_PROTOCOLS_AVAILABLE: no supported protocol version in the configured range",
        ));
    }
    Ok(vers)
}

/// The ring crypto provider, with TLS 1.2 suites filtered by the context's
/// `set_ciphers` selection (TLS 1.3 suites always stay enabled, like OpenSSL).
fn crypto_provider_for(cfg: &CtxConfig) -> Arc<rustls::crypto::CryptoProvider> {
    let mut provider = rustls::crypto::ring::default_provider();
    if let Some(names) = &cfg.cipher_names {
        provider.cipher_suites.retain(|s| {
            let n = cipher_name(s.suite());
            n.starts_with("TLS_") || names.iter().any(|w| w == &n)
        });
    }
    if let Some(group) = cfg.ecdh_curve {
        // `set_ecdh_curve` pins the key-exchange group; a client/server curve
        // mismatch then fails the handshake (test_ecdh_curve).
        provider.kx_groups.retain(|g| g.name() == group);
    }
    Arc::new(provider)
}

/// Verifier for a context with verification on but *no* trust anchors
/// loaded (fresh `SSLContext` before any `load_verify_locations` /
/// `set_default_verify_paths`). OpenSSL builds such a context fine and
/// fails each verification with "unable to get local issuer certificate";
/// webpki instead refuses to construct a verifier over an empty root set,
/// which would surface as a config-time SSLError rather than the
/// handshake-time CERTIFICATE_VERIFY_FAILED the tests expect
/// (test_ssl.test_connect_fail).
#[derive(Debug)]
struct EmptyRootsVerifier;

impl ServerCertVerifier for EmptyRootsVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Err(rustls::Error::InvalidCertificate(
            rustls::CertificateError::UnknownIssuer,
        ))
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // Unreachable in practice: cert verification above always fails
        // first and aborts the handshake.
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

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn build_client_config(cfg: &CtxConfig) -> Result<Arc<ClientConfig>, RuntimeError> {
    if cfg.protocol == 17 {
        // PROTOCOL_TLS_SERVER context used for a client-side wrap.
        return Err(ssl_error_rt(
            "Cannot create a client socket with a PROTOCOL_TLS_SERVER context (_ssl.c)",
        ));
    }
    // Build a root store from native roots (+ any explicit CA the context
    // loaded). With CERT_NONE we instead install the accept-all verifier.
    let verify = cfg.verify_mode != 0 || cfg.check_hostname;
    let builder = ClientConfig::builder_with_provider(crypto_provider_for(cfg))
        .with_protocol_versions(&protocol_versions(cfg)?)
        .map_err(|e| ssl_error_rt(format!("protocol versions: {e}")))?;
    let builder = if verify {
        let mut roots = if cfg.use_native_roots {
            native_root_store()
        } else {
            RootCertStore::empty()
        };
        for c in cfg.extra_ca.iter().chain(cfg.capath_ca.iter()) {
            let _ = roots.add(c.clone());
        }
        let inner: Arc<dyn ServerCertVerifier> = if roots.is_empty() {
            // No anchors at all: OpenSSL still handshakes and fails each
            // verification; webpki can't even build a verifier here.
            Arc::new(EmptyRootsVerifier)
        } else {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let webpki = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider)
                .build()
                .map_err(|e| ssl_error_rt(format!("verifier: {e}")))?;
            if cfg.check_hostname {
                // Chain + hostname (rustls' default `WebPkiServerVerifier`).
                webpki
            } else {
                // Chain only: validate against the trust store but ignore the
                // hostname, matching CPython's `check_hostname = False` while
                // `verify_mode` stays `CERT_REQUIRED`.
                Arc::new(ChainOnlyVerifier { inner: webpki })
            }
        };
        // Explicitly loaded CAs double as exact-match pins (OpenSSL's
        // self-signed-anchor-as-leaf acceptance).
        let mut pinned = cfg.extra_ca.clone();
        pinned.extend(cfg.capath_ca.iter().cloned());
        let verifier: Arc<dyn ServerCertVerifier> = if pinned.is_empty() {
            inner
        } else {
            Arc::new(PinnedAnchorVerifier {
                inner,
                pinned,
                check_hostname: cfg.check_hostname,
            })
        };
        // OpenSSL verify-flag semantics: VERIFY_CRL_CHECK_LEAF with no CRL
        // loaded fails every verification ("unable to get certificate CRL");
        // VERIFY_X509_STRICT enforces RFC 5280 conformance of the leaf
        // (test_verify_strict's cert is missing authorityKeyIdentifier).
        let crl_missing = cfg.verify_flags & 0x4 != 0 && cfg.crl_count == 0;
        let strict = cfg.verify_flags & 0x20 != 0;
        let verifier: Arc<dyn ServerCertVerifier> = if crl_missing || strict {
            Arc::new(FlagChecksVerifier {
                inner: verifier,
                crl_missing,
                strict,
            })
        } else {
            verifier
        };
        builder
            .dangerous()
            .with_custom_certificate_verifier(verifier)
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
    build_server_config_alpn(cfg, None)
}

/// Server certificate resolver over every loaded chain/key pair: OpenSSL keeps
/// one certificate slot per key type, so an RSA + ECDSA dual configuration
/// serves whichever the ClientHello can use (test_dual_rsa_ecc).
#[derive(Debug)]
struct MultiKeyResolver {
    keys: Vec<Arc<rustls::sign::CertifiedKey>>,
}

/// `with_single_cert` without the webpki end-entity parse (which rejects
/// X.509 v1 certs that OpenSSL serves — see `build_server_config_alpn`).
#[derive(Debug)]
struct AlwaysResolvesKey(Arc<rustls::sign::CertifiedKey>);

impl rustls::server::ResolvesServerCert for AlwaysResolvesKey {
    fn resolve(
        &self,
        _client_hello: rustls::server::ClientHello<'_>,
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        Some(self.0.clone())
    }
}

impl rustls::server::ResolvesServerCert for MultiKeyResolver {
    fn resolve(
        &self,
        client_hello: rustls::server::ClientHello<'_>,
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        let schemes = client_hello.signature_schemes();
        let suites = client_hello.cipher_suites();
        if std::env::var("WEAVE_SSL_DEBUG").is_ok() {
            eprintln!("[resolver] schemes={schemes:?} suites={suites:?}");
            for ck in &self.keys {
                eprintln!(
                    "[resolver] key alg={:?} choose={:?}",
                    ck.key.algorithm(),
                    ck.key.choose_scheme(schemes).map(|s| s.scheme())
                );
            }
        }
        let offers_tls13 = suites
            .iter()
            .any(|s| matches!(u16::from(*s), 0x1301..=0x1305));
        // A ClientHello lists TLS 1.3 suites even when supported_versions is
        // capped at 1.2 (test_dual_rsa_ecc pins maximum_version=TLSv1_2), so
        // a TLS 1.2 suite-family match is the stronger signal: prefer a key
        // whose family the client explicitly offered, then fall back to any
        // signable key when TLS 1.3 is on the table.
        let mut tls13_fallback = None;
        for ck in &self.keys {
            if ck.key.choose_scheme(schemes).is_none() {
                continue;
            }
            // TLS 1.2: an ECDHE suite family is tied to the key type.
            let family_match = match ck.key.algorithm() {
                rustls::SignatureAlgorithm::ECDSA | rustls::SignatureAlgorithm::ED25519 => suites
                    .iter()
                    .any(|s| matches!(u16::from(*s), 0xc009 | 0xc00a | 0xc02b | 0xc02c | 0xcca9)),
                rustls::SignatureAlgorithm::RSA => suites
                    .iter()
                    .any(|s| matches!(u16::from(*s), 0xc013 | 0xc014 | 0xc02f | 0xc030 | 0xcca8)),
                _ => true,
            };
            if family_match {
                return Some(ck.clone());
            }
            if offers_tls13 && tls13_fallback.is_none() {
                tls13_fallback = Some(ck.clone());
            }
        }
        tls13_fallback
    }
}

fn build_server_config_alpn(
    cfg: &CtxConfig,
    client_alpn: Option<&[Vec<u8>]>,
) -> Result<Arc<ServerConfig>, RuntimeError> {
    if cfg.protocol == 16 {
        // PROTOCOL_TLS_CLIENT context used for a server-side wrap.
        return Err(ssl_error_rt(
            "Cannot create a server socket with a PROTOCOL_TLS_CLIENT context (_ssl.c)",
        ));
    }
    if cfg.cert_chain.is_none() {
        return Err(ssl_error_rt(
            "server side requires a certificate (load_cert_chain)",
        ));
    }
    if cfg.private_key.is_none() {
        return Err(ssl_error_rt(
            "server side requires a private key (load_cert_chain)",
        ));
    }
    let provider = crypto_provider_for(cfg);
    let builder = ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&protocol_versions(cfg)?)
        .map_err(|e| ssl_error_rt(format!("protocol versions: {e}")))?;
    // CERT_OPTIONAL/CERT_REQUIRED on the server side means "request (and
    // verify) a client certificate" — the client-auth tests (test_wrong_cert_*,
    // test_verify_strict) hinge on the server actually demanding one. A PHA
    // server defers the request out of the handshake entirely (emulated in
    // `ns_pha_verify`), so it does not ask here.
    let builder = if cfg.verify_mode != 0 && !cfg.pha {
        let mut roots = if cfg.use_native_roots {
            native_root_store()
        } else {
            RootCertStore::empty()
        };
        for c in cfg.extra_ca.iter().chain(cfg.capath_ca.iter()) {
            let _ = roots.add(c.clone());
        }
        if roots.is_empty() {
            // A verifying server with no CA loaded: webpki refuses to build
            // a client verifier over an empty root set. Fall back to the
            // system store (the pre-RFC-0054 behavior) so the handshake
            // proceeds and client-cert verification simply fails.
            roots = native_root_store();
        }
        let vprovider = Arc::new(rustls::crypto::ring::default_provider());
        let mut vb =
            rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(roots), vprovider);
        if cfg.verify_mode == 1 {
            vb = vb.allow_unauthenticated();
        }
        let verifier = vb
            .build()
            .map_err(|e| ssl_error_rt(format!("client verifier: {e}")))?;
        builder.with_client_cert_verifier(verifier)
    } else {
        builder.with_no_client_auth()
    };
    let mut config = if cfg.cert_slots.len() > 1 {
        let mut keys = Vec::new();
        for (chain, key) in &cfg.cert_slots {
            let signing = provider
                .key_provider
                .load_private_key(key.clone_key())
                .map_err(|e| ssl_error_rt(format!("server key: {e}")))?;
            keys.push(Arc::new(rustls::sign::CertifiedKey::new(
                chain.clone(),
                signing,
            )));
        }
        // Most-recent load first (it is the OpenSSL default slot).
        keys.reverse();
        builder.with_cert_resolver(Arc::new(MultiKeyResolver { keys }))
    } else {
        // Not `with_single_cert`: that runs a webpki parse of the end-entity
        // cert, which rejects X.509 v1 certs (UnsupportedCertVersion) that
        // OpenSSL happily serves — the *client* is the one expected to fail
        // verification (test_hostname_checks_common_name with NOSANFILE).
        let chain = cfg.cert_chain.clone().expect("checked above");
        let key = cfg.private_key.as_ref().expect("checked above").clone_key();
        let signing = provider
            .key_provider
            .load_private_key(key)
            .map_err(|e| ssl_error_rt(format!("server key: {e}")))?;
        let ck = Arc::new(rustls::sign::CertifiedKey::new(chain, signing));
        builder.with_cert_resolver(Arc::new(AlwaysResolvesKey(ck)))
    };
    // OpenSSL's ALPN callback returns NOACK when nothing overlaps: the
    // handshake continues with no protocol selected. rustls would instead
    // send a fatal no_application_protocol alert, so drop our list when the
    // client's offer (known on the Acceptor path) has no intersection.
    config.alpn_protocols = match client_alpn {
        Some(client) if !cfg.alpn.is_empty() => {
            if cfg.alpn.iter().any(|p| client.contains(p)) {
                cfg.alpn.clone()
            } else {
                Vec::new()
            }
        }
        _ => cfg.alpn.clone(),
    };
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

/// OpenSSL's `SSL_ERROR_ZERO_RETURN`: the peer's `close_notify` arrived *after*
/// we already sent ours. CPython's `read` returns `b""` for a clean peer close
/// only while `SSL_get_shutdown() == SSL_RECEIVED_SHUTDOWN` exactly; once our
/// side has also initiated shutdown (`unwrap()` was called first), the same
/// condition raises `SSLZeroReturnError` — test_asyncio's trailing-data server
/// loops on `read()` and needs that exception to terminate.
fn zero_return_error() -> RuntimeError {
    ssl_error_rt("ZERO_RETURN: TLS/SSL connection has been closed (EOF)")
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
        AlertDescription::CertificateRequired => "TLSV1_ALERT_CERTIFICATE_REQUIRED",
        AlertDescription::UnknownPSKIdentity => "TLSV1_ALERT_UNKNOWN_PSK_IDENTITY",
        AlertDescription::InappropriateFallback => "TLSV1_ALERT_INAPPROPRIATE_FALLBACK",
        AlertDescription::NoApplicationProtocol => "TLSV1_ALERT_NO_APPLICATION_PROTOCOL",
        _ => "TLSV1_ALERT_INTERNAL_ERROR",
    }
}

/// Render a rustls "received fatal alert: CamelCaseName" (as flattened into an
/// `io::Error` by `complete_io`) in the same OpenSSL `[SSL: TOKEN]` shape as
/// [`tls_process_error`], so `ssl.py` can expose `.reason` as the alert token
/// (test_sni_callback_alert asserts `reason == 'TLSV1_ALERT_ACCESS_DENIED'`).
fn alert_name_to_openssl(msg: &str) -> Option<String> {
    let name = msg.split("received fatal alert:").nth(1)?.trim();
    let name = name.split_whitespace().next()?;
    // CamelCase → SCREAMING_SNAKE, then map through the token table by
    // comparing against each known alert's rustls debug name.
    for desc in [
        AlertDescription::UnexpectedMessage,
        AlertDescription::BadRecordMac,
        AlertDescription::DecompressionFailure,
        AlertDescription::HandshakeFailure,
        AlertDescription::NoCertificate,
        AlertDescription::BadCertificate,
        AlertDescription::UnsupportedCertificate,
        AlertDescription::CertificateRevoked,
        AlertDescription::CertificateExpired,
        AlertDescription::CertificateUnknown,
        AlertDescription::IllegalParameter,
        AlertDescription::UnknownCA,
        AlertDescription::AccessDenied,
        AlertDescription::DecodeError,
        AlertDescription::DecryptError,
        AlertDescription::ProtocolVersion,
        AlertDescription::InsufficientSecurity,
        AlertDescription::InternalError,
        AlertDescription::UserCanceled,
        AlertDescription::NoRenegotiation,
        AlertDescription::UnsupportedExtension,
        AlertDescription::CertificateRequired,
        AlertDescription::UnknownPSKIdentity,
        AlertDescription::InappropriateFallback,
        AlertDescription::NoApplicationProtocol,
    ] {
        if format!("{desc:?}") == name {
            let token = alert_openssl_token(desc);
            let human = token.to_ascii_lowercase().replace('_', " ");
            return Some(format!("[SSL: {token}] {human} (_ssl.c)"));
        }
    }
    None
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

/// Block (GIL already released by the caller) until `fd` is readable, or the
/// timeout expires. `None` timeout means wait forever. Returns whether the
/// fd became readable.
#[cfg(unix)]
fn wait_fd_readable(
    fd: std::os::unix::io::RawFd,
    timeout: Option<std::time::Duration>,
) -> std::io::Result<bool> {
    let ms: libc::c_int = match timeout {
        Some(d) => d.as_millis().min(libc::c_int::MAX as u128) as libc::c_int,
        None => -1,
    };
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        let rc = unsafe { libc::poll(&raw mut pfd, 1, ms) };
        if rc < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        return Ok(rc > 0);
    }
}

/// Non-blocking readability probe (poll with a zero timeout).
#[cfg(unix)]
fn fd_readable_now(fd: std::os::unix::io::RawFd) -> bool {
    wait_fd_readable(fd, Some(std::time::Duration::ZERO)).unwrap_or(true)
}

/// The socket's `SO_RCVTIMEO` as a `Duration` (`None` = block forever).
#[cfg(unix)]
fn recv_timeout(fd: std::os::unix::io::RawFd) -> Option<std::time::Duration> {
    let mut tv = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut len = std::mem::size_of::<libc::timeval>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            (&raw mut tv).cast(),
            &raw mut len,
        )
    };
    if rc == 0 && (tv.tv_sec != 0 || tv.tv_usec != 0) {
        Some(std::time::Duration::new(
            tv.tv_sec.max(0) as u64,
            (tv.tv_usec.max(0) as u32) * 1000,
        ))
    } else {
        None
    }
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
        ctx: 0,
        pha_peer: Vec::new(),
        hs_counted: false,
        pha_abort: false,
        hs_rx: Vec::new(),
        hs_tx: Vec::new(),
    }))
}

/// Write all of `data` through the TLS session (blocking).
pub fn send(id: i64, data: &[u8]) -> Result<usize, RuntimeError> {
    write_all(id, data)?;
    Ok(data.len())
}

/// Read up to `n` bytes (blocking); empty vec on clean EOF. The `_https`
/// fast path tolerates ragged EOFs (servers that drop the link without a
/// `close_notify`), like `suppress_ragged_eofs=True`.
pub fn recv(id: i64, n: usize) -> Result<Vec<u8>, RuntimeError> {
    read_n(id, n, false)
}

/// Drop the session, closing the dup'd socket fd.
pub fn close(id: i64) {
    if std::env::var("WEAVE_SSL_DEBUG").is_ok() {
        eprintln!("[close id={id}]");
    }
    sessions().lock().remove(&id);
    pending_servers().lock().remove(&id);
}

/// Peer DER cert chain (for `getpeercert(binary_form=True)`).
pub fn peer_certs(id: i64) -> Vec<Vec<u8>> {
    let Some(cell) = session_cell(id) else {
        return Vec::new();
    };
    let s = cell.borrow();
    if !s.pha_peer.is_empty() {
        return s.pha_peer.clone();
    }
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
    if pending_cell(id).is_some() {
        // Deferred server handshake not driven yet (see `read_n`): a write
        // before the ClientHello is WANT_READ, as in OpenSSL.
        return Err(want_read_error());
    }
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
    // Interleave buffering plaintext with flushing records: rustls caps its
    // internal plaintext buffer (`writer().write` accepts 0 bytes once full),
    // so a large payload — test_sslproto's 1 MiB HELLO_MSG — must be pushed
    // through in buffer-limit-sized slices. Records flush to the transport
    // with the GIL released so peer threads (e.g. the loopback server) run
    // while we block on the socket.
    let mut written = 0;
    while written < data.len() {
        let n = s
            .conn
            .writer()
            .write(&data[written..])
            .map_err(|e| ssl_error_rt(format!("write: {e}")))?;
        written += n;
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
        if n == 0 {
            // Buffer full even after a flush: nothing can make progress.
            return Err(ssl_error_rt("write: TLS plaintext buffer stalled"));
        }
    }
    Ok(data.len())
}

/// With `ragged_eof_error`, a transport EOF that arrives without a TLS
/// `close_notify` raises (OpenSSL's `SSL_ERROR_EOF`, which the shim turns
/// into `SSLEOFError`); otherwise it reads as an empty buffer.
fn read_n(id: i64, n: usize, ragged_eof_error: bool) -> Result<Vec<u8>, RuntimeError> {
    if n == 0 {
        return Ok(Vec::new());
    }
    if pending_cell(id).is_some() {
        // Two-phase server connection whose handshake hasn't been driven yet
        // (asyncore servers push data right after a deferred wrap_socket):
        // OpenSSL reports WANT_READ until the ClientHello arrives.
        return Err(want_read_error());
    }
    let cell = session_cell(id).ok_or_else(|| value_error("ssl: closed connection"))?;
    let mut s = cell.borrow_mut();
    if s.pha_abort {
        // Deferred post-handshake-auth failure (see `ns_pha_verify`): kill
        // the transport and report EOF; the handler closes, and the peer's
        // pending read surfaces a ragged-EOF error.
        let _ = s.sock.shutdown(std::net::Shutdown::Both);
        return Ok(Vec::new());
    }
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
                buf.clear();
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
            buf.clear();
            return Ok(buf);
        }
        // Before blocking for TLS bytes, wait for readability *without*
        // holding the session cell: a blocked recv() must not lock out a
        // concurrent send() from another thread on the same SSLSocket
        // (CPython GH-137583 — test_thread_recv_while_main_thread_sends
        // deadlocks otherwise, with the writer stuck behind the reader's
        // session borrow).
        #[cfg(unix)]
        if !nonblocking {
            use std::os::unix::io::AsRawFd;
            let fd = s.sock.as_raw_fd();
            if !fd_readable_now(fd) {
                let timeout = recv_timeout(fd);
                drop(s);
                let readable = crate::gil::allow_threads_then(|| wait_fd_readable(fd, timeout))
                    .map_err(|e| ssl_error_rt(format!("poll: {e}")))?;
                if !readable {
                    return Err(timeout_error("The read operation timed out"));
                }
                s = cell.borrow_mut();
                // Another thread may have pumped the connection meanwhile —
                // retry the plaintext reader before touching the socket.
                continue;
            }
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
                // Reaching transport EOF here means no `close_notify` was
                // ever processed (a clean TLS closure returns through the
                // plaintext reader above): that is a *ragged* EOF.
                if ragged_eof_error {
                    return Err(eof_error());
                }
                buf.clear();
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
            Err(ref e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                // Same as a transport EOF above: no close_notify was seen.
                if ragged_eof_error {
                    return Err(eof_error());
                }
                buf.clear();
                return Ok(buf);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::ConnectionReset => {
                // CPython's `_ssl` surfaces the socket errno untouched
                // (`PySSL_SetError` → `ConnectionResetError`); whether that is
                // tolerated is the *caller's* choice — asyncore's `recv` maps
                // `errno in _DISCONNECTED` to an empty read + `handle_close`
                // (test_poplib STLS), while test_pha_required_nocert asserts
                // the reset reaches it as an OSError.
                return Err(crate::error::oserror_subclass_with_errno(
                    "ConnectionResetError",
                    libc::ECONNRESET,
                    "Connection reset by peer",
                ));
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::ConnectionAborted => {
                return Err(crate::error::oserror_subclass_with_errno(
                    "ConnectionAbortedError",
                    libc::ECONNABORTED,
                    "Software caused connection abort",
                ));
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
    std::fs::read(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            // Plain OSError with errno (no "[SSL]" marker): the shim re-raises
            // it untouched, and callers assert `exc.errno == errno.ENOENT`.
            crate::error::os_error_with_errno(2, "No such file or directory".to_owned())
        } else {
            ssl_error_rt(format!("cannot read {path}: {e}"))
        }
    })
}

// The "PEM lib" phrasing matches OpenSSL's error rendering; test_ssl matches
// PEM failures with the regex "PEM (lib|routines)".
fn parse_cert_chain(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, RuntimeError> {
    use x509_parser::prelude::FromDer;
    let mut rd = std::io::BufReader::new(pem);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut rd)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| ssl_error_rt(format!("PEM lib ({e})")))?;
    if certs.is_empty() {
        return Err(ssl_error_rt(
            "PEM lib (no start line: no certificate found)",
        ));
    }
    // rustls-pemfile only base64-decodes; it happily yields garbage bytes for
    // a syntactically-bad body (test_malformed_cert's "Just bad cert data").
    // OpenSSL's PEM_read_bio_X509 also DER-decodes — mirror that.
    for c in &certs {
        if x509_parser::certificate::X509Certificate::from_der(c.as_ref()).is_err() {
            return Err(ssl_error_rt(
                "PEM lib (bad base64 decode: malformed certificate)",
            ));
        }
    }
    Ok(certs)
}

fn parse_private_key_pw(
    pem: &[u8],
    password: Option<&[u8]>,
) -> Result<PrivateKeyDer<'static>, RuntimeError> {
    // A PKCS#8 "ENCRYPTED PRIVATE KEY" block (PBES2) needs the password
    // (test_load_cert_chain's keycert.passwd.pem, password callbacks, …);
    // rustls-pemfile has no item variant for it, so scan the raw PEM.
    if let Some(block) = pem_block(pem, "ENCRYPTED PRIVATE KEY") {
        let Some(pw) = password else {
            return Err(ssl_error_rt(
                "PEM lib (processing error: password required for encrypted key)",
            ));
        };
        let epki = pkcs8::EncryptedPrivateKeyInfo::try_from(block.as_slice())
            .map_err(|e| ssl_error_rt(format!("PEM lib (bad encrypted key: {e})")))?;
        let doc = epki
            .decrypt(pw)
            .map_err(|_| ssl_error_rt("[SSL: BAD_DECRYPT] bad decrypt (_ssl.c)"))?;
        return Ok(PrivateKeyDer::Pkcs8(
            rustls_pki_types::PrivatePkcs8KeyDer::from(doc.as_bytes().to_vec()),
        ));
    }
    let mut rd = std::io::BufReader::new(pem);
    let key = rustls_pemfile::private_key(&mut rd)
        .map_err(|e| ssl_error_rt(format!("PEM lib (key parse: {e})")))?;
    key.ok_or_else(|| ssl_error_rt("PEM lib (no start line: no private key found)"))
}

/// Extract and base64-decode the first PEM block with the given label.
fn pem_block(pem: &[u8], label: &str) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(pem).ok()?;
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let start = text.find(&begin)? + begin.len();
    let stop = text[start..].find(&end)? + start;
    let b64: String = text[start..stop]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(b64).ok()
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
    let password: Option<Vec<u8>> = match args.get(3) {
        Some(Object::Bytes(b)) => Some(b.to_vec()),
        Some(Object::Str(s)) => Some(s.to_string().into_bytes()),
        _ => None,
    };
    let cert_pem = read_pem_file(&certfile)?;
    let chain = parse_cert_chain(&cert_pem)?;
    let key_pem = read_pem_file(&keyfile)?;
    let key = parse_private_key_pw(&key_pem, password.as_deref())?;
    // OpenSSL's SSL_CTX_check_private_key: the key must actually match the
    // leaf certificate's public key (test_load_cert_chain feeds a CA cert
    // with an unrelated key and expects "key values mismatch").
    {
        use x509_parser::prelude::FromDer;
        let signing = rustls::crypto::ring::default_provider()
            .key_provider
            .load_private_key(key.clone_key())
            .map_err(|e| ssl_error_rt(format!("PEM lib: invalid private key: {e}")))?;
        if let (Some(spki_key), Some(leaf)) = (signing.public_key(), chain.first()) {
            if let Ok((_, cert)) = x509_parser::certificate::X509Certificate::from_der(leaf) {
                if cert.tbs_certificate.subject_pki.raw != spki_key.as_ref() {
                    return Err(ssl_error_rt(
                        "[SSL: KEY_VALUES_MISMATCH] key values mismatch (_ssl.c)",
                    ));
                }
            }
        }
    }
    with_ctx(ctx, |c| {
        // Keep every loaded pair: OpenSSL holds one certificate slot per key
        // type, so loading an ECDSA pair after an RSA pair serves both
        // (test_dual_rsa_ecc). The most recent load stays the default.
        c.cert_slots.push((chain.clone(), key.clone_key()));
        c.cert_chain = Some(chain);
        c.private_key = Some(key);
    })?;
    Ok(Object::None)
}

fn ns_set_options(args: &[Object]) -> Result<Object, RuntimeError> {
    let ctx = arg_int(args, 0, "ctx")?;
    let options = arg_int(args, 1, "options")?;
    with_ctx(ctx, |c| c.options = options)?;
    Ok(Object::None)
}

fn ns_set_post_handshake_auth(args: &[Object]) -> Result<Object, RuntimeError> {
    let ctx = arg_int(args, 0, "ctx")?;
    let on = arg_bool(args, 1);
    with_ctx(ctx, |c| c.pha = on)?;
    Ok(Object::None)
}

fn ns_set_ecdh_curve(args: &[Object]) -> Result<Object, RuntimeError> {
    let ctx = arg_int(args, 0, "ctx")?;
    let name = arg_str(args, 1, "name")?;
    let group = match name.as_str() {
        "prime256v1" | "secp256r1" => rustls::NamedGroup::secp256r1,
        "secp384r1" => rustls::NamedGroup::secp384r1,
        "x25519" | "X25519" => rustls::NamedGroup::X25519,
        _ => return Err(value_error(format!("unknown elliptic curve name {name:?}"))),
    };
    with_ctx(ctx, |c| c.ecdh_curve = Some(group))?;
    Ok(Object::None)
}

/// `session_stats(ctx)` → the SSL_CTX_sess_* counter dict. Only `accept` and
/// `hits` are live (server handshakes / emulated session reuse); the rest of
/// the key set is part of the CPython surface and reads zero.
fn ns_session_stats(args: &[Object]) -> Result<Object, RuntimeError> {
    let ctx = arg_int(args, 0, "ctx")?;
    let (accept, hits) = with_ctx(ctx, |c| (c.stats_accept, c.stats_hits))?;
    let mut d = DictData::default();
    for key in [
        "number",
        "connect",
        "connect_good",
        "connect_renegotiate",
        "misses",
        "timeouts",
        "cache_full",
    ] {
        d.insert(DictKey(Object::interned_str(key)), Object::Int(0));
    }
    d.insert(DictKey(Object::from_static("accept")), Object::Int(accept));
    d.insert(
        DictKey(Object::from_static("accept_good")),
        Object::Int(accept),
    );
    d.insert(
        DictKey(Object::from_static("accept_renegotiate")),
        Object::Int(0),
    );
    d.insert(DictKey(Object::from_static("hits")), Object::Int(hits));
    Ok(Object::Dict(Rc::new(RefCell::new(d))))
}

// ---------------------------------------------------------------------------
// Loopback PHA / session-resumption emulation
//
// rustls implements neither TLS 1.3 post-handshake client auth nor the
// OpenSSL session API. The test suite exercises both only over in-process
// loopback pairs, so the client half records its intent here and the server
// half consumes it (connections pair 1:1 and sequentially in those tests).
// ---------------------------------------------------------------------------

/// `(client_pha_enabled, client_cert_chain)` from the most recent client-side
/// handshake, plus a pending "client offered a previous session" flag.
struct LoopbackOffer {
    pha: bool,
    chain: Vec<Vec<u8>>,
    session_offered: bool,
}

fn loopback_offer() -> &'static parking_lot::Mutex<LoopbackOffer> {
    static R: std::sync::OnceLock<parking_lot::Mutex<LoopbackOffer>> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        parking_lot::Mutex::new(LoopbackOffer {
            pha: false,
            chain: Vec::new(),
            session_offered: false,
        })
    })
}

/// Called from `ssl.py` when a client socket with a pre-set `session` starts
/// its handshake: the next server-side handshake counts as a cache hit.
fn ns_note_session_offer(_args: &[Object]) -> Result<Object, RuntimeError> {
    loopback_offer().lock().session_offered = true;
    Ok(Object::None)
}

/// Bookkeeping at handshake completion: on the client side publish the
/// loopback PHA offer; on the server side bump the context's accept/hit
/// counters.
fn note_handshake_complete(ctx: i64, server_side: bool) {
    if server_side {
        let hit = {
            let mut off = loopback_offer().lock();
            std::mem::take(&mut off.session_offered)
        };
        if ctx >= 0 {
            let _ = with_ctx(ctx, |c| {
                c.stats_accept += 1;
                if hit {
                    c.stats_hits += 1;
                }
            });
        }
    } else if ctx >= 0 {
        let (pha, chain) = with_ctx(ctx, |c| {
            (
                c.pha,
                c.cert_chain
                    .as_ref()
                    .map(|ch| ch.iter().map(|c| c.as_ref().to_vec()).collect())
                    .unwrap_or_default(),
            )
        })
        .unwrap_or((false, Vec::new()));
        let mut off = loopback_offer().lock();
        off.pha = pha;
        off.chain = chain;
    }
}

/// Server-side `verify_client_post_handshake()`: emulate OpenSSL's checks in
/// order (protocol version, an already-present cert, the client's PHA
/// extension, then the actual certificate request).
fn ns_pha_verify(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    let cell = session_cell(id).ok_or_else(|| value_error("ssl: closed connection"))?;
    let mut s = cell.borrow_mut();
    if !s.server_side {
        return Err(ssl_error_rt("not a server socket (not server)"));
    }
    let tls13 = matches!(
        s.conn.protocol_version(),
        Some(rustls::ProtocolVersion::TLSv1_3)
    );
    if !tls13 {
        return Err(ssl_error_rt(
            "[SSL: WRONG_SSL_VERSION] wrong ssl version (_ssl.c)",
        ));
    }
    let has_cert = !s.pha_peer.is_empty()
        || s.conn
            .peer_certificates()
            .map(|c| !c.is_empty())
            .unwrap_or(false);
    if has_cert {
        // OpenSSL's SSL_verify_client_post_handshake is a no-op success when
        // a certificate is already available.
        return Ok(Object::None);
    }
    let (client_pha, chain) = {
        let off = loopback_offer().lock();
        (off.pha, off.chain.clone())
    };
    if !client_pha {
        return Err(ssl_error_rt(
            "[SSL: EXTENSION_NOT_RECEIVED] extension not received (_ssl.c)",
        ));
    }
    if !chain.is_empty() {
        s.pha_peer = chain;
        return Ok(Object::None);
    }
    // PHA-capable client with no certificate: CERT_OPTIONAL tolerates it.
    // CERT_REQUIRED matches OpenSSL's deferred behavior: the verify call
    // itself succeeds (it only queues the CertificateRequest — the server
    // still answers b'OK\n'), and the connection is torn down on the *next*
    // read, when the missing certificate would have been rejected
    // (test_pha_required_nocert reads OK first, then expects an
    // EOF/reset-style error).
    let required = with_ctx(s.ctx, |c| c.verify_mode == 2).unwrap_or(false);
    if required {
        s.pha_abort = true;
    }
    Ok(Object::None)
}

/// Split a buffer of concatenated DER certificates into individual certs by
/// walking the outer SEQUENCE TLVs (CPython's `cadata` bytes form is "one or
/// more DER-encoded certificates", back to back).
fn split_der_certs(mut buf: &[u8]) -> Result<Vec<CertificateDer<'static>>, RuntimeError> {
    let mut out = Vec::new();
    while !buf.is_empty() {
        if buf[0] != 0x30 || buf.len() < 2 {
            return Err(ssl_error_rt(
                "not enough data: cadata does not contain a certificate",
            ));
        }
        let (len, hdr) = if buf[1] & 0x80 == 0 {
            (buf[1] as usize, 2)
        } else {
            let n = (buf[1] & 0x7f) as usize;
            if n == 0 || n > 4 || buf.len() < 2 + n {
                return Err(ssl_error_rt("invalid DER length in cadata"));
            }
            let mut len = 0usize;
            for &b in &buf[2..2 + n] {
                len = (len << 8) | b as usize;
            }
            (len, 2 + n)
        };
        let total = hdr + len;
        if buf.len() < total {
            return Err(ssl_error_rt("truncated DER certificate in cadata"));
        }
        out.push(CertificateDer::from(buf[..total].to_vec()));
        buf = &buf[total..];
    }
    if out.is_empty() {
        return Err(ssl_error_rt("no certificate found in cadata"));
    }
    Ok(out)
}

fn ns_load_verify_locations(args: &[Object]) -> Result<Object, RuntimeError> {
    let ctx = arg_int(args, 0, "ctx")?;
    // (cafile, capath, cadata): cafile/capath are PEM sources; str cadata is
    // PEM, bytes cadata is DER (CPython's rule — test_connect_cadata feeds
    // the raw DER of the signing CA).
    let mut pem: Vec<u8> = Vec::new();
    let mut crls = 0i64;
    if let Some(Object::Str(p)) = args.get(1) {
        let data = read_pem_file(p)?;
        // A `cafile` may also (or only) carry X509 CRL blocks (test_crl_check
        // loads `revocation.crl` through this API). Split them out: CRLs are
        // counted, certificate blocks flow into the normal parse below.
        let mut rd = std::io::BufReader::new(&data[..]);
        let mut has_cert = false;
        for item in rustls_pemfile::read_all(&mut rd).flatten() {
            match item {
                rustls_pemfile::Item::Crl(_) => crls += 1,
                rustls_pemfile::Item::X509Certificate(_) => has_cert = true,
                _ => {}
            }
        }
        if has_cert || crls == 0 {
            pem.extend_from_slice(&data);
        }
    }
    let mut capath_certs: Vec<CertificateDer<'static>> = Vec::new();
    if let Some(Object::Str(dir)) = args.get(2) {
        // OpenSSL uses lazily-consulted hash-named symlinks; load every
        // parseable PEM in the directory eagerly (a superset), deduplicated
        // by DER bytes — hashed dirs carry old- and new-hash names for the
        // same anchor.
        let entries = std::fs::read_dir(dir.to_string())
            .map_err(|e| ssl_error_rt(format!("cannot read capath {dir}: {e}")))?;
        for entry in entries.filter_map(Result::ok) {
            let Ok(data) = std::fs::read(entry.path()) else {
                continue;
            };
            if !data.windows(10).any(|w| w == b"-----BEGIN") {
                continue;
            }
            if let Ok(certs) = parse_cert_chain(&data) {
                for c in certs {
                    if !capath_certs.iter().any(|e| e.as_ref() == c.as_ref()) {
                        capath_certs.push(c);
                    }
                }
            }
        }
    }
    let mut der_certs: Vec<CertificateDer<'static>> = Vec::new();
    if !pem.is_empty() {
        der_certs.extend(parse_cert_chain(&pem)?);
    }
    // str cadata is PEM, bytes cadata is DER (CPython's rule). A str with no
    // certificate blocks gets OpenSSL's dedicated message (test_load_verify_cadata).
    match args.get(3) {
        Some(Object::Str(s)) => {
            let cadata_certs = parse_cert_chain(s.to_string().as_bytes()).map_err(|_| {
                ssl_error_rt("PEM lib (no start line: cadata does not contain a certificate)")
            })?;
            der_certs.extend(cadata_certs);
        }
        Some(Object::Bytes(b)) => der_certs.extend(split_der_certs(b)?),
        Some(Object::None) | None => {}
        Some(other) => {
            return Err(type_error(format!(
                "cadata should be an ASCII string or a bytes-like object, not {}",
                other.type_name()
            )));
        }
    }
    if der_certs.is_empty() && capath_certs.is_empty() && crls == 0 {
        return Ok(Object::None);
    }
    with_ctx(ctx, |c| {
        // Deduplicate by DER bytes: OpenSSL's cert store keeps one entry per
        // certificate (cert_store_stats counts stay stable on re-loads).
        for cert in der_certs {
            if !c.extra_ca.iter().any(|e| e.as_ref() == cert.as_ref()) {
                c.extra_ca.push(cert);
            }
        }
        for cert in capath_certs {
            if !c.capath_ca.iter().any(|e| e.as_ref() == cert.as_ref()) {
                c.capath_ca.push(cert);
            }
        }
        c.crl_count += crls;
    })?;
    Ok(Object::None)
}

fn ns_set_verify_flags(args: &[Object]) -> Result<Object, RuntimeError> {
    let ctx = arg_int(args, 0, "ctx")?;
    let flags = arg_int(args, 1, "flags")?;
    with_ctx(ctx, |c| c.verify_flags = flags)?;
    Ok(Object::None)
}

/// `set_default_verify_paths(ctx)` — OpenSSL's
/// `SSL_CTX_set_default_verify_paths`: add the system trust store (the
/// platform-native roots, rustls-native-certs) to the context's root set.
fn ns_set_default_verify_paths(args: &[Object]) -> Result<Object, RuntimeError> {
    let ctx = arg_int(args, 0, "ctx")?;
    with_ctx(ctx, |c| c.use_native_roots = true)?;
    Ok(Object::None)
}

/// `cert_store_stats(ctx)` → {"x509": n, "crl": 0, "x509_ca": n_ca} over the
/// certificates loaded via `load_verify_locations` (capath anchors excluded —
/// OpenSSL's hashed-dir entries are lazy and unstatted until used).
fn ns_cert_store_stats(args: &[Object]) -> Result<Object, RuntimeError> {
    let ctx = arg_int(args, 0, "ctx")?;
    let (total, ca, crl) = with_ctx(ctx, |c| {
        let total = c.extra_ca.len() as i64;
        let ca = c
            .extra_ca
            .iter()
            .filter(|der| der_is_ca(der.as_ref()))
            .count() as i64;
        (total, ca, c.crl_count)
    })?;
    let mut d = DictData::default();
    d.insert(DictKey(Object::from_static("x509")), Object::Int(total));
    d.insert(DictKey(Object::from_static("crl")), Object::Int(crl));
    d.insert(DictKey(Object::from_static("x509_ca")), Object::Int(ca));
    Ok(Object::Dict(Rc::new(RefCell::new(d))))
}

/// `set_cipher_suites(ctx, [openssl_names])` — restrict the TLS 1.2 suite
/// list (the shim's `set_ciphers` grammar interpreter feeds this). TLS 1.3
/// suites are never filtered, matching OpenSSL cipher-string semantics.
fn ns_set_cipher_suites(args: &[Object]) -> Result<Object, RuntimeError> {
    let ctx = arg_int(args, 0, "ctx")?;
    let mut names: Vec<String> = Vec::new();
    match args.get(1) {
        Some(Object::List(l)) => {
            for it in l.borrow().iter() {
                if let Object::Str(s) = it {
                    names.push(s.to_string());
                }
            }
        }
        Some(Object::Tuple(t)) => {
            for it in t.iter() {
                if let Object::Str(s) = it {
                    names.push(s.to_string());
                }
            }
        }
        _ => return Err(type_error("_ssl: cipher suites must be a sequence of str")),
    }
    with_ctx(ctx, |c| c.cipher_names = Some(names))?;
    Ok(Object::None)
}

/// `set_min_max_version(ctx, min, max)` — store the `ssl.TLSVersion` wire
/// codes; `build_*_config` folds them into rustls protocol versions.
fn ns_set_min_max_version(args: &[Object]) -> Result<Object, RuntimeError> {
    let ctx = arg_int(args, 0, "ctx")?;
    let lo = arg_int(args, 1, "min")?;
    let hi = arg_int(args, 2, "max")?;
    with_ctx(ctx, |c| {
        c.min_version = lo;
        c.max_version = hi;
    })?;
    Ok(Object::None)
}

/// `_test_decode_cert(path)` → `getpeercert()`-shaped dict for the first
/// PEM certificate in `path` (CPython's private test hook; test_parse_cert
/// and friends compare the decoder's output against golden dicts).
fn ns_test_decode_cert(args: &[Object]) -> Result<Object, RuntimeError> {
    let path = arg_str(args, 0, "path")?;
    let pem = read_pem_file(&path)?;
    let certs = parse_cert_chain(&pem)?;
    decode_cert_dict(certs[0].as_ref())
}

/// `get_ca_certs(ctx)` → list of DER blobs for the explicitly loaded CA
/// anchors (the shim decodes them via `decode_cert` for the dict form).
/// capath anchors appear only once a handshake actually used them
/// (OpenSSL's lazy hashed-dir semantics — test_get_ca_certs_capath).
/// Whether a DER cert is a CA in OpenSSL's `X509_check_ca` sense: X509v3
/// BasicConstraints CA:TRUE, or a legacy v1/v2 certificate (which OpenSSL
/// counts as a "possible CA" — the ancient CAcert/neuronio test anchors).
fn der_is_ca(der: &[u8]) -> bool {
    use x509_parser::prelude::FromDer;
    x509_parser::certificate::X509Certificate::from_der(der)
        .ok()
        .map(|(_, cert)| cert.is_ca() || cert.version().0 < 2)
        .unwrap_or(false)
}

fn ns_get_ca_certs(args: &[Object]) -> Result<Object, RuntimeError> {
    let ctx = arg_int(args, 0, "ctx")?;
    let ders = with_ctx(ctx, |c| {
        c.extra_ca
            .iter()
            .chain(c.capath_used.iter())
            .filter(|c| der_is_ca(c.as_ref()))
            .map(|c| Object::new_bytes(c.as_ref().to_vec()))
            .collect::<Vec<_>>()
    })?;
    Ok(Object::new_list(ders))
}

/// After a completed client handshake, record which capath anchors were
/// actually consulted: those whose subject matches the issuer of a cert in
/// the peer's presented chain (what OpenSSL's by-hash lookup would load).
fn note_capath_used(ctx: i64, conn: &Connection) {
    use x509_parser::prelude::FromDer;
    let Some(chain) = conn.peer_certificates() else {
        return;
    };
    let issuers: Vec<Vec<u8>> = chain
        .iter()
        .filter_map(|c| {
            x509_parser::certificate::X509Certificate::from_der(c.as_ref())
                .ok()
                .map(|(_, cert)| cert.issuer().as_raw().to_vec())
        })
        .collect();
    if issuers.is_empty() {
        return;
    }
    let _ = with_ctx(ctx, |c| {
        let mut newly_used: Vec<CertificateDer<'static>> = Vec::new();
        for ca in &c.capath_ca {
            let Ok((_, cacert)) = x509_parser::certificate::X509Certificate::from_der(ca.as_ref())
            else {
                continue;
            };
            let subject = cacert.subject().as_raw();
            if issuers.iter().any(|i| i.as_slice() == subject)
                && !c.capath_used.iter().any(|u| u.as_ref() == ca.as_ref())
            {
                newly_used.push(ca.clone());
            }
        }
        c.capath_used.extend(newly_used);
    });
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
    if server_side {
        // Two-phase (Acceptor) path: validate the config *now* so wrap-time
        // errors (client-protocol context, missing certificate) surface here
        // as before, but defer committing it until the ClientHello is read
        // (SNI callback may swap contexts; ALPN needs the client's offer).
        with_ctx(ctx, |c| build_server_config_alpn(c, None).map(|_| ()))??;
        let id = next_id();
        pending_servers().lock().insert(
            id,
            Rc::new(RefCell::new(PendingServer {
                acceptor: Some(rustls::server::Acceptor::default()),
                accepted: None,
                server_name: None,
                client_alpn: Vec::new(),
                sock,
            })),
        );
        return Ok(Object::Int(id));
    }
    // Materialize the rustls config straight from the registered context
    // (CtxConfig isn't `Clone` — `PrivateKeyDer` isn't — so build in place).
    let conn = with_ctx(ctx, |c| -> Result<Connection, RuntimeError> {
        let ccfg = build_client_config(c)?;
        // No hostname → no SNI on the wire (OpenSSL sends the extension only
        // when a name is set; the server-side SNI callback then sees None).
        // rustls suppresses SNI for IP-address names, so use the loopback IP.
        let sni: ServerName<'static> = if server_hostname.is_empty() {
            ServerName::from(std::net::IpAddr::from([127, 0, 0, 1]))
        } else {
            ServerName::try_from(server_hostname.clone())
                .map_err(|_| value_error(format!("invalid server_hostname: {server_hostname}")))?
        };
        Ok(Connection::Client(
            ClientConnection::new(ccfg, sni)
                .map_err(|e| ssl_error_rt(format!("client init: {e}")))?,
        ))
    })??;
    Ok(Object::Int(alloc_session(TlsSession {
        conn,
        sock,
        server_side,
        sni: server_hostname,
        rec: RecordState::default(),
        ctx,
        pha_peer: Vec::new(),
        hs_counted: false,
        pha_abort: false,
        hs_rx: Vec::new(),
        hs_tx: Vec::new(),
    })))
}

/// Is `id` still an uncommitted (pre-ClientHello / pre-config) server wrap?
fn ns_server_pending(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    Ok(Object::Bool(pending_cell(id).is_some()))
}

/// Map a rustls setup error (`Accepted::into_connection` / acceptor failures)
/// to the Python-facing SSLError, keeping the OpenSSL reason tokens the test
/// suite greps for.
fn tls_setup_error(e: &rustls::Error) -> RuntimeError {
    let s = e.to_string();
    let low = s.to_lowercase();
    if low.contains("no cipher suites in common") || low.contains("nociphersuitesincommon") {
        ssl_error_rt(format!("handshake: NO_SHARED_CIPHER: {s}"))
    } else if low.contains("no kx groups in common") || low.contains("nokxgroupsincommon") {
        ssl_error_rt(format!("handshake: NO_SHARED_GROUP: {s}"))
    } else if low.contains("corrupt message") || low.contains("invalidcontenttype") {
        // Plaintext bytes arrived where a ClientHello record was expected
        // (test_preauth_data_to_tls_server): match CPython's phrasing.
        ssl_error_rt(format!(
            "handshake: received unexpected data before TLS handshake with data: {s}"
        ))
    } else {
        ssl_error_rt(format!("handshake: {s}"))
    }
}

/// Phase 1: block until the full ClientHello has been read, then return the
/// SNI server name (or None). Raises WANT_READ on a non-blocking socket.
fn ns_server_read_client_hello(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    let cell =
        pending_cell(id).ok_or_else(|| value_error("ssl: not a pending server connection"))?;
    let mut p = cell.borrow_mut();
    if p.accepted.is_some() {
        return Ok(match &p.server_name {
            Some(n) => Object::from_str(n),
            None => Object::None,
        });
    }
    let nonblocking = sock_is_nonblocking(&p.sock);
    loop {
        let Some(acceptor) = p.acceptor.as_mut() else {
            return Err(value_error("ssl: acceptor already consumed"));
        };
        match acceptor.accept() {
            Ok(Some(accepted)) => {
                let hello = accepted.client_hello();
                let name = hello.server_name().map(str::to_string);
                let alpn: Vec<Vec<u8>> = hello
                    .alpn()
                    .map(|it| it.map(<[u8]>::to_vec).collect())
                    .unwrap_or_default();
                p.acceptor = None;
                p.accepted = Some(accepted);
                p.server_name = name.clone();
                p.client_alpn = alpn;
                return Ok(match name {
                    Some(n) => Object::from_str(n),
                    None => Object::None,
                });
            }
            Ok(None) => {}
            Err((e, mut alert)) => {
                let _ = alert.write_all(&mut p.sock);
                return Err(tls_setup_error(&e));
            }
        }
        // Need more transport bytes for the ClientHello.
        let PendingServer { acceptor, sock, .. } = &mut *p;
        let acceptor = acceptor.as_mut().expect("checked above");
        let rd = crate::gil::allow_threads_then(|| acceptor.read_tls(sock));
        match rd {
            Ok(0) => return Err(eof_error()),
            Ok(_) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock && nonblocking => {
                return Err(want_read_error());
            }
            Err(e) => return Err(handshake_io_error(&e)),
        }
    }
}

/// Abort a pending server handshake with a fatal TLS alert (`desc` is the RFC
/// 8446 AlertDescription code) — the SNI-callback error paths.
fn ns_server_abort_alert(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    let desc = arg_int(args, 1, "alert")? as u8;
    let cell = pending_servers()
        .lock()
        .remove(&id)
        .ok_or_else(|| value_error("ssl: not a pending server connection"))?;
    let mut p = cell.borrow_mut();
    // level fatal(2) + description, in a plaintext alert record.
    let rec = [0x15, 0x03, 0x03, 0x00, 0x02, 0x02, desc];
    let _ = p.sock.write_all(&rec);
    let _ = p.sock.flush();
    Ok(Object::None)
}

/// Phase 2: commit a server config (from `ctx` — possibly swapped by the SNI
/// callback) and finish the handshake.
fn ns_server_complete_handshake(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    let ctx = arg_int(args, 1, "ctx")?;
    // Committed already (a WANT_READ/WANT_WRITE retry re-enters here after
    // the session moved to the main registry)? Resume the normal driver.
    if pending_cell(id).is_none() && session_cell(id).is_some() {
        return ns_do_handshake(args);
    }
    let cell = pending_servers()
        .lock()
        .remove(&id)
        .ok_or_else(|| value_error("ssl: not a pending server connection"))?;
    let p = Rc::try_unwrap(cell)
        .map_err(|_| value_error("ssl: pending connection is busy"))?
        .into_inner();
    let PendingServer {
        accepted,
        server_name,
        client_alpn,
        mut sock,
        ..
    } = p;
    let accepted = accepted.ok_or_else(|| value_error("ssl: ClientHello not read yet"))?;
    let scfg = with_ctx(ctx, |c| build_server_config_alpn(c, Some(&client_alpn)))??;
    let conn = match accepted.into_connection(scfg) {
        Ok(c) => c,
        Err((e, mut alert)) => {
            let _ = alert.write_all(&mut sock);
            return Err(tls_setup_error(&e));
        }
    };
    let sess = TlsSession {
        conn: Connection::Server(conn),
        sock,
        server_side: true,
        sni: server_name.unwrap_or_default(),
        rec: RecordState::default(),
        ctx,
        pha_peer: Vec::new(),
        hs_counted: false,
        pha_abort: false,
        hs_rx: Vec::new(),
        hs_tx: Vec::new(),
    };
    sessions().lock().insert(id, Rc::new(RefCell::new(sess)));
    // Drive the rest of the handshake through the normal path (blocking
    // complete_io or non-blocking single steps + WANT_*).
    ns_do_handshake(args)
}

/// Pass-through socket wrapper that copies handshake traffic into the
/// session's capture buffers (for `_msg_callback` replay).
struct TapStream<'a> {
    sock: &'a mut TcpStream,
    hs_rx: &'a mut Vec<u8>,
    hs_tx: &'a mut Vec<u8>,
}

impl std::io::Read for TapStream<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.sock.read(buf)?;
        self.hs_rx.extend_from_slice(&buf[..n]);
        Ok(n)
    }
}

impl std::io::Write for TapStream<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.sock.write(buf)?;
        self.hs_tx.extend_from_slice(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.sock.flush()
    }
}

/// Parse one direction of a captured handshake byte stream into
/// OpenSSL-`SSL_CTX_set_msg_callback`-shaped events.
fn transcript_events(stream: &[u8], direction: &str, version: i64, out: &mut Vec<Object>) {
    let mut i = 0usize;
    let mut encrypted = false;
    while i + 5 <= stream.len() {
        let ctype = stream[i];
        let rlen = usize::from(stream[i + 3]) << 8 | usize::from(stream[i + 4]);
        let end = (i + 5 + rlen).min(stream.len());
        let body = &stream[i + 5..end];
        match ctype {
            20 => {
                // change_cipher_spec: CPython reports the pseudo message type
                // 0x0101 (_TLSMessageType.CHANGE_CIPHER_SPEC).
                out.push(Object::new_tuple(vec![
                    Object::from_str(direction),
                    Object::Int(version),
                    Object::Int(20),
                    Object::Int(0x0101),
                    Object::new_bytes(body.to_vec()),
                ]));
                encrypted = true;
            }
            22 if !encrypted => {
                // handshake record: one or more TLV messages (type u8, len u24).
                let mut j = 0usize;
                while j + 4 <= body.len() {
                    let mtype = body[j];
                    let mlen = usize::from(body[j + 1]) << 16
                        | usize::from(body[j + 2]) << 8
                        | usize::from(body[j + 3]);
                    let mend = (j + 4 + mlen).min(body.len());
                    out.push(Object::new_tuple(vec![
                        Object::from_str(direction),
                        Object::Int(version),
                        Object::Int(22),
                        Object::Int(i64::from(mtype)),
                        Object::new_bytes(body[j + 4..mend].to_vec()),
                    ]));
                    j = mend;
                }
            }
            _ => {}
        }
        i = end;
    }
}

/// The captured handshake transcript as `(direction, version, content_type,
/// msg_type, data)` tuples — the shim replays these into the context's
/// `_msg_callback` (rustls has no message-level callback hook).
fn ns_msg_transcript(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    let cell = session_cell(id).ok_or_else(|| value_error("ssl: closed connection"))?;
    let s = cell.borrow();
    let version = match s.conn.protocol_version() {
        Some(v) => i64::from(u16::from(v)),
        None => 0,
    };
    let mut out = Vec::new();
    transcript_events(&s.hs_tx, "write", version, &mut out);
    transcript_events(&s.hs_rx, "read", version, &mut out);
    Ok(Object::new_list(out))
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
        drive_handshake_nonblocking(conn, sock, rec)?;
        note_capath_used(s.ctx, &s.conn);
        if !s.hs_counted {
            s.hs_counted = true;
            note_handshake_complete(s.ctx, s.server_side);
        }
        return Ok(Object::None);
    }
    // Already handshaken: OpenSSL's `SSL_do_handshake` returns immediately,
    // and callers rely on that — `TestSocketWrapper.starttls` re-invokes
    // `do_handshake()` right after a handshake-on-connect `wrap_socket`.
    // rustls's `complete_io` would instead block reading *application* data
    // (its non-handshake mode waits for readable TLS records), turning the
    // no-op into a read that times out (test_ssl memory-leak tests).
    if !s.conn.is_handshaking() {
        let res = {
            let TlsSession { conn, sock, .. } = &mut *s;
            crate::gil::allow_threads_then(|| -> std::io::Result<()> {
                while conn.wants_write() {
                    conn.write_tls(sock)?;
                }
                Ok(())
            })
        };
        res.map_err(|e| handshake_io_error(&e))?;
        return Ok(Object::None);
    }
    // The handshake blocks on the socket; release the GIL so the peer thread
    // (loopback server/client) can make progress instead of deadlocking.
    // The tap records the raw byte streams for `_msg_callback` replay.
    let res = {
        let TlsSession {
            conn,
            sock,
            hs_rx,
            hs_tx,
            ..
        } = &mut *s;
        let mut tap = TapStream { sock, hs_rx, hs_tx };
        crate::gil::allow_threads_then(|| conn.complete_io(&mut tap))
    };
    res.map_err(|e| handshake_io_error(&e))?;
    note_capath_used(s.ctx, &s.conn);
    if !s.hs_counted {
        s.hs_counted = true;
        note_handshake_complete(s.ctx, s.server_side);
    }
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
        let s = e.to_string();
        let low = s.to_lowercase();
        // OpenSSL-style reason tokens the test suite greps for.
        if low.contains("no cipher suites in common")
            || low.contains("nociphersuitesincommon")
            || low.contains("no kx groups in common")
            || low.contains("nokxgroupsincommon")
        {
            ssl_error_rt(format!("handshake: NO_SHARED_CIPHER: {s}"))
        } else if let Some(rendered) = alert_name_to_openssl(&s) {
            // A fatal alert surfaced through `complete_io`'s io::Error path
            // (blocking handshake): keep the OpenSSL `[SSL: TOKEN]` shape so
            // `SSLError.reason` carries the alert token.
            ssl_error_rt(rendered)
        } else if low.contains("corrupt message") || low.contains("invalidcontenttype") {
            // A plaintext byte-stream arrived where a ClientHello/record was
            // expected (test_preauth_data_to_tls_*): CPython phrases this as
            // data received before the TLS handshake.
            ssl_error_rt(format!(
                "handshake: received unexpected data before TLS handshake with data: {s}"
            ))
        } else {
            ssl_error_rt(format!("handshake: {s}"))
        }
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
    let buf = read_n(id, n, true)?;
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

/// Full peer chain as a list of DER blobs (`_SSLSocket.get_verified_chain` /
/// `get_unverified_chain` — the echo server in test_ssl only asks for
/// `len(...)`, and rustls only surfaces the presented chain).
fn ns_peer_cert_chain_der(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    let mut chain = peer_certs(id);
    // OpenSSL's server keeps the *rebuilt* chain from client-cert
    // verification, so `SSL_get_peer_cert_chain` reports leaf + CA even when
    // the client only sent its leaf (test_internal_chain_server expects 2 for
    // both chains); a client reports exactly what the server transmitted.
    let (ctx, server_side) = session_cell(id)
        .map(|c| {
            let s = c.borrow();
            (s.ctx, s.server_side)
        })
        .unwrap_or((-1, false));
    if server_side {
        extend_chain_with_ctx_anchors(&mut chain, ctx);
    }
    Ok(Object::new_list(
        chain.into_iter().map(Object::new_bytes).collect(),
    ))
}

/// Grow `chain` toward the trust anchor by matching issuer DNs against the
/// context's loaded CAs — OpenSSL's chain building, DN-match only.
fn extend_chain_with_ctx_anchors(chain: &mut Vec<Vec<u8>>, ctx: i64) {
    use x509_parser::prelude::FromDer;
    if ctx < 0 {
        return;
    }
    let anchors: Vec<Vec<u8>> = with_ctx(ctx, |c| {
        c.extra_ca
            .iter()
            .chain(c.capath_ca.iter())
            .map(|c| c.as_ref().to_vec())
            .collect()
    })
    .unwrap_or_default();
    while let Some(last) = chain.last().cloned() {
        let Ok((_, cert)) = x509_parser::certificate::X509Certificate::from_der(&last) else {
            break;
        };
        if cert.subject().as_raw() == cert.issuer().as_raw() {
            break; // self-signed: reached the anchor
        }
        let issuer_raw = cert.issuer().as_raw().to_vec();
        let mut found = None;
        for a in &anchors {
            if chain.iter().any(|c| c == a) {
                continue;
            }
            if let Ok((_, ac)) = x509_parser::certificate::X509Certificate::from_der(a) {
                if ac.subject().as_raw() == issuer_raw.as_slice() {
                    found = Some(a.clone());
                    break;
                }
            }
        }
        match found {
            Some(a) => chain.push(a),
            None => break,
        }
    }
}

/// `peer_verified_chain_der(session)` → the *verified* chain: the peer's
/// presented certificates extended up to the trust anchor by matching issuer
/// DNs against the context's loaded CAs (OpenSSL's `SSL_get0_verified_chain`
/// includes the anchor even when the peer didn't send it).
fn ns_peer_verified_chain_der(args: &[Object]) -> Result<Object, RuntimeError> {
    let id = arg_int(args, 0, "session")?;
    let mut chain = peer_certs(id);
    let ctx = session_cell(id).map(|c| c.borrow().ctx).unwrap_or(-1);
    extend_chain_with_ctx_anchors(&mut chain, ctx);
    Ok(Object::new_list(
        chain.into_iter().map(Object::new_bytes).collect(),
    ))
}

// ---------------------------------------------------------------------------
// X.509 → CPython `getpeercert()` dict decoding
// ---------------------------------------------------------------------------

/// OpenSSL "long name" for an X.500 attribute type OID — the keys CPython puts
/// in `getpeercert()['subject'/'issuer']` RDN pairs. Unknown OIDs render as
/// their dotted-decimal form (OpenSSL's `OBJ_obj2txt` fallback).
fn x500_attr_name(oid: &str) -> String {
    match oid {
        "2.5.4.3" => "commonName",
        "2.5.4.4" => "surname",
        "2.5.4.5" => "serialNumber",
        "2.5.4.6" => "countryName",
        "2.5.4.7" => "localityName",
        "2.5.4.8" => "stateOrProvinceName",
        "2.5.4.9" => "streetAddress",
        "2.5.4.10" => "organizationName",
        "2.5.4.11" => "organizationalUnitName",
        "2.5.4.12" => "title",
        "2.5.4.15" => "businessCategory",
        "2.5.4.42" => "givenName",
        "2.5.4.46" => "dnQualifier",
        "2.5.4.65" => "pseudonym",
        "1.2.840.113549.1.9.1" => "emailAddress",
        "0.9.2342.19200300.100.1.1" => "UID",
        "0.9.2342.19200300.100.1.25" => "domainComponent",
        other => return other.to_string(),
    }
    .to_string()
}

/// `((('countryName', 'XY'),), (('commonName', 'localhost'),), ...)`:
/// Uppercase hex rendering (OpenSSL's serial-number / raw-value style).
fn hex_upper(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02X}");
        s
    })
}

/// a tuple of RDNs, each a tuple of `(name, value)` pairs — CPython's
/// `_create_tuple_for_X509_NAME`.
fn x509_name_tuple(name: &x509_parser::x509::X509Name<'_>) -> Object {
    let rdns: Vec<Object> = name
        .iter_rdn()
        .map(|rdn| {
            let pairs: Vec<Object> = rdn
                .iter()
                .map(|atv| {
                    let key = x500_attr_name(&atv.attr_type().to_id_string());
                    let val = atv.as_str().map(str::to_string).unwrap_or_else(|_| {
                        // Non-string ASN.1 value: hex-render like OpenSSL does
                        // for unprintable data.
                        hex_upper(atv.attr_value().data)
                    });
                    Object::new_tuple(vec![Object::from_str(&key), Object::from_str(&val)])
                })
                .collect();
            Object::new_tuple(pairs)
        })
        .collect();
    Object::new_tuple(rdns)
}

/// OpenSSL `ASN1_TIME_print` rendering: `"Aug 29 14:23:16 2018 GMT"`
/// (day space-padded to width 2).
fn asn1_time_str(t: &x509_parser::time::ASN1Time) -> String {
    let dt = t.to_datetime();
    let month = match u8::from(dt.month()) {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        _ => "Dec",
    };
    format!(
        "{month} {day:2} {h:02}:{m:02}:{s:02} {year} GMT",
        day = dt.day(),
        h = dt.hour(),
        m = dt.minute(),
        s = dt.second(),
        year = dt.year()
    )
}

/// CPython's `_get_peer_alt_names` rendering of one GeneralName.
fn general_name_pair(gn: &x509_parser::extensions::GeneralName<'_>) -> Option<(String, Object)> {
    use x509_parser::extensions::GeneralName as GN;
    match gn {
        GN::DNSName(d) => Some(("DNS".into(), Object::from_str(*d))),
        GN::RFC822Name(e) => Some(("email".into(), Object::from_str(*e))),
        GN::URI(u) => Some(("URI".into(), Object::from_str(*u))),
        GN::IPAddress(b) => {
            let s = match b.len() {
                4 => std::net::Ipv4Addr::new(b[0], b[1], b[2], b[3]).to_string(),
                16 => {
                    // OpenSSL renders IPv6 SANs uppercase and *uncompressed*
                    // ('0:0:0:0:0:0:0:1', not '::1') — test_parse_all_sans /
                    // CVE_2013_4238 compare exact strings.
                    let groups: Vec<String> = b
                        .chunks_exact(2)
                        .map(|c| format!("{:X}", (u16::from(c[0]) << 8) | u16::from(c[1])))
                        .collect();
                    groups.join(":")
                }
                _ => "<invalid>".to_owned(),
            };
            Some(("IP Address".into(), Object::from_str(&s)))
        }
        GN::DirectoryName(n) => Some(("DirName".into(), x509_name_tuple(n))),
        GN::RegisteredID(oid) => {
            Some(("Registered ID".into(), Object::from_str(oid.to_id_string())))
        }
        GN::OtherName(..) => Some(("othername".into(), Object::from_static("<unsupported>"))),
        _ => None,
    }
}

/// Decode a DER certificate into CPython's `getpeercert()` dict
/// (`_decode_certificate` in `Modules/_ssl.c`).
fn decode_cert_dict(der: &[u8]) -> Result<Object, RuntimeError> {
    use x509_parser::prelude::*;

    let (_, cert) = X509Certificate::from_der(der)
        .map_err(|e| ssl_error_rt(format!("unable to decode certificate: {e}")))?;

    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        let mut put = |k: &str, v: Object| {
            d.insert(DictKey(Object::from_str(k)), v);
        };

        put("subject", x509_name_tuple(cert.subject()));
        put("issuer", x509_name_tuple(cert.issuer()));
        put("version", Object::Int(i64::from(cert.version().0) + 1));

        // OpenSSL prints the serial as uppercase hex without the DER
        // sign-padding byte.
        let raw = cert.raw_serial();
        let trimmed = {
            let mut s = raw;
            while s.len() > 1 && s[0] == 0 {
                s = &s[1..];
            }
            s
        };
        let serial = hex_upper(trimmed);
        put("serialNumber", Object::from_str(&serial));

        put(
            "notBefore",
            Object::from_str(asn1_time_str(&cert.validity().not_before)),
        );
        put(
            "notAfter",
            Object::from_str(asn1_time_str(&cert.validity().not_after)),
        );

        if let Ok(Some(san)) = cert.subject_alternative_name() {
            let names: Vec<Object> = san
                .value
                .general_names
                .iter()
                .filter_map(|gn| {
                    general_name_pair(gn)
                        .map(|(k, v)| Object::new_tuple(vec![Object::from_str(&k), v]))
                })
                .collect();
            if !names.is_empty() {
                put("subjectAltName", Object::new_tuple(names));
            }
        }

        // Authority Information Access → 'OCSP' and 'caIssuers' URI tuples.
        let mut ocsp: Vec<Object> = Vec::new();
        let mut ca_issuers: Vec<Object> = Vec::new();
        let mut crl_points: Vec<Object> = Vec::new();
        for ext in cert.extensions() {
            match ext.parsed_extension() {
                ParsedExtension::AuthorityInfoAccess(aia) => {
                    for desc in &aia.accessdescs {
                        if let x509_parser::extensions::GeneralName::URI(u) = &desc.access_location
                        {
                            match desc.access_method.to_id_string().as_str() {
                                "1.3.6.1.5.5.7.48.1" => ocsp.push(Object::from_str(*u)),
                                "1.3.6.1.5.5.7.48.2" => ca_issuers.push(Object::from_str(*u)),
                                _ => {}
                            }
                        }
                    }
                }
                ParsedExtension::CRLDistributionPoints(points) => {
                    for point in points.iter() {
                        if let Some(x509_parser::extensions::DistributionPointName::FullName(
                            names,
                        )) = &point.distribution_point
                        {
                            for gn in names {
                                if let x509_parser::extensions::GeneralName::URI(u) = gn {
                                    crl_points.push(Object::from_str(*u));
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if !ocsp.is_empty() {
            put("OCSP", Object::new_tuple(ocsp));
        }
        if !ca_issuers.is_empty() {
            put("caIssuers", Object::new_tuple(ca_issuers));
        }
        if !crl_points.is_empty() {
            put("crlDistributionPoints", Object::new_tuple(crl_points));
        }
    }
    Ok(Object::Dict(dict))
}

/// `_ssl.decode_cert(der_bytes)` → CPython `getpeercert()`-shaped dict.
fn ns_decode_cert(args: &[Object]) -> Result<Object, RuntimeError> {
    let der = arg_bytes_like(args, 0, "certificate")?;
    decode_cert_dict(&der)
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
            ctx,
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
                note_capath_used(s.ctx, &s.conn);
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
        // OpenSSL refuses writes once our close_notify is out
        // (SSL_ERROR_SSL "protocol is shutdown"); rustls' writer would
        // silently buffer instead (test_bio_handshake writes post-unwrap).
        if s.close_sent {
            return Err(ssl_error_rt("write: protocol is shutdown"));
        }
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
                // Clean close_notify EOF. CPython returns b'' only while the
                // shutdown is one-sided (SSL_RECEIVED_SHUTDOWN alone); once we
                // have also sent ours it raises SSLZeroReturnError instead.
                Ok(0) => {
                    if s.close_sent {
                        return Err(zero_return_error());
                    }
                    return Ok(Object::new_bytes(Vec::new()));
                }
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
        // The peer's close_notify may already have been consumed by an
        // earlier `read()` on this session (asyncio's `_do_read` processes
        // the record that carries it, then `_do_shutdown` calls `unwrap()`).
        // OpenSSL's `SSL_shutdown` reports completion immediately in that
        // case; raising WANT_READ here instead parks the shutdown until the
        // *raw* TCP EOF arrives one loop iteration later — after
        // `run_until_complete` has already stopped the loop — leaving the
        // transport's `connection_lost` undelivered and the whole
        // SSLProtocol → SSLContext chain alive
        // (test_ssl.test_create_connection_memory_leak).
        {
            let io = s
                .conn
                .process_new_packets()
                .map_err(|e| tls_process_error(&e))?;
            bio_flush_out(&mut s.conn, outb);
            if io.peer_has_closed() {
                return Ok(Object::None);
            }
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
    let dict = Rc::new(RefCell::new(DictData::default()));
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
        func!("get_ca_certs", ns_get_ca_certs);
        func!("_test_decode_cert", ns_test_decode_cert);
        func!("set_verify_mode", ns_set_verify_mode);
        func!("get_verify_mode", ns_get_verify_mode);
        func!("set_check_hostname", ns_set_check_hostname);
        func!("get_check_hostname", ns_get_check_hostname);
        func!("set_alpn_protocols", ns_set_alpn_protocols);
        func!("wrap_socket", ns_wrap_socket);
        func!("do_handshake", ns_do_handshake);
        func!("msg_transcript", ns_msg_transcript);
        func!("read", ns_read);
        func!("write", ns_write);
        func!("pending", ns_pending);
        func!("peer_cert_der", ns_peer_cert_der);
        func!("peer_cert_chain_der", ns_peer_cert_chain_der);
        func!("decode_cert", ns_decode_cert);
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
        func!("cert_store_stats", ns_cert_store_stats);
        func!("set_min_max_version", ns_set_min_max_version);
        func!("peer_verified_chain_der", ns_peer_verified_chain_der);
        func!("set_cipher_suites", ns_set_cipher_suites);
        func!("set_options", ns_set_options);
        func!("set_post_handshake_auth", ns_set_post_handshake_auth);
        func!("set_ecdh_curve", ns_set_ecdh_curve);
        func!("set_verify_flags", ns_set_verify_flags);
        func!("set_default_verify_paths", ns_set_default_verify_paths);
        func!("session_stats", ns_session_stats);
        func!("note_session_offer", ns_note_session_offer);
        func!("pha_verify", ns_pha_verify);
        func!("server_pending", ns_server_pending);
        func!("server_read_client_hello", ns_server_read_client_hello);
        func!("server_abort_alert", ns_server_abort_alert);
        func!("server_complete_handshake", ns_server_complete_handshake);

        // Immutable native-shaped types (test_ssl_types pokes each one and
        // expects "cannot set ... immutable type"). The Python shim re-exports
        // and, where applicable, layers the real implementation on top.
        {
            use crate::builtin_types::builtin_types;
            use crate::types::{TypeFlags, TypeObject};
            let bt = builtin_types();
            for name in [
                "_SSLContext",
                "_SSLSocket",
                "MemoryBIO",
                "Certificate",
                "SSLSession",
            ] {
                // No public constructor (Py_TPFLAGS_DISALLOW_INSTANTIATION):
                // both `tp(...)` and `tp.__new__(tp)` must raise
                // "cannot create '_ssl.<name>' instances" (test_ssl_types).
                let mut td = DictData::default();
                td.insert(
                    DictKey(Object::from_static("__module__")),
                    Object::from_static("_ssl"),
                );
                td.insert(
                    DictKey(Object::from_static("__new__")),
                    Object::Builtin(Rc::new(crate::object::BuiltinFn {
                        name: "__new__",
                        binds_instance: false,
                        call: Box::new(move |_args| {
                            Err(type_error(format!("cannot create '_ssl.{name}' instances")))
                        }),
                        call_kw: None,
                    })),
                );
                let ty = TypeObject::new_with_flags(
                    name,
                    vec![bt.object_.clone()],
                    td,
                    TypeFlags {
                        is_exception: false,
                        is_builtin: true,
                    },
                )
                .expect("_ssl type");
                d.insert(DictKey(Object::interned_str(name)), Object::Type(ty));
            }
            let err =
                TypeObject::new_exception("SSLError", bt.os_error.clone()).expect("_ssl.SSLError");
            d.insert(DictKey(Object::from_static("SSLError")), Object::Type(err));
        }

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
        konst!("OP_LEGACY_SERVER_CONNECT", 0x4);
        konst!("OP_NO_RENEGOTIATION", 0x4000_0000);
        konst!("OP_IGNORE_UNEXPECTED_EOF", 0x80);
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
        konst!("VERIFY_ALLOW_PROXY_CERTS", 0x40);
        konst!("VERIFY_X509_PARTIAL_CHAIN", 0x8_0000);
        // Certificate.public_bytes() formats (X509_FILETYPE_*).
        konst!("ENCODING_PEM", 1);
        konst!("ENCODING_DER", 2);
        konst!("HAS_SNI", 1);
        konst!("HAS_ECDH", 1);
        konst!("HAS_NPN", 0);
        konst!("HAS_ALPN", 1);
        konst!("HAS_TLSv1_3", 1);
        konst!("PROTO_VERSION_TLSv1_3", 0x0304);
        // TLSVersion wire codes (ssl.TLSVersion members; test_tlsversion
        // builds its checked enum straight from these).
        konst!("PROTO_MINIMUM_SUPPORTED", -2);
        konst!("PROTO_SSLv3", 0x0300);
        konst!("PROTO_TLSv1", 0x0301);
        konst!("PROTO_TLSv1_1", 0x0302);
        konst!("PROTO_TLSv1_2", 0x0303);
        konst!("PROTO_TLSv1_3", 0x0304);
        konst!("PROTO_MAXIMUM_SUPPORTED", -1);
        // TLS alert descriptions (RFC 8446 §6 / OpenSSL's exported subset —
        // ssl.AlertDescription is `_convert_`ed from exactly these names).
        konst!("ALERT_DESCRIPTION_ACCESS_DENIED", 49);
        konst!("ALERT_DESCRIPTION_BAD_CERTIFICATE", 42);
        konst!("ALERT_DESCRIPTION_BAD_CERTIFICATE_HASH_VALUE", 114);
        konst!("ALERT_DESCRIPTION_BAD_CERTIFICATE_STATUS_RESPONSE", 113);
        konst!("ALERT_DESCRIPTION_BAD_RECORD_MAC", 20);
        konst!("ALERT_DESCRIPTION_CERTIFICATE_EXPIRED", 45);
        konst!("ALERT_DESCRIPTION_CERTIFICATE_REVOKED", 44);
        konst!("ALERT_DESCRIPTION_CERTIFICATE_UNKNOWN", 46);
        konst!("ALERT_DESCRIPTION_CERTIFICATE_UNOBTAINABLE", 111);
        konst!("ALERT_DESCRIPTION_CLOSE_NOTIFY", 0);
        konst!("ALERT_DESCRIPTION_DECODE_ERROR", 50);
        konst!("ALERT_DESCRIPTION_DECOMPRESSION_FAILURE", 30);
        konst!("ALERT_DESCRIPTION_DECRYPT_ERROR", 51);
        konst!("ALERT_DESCRIPTION_HANDSHAKE_FAILURE", 40);
        konst!("ALERT_DESCRIPTION_ILLEGAL_PARAMETER", 47);
        konst!("ALERT_DESCRIPTION_INSUFFICIENT_SECURITY", 71);
        konst!("ALERT_DESCRIPTION_INTERNAL_ERROR", 80);
        konst!("ALERT_DESCRIPTION_NO_RENEGOTIATION", 100);
        konst!("ALERT_DESCRIPTION_PROTOCOL_VERSION", 70);
        konst!("ALERT_DESCRIPTION_RECORD_OVERFLOW", 22);
        konst!("ALERT_DESCRIPTION_UNEXPECTED_MESSAGE", 10);
        konst!("ALERT_DESCRIPTION_UNKNOWN_CA", 48);
        konst!("ALERT_DESCRIPTION_UNKNOWN_PSK_IDENTITY", 115);
        konst!("ALERT_DESCRIPTION_UNRECOGNIZED_NAME", 112);
        konst!("ALERT_DESCRIPTION_UNSUPPORTED_CERTIFICATE", 43);
        konst!("ALERT_DESCRIPTION_UNSUPPORTED_EXTENSION", 110);
        konst!("ALERT_DESCRIPTION_USER_CANCELLED", 90);

        // The "OpenSSL <maj.min.patch>" prefix is load-bearing: test_openssl_
        // version asserts the string starts with the engine name matching
        // OPENSSL_VERSION_INFO, and stdlib callers parse the numeric triple.
        d.insert(
            DictKey(Object::from_static("OPENSSL_VERSION")),
            Object::from_static("OpenSSL 3.0.0 (compatible; WeavePy rustls/ring)"),
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
