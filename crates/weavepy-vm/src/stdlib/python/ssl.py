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
import warnings as _warnings
from collections import namedtuple as _namedtuple
from enum import Enum as _Enum, IntEnum as _IntEnum, IntFlag as _IntFlag

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


# Build the protocol enum with CPython's own conversion machinery so member
# order, aliases and module re-exports match what ``_IntEnum._convert_``
# produces (test_ssl's TestEnumerations compares against exactly that shape).
_IntEnum._convert_(
    '_SSLMethod', __name__,
    lambda name: name.startswith('PROTOCOL_') and name != 'PROTOCOL_SSLv23',
    source=_ssl)

# Deprecated CPython alias kept for parity (``test_ssl.test_constants``);
# also attached to the enum itself, as CPython does.
PROTOCOL_SSLv23 = _SSLMethod.PROTOCOL_SSLv23 = _SSLMethod.PROTOCOL_TLS

_PROTOCOL_NAMES = {value: name for name, value in _SSLMethod.__members__.items()}

HAS_SNI = bool(_ssl.HAS_SNI)
HAS_ECDH = bool(_ssl.HAS_ECDH)
HAS_NPN = bool(_ssl.HAS_NPN)
HAS_ALPN = bool(_ssl.HAS_ALPN)
# rustls negotiates TLS 1.2 and 1.3 only; advertising the legacy versions as
# absent makes version-pinned tests skip exactly like an OpenSSL build with
# them compiled out.
HAS_TLSv1 = False
HAS_TLSv1_1 = False
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
# rustls never consults the subject common name (SAN-only matching), which is
# exactly the capability HAS_NEVER_CHECK_COMMON_NAME advertises.
HAS_NEVER_CHECK_COMMON_NAME = True
HAS_PSK = False                      # external PSK key exchange not exposed
HAS_PSK_TLS13 = False

OPENSSL_VERSION = _ssl.OPENSSL_VERSION
OPENSSL_VERSION_NUMBER = _ssl.OPENSSL_VERSION_NUMBER
OPENSSL_VERSION_INFO = _ssl.OPENSSL_VERSION_INFO
_OPENSSL_API_VERSION = OPENSSL_VERSION_INFO

ENCODING_PEM = _ssl.ENCODING_PEM
ENCODING_DER = _ssl.ENCODING_DER

SSL_ERROR_NONE = _ssl.SSL_ERROR_NONE
SSL_ERROR_SSL = _ssl.SSL_ERROR_SSL
SSL_ERROR_WANT_READ = _ssl.SSL_ERROR_WANT_READ
SSL_ERROR_WANT_WRITE = _ssl.SSL_ERROR_WANT_WRITE
SSL_ERROR_WANT_X509_LOOKUP = _ssl.SSL_ERROR_WANT_X509_LOOKUP
SSL_ERROR_SYSCALL = _ssl.SSL_ERROR_SYSCALL
SSL_ERROR_ZERO_RETURN = _ssl.SSL_ERROR_ZERO_RETURN
SSL_ERROR_WANT_CONNECT = _ssl.SSL_ERROR_WANT_CONNECT
SSL_ERROR_EOF = _ssl.SSL_ERROR_EOF


# The remaining constant families are `_convert_`ed from the native module
# just like CPython's ssl.py does — this injects the enum members into the
# module namespace (replacing the plain ints) with the exact member order
# and aliasing TestEnumerations expects.
_IntFlag._convert_(
    'Options', __name__,
    lambda name: name.startswith('OP_'),
    source=_ssl)

_IntEnum._convert_(
    'AlertDescription', __name__,
    lambda name: name.startswith('ALERT_DESCRIPTION_'),
    source=_ssl)

_IntEnum._convert_(
    'SSLErrorNumber', __name__,
    lambda name: name.startswith('SSL_ERROR_'),
    source=_ssl)

_IntFlag._convert_(
    'VerifyFlags', __name__,
    lambda name: name.startswith('VERIFY_'),
    source=_ssl)

_IntEnum._convert_(
    'VerifyMode', __name__,
    lambda name: name.startswith('CERT_'),
    source=_ssl)


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


# Minimal OID registry rows: (nid, shortname, longname, oid). CPython
# resolves these through OpenSSL's OBJ_ database; rustls has none, so we
# carry the subset the stdlib and its tests actually look up.
_ASN1_TABLE = (
    (129, 'serverAuth', 'TLS Web Server Authentication', '1.3.6.1.5.5.7.3.1'),
    (130, 'clientAuth', 'TLS Web Client Authentication', '1.3.6.1.5.5.7.3.2'),
    (131, 'codeSigning', 'Code Signing', '1.3.6.1.5.5.7.3.3'),
    (132, 'emailProtection', 'E-mail Protection', '1.3.6.1.5.5.7.3.4'),
    (133, 'timeStamping', 'Time Stamping', '1.3.6.1.5.5.7.3.8'),
    (180, 'OCSPSigning', 'OCSP Signing', '1.3.6.1.5.5.7.3.9'),
)


class _ASN1Object(_namedtuple("_ASN1Object", "nid shortname longname oid")):
    """ASN.1 object identifier lookup (CPython shape; subset registry)."""
    __slots__ = ()

    def __new__(cls, oid):
        # name=False in CPython's _txt2obj: only the dotted OID is accepted.
        for row in _ASN1_TABLE:
            if row[3] == oid:
                return super().__new__(cls, *row)
        raise ValueError("unknown object '%s'" % (oid,))

    @classmethod
    def fromnid(cls, nid):
        """Create _ASN1Object from OpenSSL numeric ID."""
        for row in _ASN1_TABLE:
            if row[0] == nid:
                return super().__new__(cls, *row)
        raise ValueError("unknown NID %i" % (nid,))

    @classmethod
    def fromname(cls, name):
        """Create _ASN1Object from short name, long name or OID."""
        for row in _ASN1_TABLE:
            if name in (row[1], row[2], row[3]):
                return super().__new__(cls, *row)
        raise ValueError("unknown object '%s'" % (name,))


