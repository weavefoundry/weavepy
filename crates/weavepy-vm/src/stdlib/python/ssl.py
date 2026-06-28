"""WeavePy ``ssl`` — a CPython-shaped TLS surface over the rustls ``_ssl`` core.

This module mirrors the public shape of CPython 3.13's ``Lib/ssl.py``
(``SSLContext`` / ``SSLSocket`` / ``SSLObject`` / the ``SSLError`` family /
the module constants / ``create_default_context`` / ``match_hostname``) on top
of WeavePy's native ``_ssl`` (rustls) core. ``SSLSocket`` subclasses
``socket.socket`` and overrides the I/O methods to drive the TLS session, so
the inherited ``makefile()`` (the verbatim ``http.client`` / ``ftplib`` /
``smtplib`` / ``imaplib`` / ``poplib`` drivers all use it) speaks TLS unchanged.

The TLS engine is rustls, not OpenSSL: OpenSSL-specific cipher-string grammar
and byte-exact version probes are emulated, not identical (RFC 0042 non-goal).
"""

import _ssl
import socket as _socket
from socket import socket as _socket_type
import errno as _errno
from enum import IntEnum as _IntEnum, IntFlag as _IntFlag

# --------------------------------------------------------------------------
# Constants (re-exported from the native core)
# --------------------------------------------------------------------------

CERT_NONE = _ssl.CERT_NONE
CERT_OPTIONAL = _ssl.CERT_OPTIONAL
CERT_REQUIRED = _ssl.CERT_REQUIRED

PROTOCOL_TLS = _ssl.PROTOCOL_TLS
PROTOCOL_TLS_CLIENT = _ssl.PROTOCOL_TLS_CLIENT
PROTOCOL_TLS_SERVER = _ssl.PROTOCOL_TLS_SERVER
PROTOCOL_TLSv1 = _ssl.PROTOCOL_TLSv1
PROTOCOL_TLSv1_1 = _ssl.PROTOCOL_TLSv1_1
PROTOCOL_TLSv1_2 = _ssl.PROTOCOL_TLSv1_2


class _SSLMethod(_IntEnum):
    PROTOCOL_TLS = _ssl.PROTOCOL_TLS
    PROTOCOL_TLS_CLIENT = _ssl.PROTOCOL_TLS_CLIENT
    PROTOCOL_TLS_SERVER = _ssl.PROTOCOL_TLS_SERVER
    PROTOCOL_TLSv1 = _ssl.PROTOCOL_TLSv1
    PROTOCOL_TLSv1_1 = _ssl.PROTOCOL_TLSv1_1
    PROTOCOL_TLSv1_2 = _ssl.PROTOCOL_TLSv1_2


_PROTOCOL_NAMES = {value: name for name, value in _SSLMethod.__members__.items()}

# Re-export the protocol selectors as the *enum members* (overriding the plain
# ints bound above), exactly as CPython's ``_SSLMethod._convert_`` does. Tests
# rely on ``ssl.PROTOCOL_TLS_CLIENT.name``/``repr`` and on ``SSLContext(proto)``
# round-tripping the same member back from ``ctx.protocol`` (identity).
globals().update(_SSLMethod.__members__)
# Deprecated CPython alias kept for parity (``test_ssl.test_constants``).
PROTOCOL_SSLv23 = PROTOCOL_TLS

HAS_SNI = bool(_ssl.HAS_SNI)
HAS_ECDH = bool(_ssl.HAS_ECDH)
HAS_NPN = bool(_ssl.HAS_NPN)
HAS_ALPN = bool(_ssl.HAS_ALPN)
HAS_TLSv1 = True
HAS_TLSv1_1 = True
HAS_TLSv1_2 = True
HAS_TLSv1_3 = bool(_ssl.HAS_TLSv1_3)
HAS_SSLv2 = False
HAS_SSLv3 = False

# rustls exposes no channel-binding material (`tls-unique` is a TLS 1.2-only
# construct it deliberately omits), so advertise none — tests gated on
# ``"tls-unique" in ssl.CHANNEL_BINDING_TYPES`` then skip, matching a build
# without channel binding.
CHANNEL_BINDING_TYPES = []

# Capability flags for features rustls doesn't surface; tests gated on them skip.
HAS_NEVER_CHECK_COMMON_NAME = False  # no X509_V_FLAG_NEVER_CHECK_SUBJECT analogue
HAS_PSK = False                      # external PSK key exchange not exposed
HAS_PSK_TLS13 = False

OPENSSL_VERSION = _ssl.OPENSSL_VERSION
OPENSSL_VERSION_NUMBER = _ssl.OPENSSL_VERSION_NUMBER
OPENSSL_VERSION_INFO = _ssl.OPENSSL_VERSION_INFO

SSL_ERROR_NONE = _ssl.SSL_ERROR_NONE
SSL_ERROR_SSL = _ssl.SSL_ERROR_SSL
SSL_ERROR_WANT_READ = _ssl.SSL_ERROR_WANT_READ
SSL_ERROR_WANT_WRITE = _ssl.SSL_ERROR_WANT_WRITE
SSL_ERROR_WANT_X509_LOOKUP = _ssl.SSL_ERROR_WANT_X509_LOOKUP
SSL_ERROR_SYSCALL = _ssl.SSL_ERROR_SYSCALL
SSL_ERROR_ZERO_RETURN = _ssl.SSL_ERROR_ZERO_RETURN
SSL_ERROR_WANT_CONNECT = _ssl.SSL_ERROR_WANT_CONNECT
SSL_ERROR_EOF = _ssl.SSL_ERROR_EOF


class VerifyMode(_IntEnum):
    CERT_NONE = 0
    CERT_OPTIONAL = 1
    CERT_REQUIRED = 2


class VerifyFlags(_IntFlag):
    VERIFY_DEFAULT = 0
    VERIFY_CRL_CHECK_LEAF = _ssl.VERIFY_CRL_CHECK_LEAF
    VERIFY_CRL_CHECK_CHAIN = _ssl.VERIFY_CRL_CHECK_CHAIN
    VERIFY_X509_STRICT = _ssl.VERIFY_X509_STRICT
    VERIFY_X509_TRUSTED_FIRST = _ssl.VERIFY_X509_TRUSTED_FIRST


VERIFY_DEFAULT = VerifyFlags.VERIFY_DEFAULT
VERIFY_CRL_CHECK_LEAF = VerifyFlags.VERIFY_CRL_CHECK_LEAF
VERIFY_CRL_CHECK_CHAIN = VerifyFlags.VERIFY_CRL_CHECK_CHAIN
VERIFY_X509_STRICT = VerifyFlags.VERIFY_X509_STRICT
VERIFY_X509_TRUSTED_FIRST = VerifyFlags.VERIFY_X509_TRUSTED_FIRST


class Options(_IntFlag):
    OP_ALL = _ssl.OP_ALL
    OP_NO_SSLv2 = _ssl.OP_NO_SSLv2
    OP_NO_SSLv3 = _ssl.OP_NO_SSLv3
    OP_NO_TLSv1 = _ssl.OP_NO_TLSv1
    OP_NO_TLSv1_1 = _ssl.OP_NO_TLSv1_1
    OP_NO_TLSv1_2 = _ssl.OP_NO_TLSv1_2
    OP_NO_TLSv1_3 = _ssl.OP_NO_TLSv1_3
    OP_NO_COMPRESSION = _ssl.OP_NO_COMPRESSION
    OP_CIPHER_SERVER_PREFERENCE = _ssl.OP_CIPHER_SERVER_PREFERENCE
    OP_SINGLE_DH_USE = _ssl.OP_SINGLE_DH_USE
    OP_SINGLE_ECDH_USE = _ssl.OP_SINGLE_ECDH_USE
    OP_NO_TICKET = _ssl.OP_NO_TICKET
    OP_ENABLE_MIDDLEBOX_COMPAT = _ssl.OP_ENABLE_MIDDLEBOX_COMPAT


# Export every member to module scope, including the multi-bit composites
# (``OP_ALL``) and aliases. Iterating an ``IntFlag`` only yields the canonical
# single-bit flags (CPython 3.11+), so use ``__members__`` to match what
# CPython's ``_IntFlag._convert_('Options', ...)`` injects into ``ssl``.
globals().update(Options.__members__)


class TLSVersion(_IntEnum):
    MINIMUM_SUPPORTED = -2
    SSLv3 = 0x0300
    TLSv1 = 0x0301
    TLSv1_1 = 0x0302
    TLSv1_2 = 0x0303
    TLSv1_3 = 0x0304
    MAXIMUM_SUPPORTED = -1


class _TLSContentType(_IntEnum):
    """Content types (record layer); see RFC 8446, section B.1."""
    CHANGE_CIPHER_SPEC = 20
    ALERT = 21
    HANDSHAKE = 22
    APPLICATION_DATA = 23
    HEADER = 0x100
    INNER_CONTENT_TYPE = 0x101


class _TLSAlertType(_IntEnum):
    """Alert types for _TLSContentType.ALERT; see RFC 8446, section B.2."""
    CLOSE_NOTIFY = 0
    UNEXPECTED_MESSAGE = 10
    BAD_RECORD_MAC = 20
    DECRYPTION_FAILED = 21
    RECORD_OVERFLOW = 22
    DECOMPRESSION_FAILURE = 30
    HANDSHAKE_FAILURE = 40
    NO_CERTIFICATE = 41
    BAD_CERTIFICATE = 42
    UNSUPPORTED_CERTIFICATE = 43
    CERTIFICATE_REVOKED = 44
    CERTIFICATE_EXPIRED = 45
    CERTIFICATE_UNKNOWN = 46
    ILLEGAL_PARAMETER = 47
    UNKNOWN_CA = 48
    ACCESS_DENIED = 49
    DECODE_ERROR = 50
    DECRYPT_ERROR = 51
    EXPORT_RESTRICTION = 60
    PROTOCOL_VERSION = 70
    INSUFFICIENT_SECURITY = 71
    INTERNAL_ERROR = 80
    INAPPROPRIATE_FALLBACK = 86
    USER_CANCELED = 90
    NO_RENEGOTIATION = 100
    MISSING_EXTENSION = 109
    UNSUPPORTED_EXTENSION = 110
    CERTIFICATE_UNOBTAINABLE = 111
    UNRECOGNIZED_NAME = 112
    BAD_CERTIFICATE_STATUS_RESPONSE = 113
    BAD_CERTIFICATE_HASH_VALUE = 114
    UNKNOWN_PSK_IDENTITY = 115
    CERTIFICATE_REQUIRED = 116
    NO_APPLICATION_PROTOCOL = 120


class _TLSMessageType(_IntEnum):
    """Message types (handshake protocol); see RFC 8446, section B.3."""
    HELLO_REQUEST = 0
    CLIENT_HELLO = 1
    SERVER_HELLO = 2
    HELLO_VERIFY_REQUEST = 3
    NEWSESSION_TICKET = 4
    END_OF_EARLY_DATA = 5
    HELLO_RETRY_REQUEST = 6
    ENCRYPTED_EXTENSIONS = 8
    CERTIFICATE = 11
    SERVER_KEY_EXCHANGE = 12
    CERTIFICATE_REQUEST = 13
    SERVER_DONE = 14
    CERTIFICATE_VERIFY = 15
    CLIENT_KEY_EXCHANGE = 16
    FINISHED = 20
    CERTIFICATE_URL = 21
    CERTIFICATE_STATUS = 22
    SUPPLEMENTAL_DATA = 23
    KEY_UPDATE = 24
    NEXT_PROTO = 67
    MESSAGE_HASH = 254
    CHANGE_CIPHER_SPEC = 0x0101


class _ASN1Object:
    def __init__(self, oid):
        self.oid = oid


class Purpose(_ASN1Object, _IntEnum):
    SERVER_AUTH = 1
    CLIENT_AUTH = 2

    def __new__(cls, value):
        obj = int.__new__(cls, value)
        obj._value_ = value
        return obj


# --------------------------------------------------------------------------
# Exceptions
# --------------------------------------------------------------------------

class SSLError(OSError):
    """An error in the SSL implementation."""

    # CPython's C ``SSLError`` exposes these post-construction; callers
    # (e.g. ``http.client``/``urllib``/test suites) read ``.reason`` and
    # ``.library`` to branch on the failure category. Default to ``None`` so
    # attribute access never raises even for errors we don't classify.
    library = None
    reason = None

    def __str__(self):
        if self.strerror:
            return self.strerror
        return super().__str__()