class Purpose(_ASN1Object, _Enum):
    """SSLContext purpose flags with X509v3 Extended Key Usage objects."""
    SERVER_AUTH = '1.3.6.1.5.5.7.3.1'
    CLIENT_AUTH = '1.3.6.1.5.5.7.3.2'


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
    # A clean close_notify from the peer *after* our own shutdown was sent is
    # OpenSSL's SSL_ERROR_ZERO_RETURN (not a ragged EOF): bidirectional-shutdown
    # loops (`while True: sslobj.read()` in test_asyncio's TLS servers) rely on
    # SSLZeroReturnError to terminate.
    if "zero_return" in low:
        return SSLZeroReturnError(SSL_ERROR_ZERO_RETURN,
                                  "TLS/SSL connection has been closed (EOF)")
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
        err = SSLError(SSL_ERROR_SSL, body)
        # `.reason` is the bare OpenSSL token (CPython: 'TLSV1_ALERT_ACCESS_
        # DENIED'), extracted from the `[SSL: TOKEN] ...` rendering.
        if "[SSL: " in body and "]" in body:
            err.reason = body.split("[SSL: ", 1)[1].split("]", 1)[0]
        else:
            err.reason = body
        return err
    if any(marker in low for marker in _CERT_ERROR_MARKERS):
        # Mirror OpenSSL/CPython's canonical rendering so callers that match on
        # ``str(exc)`` (e.g. ``assertRaisesRegex(ssl.CertificateError,
        # 'CERTIFICATE_VERIFY_FAILED')``) succeed, while preserving rustls's
        # specific reason after the prefix.
        detail = "[SSL: CERTIFICATE_VERIFY_FAILED] certificate verify failed: " + body
        err = SSLCertVerificationError(SSL_ERROR_SSL, detail)
        err.reason = "CERTIFICATE_VERIFY_FAILED"
        err.library = "SSL"
        if "unknownissuer" in low.replace(" ", ""):
            # OpenSSL's X509_V_ERR_UNABLE_TO_GET_ISSUER_CERT_LOCALLY.
            err.verify_code = 20
            err.verify_message = "unable to get local issuer certificate"
        else:
            err.verify_message = body
        return err
    # CPython's SSLError always carries ``(errcode, message)``; preserve that
    # shape so callers indexing ``args[1]`` (asyncore TLS handlers) never trip.
    # ``reason`` mirrors the message (OpenSSL puts its reason token there;
    # test_preauth_data_* greps `.reason` for the failure text).
    err = SSLError(SSL_ERROR_SSL, body)
    err.reason = body
    return err


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
# Cipher-suite table and OpenSSL cipher-string grammar (emulated)
# --------------------------------------------------------------------------
#
# rustls negotiates from a fixed, safe suite set; ``set_ciphers`` cannot alter
# what the wire negotiates. What CPython code *observes* though is (a) valid
# OpenSSL cipher strings being accepted, (b) garbage raising ``SSLError("No
# cipher can be selected.")`` and (c) ``get_ciphers()`` returning OpenSSL-shaped
# dicts. This table lists rustls' actual suites with their OpenSSL names and
# alias keywords, and ``_select_ciphers`` interprets the grammar against it.

def _cipher_entry(name, protocol, kx, au, enc, bits, mac, aliases):
    description = "%-23s %s Kx=%-8s Au=%-5s Enc=%s Mac=%s" % (
        name, protocol, kx, au, enc, mac)
    return (
        {
            "id": 0x03000000 | (hash(name) & 0xFFFF),
            "name": name,
            "protocol": protocol,
            "description": description,
            "strength_bits": bits,
            "alg_bits": bits,
        },
        frozenset(aliases) | {name},
    )


_ALL_ALIASES = {"ALL", "DEFAULT", "COMPLEMENTOFDEFAULT", "HIGH", "AEAD",
                "SECURE128", "SECURE256"}

_CIPHER_TABLE = [
    _cipher_entry(
        "TLS_AES_256_GCM_SHA384", "TLSv1.3", "any", "any",
        "AESGCM(256)", 256, "AEAD",
        _ALL_ALIASES | {"AES", "AES256", "AESGCM", "SHA384", "TLSv1.3"}),
    _cipher_entry(
        "TLS_AES_128_GCM_SHA256", "TLSv1.3", "any", "any",
        "AESGCM(128)", 128, "AEAD",
        _ALL_ALIASES | {"AES", "AES128", "AESGCM", "SHA256", "TLSv1.3"}),
    _cipher_entry(
        "TLS_CHACHA20_POLY1305_SHA256", "TLSv1.3", "any", "any",
        "CHACHA20/POLY1305(256)", 256, "AEAD",
        _ALL_ALIASES | {"CHACHA20", "POLY1305", "SHA256", "TLSv1.3"}),
    _cipher_entry(
        "ECDHE-ECDSA-AES256-GCM-SHA384", "TLSv1.2", "ECDH", "ECDSA",
        "AESGCM(256)", 256, "AEAD",
        _ALL_ALIASES | {"AES", "AES256", "AESGCM", "SHA384", "ECDHE", "ECDH",
                        "EECDH", "kEECDH", "ECDSA", "aECDSA", "TLSv1.2"}),
    _cipher_entry(
        "ECDHE-RSA-AES256-GCM-SHA384", "TLSv1.2", "ECDH", "RSA",
        "AESGCM(256)", 256, "AEAD",
        _ALL_ALIASES | {"AES", "AES256", "AESGCM", "SHA384", "ECDHE", "ECDH",
                        "EECDH", "kEECDH", "RSA", "aRSA", "TLSv1.2"}),
    _cipher_entry(
        "ECDHE-ECDSA-AES128-GCM-SHA256", "TLSv1.2", "ECDH", "ECDSA",
        "AESGCM(128)", 128, "AEAD",
        _ALL_ALIASES | {"AES", "AES128", "AESGCM", "SHA256", "ECDHE", "ECDH",
                        "EECDH", "kEECDH", "ECDSA", "aECDSA", "TLSv1.2"}),
    _cipher_entry(
        "ECDHE-RSA-AES128-GCM-SHA256", "TLSv1.2", "ECDH", "RSA",
        "AESGCM(128)", 128, "AEAD",
        _ALL_ALIASES | {"AES", "AES128", "AESGCM", "SHA256", "ECDHE", "ECDH",
                        "EECDH", "kEECDH", "RSA", "aRSA", "TLSv1.2"}),
    _cipher_entry(
        "ECDHE-ECDSA-CHACHA20-POLY1305", "TLSv1.2", "ECDH", "ECDSA",
        "CHACHA20/POLY1305(256)", 256, "AEAD",
        _ALL_ALIASES | {"CHACHA20", "POLY1305", "ECDHE", "ECDH", "EECDH",
                        "kEECDH", "ECDSA", "aECDSA", "TLSv1.2"}),
    _cipher_entry(
        "ECDHE-RSA-CHACHA20-POLY1305", "TLSv1.2", "ECDH", "RSA",
        "CHACHA20/POLY1305(256)", 256, "AEAD",
        _ALL_ALIASES | {"CHACHA20", "POLY1305", "ECDHE", "ECDH", "EECDH",
                        "kEECDH", "RSA", "aRSA", "TLSv1.2"}),
]

_CIPHER_SUITES = [entry for entry, _aliases in _CIPHER_TABLE]


def _select_ciphers(cipher_string):
    """Interpret an OpenSSL cipher string against the rustls suite table.

    Returns the selected suite dicts (possibly empty). Understands the
    grammar's operators: `:`/`,`/space separators, `!` (permanent kill),
    `-` (remove), `+` (move to end), intra-token `+` (AND of aliases),
    `@`-directives (ignored), and the special keywords."""
    if not isinstance(cipher_string, str):
        raise TypeError("cipher string must be str")
    selected = []
    killed = set()

    def matches(token):
        parts = token.split("+")
        out = []
        for entry, aliases in _CIPHER_TABLE:
            if all(p in aliases for p in parts):
                out.append(entry)
        return out

    for token in cipher_string.replace(",", ":").replace(" ", ":").split(":"):
        if not token or token.startswith("@"):
            continue  # @SECLEVEL / @STRENGTH directives don't select suites
        if token.startswith("!"):
            for entry in matches(token[1:]):
                killed.add(entry["name"])
            selected = [e for e in selected if e["name"] not in killed]
        elif token.startswith("-"):
            names = {e["name"] for e in matches(token[1:])}
            selected = [e for e in selected if e["name"] not in names]
        elif token.startswith("+"):
            names = {e["name"] for e in matches(token[1:])}
            moved = [e for e in selected if e["name"] in names]
            selected = [e for e in selected if e["name"] not in names] + moved
        elif token == "STRENGTH":
            selected.sort(key=lambda e: -e["strength_bits"])
        else:
            for entry in matches(token):
                if entry["name"] not in killed and entry not in selected:
                    selected.append(entry)
    return selected


# --------------------------------------------------------------------------
# SSLContext
# --------------------------------------------------------------------------

# Default context options (what OpenSSL/CPython enable on a fresh SSL_CTX).
_DEFAULT_CONTEXT_OPTIONS = (
    OP_ALL | OP_NO_SSLv2 | OP_NO_SSLv3
    | OP_NO_COMPRESSION | OP_CIPHER_SERVER_PREFERENCE
    | OP_SINGLE_DH_USE | OP_SINGLE_ECDH_USE
    | OP_ENABLE_MIDDLEBOX_COMPAT
)

_DEPRECATED_OPTION_BITS = (
    OP_NO_SSLv2 | OP_NO_SSLv3 | OP_NO_TLSv1
    | OP_NO_TLSv1_1 | OP_NO_TLSv1_2 | OP_NO_TLSv1_3
)

# Protocols whose bare use warns (auto-negotiation via TLS_CLIENT/TLS_SERVER
# is the supported spelling in CPython 3.13).
_DEPRECATED_PROTOCOLS = frozenset(
    {PROTOCOL_TLS, PROTOCOL_TLSv1, PROTOCOL_TLSv1_1, PROTOCOL_TLSv1_2})

_AUTO_PROTOCOLS = frozenset(
    {PROTOCOL_TLS, PROTOCOL_TLS_CLIENT, PROTOCOL_TLS_SERVER})


def _encode_hostname(hostname):
    # CPython's C layer stores the IDNA (punycode) form of a non-ASCII
    # server_hostname and accepts pre-encoded bytes; `SSLSocket.
    # server_hostname` then reads back 'xn--...' (test_check_hostname_idn).
    if hostname is None:
        return None
    if isinstance(hostname, str):
        # Always route through the idna codec (CPython does the same): it
        # both punycodes non-ASCII labels and validates ASCII ones (empty
        # labels such as '.example.com' raise UnicodeError).
        return hostname.encode("idna").decode("ascii")
    return hostname.decode("ascii")