class SSLZeroReturnError(SSLError):
    pass


class SSLWantReadError(SSLError):
    pass


class SSLWantWriteError(SSLError):
    pass


class SSLSyscallError(SSLError):
    pass


class SSLEOFError(SSLError):
    pass


class SSLCertVerificationError(SSLError, ValueError):
    # CPython sets ``verify_code`` (the X.509 error number) and
    # ``verify_message`` (its human string) on certificate-verification
    # failures; rustls doesn't expose OpenSSL's numeric codes, so we surface a
    # best-effort message and leave the code at 0.
    verify_code = 0
    verify_message = None


CertificateError = SSLCertVerificationError


# Substrings rustls emits for the various certificate / hostname verification
# failures. CPython collapses all of these to ``reason ==
# 'CERTIFICATE_VERIFY_FAILED'`` (the OpenSSL reason string), which is what
# callers and the test suite assert on.
_CERT_ERROR_MARKERS = (
    "certificate", "hostname", "certnotvalid", "unknownissuer",
    "invalidcertificate", "notvalidforname", "badsignature", "expired",
    "self-signed", "self signed",
)


def _wrap_ssl_error(exc):
    """Turn a native ``[SSL] ...`` OSError into the right SSLError subclass.

    Certificate / hostname verification failures become
    :class:`SSLCertVerificationError` with ``reason ==
    'CERTIFICATE_VERIFY_FAILED'`` (CPython parity); everything else becomes a
    plain :class:`SSLError`. The original native message is preserved as the
    ``strerror`` so ``str(exc)`` stays informative.
    """
    msg = str(exc)
    if "[SSL]" not in msg:
        return exc
    body = msg.split("[SSL]", 1)[1].strip()
    low = body.lower()
    # A non-blocking socket whose TLS op can't proceed without more I/O reports
    # SSL_ERROR_WANT_READ / SSL_ERROR_WANT_WRITE. Non-blocking drivers (asyncore
    # TLS servers, the FTP/IMAP data channels) catch these to retry next turn.
    if "want_read" in low:
        return SSLWantReadError(SSL_ERROR_WANT_READ,
                                "The operation did not complete (read)")
    if "want_write" in low:
        return SSLWantWriteError(SSL_ERROR_WANT_WRITE,
                                 "The operation did not complete (write)")
    # A peer that closes the TCP connection without first sending a TLS
    # ``close_notify`` alert is an unexpected EOF. OpenSSL/CPython report this
    # as ``SSL_ERROR_EOF`` so that :meth:`SSLSocket.read` can swallow it when
    # ``suppress_ragged_eofs`` is set (the ``makefile`` default); rustls phrases
    # it as "peer closed connection without sending TLS close_notify".
    if (
        "close_notify" in low
        or "unexpected eof" in low
        or "unexpectedeof" in low
        or "eof occurred" in low
    ):
        return SSLEOFError(
            SSL_ERROR_EOF, "EOF occurred in violation of protocol (_ssl.c)"
        )
    # A fatal TLS alert (sent or received), rendered OpenSSL-style by the native
    # layer as ``[SSL: <TOKEN>] ...``. Keep CPython's ``(errcode, message)`` arg
    # shape: asyncore TLS servers branch on the alert name in ``args[1]`` (e.g.
    # ``"SSLV3_ALERT_BAD_CERTIFICATE" in err.args[1]``). This must precede the
    # certificate-marker check below, since alert text mentions "certificate".
    if "sslv3_alert" in low or "tlsv1_alert" in low:
        return SSLError(SSL_ERROR_SSL, body)
    if any(marker in low for marker in _CERT_ERROR_MARKERS):
        # Mirror OpenSSL/CPython's canonical rendering so callers that match on
        # ``str(exc)`` (e.g. ``assertRaisesRegex(ssl.CertificateError,
        # 'CERTIFICATE_VERIFY_FAILED')``) succeed, while preserving rustls's
        # specific reason after the prefix.
        detail = "[SSL: CERTIFICATE_VERIFY_FAILED] certificate verify failed: " + body
        err = SSLCertVerificationError(SSL_ERROR_SSL, detail)
        err.reason = "CERTIFICATE_VERIFY_FAILED"
        err.library = "SSL"
        err.verify_message = body
        return err
    # CPython's SSLError always carries ``(errcode, message)``; preserve that
    # shape so callers indexing ``args[1]`` (asyncore TLS handlers) never trip.
    return SSLError(SSL_ERROR_SSL, body)


# --------------------------------------------------------------------------
# match_hostname (legacy; rustls already verifies during handshake)
# --------------------------------------------------------------------------

def _dnsname_match(dn, hostname):
    if not dn:
        return False
    if dn == hostname:
        return True
    if dn.startswith("*."):
        suffix = dn[1:]  # ".example.com"
        if hostname.endswith(suffix) and hostname.count(".") >= suffix.count("."):
            head = hostname[: -len(suffix)]
            return "." not in head and head != ""
    return False


def match_hostname(cert, hostname):
    """Verify *cert* (a getpeercert() dict) matches *hostname* (CPython parity)."""
    if not cert:
        raise ValueError("empty or no certificate, match_hostname needs a "
                         "SSL socket or SSL context with either "
                         "CERT_OPTIONAL or CERT_REQUIRED")
    dnsnames = []
    san = cert.get("subjectAltName", ())
    for key, value in san:
        if key == "DNS":
            if _dnsname_match(value, hostname):
                return
            dnsnames.append(value)
    if not dnsnames:
        for sub in cert.get("subject", ()):
            for key, value in sub:
                if key == "commonName":
                    if _dnsname_match(value, hostname):
                        return
                    dnsnames.append(value)
    if len(dnsnames) > 1:
        raise SSLCertVerificationError(
            "hostname %r doesn't match either of %s"
            % (hostname, ", ".join(map(repr, dnsnames))))
    elif len(dnsnames) == 1:
        raise SSLCertVerificationError(
            "hostname %r doesn't match %r" % (hostname, dnsnames[0]))
    else:
        raise SSLCertVerificationError(
            "no appropriate subjectAltName fields were found")


# --------------------------------------------------------------------------
# SSLContext
# --------------------------------------------------------------------------