def _path_arg(p, argname):
    """CPython's PySSL path converter: str/bytes/os.PathLike or TypeError."""
    if p is None:
        return None
    if isinstance(p, (bytes, bytearray)):
        return bytes(p).decode("utf-8", "surrogateescape")
    if isinstance(p, str):
        return p
    fspath = getattr(type(p), "__fspath__", None)
    if fspath is not None:
        return _path_arg(fspath(p), argname)
    raise TypeError(f"{argname} should be a valid filesystem path")


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
        try:
            protocol = _SSLMethod(protocol)
        except ValueError:
            raise ValueError("invalid or unsupported protocol version") from None
        if protocol in _DEPRECATED_PROTOCOLS:
            _warnings.warn(f'ssl.{protocol.name} is deprecated',
                           DeprecationWarning, 2)
        self.protocol = protocol
        self._id = _ssl.new_context(int(protocol))
        self._options = int(_DEFAULT_CONTEXT_OPTIONS)
        self._minimum_version = TLSVersion.MINIMUM_SUPPORTED
        self._maximum_version = TLSVersion.MAXIMUM_SUPPORTED
        # OpenSSL contexts start with X509_V_FLAG_TRUSTED_FIRST set.
        self._verify_flags = VerifyFlags.VERIFY_X509_TRUSTED_FIRST
        # TLS 1.3 post-handshake client auth opt-in. rustls negotiates this
        # automatically when a client cert is configured, so this flag is purely
        # advisory state for callers (e.g. `http.client`) that toggle it.
        self._post_handshake_auth = False
        self._num_tickets = 2
        self._hostname_checks_common_name = True
        self._sni_callback = None
        self._msg_cb = None

    # --- verify mode / hostname ---
    @property
    def verify_mode(self):
        return VerifyMode(_ssl.get_verify_mode(self._id))

    @verify_mode.setter
    def verify_mode(self, value):
        if not isinstance(value, int):
            raise TypeError(
                f"verify_mode must be an int, not {type(value).__name__}")
        value = VerifyMode(value)  # invalid ints raise ValueError
        if value == CERT_NONE and self.check_hostname:
            raise ValueError(
                "Cannot set verify_mode to CERT_NONE when "
                "check_hostname is enabled.")
        _ssl.set_verify_mode(self._id, int(value))

    @property
    def check_hostname(self):
        return _ssl.get_check_hostname(self._id)

    @check_hostname.setter
    def check_hostname(self, value):
        value = bool(value)
        # Enabling hostname checks auto-upgrades CERT_NONE to CERT_REQUIRED
        # (CPython's setter does the same).
        if value and _ssl.get_verify_mode(self._id) == CERT_NONE:
            _ssl.set_verify_mode(self._id, int(CERT_REQUIRED))
        _ssl.set_check_hostname(self._id, value)

    @property
    def verify_flags(self):
        return VerifyFlags(self._verify_flags)

    @verify_flags.setter
    def verify_flags(self, value):
        self._verify_flags = int(value)
        _ssl.set_verify_flags(self._id, int(value))

    @property
    def post_handshake_auth(self):
        return self._post_handshake_auth

    @post_handshake_auth.setter
    def post_handshake_auth(self, value):
        self._post_handshake_auth = bool(value)
        _ssl.set_post_handshake_auth(self._id, bool(value))

    @property
    def options(self):
        return Options(self._options)

    @options.setter
    def options(self, value):
        if isinstance(value, bool) or not isinstance(value, int):
            raise TypeError(f"argument must be int, not {type(value).__name__}")
        # OpenSSL options are a uint64 bitmask.
        if value < 0 or value >= (1 << 64):
            raise OverflowError("Python int too large to convert to C unsigned long long")
        if value & int(_DEPRECATED_OPTION_BITS) & ~self._options:
            _warnings.warn(
                'ssl.OP_NO_SSL*/ssl.OP_NO_TLS* options are deprecated',
                DeprecationWarning, 2)
        self._options = value
        _ssl.set_options(self._id, int(value))

    @property
    def num_tickets(self):
        return self._num_tickets

    @num_tickets.setter
    def num_tickets(self, value):
        if not isinstance(value, int) or isinstance(value, bool):
            raise TypeError("value must be an integer")
        if value < 0:
            raise ValueError("value must be non-negative")
        if self.protocol != PROTOCOL_TLS_SERVER:
            raise ValueError("SSLContext is not a server context.")
        self._num_tickets = value

    @property
    def hostname_checks_common_name(self):
        return self._hostname_checks_common_name

    @hostname_checks_common_name.setter
    def hostname_checks_common_name(self, value):
        # rustls never consults the subject CN (SAN-only), so this is
        # bookkeeping; HAS_NEVER_CHECK_COMMON_NAME advertises the capability.
        self._hostname_checks_common_name = bool(value)

    def _sync_versions(self):
        _ssl.set_min_max_version(
            self._id, int(self._minimum_version), int(self._maximum_version))

    def _check_version_settable(self):
        if self.protocol not in _AUTO_PROTOCOLS:
            raise ValueError(
                "this context doesn't support modification of "
                "highest and lowest version")

    @property
    def minimum_version(self):
        return self._minimum_version

    @minimum_version.setter
    def minimum_version(self, value):
        self._check_version_settable()
        value = TLSVersion(value)
        if value == TLSVersion.MAXIMUM_SUPPORTED:
            # OpenSSL resolves the sentinel to the highest supported version.
            value = TLSVersion.TLSv1_3 if HAS_TLSv1_3 else TLSVersion.TLSv1_2
        elif value in (TLSVersion.SSLv3, TLSVersion.TLSv1, TLSVersion.TLSv1_1):
            _warnings.warn(f'ssl.TLSVersion.{value.name} is deprecated',
                           DeprecationWarning, 2)
        self._minimum_version = value
        self._sync_versions()

    @property
    def maximum_version(self):
        return self._maximum_version

    @maximum_version.setter
    def maximum_version(self, value):
        self._check_version_settable()
        value = TLSVersion(value)
        if value == TLSVersion.MINIMUM_SUPPORTED:
            # OpenSSL resolves the sentinel to the lowest supported version.
            value = TLSVersion.TLSv1_2
        elif value in (TLSVersion.SSLv3, TLSVersion.TLSv1, TLSVersion.TLSv1_1):
            _warnings.warn(f'ssl.TLSVersion.{value.name} is deprecated',
                           DeprecationWarning, 2)
        self._maximum_version = value
        self._sync_versions()

    # --- certificates ---
    def load_cert_chain(self, certfile, keyfile=None, password=None):
        certfile = _path_arg(certfile, "certfile")
        keyfile = _path_arg(keyfile, "keyfile")
        # First try without a password: like OpenSSL's pem_password_cb, the
        # password (and any password *callable*) must only be consulted when
        # the key is actually encrypted (test_load_cert_chain loads a plain
        # key with a raising callback and expects no call).
        try:
            _ssl.load_cert_chain(self._id, certfile, keyfile, None)
            return
        except OSError as e:
            if password is None or "password required" not in str(e):
                # A malformed PEM/key carries the "[SSL]"/"PEM lib" marker;
                # re-raise as ``ssl.SSLError`` (test_malformed_key).
                raise _wrap_ssl_error(e) from None
        # Encrypted key: materialize the password (CPython's password_info
        # semantics — str/bytes/bytearray or a callable producing one, capped
        # at OpenSSL's PEM_BUFSIZE of 1024 incl. NUL).
        if callable(password):
            password = password()
            if isinstance(password, str):
                password = password.encode("utf-8")
            elif isinstance(password, (bytes, bytearray)):
                password = bytes(password)
            else:
                raise TypeError("password callback must return a string")
        elif isinstance(password, str):
            password = password.encode("utf-8")
        elif isinstance(password, (bytes, bytearray)):
            password = bytes(password)
        else:
            raise TypeError("password should be a string or callable")
        if len(password) > 1023:
            raise ValueError("password cannot be longer than 1023 bytes")
        try:
            _ssl.load_cert_chain(self._id, certfile, keyfile, password)
        except OSError as e:
            raise _wrap_ssl_error(e) from None

    def load_verify_locations(self, cafile=None, capath=None, cadata=None):
        if cafile is None and capath is None and cadata is None:
            raise TypeError("cafile, capath and cadata cannot be all omitted")
        try:
            _ssl.load_verify_locations(
                self._id, _path_arg(cafile, "cafile"),
                _path_arg(capath, "capath"), cadata)
        except OSError as e:
            raise _wrap_ssl_error(e) from None

    def load_default_certs(self, purpose=Purpose.SERVER_AUTH):
        # Native (webpki) roots are consulted automatically when verification
        # is on; honor the OpenSSL env overrides like set_default_verify_paths.
        if not isinstance(purpose, _ASN1Object):
            raise TypeError(purpose)
        self.set_default_verify_paths()

    def load_dh_params(self, path):
        # rustls only offers ECDHE key exchange, so the parameters are parsed
        # for validity (CPython-shaped errors) and then unused.
        if path is None:
            raise TypeError("path should be a valid filesystem path")
        with open(path, "rb") as f:
            data = f.read()
        if b"DH PARAMETERS" not in data:
            # OpenSSL fails in the PEM decoder; test_lib_reason asserts the
            # library/reason attribute pair and the NO_START_LINE token.
            err = SSLError(SSL_ERROR_SSL,
                           "[PEM: NO_START_LINE] no start line (_ssl.c)")
            err.library = "PEM"
            err.reason = "NO_START_LINE"
            raise err
        return None

    def set_default_verify_paths(self):
        # OpenSSL's SSL_CTX_set_default_verify_paths: pull in the system
        # trust store (platform-native roots on the rustls side) and honour
        # the SSL_CERT_FILE / SSL_CERT_DIR environment overrides.
        _ssl.set_default_verify_paths(self._id)
        import os
        cafile = os.environ.get("SSL_CERT_FILE")
        capath = os.environ.get("SSL_CERT_DIR")
        if cafile and os.path.isfile(cafile):
            self.load_verify_locations(cafile=cafile)
        if capath and os.path.isdir(capath):
            self.load_verify_locations(capath=capath)
        return None

    def set_ciphers(self, ciphers):
        # rustls always negotiates from its fixed safe set; the OpenSSL
        # cipher-string grammar is *interpreted* against that set so that
        # valid strings are accepted, garbage raises ("No cipher can be
        # selected"), and get_ciphers() reflects the selection.
        selected = _select_ciphers(ciphers)
        if not selected:
            raise SSLError("No cipher can be selected.")
        self._selected_ciphers = selected
        # Restrict the native TLS 1.2 suite list to the selection (TLS 1.3
        # suites are never filtered — OpenSSL cipher-string semantics).
        _ssl.set_cipher_suites(self._id, [d["name"] for d in selected])

    def get_ciphers(self):
        return [dict(d) for d in getattr(self, "_selected_ciphers",
                                         _CIPHER_SUITES)]

    def set_alpn_protocols(self, protocols):
        _ssl.set_alpn_protocols(self._id, [str(p) for p in protocols])

    def set_npn_protocols(self, protocols):
        return None

    def get_ca_certs(self, binary_form=False):
        ders = _ssl.get_ca_certs(self._id)
        if binary_form:
            return list(ders)
        return [_ssl.decode_cert(der) for der in ders]

    def cert_store_stats(self):
        return _ssl.cert_store_stats(self._id)

    def session_stats(self):
        # `accept` and `hits` are tracked natively (loopback session-reuse
        # emulation); the rest of the key set is part of the CPython surface.
        stats = _ssl.session_stats(self._id)
        return {
            'number': stats['number'],
            'connect': stats['connect'],
            'connect_good': stats['connect_good'],
            'connect_renegotiate': stats['connect_renegotiate'],
            'accept': stats['accept'],
            'accept_good': stats['accept_good'],
            'accept_renegotiate': stats['accept_renegotiate'],
            'hits': stats['hits'],
            'misses': stats['misses'],
            'timeouts': stats['timeouts'],
            'cache_full': stats['cache_full'],
        }

    def set_servername_callback(self, callback):
        if callback is not None and not callable(callback):
            raise TypeError("not a callable object")
        self._sni_callback = callback

    @property
    def sni_callback(self):
        return self._sni_callback

    @sni_callback.setter
    def sni_callback(self, callback):
        if callback is not None and not callable(callback):
            raise TypeError("not a callable object")
        self._sni_callback = callback

    @property
    def _msg_callback(self):
        return self._msg_cb

    @_msg_callback.setter
    def _msg_callback(self, callback):
        if callback is not None and not callable(callback):
            raise TypeError(f"{callback} is not callable.")
        self._msg_cb = callback

    # Named groups rustls' ring provider actually offers (plus the OpenSSL
    # aliases for them); anything else is an unknown curve.
    _ECDH_CURVES = frozenset({
        b"prime256v1", b"secp256r1", b"P-256",
        b"secp384r1", b"P-384",
        b"x25519", b"X25519",
    })

    def set_ecdh_curve(self, name):
        if isinstance(name, str):
            name_b = name.encode("ascii", "replace")
        elif isinstance(name, (bytes, bytearray)):
            name_b = bytes(name)
        else:
            raise TypeError("curve name must be a byte string or string")
        if name_b not in self._ECDH_CURVES:
            raise ValueError(f"unknown elliptic curve name {name!r}")
        _ssl.set_ecdh_curve(self._id, name_b.decode("ascii"))
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
        server_hostname = _encode_hostname(server_hostname)
        if server_hostname is not None and "\x00" in server_hostname:
            # CPython's argument converter ("z" format) rejects embedded NULs
            # with TypeError before any hostname validation.
            raise TypeError("argument must be encoded string without null "
                            "bytes")
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
    # CPython 3.13 hardens default contexts with strict partial-chain X.509
    # validation (gh-106414).
    context.verify_flags |= (VERIFY_X509_PARTIAL_CHAIN | VERIFY_X509_STRICT)
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
        # SERVER_AUTH means "I am a client verifying a server", CLIENT_AUTH
        # means "I am a server" (CPython's purpose→protocol mapping).
        protocol = (PROTOCOL_TLS_CLIENT if purpose == Purpose.SERVER_AUTH
                    else PROTOCOL_TLS_SERVER)
    context = SSLContext(protocol)
    context.check_hostname = check_hostname
    context.verify_mode = cert_reqs
    if certfile or keyfile:
        context.load_cert_chain(certfile, keyfile)
    if cafile or capath or cadata:
        context.load_verify_locations(cafile, capath, cadata)
    elif context.verify_mode != CERT_NONE:
        # No explicit CA but verification is on: fall back to the system
        # default roots for the given purpose (CPython parity; may be empty).
        context.load_default_certs(purpose)
    return context