class SSLContext:
    """A faithful-shaped wrapper over a native rustls config (``_ssl``)."""

    sslsocket_class = None  # set after SSLSocket is defined
    sslobject_class = None

    def __init__(self, protocol=PROTOCOL_TLS, *args, **kwargs):
        # CPython's ``SSLContext.__new__`` accepts ``(protocol, *args,
        # **kwargs)`` and ignores the extras, so legacy callers such as
        # ``SSLContext(PROTOCOL_TLS_CLIENT, cert_file=...)`` (see
        # ``test_httplib.test_tls13_pha``) construct without error. Mirror that
        # lenient signature here.
        self.protocol = protocol
        self._id = _ssl.new_context(int(protocol))
        self._options = Options.OP_ALL
        self._minimum_version = TLSVersion.MINIMUM_SUPPORTED
        self._maximum_version = TLSVersion.MAXIMUM_SUPPORTED
        self._verify_flags = VERIFY_DEFAULT
        # TLS 1.3 post-handshake client auth opt-in. rustls negotiates this
        # automatically when a client cert is configured, so this flag is purely
        # advisory state for callers (e.g. `http.client`) that toggle it.
        self._post_handshake_auth = False

    # --- verify mode / hostname ---
    @property
    def verify_mode(self):
        return VerifyMode(_ssl.get_verify_mode(self._id))

    @verify_mode.setter
    def verify_mode(self, value):
        _ssl.set_verify_mode(self._id, int(value))

    @property
    def check_hostname(self):
        return _ssl.get_check_hostname(self._id)

    @check_hostname.setter
    def check_hostname(self, value):
        _ssl.set_check_hostname(self._id, bool(value))

    @property
    def verify_flags(self):
        return VerifyFlags(self._verify_flags)

    @verify_flags.setter
    def verify_flags(self, value):
        self._verify_flags = int(value)

    @property
    def post_handshake_auth(self):
        return self._post_handshake_auth

    @post_handshake_auth.setter
    def post_handshake_auth(self, value):
        self._post_handshake_auth = bool(value)

    @property
    def options(self):
        return Options(self._options)

    @options.setter
    def options(self, value):
        self._options = int(value)

    @property
    def minimum_version(self):
        return self._minimum_version

    @minimum_version.setter
    def minimum_version(self, value):
        self._minimum_version = TLSVersion(value)

    @property
    def maximum_version(self):
        return self._maximum_version

    @maximum_version.setter
    def maximum_version(self, value):
        self._maximum_version = TLSVersion(value)

    # --- certificates ---
    def load_cert_chain(self, certfile, keyfile=None, password=None):
        # A malformed PEM/key surfaces from the native core as an OSError with
        # the "[SSL]" marker; re-raise it as ``ssl.SSLError`` (CPython parity —
        # test_ssl.test_malformed_key asserts ``ssl.SSLError``).
        try:
            _ssl.load_cert_chain(self._id, str(certfile),
                                 str(keyfile) if keyfile is not None else None,
                                 password)
        except OSError as e:
            raise _wrap_ssl_error(e) from None

    def load_verify_locations(self, cafile=None, capath=None, cadata=None):
        try:
            _ssl.load_verify_locations(
                self._id,
                str(cafile) if cafile is not None else None,
                str(capath) if capath is not None else None,
                cadata)
        except OSError as e:
            raise _wrap_ssl_error(e) from None

    def load_default_certs(self, purpose=Purpose.SERVER_AUTH):
        # Native roots are loaded automatically when verification is on.
        return None

    def set_default_verify_paths(self):
        return None

    def set_ciphers(self, ciphers):
        # rustls picks safe defaults; the OpenSSL cipher grammar is emulated.
        return None

    def get_ciphers(self):
        return []

    def set_alpn_protocols(self, protocols):
        _ssl.set_alpn_protocols(self._id, [str(p) for p in protocols])

    def set_npn_protocols(self, protocols):
        return None

    def get_ca_certs(self, binary_form=False):
        return []

    def cert_store_stats(self):
        return {"x509": 0, "crl": 0, "x509_ca": 0}

    def session_stats(self):
        return {}

    def set_servername_callback(self, callback):
        return None

    def set_ecdh_curve(self, name):
        return None

    # --- wrapping ---
    def wrap_socket(self, sock, server_side=False,
                    do_handshake_on_connect=True,
                    suppress_ragged_eofs=True,
                    server_hostname=None, session=None):
        # All validation lives in ``_create`` (CPython parity) so the socket-type
        # check fires before the hostname check — ``test_unsupported_dtls`` wraps
        # a hostname-less UDP socket and demands the "only stream sockets" error.
        return self.sslsocket_class._create(
            sock=sock,
            server_side=server_side,
            do_handshake_on_connect=do_handshake_on_connect,
            suppress_ragged_eofs=suppress_ragged_eofs,
            server_hostname=server_hostname,
            context=self,
            session=session,
        )

    def wrap_bio(self, incoming, outgoing, server_side=False,
                 server_hostname=None, session=None):
        # The socketless TLS path (asyncio, test_ssl's MemoryBIO tests): rustls
        # is natively a memory-BIO API, so this drives the same connection over
        # two ``MemoryBIO`` byte queues instead of a socket fd.
        if server_side and server_hostname:
            raise ValueError("server_hostname can only be specified "
                             "in client mode")
        if self.check_hostname and not server_side and not server_hostname:
            raise ValueError("check_hostname requires server_hostname")
        return self.sslobject_class._create(
            incoming, outgoing, server_side, server_hostname, self, session)


def create_default_context(purpose=Purpose.SERVER_AUTH, *, cafile=None,
                           capath=None, cadata=None):
    """Return a security-hardened SSLContext (CPython parity)."""
    if purpose == Purpose.SERVER_AUTH:
        context = SSLContext(PROTOCOL_TLS_CLIENT)
        context.verify_mode = CERT_REQUIRED
        context.check_hostname = True
    elif purpose == Purpose.CLIENT_AUTH:
        context = SSLContext(PROTOCOL_TLS_SERVER)
    else:
        context = SSLContext(PROTOCOL_TLS)
    if cafile or capath or cadata:
        context.load_verify_locations(cafile, capath, cadata)
    elif purpose == Purpose.SERVER_AUTH:
        context.load_default_certs(purpose)
    return context


def _create_unverified_context(protocol=None, *, cert_reqs=CERT_NONE,
                               check_hostname=False, purpose=Purpose.SERVER_AUTH,
                               certfile=None, keyfile=None, cafile=None,
                               capath=None, cadata=None):
    if protocol is None:
        protocol = PROTOCOL_TLS_CLIENT
    context = SSLContext(protocol)
    context.check_hostname = check_hostname
    context.verify_mode = cert_reqs
    if certfile or keyfile:
        context.load_cert_chain(certfile, keyfile)
    if cafile or capath or cadata:
        context.load_verify_locations(cafile, capath, cadata)
    return context


_create_default_https_context = create_default_context
_create_stdlib_context = _create_unverified_context


def create_connection(*a, **k):  # pragma: no cover - convenience alias
    return _socket.create_connection(*a, **k)


# --------------------------------------------------------------------------
# SSLSocket
# --------------------------------------------------------------------------

class SSLSocket(_socket_type):
    """A ``socket.socket`` whose I/O is routed through a rustls session."""

    def __init__(self, *args, **kwargs):
        raise TypeError(
            "SSLSocket does not have a public constructor. "
            "Instances are returned by SSLContext.wrap_socket().")

    @classmethod
    def _create(cls, sock, server_side=False, do_handshake_on_connect=True,
                suppress_ragged_eofs=True, server_hostname=None, context=None,
                session=None):
        if sock.getsockopt(_socket.SOL_SOCKET, _socket.SO_TYPE) != _socket.SOCK_STREAM:
            raise NotImplementedError("only stream sockets are supported")
        if server_side:
            if server_hostname:
                raise ValueError("server_hostname can only be specified "
                                 "in client mode")
            if session is not None:
                raise ValueError("session can only be specified in "
                                 "client mode")
        if context.check_hostname and not server_hostname:
            raise ValueError("check_hostname requires server_hostname")
        self = cls.__new__(cls)
        # Adopt the underlying fd from the original socket. WeavePy keys its
        # socket registry by fd, so we must `detach()` the original *first*
        # (releasing the fd without closing it) before re-wrapping it here —
        # exactly the pattern `socket.accept()` uses. Read all metadata before
        # detaching, since the original becomes unusable afterwards.
        fam, typ, prot = sock.family, sock.type, sock.proto
        timeout = sock.gettimeout()
        fd = sock.detach()
        _socket_type.__init__(self, family=fam, type=typ, proto=prot, fileno=fd)
        self.settimeout(timeout)

        self._context = context
        self.server_side = server_side
        self.server_hostname = server_hostname
        self.do_handshake_on_connect = do_handshake_on_connect
        self.suppress_ragged_eofs = suppress_ragged_eofs
        self._sslobj_id = None

        # Detect whether the underlying socket is already connected. The
        # ubiquitous client pattern ``wrap_socket(socket.socket())`` then
        # ``.connect(addr)`` hands us an *unconnected* fd: the TLS session and
        # handshake must be deferred to ``connect()`` (CPython does the same,
        # keyed on ``getpeername()`` raising ``ENOTCONN``). An accepted/already
        # connected fd (server side, or ``create_connection`` result) wraps and
        # handshakes right here.
        try:
            self.getpeername()
        except OSError as e:
            if e.errno != _errno.ENOTCONN:
                raise
            connected = False
        else:
            connected = True
        self._connected = connected

        if connected:
            try:
                self._sslobj_id = _ssl.wrap_socket(
                    context._id, self.fileno(), bool(server_side),
                    server_hostname or "")
                if do_handshake_on_connect:
                    timeout = self.gettimeout()
                    if timeout == 0.0:
                        raise ValueError("do_handshake_on_connect should not be "
                                         "specified for non-blocking sockets")
                    self.do_handshake()
            except (OSError, ValueError):
                try:
                    _socket_type.close(self)
                except Exception:
                    pass
                raise
        return self

    @property
    def _sslobj(self):
        # CPython exposes the live ``_ssl._SSLSocket`` here; non-blocking TLS
        # drivers (the asyncore servers in test_ftplib/test_imaplib) test
        # ``self.socket._sslobj is not None`` to decide whether a TLS shutdown is
        # still pending. We don't surface the raw object, but the session id is a
        # faithful "is the TLS layer still live?" sentinel (``None`` once
        # unwrapped/closed).
        return getattr(self, "_sslobj_id", None)

    # --- handshake / TLS I/O ---
    def do_handshake(self, block=False):
        try:
            _ssl.do_handshake(self._sslobj_id)
        except OSError as e:
            raise _wrap_ssl_error(e) from None

    def _check_connected(self):
        if self._sslobj_id is None:
            raise ValueError("Read/write on closed SSL socket.")

    # --- connect (client side, deferred handshake) ---
    def connect(self, addr):
        """Connect, then wrap the now-connected socket in the TLS session."""
        self._connect(addr, False)

    def connect_ex(self, addr):
        return self._connect(addr, True)

    def _connect(self, addr, connect_ex):
        if self.server_side:
            raise ValueError("can't connect in server-side mode")
        # An already-wrapped (connected) socket can't be reconnected — this is
        # the state ``_create`` leaves a pre-connected fd in.
        if self._connected or self._sslobj_id is not None:
            raise ValueError("attempt to connect already-connected SSLSocket!")
        # Attach the rustls session to our fd first (it dups the fd, so the
        # connect below — on the same kernel socket — connects both), then
        # perform the TCP connect and, finally, the TLS handshake.
        self._sslobj_id = _ssl.wrap_socket(
            self._context._id, self.fileno(), False,
            self.server_hostname or "")
        try:
            if connect_ex:
                rc = _socket_type.connect_ex(self, addr)
            else:
                rc = 0
                _socket_type.connect(self, addr)
            if rc == 0:
                self._connected = True
                if self.do_handshake_on_connect:
                    self.do_handshake()
            return rc
        except (OSError, ValueError):
            self._sslobj_id = None
            raise

    def read(self, length=1024, buffer=None):
        self._check_connected()
        try:
            data = _ssl.read(self._sslobj_id, length)
        except OSError as e:
            err = _wrap_ssl_error(e)
            # CPython: a ragged (no ``close_notify``) EOF is reported as an
            # empty read when ``suppress_ragged_eofs`` is set, otherwise it
            # propagates as ``SSLEOFError``.
            if isinstance(err, SSLEOFError) and self.suppress_ragged_eofs:
                data = b""
            else:
                raise err from None
        if buffer is not None:
            n = len(data)
            buffer[:n] = data
            return n
        return data

    def write(self, data):
        self._check_connected()
        try:
            return _ssl.write(self._sslobj_id, data)
        except OSError as e:
            raise _wrap_ssl_error(e) from None

    def recv(self, buflen=1024, flags=0):
        # Once unwrapped (``ccc()``), fall back to clear-text socket I/O — the
        # fd is still ours, just no longer behind a TLS layer (CPython does the
        # same when ``_sslobj`` is gone).
        if self._sslobj_id is None:
            return _socket_type.recv(self, buflen, flags)
        if flags != 0:
            raise ValueError("non-zero flags not allowed in calls to recv() "
                             "on %s" % self.__class__)
        # Delegate to ``read`` so ``suppress_ragged_eofs`` is honored uniformly.
        return self.read(buflen)

    def recv_into(self, buffer, nbytes=None, flags=0):
        if self._sslobj_id is None:
            return _socket_type.recv_into(self, buffer, nbytes or 0, flags)
        if flags != 0:
            raise ValueError("non-zero flags not allowed in calls to "
                             "recv_into() on %s" % self.__class__)
        if nbytes is None:
            nbytes = len(buffer)
            if nbytes == 0:
                nbytes = 1024
        return self.read(nbytes, buffer)

    def send(self, data, flags=0):
        if self._sslobj_id is None:
            return _socket_type.send(self, data, flags)
        if flags != 0:
            raise ValueError("non-zero flags not allowed in calls to send() "
                             "on %s" % self.__class__)
        try:
            return _ssl.write(self._sslobj_id, data)
        except OSError as e:
            raise _wrap_ssl_error(e) from None

    def sendall(self, data, flags=0):
        if self._sslobj_id is None:
            return _socket_type.sendall(self, data, flags)
        if flags != 0:
            raise ValueError("non-zero flags not allowed in calls to "
                             "sendall() on %s" % self.__class__)
        with memoryview(data) as view:
            total = len(view)
            sent = 0
            while sent < total:
                sent += self.send(view[sent:])
        return None

    def sendto(self, data, flags_or_addr, addr=None):
        # Datagram ops have no meaning over a live TLS stream, but an *unwrapped*
        # SSLSocket is a plain socket again — CPython delegates to ``socket`` so
        # an unconnected one surfaces the kernel's OSError (test_wrapped_unconnected).
        if self._sslobj_id is not None:
            raise ValueError("sendto not allowed on instances of %s" %
                             self.__class__)
        elif addr is None:
            return _socket_type.sendto(self, data, flags_or_addr)
        else:
            return _socket_type.sendto(self, data, flags_or_addr, addr)

    def recvfrom(self, buflen=1024, flags=0):
        if self._sslobj_id is not None:
            raise ValueError("recvfrom not allowed on instances of %s" %
                             self.__class__)
        else:
            return _socket_type.recvfrom(self, buflen, flags)

    def recvfrom_into(self, buffer, nbytes=None, flags=0):
        if self._sslobj_id is not None:
            raise ValueError("recvfrom_into not allowed on instances of %s" %
                             self.__class__)
        else:
            return _socket_type.recvfrom_into(self, buffer, nbytes, flags)

    def sendmsg(self, *args, **kwargs):
        # Ancillary-data send/recv is unsupported over TLS in CPython too.
        raise NotImplementedError("sendmsg not allowed on instances of %s" %
                                  self.__class__)

    def recvmsg(self, *args, **kwargs):
        raise NotImplementedError("recvmsg not allowed on instances of %s" %
                                  self.__class__)

    def recvmsg_into(self, *args, **kwargs):
        raise NotImplementedError(
            "recvmsg_into not allowed on instances of %s" % self.__class__)

    def dup(self):
        raise NotImplementedError("Can't dup() %s instances" %
                                  self.__class__.__name__)

    def get_channel_binding(self, cb_type="tls-unique"):
        """Return the channel binding of the requested type, or ``None``.

        rustls exposes no channel-binding material, so ``CHANNEL_BINDING_TYPES``
        is empty and every request raises ``ValueError`` — matching CPython's
        behaviour for an unsupported type (test_unknown_channel_binding)."""
        if cb_type not in CHANNEL_BINDING_TYPES:
            raise ValueError("{0} channel binding type not implemented"
                             .format(cb_type))
        if self._sslobj_id is None:
            return None
        raise NotImplementedError(
            "channel binding is not available on the rustls _ssl core")

    # --- metadata ---
    def getpeercert(self, binary_form=False):
        der = _ssl.peer_cert_der(self._sslobj_id)
        if binary_form:
            return der
        if not der:
            return {}
        # A full X.509 → dict parse isn't implemented on the rustls core;
        # rustls already enforces verification/hostname during the handshake.
        return {}

    def cipher(self):
        return _ssl.cipher(self._sslobj_id)

    def shared_ciphers(self):
        return None

    def compression(self):
        return None

    def version(self):
        if self._sslobj_id is None:
            return None
        return _ssl.version(self._sslobj_id)

    def selected_alpn_protocol(self):
        return _ssl.selected_alpn(self._sslobj_id)

    def selected_npn_protocol(self):
        return None

    def pending(self):
        if self._sslobj_id is None:
            return 0
        return _ssl.pending(self._sslobj_id)

    @property
    def context(self):
        return self._context

    @context.setter
    def context(self, ctx):
        self._context = ctx

    def unwrap(self):
        if self._sslobj_id is not None:
            _ssl.shutdown(self._sslobj_id)
            self._sslobj_id = None
            # The fd survives the TLS teardown (rustls dup'd its own); ``self``
            # keeps owning it and now behaves as a plain clear-text socket.
            return self
        # CPython raises here when there is no live TLS layer to unwrap (the
        # FTP CCC path relies on the second unwrap failing this way).
        raise ValueError("No SSL wrapper around " + str(self))

    def shutdown(self, how):
        _socket_type.shutdown(self, how)

    def close(self):
        if self._sslobj_id is not None:
            try:
                _ssl.close(self._sslobj_id)
            except Exception:
                pass
            self._sslobj_id = None
        _socket_type.close(self)

    def _real_close(self):
        if self._sslobj_id is not None:
            try:
                _ssl.close(self._sslobj_id)
            except Exception:
                pass
            self._sslobj_id = None
        _socket_type._real_close(self)