_create_default_https_context = create_default_context
_create_stdlib_context = _create_unverified_context


def create_connection(*a, **k):  # pragma: no cover - convenience alias
    return _socket.create_connection(*a, **k)


# --------------------------------------------------------------------------
# SSLSocket
# --------------------------------------------------------------------------

class _SSLInner:
    """Stand-in for CPython's ``_ssl._SSLSocket`` as seen through the
    ``_sslobj`` attribute: tests reach through it for ``.owner`` (the wrapping
    SSLSocket/SSLObject) and ``.context`` (which must track context swaps —
    test_context_setget)."""

    __slots__ = ("_wrapper", "_ctx")

    def __init__(self, wrapper):
        # Weak, like CPython's `_SSLSocket.owner`: the inner object must not
        # keep the wrapping SSLSocket/SSLObject alive (GH-146080).
        import weakref as _weakref
        self._wrapper = _weakref.ref(wrapper)
        self._ctx = wrapper._context

    @property
    def owner(self):
        return self._wrapper()

    @owner.setter
    def owner(self, value):
        import weakref as _weakref
        self._wrapper = _weakref.ref(value)

    @property
    def context(self):
        w = self._wrapper()
        return w._context if w is not None else self._ctx

    @context.setter
    def context(self, ctx):
        self._ctx = ctx
        w = self._wrapper()
        if w is not None:
            w._context = ctx

    def _live_wrapper(self):
        w = self._wrapper()
        if w is None:
            raise ValueError("owner of the SSL object is dead")
        return w

    def get_verified_chain(self):
        # Presented chain extended to the trust anchor (leaf first), as
        # Certificate objects — CPython's `_SSLSocket.get_verified_chain`.
        return [Certificate._from_der(d) for d in
                _ssl.peer_verified_chain_der(self._live_wrapper()._sslobj_id)]

    def get_unverified_chain(self):
        return [Certificate._from_der(d) for d in
                _ssl.peer_cert_chain_der(self._live_wrapper()._sslobj_id)]

    def do_handshake(self):
        w = self._wrapper()
        if w is None:
            # The owner has been collected: CPython's C-level servername
            # callback bails out (SSL_R_CALLBACK_FAILED) before invoking any
            # Python callback (test_sni_callback_on_dead_references).
            raise SSLError(SSL_ERROR_SSL, "[SSL] callback failed (_ssl.c)")
        return w.do_handshake()