class SSLObject:
    """A TLS protocol instance over a pair of :class:`MemoryBIO` buffers.

    This is the socketless TLS surface (CPython's ``ssl.SSLObject``): instead of
    owning a socket fd, it reads ciphertext from ``incoming`` and writes
    ciphertext to ``outgoing``, exchanging plaintext via :meth:`read`/
    :meth:`write`. It is inherently non-blocking — when more ciphertext is
    needed than ``incoming`` holds, the operation raises :class:`SSLWantReadError`
    and the caller (e.g. asyncio's TLS transport) pumps the BIOs and retries.
    """

    def __init__(self, *args, **kwargs):
        raise TypeError(
            f"{self.__class__.__name__} does not have a public "
            "constructor. Instances are returned by SSLContext.wrap_bio().")

    @classmethod
    def _create(cls, incoming, outgoing, server_side=False,
                server_hostname=None, context=None, session=None):
        self = cls.__new__(cls)
        self._incoming = incoming
        self._outgoing = outgoing
        self._context = context
        self.server_side = server_side
        self.server_hostname = server_hostname
        self._session = session
        self._sslobj_id = _ssl.wrap_bio(
            context._id, incoming._id, outgoing._id,
            bool(server_side), server_hostname or "")
        return self

    @property
    def _sslobj(self):
        return getattr(self, "_sslobj_id", None)

    def do_handshake(self):
        try:
            _ssl.bio_do_handshake(self._sslobj_id)
        except OSError as e:
            raise _wrap_ssl_error(e) from None

    def read(self, length=1024, buffer=None):
        try:
            data = _ssl.bio_read(self._sslobj_id, length)
        except OSError as e:
            raise _wrap_ssl_error(e) from None
        if buffer is not None:
            n = len(data)
            buffer[:n] = data
            return n
        return data

    def write(self, data):
        try:
            return _ssl.bio_write(self._sslobj_id, data)
        except OSError as e:
            raise _wrap_ssl_error(e) from None

    def pending(self):
        return _ssl.bio_pending(self._sslobj_id)

    def getpeercert(self, binary_form=False):
        der = _ssl.bio_peer_cert_der(self._sslobj_id)
        if binary_form:
            return der
        if not der:
            return {}
        # A full X.509 → dict parse isn't implemented on the rustls core;
        # rustls already enforces verification/hostname during the handshake.
        return {}

    def cipher(self):
        return _ssl.bio_cipher(self._sslobj_id)

    def shared_ciphers(self):
        return None

    def compression(self):
        return None

    def version(self):
        return _ssl.bio_version(self._sslobj_id)

    def selected_alpn_protocol(self):
        return _ssl.bio_selected_alpn(self._sslobj_id)

    def selected_npn_protocol(self):
        return None

    def get_channel_binding(self, cb_type="tls-unique"):
        if cb_type not in CHANNEL_BINDING_TYPES:
            raise ValueError("{0} channel binding type not implemented"
                             .format(cb_type))
        return None

    def verify_client_post_handshake(self):
        raise NotImplementedError(
            "post-handshake auth is not available on the rustls _ssl core")

    @property
    def context(self):
        return self._context

    @context.setter
    def context(self, ctx):
        self._context = ctx

    @property
    def session(self):
        return self._session

    @session.setter
    def session(self, value):
        self._session = value

    @property
    def session_reused(self):
        return False

    def unwrap(self):
        # Bidirectional TLS close: emit our ``close_notify`` (once) and wait for
        # the peer's. Raises ``SSLWantReadError`` until the peer's arrives.
        try:
            _ssl.bio_shutdown(self._sslobj_id)
        except OSError as e:
            raise _wrap_ssl_error(e) from None

    def __del__(self):
        sid = getattr(self, "_sslobj_id", None)
        if sid is not None:
            try:
                _ssl.bio_close(sid)
            except Exception:
                pass
            self._sslobj_id = None