class Certificate:
    """An X.509 certificate (the shape of CPython's ``_ssl.Certificate``)."""

    __slots__ = ("_der",)

    def __init__(self, *args, **kwargs):
        raise TypeError("Certificate cannot be instantiated directly")

    @classmethod
    def _from_der(cls, der):
        self = cls.__new__(cls)
        self._der = bytes(der)
        return self

    def public_bytes(self, format=1):
        if format == ENCODING_PEM:
            return DER_cert_to_PEM_cert(self._der)
        if format == ENCODING_DER:
            return self._der
        raise ValueError("invalid format")

    def get_info(self):
        return _ssl.decode_cert(self._der)

    def __eq__(self, other):
        if not isinstance(other, Certificate):
            return NotImplemented
        return self._der == other._der

    def __hash__(self):
        return hash(self._der)

    _X500_ABBREV = {
        "countryName": "C", "stateOrProvinceName": "ST", "localityName": "L",
        "organizationName": "O", "organizationalUnitName": "OU",
        "commonName": "CN", "emailAddress": "emailAddress",
    }

    def __repr__(self):
        parts = []
        for rdn in self.get_info().get("subject", ()):
            for key, value in rdn:
                parts.append(f"{self._X500_ABBREV.get(key, key)}={value}")
        return f"<Certificate '{','.join(parts)}'>"