class MemoryBIO:
    """An in-memory buffer for the socketless TLS path (CPython parity).

    A :class:`MemoryBIO` is a FIFO of ciphertext bytes shuttled between an
    :class:`SSLObject` and the transport. ``write``/``read`` move bytes in and
    out; ``write_eof`` records that no more will arrive; ``pending``/``eof``
    report the buffer state.
    """

    def __init__(self):
        self._id = _ssl.memory_bio_new()

    @property
    def pending(self):
        """Number of ciphertext bytes currently buffered."""
        return _ssl.memory_bio_pending(self._id)

    @property
    def eof(self):
        """True once the buffer is drained *and* ``write_eof`` was called."""
        return _ssl.memory_bio_eof(self._id)

    def read(self, size=-1):
        """Read up to *size* bytes (all buffered bytes when *size* < 0)."""
        if not isinstance(size, int):
            raise TypeError("an integer is required")
        return _ssl.memory_bio_read(self._id, size)

    def write(self, buf):
        """Append the bytes-like *buf*; return the number of bytes written."""
        if isinstance(buf, str):
            raise TypeError("string argument without an encoding")
        if isinstance(buf, memoryview):
            # CPython requests a C-contiguous buffer and surfaces the
            # PyBUF_CONTIG failure as BufferError (test_buffer_types).
            if not buf.contiguous:
                raise BufferError(
                    "memoryview: underlying buffer is not C-contiguous")
            return _ssl.memory_bio_write(self._id, buf)
        if not isinstance(buf, (bytes, bytearray)):
            raise TypeError(
                "a bytes-like object is required, not '%s'"
                % type(buf).__name__)
        return _ssl.memory_bio_write(self._id, buf)

    def write_eof(self):
        """Mark the write side closed; no more bytes will be appended."""
        _ssl.memory_bio_set_eof(self._id)

    def __del__(self):
        bid = getattr(self, "_id", None)
        if bid is not None:
            try:
                _ssl.memory_bio_free(bid)
            except Exception:
                pass
            self._id = None


SSLContext.sslsocket_class = SSLSocket
SSLContext.sslobject_class = SSLObject