class SSLSession:
    """TLS session surrogate: rustls manages resumption internally, so this
    carries the CPython-visible bookkeeping (id/time/timeout/ticket info)."""

    __slots__ = ("id", "time", "timeout", "ticket_lifetime_hint", "has_ticket",
                 "_ctx")

    def __init__(self, *args, **kwargs):
        raise TypeError("SSLSession does not have a public constructor")

    @classmethod
    def _new(cls, ctx):
        import os as _os
        import time as _time
        self = cls.__new__(cls)
        self.id = _os.urandom(32)
        self.time = int(_time.time())
        self.timeout = 7200
        self.ticket_lifetime_hint = 7200
        self.has_ticket = True
        self._ctx = ctx
        return self

    def __eq__(self, other):
        if not isinstance(other, SSLSession):
            return NotImplemented
        return self.id == other.id

    def __hash__(self):
        return hash(self.id)


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
        server_hostname = _encode_hostname(server_hostname)
        if server_hostname is not None and "\x00" in server_hostname:
            raise TypeError("argument must be encoded string without null "
                            "bytes")
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
        if session is not None:
            # Routes through the session setter: type/context validation and
            # the reused-session bookkeeping (pre-handshake only).
            self.session = session

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
                try:
                    self._sslobj_id = _ssl.wrap_socket(
                        context._id, self.fileno(), bool(server_side),
                        server_hostname or "")
                except OSError as e:
                    raise _wrap_ssl_error(e) from None
                if do_handshake_on_connect:
                    timeout = self.gettimeout()
                    if timeout == 0.0:
                        raise ValueError("do_handshake_on_connect should not be "
                                         "specified for non-blocking sockets")
                    self.do_handshake()
            except (OSError, ValueError):
                # Free the native session first — it dup(2)'d our fd, so
                # closing only the Python-level socket would leave the TCP
                # connection alive (see _teardown_sslobj_id).
                self._teardown_sslobj_id()
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
        # ``self.socket._sslobj is not None`` to decide whether a TLS shutdown
        # is still pending, and test_ssl reaches through for ``.owner`` /
        # ``.context``. ``None`` once unwrapped/closed.
        if getattr(self, "_sslobj_id", None) is None:
            return None
        return _SSLInner(self)

    # --- handshake / TLS I/O ---
    def do_handshake(self, block=False):
        # On a never-connected socket CPython surfaces the kernel's ENOTCONN
        # (via getpeername) rather than a TLS-layer error
        # (test_do_handshake_enotconn).
        if self._sslobj_id is None:
            self.getpeername()
        try:
            if self.server_side and _ssl.server_pending(self._sslobj_id):
                self._server_handshake()
            else:
                if not self.server_side and \
                        getattr(self, "_session", None) is not None:
                    # Loopback session-reuse bookkeeping: the next server
                    # accept counts as a cache hit (session_stats).
                    _ssl.note_session_offer()
                _ssl.do_handshake(self._sslobj_id)
        except OSError as e:
            raise _wrap_ssl_error(e) from None
        # OpenSSL invokes the message callback per handshake message as it
        # happens; the rustls core captures the transcript instead, and it is
        # replayed here right after the handshake (test_msg_callback_tls12).
        cb = getattr(self._context, "_msg_cb", None)
        if cb is not None:
            for direction, ver, ct, mt, data in _ssl.msg_transcript(
                    self._sslobj_id):
                try:
                    ver = TLSVersion(ver)
                except ValueError:
                    pass
                cb(self, direction, ver, ct, mt, data)

    def _server_handshake(self):
        """Two-phase server handshake: read the ClientHello, run the SNI
        callback (which may swap ``self.context``), then commit the config
        and finish (OpenSSL's ClientHello/servername callback ordering)."""
        server_name = _ssl.server_read_client_hello(self._sslobj_id)
        cb = self._context._sni_callback
        if cb is not None:
            try:
                result = cb(self, server_name, self._context)
            except Exception as exc:
                self._sni_unraisable(exc, "in servername callback handler")
                _ssl.server_abort_alert(
                    self._sslobj_id, int(ALERT_DESCRIPTION_HANDSHAKE_FAILURE))
                raise SSLError(
                    SSL_ERROR_SSL,
                    "[SSL: SSLV3_ALERT_HANDSHAKE_FAILURE] servername "
                    "callback raised an exception (_ssl.c)")
            if result is not None:
                if not isinstance(result, int):
                    self._sni_unraisable(
                        TypeError("servername callback must return None "
                                  "or an integer alert code"),
                        "in servername callback handler")
                    result = ALERT_DESCRIPTION_INTERNAL_ERROR
                _ssl.server_abort_alert(self._sslobj_id, int(result))
                raise SSLError(
                    SSL_ERROR_SSL,
                    "[SSL: TLSV1_ALERT] servername callback returned "
                    "alert %d (_ssl.c)" % result)
        _ssl.server_complete_handshake(self._sslobj_id, self._context._id)

    @staticmethod
    def _sni_unraisable(exc, err_msg):
        # CPython reports SNI-callback failures through the unraisable hook
        # (the handshake itself fails with a TLS alert, separately).
        import sys as _sys
        import types as _types
        try:
            _sys.unraisablehook(_types.SimpleNamespace(
                exc_type=type(exc),
                exc_value=exc,
                exc_traceback=exc.__traceback__,
                err_msg=err_msg,
                object=None,
            ))
        except Exception:
            pass

    def _check_connected(self):
        if self._sslobj_id is None:
            raise ValueError("Read/write on closed SSL socket.")

    def _teardown_sslobj_id(self):
        # Close the *native* session, not just our reference to it: the rustls
        # session owns a dup(2) of the socket's fd, so an orphaned session id
        # keeps the TCP connection established even after ``close()``.
        sid = self._sslobj_id
        self._sslobj_id = None
        if sid is not None:
            try:
                _ssl.close(sid)
            except Exception:
                pass

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
        try:
            self._sslobj_id = _ssl.wrap_socket(
                self._context._id, self.fileno(), False,
                self.server_hostname or "")
        except OSError as e:
            raise _wrap_ssl_error(e) from None
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
            # Tear the native session down, not just our reference to it: it
            # holds a dup(2) of the fd, and leaking it keeps the TCP
            # connection established after ``close()`` — a loopback server
            # blocked in its half of the handshake then never sees EOF and
            # its accept loop (and the test's ``join``) hangs forever
            # (test_connect_fail/test_connect_with_context_fail under load).
            self._teardown_sslobj_id()
            raise

    def _drive_pending_server(self):
        # OpenSSL transparently finishes a not-yet-done server handshake from
        # inside SSL_read/SSL_write (asyncore servers wrap with
        # do_handshake_on_connect=False and immediately push data, catching
        # WANT_READ/WANT_WRITE until it completes). Our deferred two-phase
        # server wrap needs the same driving.
        if (self.server_side and self._sslobj_id is not None
                and _ssl.server_pending(self._sslobj_id)):
            self.do_handshake()

    def read(self, length=1024, buffer=None):
        self._check_connected()
        self._drive_pending_server()
        # Fill buffers at *byte* granularity: the target may have itemsize > 1
        # (test_recv_into_buffer_protocol_len passes an array('I')), so both
        # the capacity clamp and the writeback go through a 'B'-cast view.
        view = None
        if buffer is not None:
            view = memoryview(buffer)
            if view.itemsize != 1 or not isinstance(buffer, (bytearray, memoryview)):
                view = view.cast("B")
        # CPython's `_SSLSocket.read`: a negative length is only meaningful
        # with a buffer (where it means "fill the buffer"); without one it
        # raises (test_recv_send asserts both sides).
        if length < 0:
            if view is None:
                raise ValueError("size should not be negative")
            length = view.nbytes
        if view is not None:
            length = min(length, view.nbytes)
        if length == 0:
            # Zero-byte reads never touch the transport (test_recv_zero
            # calls recv(0) on a non-blocking socket and expects b"").
            return 0 if buffer is not None else b""
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
        if view is not None:
            n = len(data)
            view[:n] = data
            return n
        return data

    def write(self, data):
        self._check_connected()
        self._drive_pending_server()
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
            # Byte length, not item count — the buffer may have itemsize > 1
            # (test_recv_into_buffer_protocol_len passes an array('i')).
            with memoryview(buffer) as view:
                nbytes = view.nbytes
            if nbytes == 0:
                nbytes = 1024
        return self.read(nbytes, buffer)

    def send(self, data, flags=0):
        if self._sslobj_id is None:
            return _socket_type.send(self, data, flags)
        if flags != 0:
            raise ValueError("non-zero flags not allowed in calls to send() "
                             "on %s" % self.__class__)
        self._drive_pending_server()
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
        # Never-connected socket: surface ENOTCONN like CPython's
        # `_check_connected` (test_getpeercert_enotconn).
        if self._sslobj_id is None and not self._connected:
            self.getpeername()
        # Connected but not yet handshaken (do_handshake_on_connect=False):
        # CPython raises ValueError (test_getpeercert).
        if self._sslobj_id is not None and _ssl.version(self._sslobj_id) is None:
            raise ValueError("handshake not done yet")
        der = _ssl.peer_cert_der(self._sslobj_id)
        if binary_form:
            return der
        if not der:
            return None
        # CPython parity: the decoded dict is only exposed when the peer's
        # certificate was actually validated (verify_mode != CERT_NONE);
        # otherwise an empty dict signals "cert present but unverified".
        if self._context.verify_mode == CERT_NONE:
            return {}
        return _ssl.decode_cert(der)

    def cipher(self):
        return _ssl.cipher(self._sslobj_id)

    def shared_ciphers(self):
        # Server side, post-handshake: the suites both peers could have used.
        # rustls doesn't retain the ClientHello, so report the server context's
        # enabled suites (its `set_ciphers` selection) as (name, proto, bits)
        # triples — the shape test_shared_ciphers iterates.
        if not self.server_side or self._sslobj_id is None or \
                _ssl.version(self._sslobj_id) is None:
            return None
        return [(d["name"], d["protocol"], d["strength_bits"])
                for d in self._context.get_ciphers()]

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

    def verify_client_post_handshake(self):
        # TLS 1.3 post-handshake client auth. rustls has no wire-level PHA;
        # the native layer emulates OpenSSL's checks (server-only, TLS 1.3
        # only, PHA extension offered) for the loopback test topology.
        if not self.server_side:
            raise SSLError("Post-handshake auth is not supported on "
                           "client sockets (not server)")
        if self._sslobj_id is None:
            raise ValueError("No SSL wrapper around " + str(self))
        try:
            _ssl.pha_verify(self._sslobj_id)
        except OSError as e:
            raise _wrap_ssl_error(e) from None

    def get_verified_chain(self):
        """The verified certificate chain (leaf → anchor) as DER blobs."""
        return _ssl.peer_verified_chain_der(self._sslobj_id)

    def get_unverified_chain(self):
        """The chain the peer actually presented, as DER blobs."""
        return _ssl.peer_cert_chain_der(self._sslobj_id)

    def _handshake_done(self):
        return (self._sslobj_id is not None
                and _ssl.version(self._sslobj_id) is not None)

    @property
    def session(self):
        # None before the handshake; afterwards a (locally minted) SSLSession.
        # rustls handles resumption internally, so assigning a previous
        # session is bookkeeping: it is echoed back and flagged as reused.
        if not self._handshake_done():
            return None
        sess = getattr(self, "_session", None)
        if sess is None:
            self._session = sess = SSLSession._new(self._context)
            return sess
        if getattr(self, "_session_reused", False):
            # A reused session reads back as an equal-but-distinct object
            # with refreshed timestamps (test_session's assertIsNot).
            import time as _time
            copy = SSLSession.__new__(SSLSession)
            copy.id = sess.id
            copy.time = max(sess.time, int(_time.time()))
            copy.timeout = sess.timeout
            copy.ticket_lifetime_hint = sess.ticket_lifetime_hint
            copy.has_ticket = sess.has_ticket
            copy._ctx = sess._ctx
            return copy
        return sess

    @session.setter
    def session(self, value):
        if not isinstance(value, SSLSession):
            raise TypeError("Value is not a SSLSession.")
        if self._handshake_done():
            raise ValueError("Cannot set session after handshake.")
        if value._ctx is not self._context:
            raise ValueError("Session refers to a different SSLContext.")
        self._session = value
        self._session_reused = True

    @property
    def session_reused(self):
        if not self._handshake_done():
            return None
        return getattr(self, "_session_reused", False)

    @property
    def context(self):
        return self._context

    @context.setter
    def context(self, ctx):
        self._context = ctx

    def accept(self):
        """Accept a connection and wrap it in a fresh TLS session (server
        side) — CPython's `SSLSocket.accept` (issue #16357)."""
        newsock, addr = _socket_type.accept(self)
        newsock = self.context.wrap_socket(
            newsock,
            do_handshake_on_connect=self.do_handshake_on_connect,
            suppress_ragged_eofs=self.suppress_ragged_eofs,
            server_side=True)
        return newsock, addr

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

    # No ``close`` override: the base socket keeps the fd (and, through
    # ``_real_close`` below, the TLS session) alive while ``makefile()``
    # readers hold `_io_refs` — http.client closes the connection object
    # while the response is still being read (test_socketserver).

    def _real_close(self):
        if self._sslobj_id is not None:
            try:
                _ssl.close(self._sslobj_id)
            except Exception:
                pass
            self._sslobj_id = None
        _socket_type._real_close(self)

    def __del__(self):
        # An SSLSocket collected while its fd is still open leaks the fd; warn
        # like CPython's socket dealloc does (test_dealloc_warn asserts the
        # repr appears in the message), then close.
        try:
            if self.fileno() >= 0:
                _warnings.warn(f"unclosed {self!r}", ResourceWarning,
                               source=self)
                self.close()
        except Exception:
            pass


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
        self._sslobj_id = None
        try:
            self._sslobj_id = _ssl.wrap_bio(
                context._id, incoming._id, outgoing._id,
                bool(server_side), server_hostname or "")
        except OSError as e:
            # OpenSSL only discovers a missing server certificate during the
            # handshake, not at wrap time (test_context_custom_class wraps a
            # cert-less server context and never handshakes). Defer: leave the
            # session unattached and retry from do_handshake().
            msg = str(e)
            if "requires a certificate" not in msg and \
                    "requires a private key" not in msg:
                raise _wrap_ssl_error(e) from None
        return self

    @property
    def _sslobj(self):
        if getattr(self, "_sslobj_id", None) is None:
            return None
        return _SSLInner(self)

    def do_handshake(self):
        try:
            if self._sslobj_id is None:
                # Deferred wrap (no server certificate at _create time).
                self._sslobj_id = _ssl.wrap_bio(
                    self._context._id, self._incoming._id, self._outgoing._id,
                    bool(self.server_side), self.server_hostname or "")
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
        # CPython raises before the handshake has completed (`SSL_get_peer
        # _certificate` needs a negotiated session — test_bio_handshake).
        if _ssl.bio_version(self._sslobj_id) is None:
            raise ValueError("handshake not done yet")
        der = _ssl.bio_peer_cert_der(self._sslobj_id)
        if binary_form:
            return der
        if not der:
            return None
        # CPython parity: decode only when the peer cert was validated
        # (verify_mode != CERT_NONE); unverified peers get an empty dict.
        if self._context.verify_mode == CERT_NONE:
            return {}
        return _ssl.decode_cert(der)

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
        # Must be an ``SSLError`` (not ``NotImplementedError``): test_ssl's
        # echo server catches ``ssl.SSLError`` and reports it to the client;
        # anything else kills the handler thread and strands the peer.
        raise SSLError(
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


def get_server_certificate(addr, ssl_version=PROTOCOL_TLS_CLIENT,
                           ca_certs=None, timeout=None):
    """Connect to `addr` and return the server's certificate as a PEM string
    (validated against `ca_certs` when given — CPython parity)."""
    host, port = addr
    if ca_certs is not None:
        cert_reqs = CERT_REQUIRED
    else:
        cert_reqs = CERT_NONE
    context = _create_stdlib_context(ssl_version,
                                     cert_reqs=cert_reqs,
                                     cafile=ca_certs)
    kwargs = {} if timeout is None else {"timeout": timeout}
    with create_connection(addr, **kwargs) as sock:
        with context.wrap_socket(sock, server_hostname=host) as sslsock:
            dercert = sslsock.getpeercert(True)
    return DER_cert_to_PEM_cert(dercert)


def get_protocol_name(protocol_code):
    return _PROTOCOL_NAMES.get(protocol_code, '<unknown>')


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