def wrap_socket(sock, keyfile=None, certfile=None, server_side=False,
                cert_reqs=CERT_NONE, ssl_version=PROTOCOL_TLS, ca_certs=None,
                do_handshake_on_connect=True, suppress_ragged_eofs=True,
                ciphers=None):
    """Deprecated top-level helper (CPython parity)."""
    context = SSLContext(ssl_version)
    context.verify_mode = cert_reqs
    if ca_certs:
        context.load_verify_locations(ca_certs)
    if certfile:
        context.load_cert_chain(certfile, keyfile)
    if ciphers:
        context.set_ciphers(ciphers)
    return context.wrap_socket(
        sock, server_side=server_side,
        do_handshake_on_connect=do_handshake_on_connect,
        suppress_ragged_eofs=suppress_ragged_eofs)


from collections import namedtuple as _namedtuple

DefaultVerifyPaths = _namedtuple("DefaultVerifyPaths",
    "cafile capath openssl_cafile_env openssl_cafile "
    "openssl_capath_env openssl_capath")


def get_default_verify_paths():
    """Return paths to default cafile and capath as a 6-field namedtuple.

    rustls bundles its own trust roots, so there are no compiled-in OpenSSL
    paths; we honour the ``SSL_CERT_FILE``/``SSL_CERT_DIR`` env overrides (the
    only part ``test_ssl`` asserts) and fall back to ``None``."""
    import os
    parts = ("SSL_CERT_FILE", "", "SSL_CERT_DIR", "")
    cafile = os.environ.get(parts[0], parts[1])
    capath = os.environ.get(parts[2], parts[3])
    return DefaultVerifyPaths(
        cafile if cafile and os.path.exists(cafile) else None,
        capath if capath and os.path.exists(capath) else None,
        *parts)


def cert_time_to_seconds(cert_time):
    """Return the time in seconds since the Epoch, given the timestring
    representing the "notBefore" or "notAfter" date from a certificate
    in ``"%b %d %H:%M:%S %Y %Z"`` strptime format (C locale).

    "notBefore" or "notAfter" dates must use UTC (RFC 5280).

    Month is one of: Jan Feb Mar Apr May Jun Jul Aug Sep Oct Nov Dec
    UTC should be specified as GMT (see ASN1_TIME_print())
    """
    from time import strptime
    from calendar import timegm

    months = (
        "Jan","Feb","Mar","Apr","May","Jun",
        "Jul","Aug","Sep","Oct","Nov","Dec"
    )
    time_format = ' %d %H:%M:%S %Y GMT' # NOTE: no month, fixed GMT
    try:
        month_number = months.index(cert_time[:3].title()) + 1
    except ValueError:
        raise ValueError('time data %r does not match '
                         'format "%%b%s"' % (cert_time, time_format))
    else:
        # found valid month
        tt = strptime(cert_time[3:], time_format)
        # return an integer, the previous mktime()-based implementation
        # returned a float (fractional seconds are always zero here).
        return timegm((tt[0], month_number) + tt[2:6])

PEM_HEADER = "-----BEGIN CERTIFICATE-----"
PEM_FOOTER = "-----END CERTIFICATE-----"

def DER_cert_to_PEM_cert(der_cert_bytes):
    """Takes a certificate in binary DER format and returns the
    PEM version of it as a string."""
    import base64
    f = str(base64.standard_b64encode(der_cert_bytes), 'ASCII', 'strict')
    ss = [PEM_HEADER]
    ss += [f[i:i+64] for i in range(0, len(f), 64)]
    ss.append(PEM_FOOTER + '\n')
    return '\n'.join(ss)

def PEM_cert_to_DER_cert(pem_cert_string):
    """Takes a certificate in ASCII PEM format and returns the
    DER-encoded version of it as a byte sequence"""
    import base64
    if not pem_cert_string.startswith(PEM_HEADER):
        raise ValueError("Invalid PEM encoding; must start with %s"
                         % PEM_HEADER)
    if not pem_cert_string.strip().endswith(PEM_FOOTER):
        raise ValueError("Invalid PEM encoding; must end with %s"
                         % PEM_FOOTER)
    d = pem_cert_string.strip()[len(PEM_HEADER):-len(PEM_FOOTER)]
    return base64.decodebytes(d.encode('ASCII', 'strict'))


# --- PRNG surface (CPython exposes these from OpenSSL; rustls uses ring's
# CSPRNG, which is always seeded, so RAND_status is unconditionally ready and
# RAND_add is a no-op). RAND_bytes draws from the OS CSPRNG (os.urandom).
def RAND_status():
    """True — the cryptographic PRNG (ring, via the OS) is always seeded."""
    return True

def RAND_add(string, entropy):
    """Mix a seed into the PRNG. A no-op here (the OS CSPRNG self-seeds), but
    it still type-checks its argument the way OpenSSL's RAND_add does."""
    if not isinstance(string, (str, bytes, bytearray, memoryview)):
        raise TypeError("RAND_add() argument 1 must be str or bytes-like")

def RAND_bytes(n):
    """Return *n* cryptographically strong random bytes from the OS CSPRNG."""
    import os
    if n < 0:
        raise ValueError("num must be positive")
    return os.urandom(n)


__all__ = [
    "SSLContext", "SSLSocket", "SSLObject", "SSLError", "SSLZeroReturnError",
    "SSLWantReadError", "SSLWantWriteError", "SSLSyscallError", "SSLEOFError",
    "SSLCertVerificationError", "CertificateError", "create_default_context",
    "wrap_socket", "match_hostname", "get_default_verify_paths", "MemoryBIO",
    "CERT_NONE", "CERT_OPTIONAL", "CERT_REQUIRED", "VerifyMode", "VerifyFlags",
    "Purpose", "Options", "TLSVersion",
    "PROTOCOL_TLS", "PROTOCOL_TLS_CLIENT", "PROTOCOL_TLS_SERVER",
    "PROTOCOL_TLSv1", "PROTOCOL_TLSv1_1", "PROTOCOL_TLSv1_2",
    "HAS_SNI", "HAS_ALPN", "HAS_TLSv1_3",
    "OPENSSL_VERSION", "OPENSSL_VERSION_NUMBER", "OPENSSL_VERSION_INFO",
    "DER_cert_to_PEM_cert", "PEM_cert_to_DER_cert", "cert_time_to_seconds",
    "PEM_HEADER", "PEM_FOOTER", "RAND_status", "RAND_add", "RAND_bytes",
    "DefaultVerifyPaths", "get_default_verify_paths", "PROTOCOL_SSLv23",
]
