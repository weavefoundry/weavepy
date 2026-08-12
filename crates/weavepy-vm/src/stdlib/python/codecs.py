"""User-visible ``codecs`` module (RFC 0019).

The heavy lifting lives in `_codecs`. This module hosts the
public surface — `encode`, `decode`, `lookup`, `register`,
`register_error`, the `IncrementalEncoder`/`IncrementalDecoder`
shells, and the `BOM_*` constants.
"""

import _codecs
import sys

BOM = _codecs.BOM
BOM_UTF8 = _codecs.BOM_UTF8
BOM_UTF16 = _codecs.BOM_UTF16
BOM_UTF16_LE = _codecs.BOM_UTF16_LE
BOM_UTF16_BE = _codecs.BOM_UTF16_BE
BOM_UTF32 = _codecs.BOM_UTF32
BOM_UTF32_LE = _codecs.BOM_UTF32_LE
BOM_UTF32_BE = _codecs.BOM_UTF32_BE
BOM_LE = BOM_UTF16_LE
BOM_BE = BOM_UTF16_BE

# Per-codec `_codecs` entry points, re-exported like CPython's
# `from _codecs import *` (test_codecs calls e.g. `codecs.utf_7_decode`
# / `codecs.utf_16_ex_decode` directly).
utf_7_encode = _codecs.utf_7_encode
utf_7_decode = _codecs.utf_7_decode
utf_8_encode = _codecs.utf_8_encode
utf_8_decode = _codecs.utf_8_decode
utf_16_encode = _codecs.utf_16_encode
utf_16_decode = _codecs.utf_16_decode
utf_16_le_encode = _codecs.utf_16_le_encode
utf_16_le_decode = _codecs.utf_16_le_decode
utf_16_be_encode = _codecs.utf_16_be_encode
utf_16_be_decode = _codecs.utf_16_be_decode
utf_16_ex_decode = _codecs.utf_16_ex_decode
utf_32_encode = _codecs.utf_32_encode
utf_32_decode = _codecs.utf_32_decode
utf_32_le_encode = _codecs.utf_32_le_encode
utf_32_le_decode = _codecs.utf_32_le_decode
utf_32_be_encode = _codecs.utf_32_be_encode
utf_32_be_decode = _codecs.utf_32_be_decode
utf_32_ex_decode = _codecs.utf_32_ex_decode
ascii_encode = _codecs.ascii_encode
ascii_decode = _codecs.ascii_decode
latin_1_encode = _codecs.latin_1_encode
latin_1_decode = _codecs.latin_1_decode
raw_unicode_escape_encode = _codecs.raw_unicode_escape_encode
raw_unicode_escape_decode = _codecs.raw_unicode_escape_decode
unicode_escape_encode = _codecs.unicode_escape_encode
unicode_escape_decode = _codecs.unicode_escape_decode
readbuffer_encode = _codecs.readbuffer_encode

# Windows-only code-page entry points (RFC 0063). CPython's
# `from _codecs import *` picks these up only on win32 builds, where
# `encodings/mbcs.py` and `encodings/oem.py` import them from here.
try:
    mbcs_encode = _codecs.mbcs_encode
    mbcs_decode = _codecs.mbcs_decode
    oem_encode = _codecs.oem_encode
    oem_decode = _codecs.oem_decode
    code_page_encode = _codecs.code_page_encode
    code_page_decode = _codecs.code_page_decode
except AttributeError:
    pass


_USER_CODECS = {}
_ERROR_HANDLERS = {}


class CodecInfo:
    """Information returned by `codecs.lookup`. Behaves like a
    4-tuple of `(encode, decode, streamreader, streamwriter)` for
    indexed access, plus the named-attribute style modern code
    uses."""

    def __init__(self, encode, decode, streamreader=None, streamwriter=None,
                 incrementalencoder=None, incrementaldecoder=None, name=None,
                 *, _is_text_encoding=None):
        self.encode = encode
        self.decode = decode
        self.streamreader = streamreader
        self.streamwriter = streamwriter
        self.incrementalencoder = incrementalencoder
        self.incrementaldecoder = incrementaldecoder
        self.name = name
        # CPython marks binary transforms (hex/base64/zlib/…) with
        # `_is_text_encoding = False` so `io.TextIOWrapper` and friends can
        # reject them; text codecs default to True.
        if _is_text_encoding is not None:
            self._is_text_encoding = _is_text_encoding

    _is_text_encoding = True

    def __repr__(self):
        return "<%s.%s object for encoding %s at %#x>" % (
            self.__class__.__module__, self.__class__.__qualname__,
            self.name, id(self))

    def __getitem__(self, idx):
        return (self.encode, self.decode, self.streamreader, self.streamwriter)[idx]

    def __iter__(self):
        return iter((self.encode, self.decode, self.streamreader, self.streamwriter))

    def __len__(self):
        return 4

    # CPython's CodecInfo is a tuple subclass: two lookups of the same codec
    # (or a pickle round-trip) compare equal by their 4-tuple.
    def __eq__(self, other):
        if isinstance(other, CodecInfo):
            other = tuple(other)
        if isinstance(other, tuple):
            return tuple(self) == other
        return NotImplemented

    def __hash__(self):
        return hash(tuple(self))


def _make_codec(encoding, encode_fn, decode_fn, _is_text_encoding=True,
                partial_decode_fn=None):
    # Build the CPython-shaped codec surface on top of a stateless
    # (encode, decode) pair — real `StreamReader`/`StreamWriter` subclasses
    # (CPython's `encodings.*` modules do exactly this) plus incremental
    # factories. When `partial_decode_fn` is given it speaks the stateful
    # `decode(data, errors, final)` protocol (the UTF `_codecs` natives) and
    # drives the incremental decoder / stream reader; otherwise the
    # stateless pair is wrapped generically.
    class _Reader(StreamReader):
        # Binary transform codecs (base64/zlib/…) decode to bytes; their
        # stream readers buffer bytes (CPython `charbuffertype = bytes`).
        if not _is_text_encoding:
            charbuffertype = bytes

        def decode(self, input, errors="strict"):
            if partial_decode_fn is not None:
                # Explicit final=False: the UTF natives default to it but
                # the escape codecs default to final=True.
                return partial_decode_fn(input, errors, False)
            return decode_fn(input, errors)

    class _Writer(StreamWriter):
        def encode(self, input, errors="strict"):
            return encode_fn(input, errors)

    def _mk_incremental_encoder(errors="strict"):
        return _FuncIncrementalEncoder(encode_fn, errors)

    def _mk_incremental_decoder(errors="strict"):
        if partial_decode_fn is not None:
            return _StatefulFuncIncrementalDecoder(partial_decode_fn, errors)
        return _FuncIncrementalDecoder(decode_fn, errors)

    return CodecInfo(
        encode=encode_fn,
        decode=decode_fn,
        streamreader=_Reader,
        streamwriter=_Writer,
        incrementalencoder=_mk_incremental_encoder,
        incrementaldecoder=_mk_incremental_decoder,
        name=encoding,
        _is_text_encoding=_is_text_encoding,
    )


_BUILTIN_NAMES = {
    "utf-8": ("utf_8_encode", "utf_8_decode"),
    "utf_8": ("utf_8_encode", "utf_8_decode"),
    "utf8": ("utf_8_encode", "utf_8_decode"),
    "utf-7": ("utf_7_encode", "utf_7_decode"),
    "utf_7": ("utf_7_encode", "utf_7_decode"),
    "utf7": ("utf_7_encode", "utf_7_decode"),
    "u7": ("utf_7_encode", "utf_7_decode"),
    "unicode-1-1-utf-7": ("utf_7_encode", "utf_7_decode"),
    "utf-16": ("utf_16_encode", "utf_16_decode"),
    "utf_16": ("utf_16_encode", "utf_16_decode"),
    "utf-16-le": ("utf_16_le_encode", "utf_16_le_decode"),
    "utf_16_le": ("utf_16_le_encode", "utf_16_le_decode"),
    "utf-16-be": ("utf_16_be_encode", "utf_16_be_decode"),
    "utf_16_be": ("utf_16_be_encode", "utf_16_be_decode"),
    "utf-32": ("utf_32_encode", "utf_32_decode"),
    "utf_32": ("utf_32_encode", "utf_32_decode"),
    "utf-32-le": ("utf_32_le_encode", "utf_32_le_decode"),
    "utf_32_le": ("utf_32_le_encode", "utf_32_le_decode"),
    "utf-32-be": ("utf_32_be_encode", "utf_32_be_decode"),
    "utf_32_be": ("utf_32_be_encode", "utf_32_be_decode"),
    "ascii": ("ascii_encode", "ascii_decode"),
    "us-ascii": ("ascii_encode", "ascii_decode"),
    "latin-1": ("latin_1_encode", "latin_1_decode"),
    "latin_1": ("latin_1_encode", "latin_1_decode"),
    "latin1": ("latin_1_encode", "latin_1_decode"),
    "iso-8859-1": ("latin_1_encode", "latin_1_decode"),
    "iso8859-1": ("latin_1_encode", "latin_1_decode"),
    "cp1252": ("cp1252_encode", "cp1252_decode"),
    "windows-1252": ("cp1252_encode", "cp1252_decode"),
    "raw_unicode_escape": ("raw_unicode_escape_encode", "raw_unicode_escape_decode"),
    "unicode_escape": ("unicode_escape_encode", "unicode_escape_decode"),
}

# `_codecs` decoders that speak the stateful `(input, errors, final)`
# protocol: their stateless `CodecInfo.decode` must pass `final=True`
# (CPython's `encodings/utf_8.py` etc. do exactly this) while the raw
# function drives the incremental decoder / stream reader.
_STATEFUL_DECODE_FNS = {
    "utf_8_decode",
    "utf_7_decode",
    "utf_16_decode",
    "utf_16_le_decode",
    "utf_16_be_decode",
    "utf_32_decode",
    "utf_32_le_decode",
    "utf_32_be_decode",
    "unicode_escape_decode",
    "raw_unicode_escape_decode",
}


# CPython's `CodecInfo.name` for each canonical `encodings.<module>` whose
# display name differs from the module key (each module's `getregentry()`
# hard-codes it). Everything absent maps to itself (`cp1251` → `cp1251`,
# `shift_jis` → `shift_jis`, …). Generated against CPython 3.13.
_DISPLAY_NAMES = {
    "base64_codec": "base64",
    "bz2_codec": "bz2",
    "hex_codec": "hex",
    "hp_roman8": "hp-roman8",
    "iso8859_1": "iso8859-1",
    "iso8859_10": "iso8859-10",
    "iso8859_11": "iso8859-11",
    "iso8859_13": "iso8859-13",
    "iso8859_14": "iso8859-14",
    "iso8859_15": "iso8859-15",
    "iso8859_16": "iso8859-16",
    "iso8859_2": "iso8859-2",
    "iso8859_3": "iso8859-3",
    "iso8859_4": "iso8859-4",
    "iso8859_5": "iso8859-5",
    "iso8859_6": "iso8859-6",
    "iso8859_7": "iso8859-7",
    "iso8859_8": "iso8859-8",
    "iso8859_9": "iso8859-9",
    "koi8_r": "koi8-r",
    "koi8_t": "koi8-t",
    "koi8_u": "koi8-u",
    "latin_1": "iso8859-1",
    "mac_arabic": "mac-arabic",
    "mac_croatian": "mac-croatian",
    "mac_cyrillic": "mac-cyrillic",
    "mac_farsi": "mac-farsi",
    "mac_greek": "mac-greek",
    "mac_iceland": "mac-iceland",
    "mac_latin2": "mac-latin2",
    "mac_roman": "mac-roman",
    "mac_romanian": "mac-romanian",
    "mac_turkish": "mac-turkish",
    "quopri_codec": "quopri",
    "raw_unicode_escape": "raw-unicode-escape",
    "rot_13": "rot-13",
    "tis_620": "tis-620",
    "unicode_escape": "unicode-escape",
    "utf_16": "utf-16",
    "utf_16_be": "utf-16-be",
    "utf_16_le": "utf-16-le",
    "utf_32": "utf-32",
    "utf_32_be": "utf-32-be",
    "utf_32_le": "utf-32-le",
    "utf_7": "utf-7",
    "utf_8": "utf-8",
    "utf_8_sig": "utf-8-sig",
    "uu_codec": "uu",
    "zlib_codec": "zlib",
}


def _normalise(name):
    # CPython `normalizestring` (codecs.c): the `encodings.normalize_encoding`
    # collapse (runs of punctuation → one underscore, non-ASCII dropped,
    # `.` kept) plus lower-casing — `'AAA---8'` and `'aaa\xe9-8'` both
    # normalize to `'aaa_8'`.
    return _cpy_normalize_encoding(name).lower()


# CPython `encodings.normalize_encoding` (vendored logic): collapse runs of
# punctuation to a single underscore, keep alphanumerics and `.`, drop
# non-ASCII. `'UTF-16LE'` → `'UTF_16LE'`, `'latin 1'` → `'latin_1'`.
def _cpy_normalize_encoding(encoding):
    if isinstance(encoding, bytes):
        encoding = str(encoding, "ascii")
    chars = []
    punct = False
    for c in encoding:
        if c.isalnum() or c == ".":
            if punct and chars:
                chars.append("_")
            if c.isascii():
                chars.append(c)
            punct = False
        else:
            punct = True
    return "".join(chars)


_ENC_ALIASES = None


def _alias_resolve(encoding):
    """Resolve an encoding-name alias through CPython's `encodings.aliases`
    registry (the first step of `encodings.search_function`). Returns the
    canonical name or `None` when the name is not an alias."""
    global _ENC_ALIASES
    if _ENC_ALIASES is None:
        try:
            from encodings.aliases import aliases as _ENC_ALIASES
        except ImportError:  # pragma: no cover — frozen module always present
            _ENC_ALIASES = {}
    norm = _cpy_normalize_encoding(encoding)
    return _ENC_ALIASES.get(norm) or _ENC_ALIASES.get(norm.replace(".", "_"))


# ---------- charmap codec primitives ----------
#
# CPython exposes `charmap_encode`/`charmap_decode`/`charmap_build` (and the
# `make_identity_dict`/`make_encoding_map` helpers) from the C `_codecs`
# module, pulled into `codecs` via `from _codecs import *`. The frozen
# single-byte `encodings.*` codepage modules (cp037, cp737, …) are generated
# by CPython's `gencodec.py` and map bytes<->Unicode through a 256-entry table
# using exactly these. WeavePy serves the common encodings natively, so these
# faithful pure-Python equivalents only drive the on-demand frozen codepages.


def escape_decode(data, errors="strict"):
    """Decode Python string-literal escapes in a bytes object.

    CPython exposes this from `_codecs` (re-exported by `codecs` via its
    star-import); `pickle` uses it to load protocol-0 STRING opcodes
    (`test_datetime.test_compat_unpickle` round-trips Python-2 pickles
    through it). Returns `(decoded_bytes, consumed_length)`.
    """
    if isinstance(data, str):
        data = data.encode("latin-1")
    data = bytes(data)
    simple = {
        0x5C: 0x5C,  # backslash
        0x27: 0x27,  # '
        0x22: 0x22,  # "
        0x61: 0x07,  # \a
        0x62: 0x08,  # \b
        0x66: 0x0C,  # \f
        0x6E: 0x0A,  # \n
        0x72: 0x0D,  # \r
        0x74: 0x09,  # \t
        0x76: 0x0B,  # \v
    }
    out = bytearray()
    i = 0
    n = len(data)
    first_invalid = None  # deferred DeprecationWarning message
    while i < n:
        c = data[i]
        if c != 0x5C:
            out.append(c)
            i += 1
            continue
        i += 1
        if i >= n:
            raise ValueError("Trailing \\ in string")
        c = data[i]
        i += 1
        if c == 0x0A:
            pass  # line continuation: swallowed
        elif c in simple:
            out.append(simple[c])
        elif 0x30 <= c <= 0x37:
            # Octal escape: up to three digits.
            v = c - 0x30
            for _ in range(2):
                if i < n and 0x30 <= data[i] <= 0x37:
                    v = (v << 3) + (data[i] - 0x30)
                    i += 1
                else:
                    break
            if v > 0o377 and first_invalid is None:
                first_invalid = "invalid octal escape sequence '\\%o'" % v
            out.append(v & 0xFF)
        elif c == 0x78:  # \xHH
            hexdig = b"0123456789abcdefABCDEF"
            if i + 2 <= n and data[i] in hexdig and data[i + 1] in hexdig:
                out.append(int(data[i:i + 2], 16))
                i += 2
            else:
                if errors == "strict":
                    raise ValueError(
                        "invalid \\x escape at position %d" % (i - 2))
                elif errors == "replace":
                    out.append(0x3F)  # '?'
                elif errors != "ignore":
                    raise ValueError(
                        "decoding error; unknown error handling code: %s"
                        % errors)
                # CPython skips the (single) hexdigit of a truncated escape.
                if i < n and data[i] in hexdig:
                    i += 1
        else:
            # Unknown escape: kept verbatim (backslash included), with a
            # DeprecationWarning for the first one (CPython
            # `_PyBytes_DecodeEscape2`).
            if first_invalid is None:
                first_invalid = "invalid escape sequence '\\%s'" % chr(c)
            out.append(0x5C)
            out.append(c)
    if first_invalid is not None:
        import warnings
        warnings.warn(first_invalid, DeprecationWarning, stacklevel=2)
    return bytes(out), n


def escape_encode(data, errors="strict"):
    """Inverse of `escape_decode`: bytes → Python-literal escaped bytes."""
    # CPython's `escape_encode` takes exactly `bytes` (`O!` converter):
    # even `bytearray` is a TypeError.
    if type(data) is not bytes:
        raise TypeError(
            "escape_encode() argument 'data' must be bytes, not %s"
            % type(data).__name__)
    out = bytearray()
    for c in bytes(data):
        if c in (0x27, 0x5C):  # ' and backslash
            out.append(0x5C)
            out.append(c)
        elif c == 0x09:
            out += b"\\t"
        elif c == 0x0A:
            out += b"\\n"
        elif c == 0x0D:
            out += b"\\r"
        elif c < 0x20 or c >= 0x7F:
            out += ("\\x%02x" % c).encode("ascii")
        else:
            out.append(c)
    return bytes(out), len(data)


def make_identity_dict(rng):
    """`{i: i for i in rng}` (CPython ``codecs.make_identity_dict``)."""
    return {i: i for i in rng}


def make_encoding_map(decoding_map):
    """Invert a decoding map to an encoding map; a target reachable from more
    than one source maps to ``None`` so the encoder rejects it as undefined
    (CPython ``codecs.make_encoding_map``)."""
    m = {}
    for k, v in decoding_map.items():
        if v not in m:
            m[v] = k
        else:
            m[v] = None
    return m


def charmap_build(decoding_table):
    """Build the ``{unicode-ordinal: byte}`` encode map from a 256-char decode
    table (CPython's C ``PyUnicode_BuildEncodingMap``). ``'\\ufffe'`` marks an
    undefined byte in the decode table and is skipped so it never becomes
    encodable."""
    return {ord(c): i for (i, c) in enumerate(decoding_table) if c != "\ufffe"}


def _charmap_decode_lookup(mapping, b):
    """CPython ``charmapdecode_lookup``: map byte *b* through *mapping*.
    Returns the replacement ``str``, or ``None`` for an undefined position
    (missing key / ``None`` / ``'\\ufffe'``). Wrong-typed values raise
    TypeError; other lookup exceptions propagate."""
    if isinstance(mapping, str):
        if b >= len(mapping):
            return None
        w = mapping[b]
        return None if w == "\ufffe" else w
    try:
        w = mapping[b]
    except LookupError:
        return None
    if w is None:
        return None
    if isinstance(w, int):
        if not 0 <= w < 0x110000:
            raise TypeError("character mapping must be in range(0x110000)")
        return chr(w) if w != 0xFFFE else None
    if isinstance(w, str):
        return None if w == "\ufffe" else w
    raise TypeError("character mapping must return integer, None or str")


def charmap_decode(input, errors="strict", mapping=None):
    """Decode *input* bytes via *mapping* (a 256-char decode ``str`` or an
    ``{byte: char-or-ordinal}`` dict); ``None`` decodes as latin-1. Mirrors the
    C ``_codecs.charmap_decode`` incl. the ``'\\ufffe'`` / missing-key
    "character maps to <undefined>" error, routed through the error handler."""
    # CPython's `y*` converter: only buffer-protocol input (an int would
    # otherwise become `bytes(42)` — 42 zero bytes).
    if not isinstance(input, (bytes, bytearray, memoryview)):
        raise TypeError(
            "charmap_decode() argument 'data' must be a bytes-like object, "
            "not %s" % type(input).__name__)
    data = bytes(input)
    if mapping is None:
        return (data.decode("latin-1", errors), len(data))
    out = []
    i = 0
    n = len(data)
    handler = None
    while i < n:
        ch = _charmap_decode_lookup(mapping, data[i])
        if ch is not None:
            out.append(ch)
            i += 1
            continue
        exc = UnicodeDecodeError(
            "charmap", data, i, i + 1, "character maps to <undefined>"
        )
        if handler is None:
            handler = lookup_error(errors)
        res = handler(exc)
        if (
            not isinstance(res, tuple)
            or len(res) != 2
            or not isinstance(res[0], str)
            or not isinstance(res[1], int)
        ):
            raise TypeError("decoding error handler must return (str, int) tuple")
        repl, newpos = res
        if newpos < 0:
            newpos += n
        if not 0 <= newpos <= n:
            raise IndexError(
                "position %d from error handler out of bounds" % newpos
            )
        out.append(repl)
        i = newpos
    return ("".join(out), len(data))


def _charmap_encode_lookup(mapping, cp):
    """CPython ``charmapencode_lookup``: map ordinal *cp* through *mapping*.
    Returns the output ``bytes``, or ``None`` for an undefined position
    (missing key / ``None``). Wrong-typed values raise TypeError; other
    lookup exceptions propagate."""
    try:
        w = mapping[cp]
    except LookupError:
        return None
    if w is None:
        return None
    if isinstance(w, int):
        if not 0 <= w < 256:
            raise TypeError("character mapping must be in range(256)")
        return bytes([w])
    if isinstance(w, bytes):
        return w
    raise TypeError("character mapping must return integer, bytes or None, not %r" % (w,))


def charmap_encode(input, errors="strict", mapping=None):
    """Encode *input* str via *mapping* (an ``{unicode-ordinal: byte-or-bytes}``
    dict as built by :func:`charmap_build`); ``None`` encodes as latin-1.
    Mirrors the C ``_codecs.charmap_encode``: undefined positions run the
    error handler and ``str`` replacements are re-encoded through the same
    charmap ("character maps to <undefined>" if that fails too)."""
    if not isinstance(input, str):
        raise TypeError(
            "charmap_encode() argument 'str' must be str, not %s"
            % type(input).__name__)
    if mapping is None:
        return (input.encode("latin-1", errors), len(input))
    out = bytearray()
    i = 0
    n = len(input)
    handler = None
    while i < n:
        b = _charmap_encode_lookup(mapping, ord(input[i]))
        if b is not None:
            out += b
            i += 1
            continue
        # Collect the full run of unencodable characters (CPython batches
        # them into one handler call).
        start = i
        end = i + 1
        while end < n and _charmap_encode_lookup(mapping, ord(input[end])) is None:
            end += 1
        exc = UnicodeEncodeError(
            "charmap", input, start, end, "character maps to <undefined>"
        )
        if handler is None:
            handler = lookup_error(errors)
        res = handler(exc)
        if (
            not isinstance(res, tuple)
            or len(res) != 2
            or not isinstance(res[0], (str, bytes))
            or not isinstance(res[1], int)
        ):
            raise TypeError(
                "encoding error handler must return (str/bytes, int) tuple"
            )
        repl, newpos = res
        if newpos < 0:
            newpos += n
        if not 0 <= newpos <= n:
            raise IndexError(
                "position %d from error handler out of bounds" % newpos
            )
        if isinstance(repl, str):
            # Re-encode the replacement through the charmap itself.
            for rc in repl:
                rb = _charmap_encode_lookup(mapping, ord(rc))
                if rb is None:
                    raise exc
                out += rb
        else:
            out += repl
        i = newpos
    return (bytes(out), len(input))


def _rot13_encode(s, errors="strict"):
    out = []
    for ch in s:
        c = ord(ch)
        if ord("a") <= c <= ord("z"):
            out.append(chr((c - ord("a") + 13) % 26 + ord("a")))
        elif ord("A") <= c <= ord("Z"):
            out.append(chr((c - ord("A") + 13) % 26 + ord("A")))
        else:
            out.append(ch)
    return "".join(out), len(s)


def _rot13_decode(b, errors="strict"):
    return _rot13_encode(b, errors)


def _hex_encode(s, errors="strict"):
    if isinstance(s, str):
        s = s.encode("ascii")
    return "".join(f"{x:02x}" for x in s).encode("ascii"), len(s)


def _hex_decode(b, errors="strict"):
    if not isinstance(b, str):
        b = bytes(b).decode("ascii")
    return bytes.fromhex(b), len(b)


# CPython's `encodings/base64_codec.py` — a bytes→bytes "transform" codec.
def _base64_encode(input, errors="strict"):
    assert errors == "strict"
    import base64 as _b64

    return (_b64.encodebytes(bytes(input)), len(input))


def _base64_decode(input, errors="strict"):
    assert errors == "strict"
    import base64 as _b64

    return (_b64.decodebytes(bytes(input)), len(input))


# CPython's `encodings/quopri_codec.py` (implemented over `binascii` since the
# pure-Python `quopri` module isn't vendored). `quopri.encode(..., quotetabs=1)`
# is equivalent to `binascii.b2a_qp(quotetabs=True)`.
def _quopri_encode(input, errors="strict"):
    assert errors == "strict"
    import binascii

    return (binascii.b2a_qp(bytes(input), quotetabs=True), len(input))


def _quopri_decode(input, errors="strict"):
    assert errors == "strict"
    import binascii

    return (binascii.a2b_qp(bytes(input)), len(input))


# CPython's `encodings/zlib_codec.py`.
def _zlib_encode(input, errors="strict"):
    assert errors == "strict"
    import zlib

    return (zlib.compress(bytes(input)), len(input))


def _zlib_decode(input, errors="strict"):
    assert errors == "strict"
    import zlib

    return (zlib.decompress(bytes(input)), len(input))


# CPython's `encodings/bz2_codec.py`.
def _bz2_encode(input, errors="strict"):
    assert errors == "strict"
    import bz2

    return (bz2.compress(bytes(input)), len(input))


def _bz2_decode(input, errors="strict"):
    assert errors == "strict"
    import bz2

    return (bz2.decompress(bytes(input)), len(input))


# CPython's `encodings/uu_codec.py`.
def _uu_encode(input, errors="strict", filename="<data>", mode=0o666):
    assert errors == "strict"
    import binascii

    data = bytes(input)
    filename = filename.replace("\n", "\\n").replace("\r", "\\r")
    out = [("begin %o %s\n" % (mode & 0o777, filename)).encode("ascii")]
    for i in range(0, len(data), 45):
        out.append(binascii.b2a_uu(data[i : i + 45]))
    out.append(b" \nend\n")
    return (b"".join(out), len(input))


def _uu_decode(input, errors="strict"):
    assert errors == "strict"
    import binascii

    data = bytes(input)
    lines = data.splitlines(keepends=True)
    pos = 0
    while True:
        if pos >= len(lines):
            raise ValueError('Missing "begin" line in input data')
        s = lines[pos]
        pos += 1
        if s[:5] == b"begin":
            break
    out = []
    while pos < len(lines):
        s = lines[pos]
        pos += 1
        if not s or s == b"end\n":
            break
        try:
            chunk = binascii.a2b_uu(s)
        except binascii.Error:
            # Workaround for broken uuencoders by /Fredrik Lundh.
            nbytes = (((s[0] - 32) & 63) * 4 + 5) // 3
            chunk = binascii.a2b_uu(s[:nbytes])
        out.append(chunk)
    return (b"".join(out), len(input))


_PURE_CODECS = {
    "rot_13": (_rot13_encode, _rot13_decode),
    "rot13": (_rot13_encode, _rot13_decode),
    "hex": (_hex_encode, _hex_decode),
    "hex_codec": (_hex_encode, _hex_decode),
    "base64": (_base64_encode, _base64_decode),
    "base64_codec": (_base64_encode, _base64_decode),
    "base_64": (_base64_encode, _base64_decode),
    "quopri": (_quopri_encode, _quopri_decode),
    "quopri_codec": (_quopri_encode, _quopri_decode),
    "quotedprintable": (_quopri_encode, _quopri_decode),
    "quoted_printable": (_quopri_encode, _quopri_decode),
    "zlib": (_zlib_encode, _zlib_decode),
    "zlib_codec": (_zlib_encode, _zlib_decode),
    "zip": (_zlib_encode, _zlib_decode),
    "bz2": (_bz2_encode, _bz2_decode),
    "bz2_codec": (_bz2_encode, _bz2_decode),
    "uu": (_uu_encode, _uu_decode),
    "uu_codec": (_uu_encode, _uu_decode),
}


def _utf_8_sig_encode(input, errors="strict"):
    return (BOM_UTF8 + _codecs.utf_8_encode(input, errors)[0], len(input))


def _utf_8_sig_decode(input, errors="strict"):
    input = bytes(input)
    prefix = 0
    if input[:3] == BOM_UTF8:
        input = input[3:]
        prefix = 3
    (output, consumed) = _codecs.utf_8_decode(input, errors)
    return (output, consumed + prefix)


# ---------- stateless codec base class ----------
#
# CPython's `codecs.Codec` is the stateless encode/decode base that the pure-
# Python `encodings.*` codecs (e.g. `idna`, `punycode`) subclass. WeavePy serves
# most codecs natively, but those two are frozen Python modules that do
# `class Codec(codecs.Codec)` / `class StreamWriter(Codec, codecs.StreamWriter)`,
# so the name must exist here for class-definition (import) time to succeed.


class Codec:
    """Stateless encoder/decoder base (CPython ``codecs.Codec``)."""

    def encode(self, input, errors="strict"):
        raise NotImplementedError

    def decode(self, input, errors="strict"):
        raise NotImplementedError


# ---------- incremental codec base classes ----------
#
# These base classes must precede the concrete `_UTF8Sig*` /
# `_Func*` subclasses (and any other module-level `class X(Incremental…)`)
# so the names resolve at class-definition (import) time.


class IncrementalEncoder:
    def __init__(self, errors="strict"):
        self.errors = errors

    def encode(self, input, final=False):
        raise NotImplementedError

    def reset(self):
        pass

    def getstate(self):
        return 0

    def setstate(self, state):
        pass


class IncrementalDecoder:
    def __init__(self, errors="strict"):
        self.errors = errors

    def decode(self, input, final=False):
        raise NotImplementedError

    def reset(self):
        pass

    def getstate(self):
        return (b"", 0)

    def setstate(self, state):
        pass


class BufferedIncrementalEncoder(IncrementalEncoder):
    """Base for encoders that may buffer a trailing partial character."""

    def __init__(self, errors="strict"):
        super().__init__(errors)
        self.buffer = ""

    def _buffer_encode(self, input, errors, final):
        raise NotImplementedError

    def encode(self, input, final=False):
        data = self.buffer + input
        (result, consumed) = self._buffer_encode(data, self.errors, final)
        self.buffer = data[consumed:]
        return result

    def reset(self):
        IncrementalEncoder.reset(self)
        self.buffer = ""

    def getstate(self):
        return self.buffer or 0

    def setstate(self, state):
        self.buffer = state or ""


class BufferedIncrementalDecoder(IncrementalDecoder):
    """Base for decoders that may buffer a trailing partial byte sequence."""

    def __init__(self, errors="strict"):
        super().__init__(errors)
        self.buffer = b""

    def _buffer_decode(self, input, errors, final):
        raise NotImplementedError

    def decode(self, input, final=False):
        data = self.buffer + bytes(input)
        (result, consumed) = self._buffer_decode(data, self.errors, final)
        self.buffer = data[consumed:]
        return result

    def reset(self):
        IncrementalDecoder.reset(self)
        self.buffer = b""

    def getstate(self):
        return (self.buffer, 0)

    def setstate(self, state):
        self.buffer = state[0]


class StreamWriter(Codec):
    """CPython ``codecs.StreamWriter`` (vendored verbatim from 3.13)."""

    def __init__(self, stream, errors='strict'):
        self.stream = stream
        self.errors = errors

    def write(self, object):
        """ Writes the object's contents encoded to self.stream.
        """
        data, consumed = self.encode(object, self.errors)
        self.stream.write(data)

    def writelines(self, list):
        """ Writes the concatenated list of strings to the stream
            using .write().
        """
        self.write(''.join(list))

    def reset(self):
        pass

    def seek(self, offset, whence=0):
        self.stream.seek(offset, whence)
        if whence == 0 and offset == 0:
            self.reset()

    def __getattr__(self, name, getattr=getattr):
        """ Inherit all other methods from the underlying stream.
        """
        return getattr(self.stream, name)

    def __enter__(self):
        return self

    def __exit__(self, type, value, tb):
        self.stream.close()

    def __reduce_ex__(self, proto):
        raise TypeError("can't serialize %s" % self.__class__.__name__)


class StreamReader(Codec):
    """CPython ``codecs.StreamReader`` (vendored verbatim from 3.13):
    real ``bytebuffer``/``charbuffer``/``linebuffer`` bookkeeping so
    ``read(size, chars, firstline)`` / ``readline(size, keepends)``
    behave exactly like upstream."""

    charbuffertype = str

    def __init__(self, stream, errors='strict'):
        self.stream = stream
        self.errors = errors
        self.bytebuffer = b""
        self._empty_charbuffer = self.charbuffertype()
        self.charbuffer = self._empty_charbuffer
        self.linebuffer = None

    def decode(self, input, errors='strict'):
        raise NotImplementedError

    def read(self, size=-1, chars=-1, firstline=False):
        # If we have lines cached, first merge them back into characters
        if self.linebuffer:
            self.charbuffer = self._empty_charbuffer.join(self.linebuffer)
            self.linebuffer = None

        if chars < 0:
            # For compatibility with other read() methods that take a
            # single argument
            chars = size

        # read until we get the required number of characters (if available)
        while True:
            # can the request be satisfied from the character buffer?
            if chars >= 0:
                if len(self.charbuffer) >= chars:
                    break
            # we need more data
            if size < 0:
                newdata = self.stream.read()
            else:
                newdata = self.stream.read(size)
            # decode bytes (those remaining from the last call included)
            data = self.bytebuffer + newdata
            if not data:
                break
            try:
                newchars, decodedbytes = self.decode(data, self.errors)
            except UnicodeDecodeError as exc:
                if firstline:
                    newchars, decodedbytes = \
                        self.decode(data[:exc.start], self.errors)
                    lines = newchars.splitlines(keepends=True)
                    if len(lines)<=1:
                        raise
                else:
                    raise
            # keep undecoded bytes until the next call
            self.bytebuffer = data[decodedbytes:]
            # put new characters in the character buffer
            self.charbuffer += newchars
            # there was no data available
            if not newdata:
                break
        if chars < 0:
            # Return everything we've got
            result = self.charbuffer
            self.charbuffer = self._empty_charbuffer
        else:
            # Return the first chars characters
            result = self.charbuffer[:chars]
            self.charbuffer = self.charbuffer[chars:]
        return result

    def readline(self, size=None, keepends=True):
        # If we have lines cached from an earlier read, return
        # them unconditionally
        if self.linebuffer:
            line = self.linebuffer[0]
            del self.linebuffer[0]
            if len(self.linebuffer) == 1:
                # revert to charbuffer mode; we might need more data
                # next time
                self.charbuffer = self.linebuffer[0]
                self.linebuffer = None
            if not keepends:
                line = line.splitlines(keepends=False)[0]
            return line

        readsize = size or 72
        line = self._empty_charbuffer
        # If size is given, we call read() only once
        while True:
            data = self.read(readsize, firstline=True)
            if data:
                # If we're at a "\r" read one extra character (which might
                # be a "\n") to get a proper line ending. If the stream is
                # temporarily exhausted we return the wrong line ending.
                if (isinstance(data, str) and data.endswith("\r")) or \
                   (isinstance(data, bytes) and data.endswith(b"\r")):
                    data += self.read(size=1, chars=1)

            line += data
            lines = line.splitlines(keepends=True)
            if lines:
                if len(lines) > 1:
                    # More than one line result; the first line is a full line
                    # to return
                    line = lines[0]
                    del lines[0]
                    if len(lines) > 1:
                        # cache the remaining lines
                        lines[-1] += self.charbuffer
                        self.linebuffer = lines
                        self.charbuffer = None
                    else:
                        # only one remaining line, put it back into charbuffer
                        self.charbuffer = lines[0] + self.charbuffer
                    if not keepends:
                        line = line.splitlines(keepends=False)[0]
                    break
                line0withend = lines[0]
                line0withoutend = lines[0].splitlines(keepends=False)[0]
                if line0withend != line0withoutend: # We really have a line end
                    # Put the rest back together and keep it until the next call
                    self.charbuffer = self._empty_charbuffer.join(lines[1:]) + \
                                      self.charbuffer
                    if keepends:
                        line = line0withend
                    else:
                        line = line0withoutend
                    break
            # we didn't get anything or this was our only try
            if not data or size is not None:
                if line and not keepends:
                    line = line.splitlines(keepends=False)[0]
                break
            if readsize < 8000:
                readsize *= 2
        return line

    def readlines(self, sizehint=None, keepends=True):
        data = self.read()
        return data.splitlines(keepends)

    def reset(self):
        self.bytebuffer = b""
        self.charbuffer = self._empty_charbuffer
        self.linebuffer = None

    def seek(self, offset, whence=0):
        """ Set the input stream's current position.

            Resets the codec buffers used for keeping state.
        """
        self.stream.seek(offset, whence)
        self.reset()

    def __next__(self):
        """ Return the next decoded line from the input stream."""
        line = self.readline()
        if line:
            return line
        raise StopIteration

    def __iter__(self):
        return self

    def __getattr__(self, name, getattr=getattr):
        """ Inherit all other methods from the underlying stream.
        """
        return getattr(self.stream, name)

    def __enter__(self):
        return self

    def __exit__(self, type, value, tb):
        self.stream.close()

    def __reduce_ex__(self, proto):
        raise TypeError("can't serialize %s" % self.__class__.__name__)


class StreamReaderWriter:
    """CPython ``codecs.StreamReaderWriter`` (vendored verbatim)."""

    # Optional attributes set by the file wrappers below
    encoding = 'unknown'

    def __init__(self, stream, Reader, Writer, errors='strict'):
        self.stream = stream
        self.reader = Reader(stream, errors)
        self.writer = Writer(stream, errors)
        self.errors = errors

    def read(self, size=-1):
        return self.reader.read(size)

    def readline(self, size=None, keepends=True):
        return self.reader.readline(size, keepends)

    def readlines(self, sizehint=None, keepends=True):
        return self.reader.readlines(sizehint, keepends)

    def __next__(self):
        """ Return the next decoded line from the input stream."""
        return next(self.reader)

    def __iter__(self):
        return self

    def write(self, data):
        return self.writer.write(data)

    def writelines(self, list):
        return self.writer.writelines(list)

    def reset(self):
        self.reader.reset()
        self.writer.reset()

    def seek(self, offset, whence=0):
        self.stream.seek(offset, whence)
        self.reader.reset()
        if whence == 0 and offset == 0:
            self.writer.reset()

    def __getattr__(self, name, getattr=getattr):
        """ Inherit all other methods from the underlying stream.
        """
        return getattr(self.stream, name)

    # these are needed to make "with StreamReaderWriter(...)" work properly

    def __enter__(self):
        return self

    def __exit__(self, type, value, tb):
        self.stream.close()

    def __reduce_ex__(self, proto):
        raise TypeError("can't serialize %s" % self.__class__.__name__)


class StreamRecoder:
    """CPython ``codecs.StreamRecoder`` (vendored verbatim): transcodes
    between a frontend data encoding and a backend file encoding."""

    # Optional attributes set by the file wrappers below
    data_encoding = 'unknown'
    file_encoding = 'unknown'

    def __init__(self, stream, encode, decode, Reader, Writer,
                 errors='strict'):
        self.stream = stream
        self.encode = encode
        self.decode = decode
        self.reader = Reader(stream, errors)
        self.writer = Writer(stream, errors)
        self.errors = errors

    def read(self, size=-1):
        data = self.reader.read(size)
        data, bytesencoded = self.encode(data, self.errors)
        return data

    def readline(self, size=None):
        if size is None:
            data = self.reader.readline()
        else:
            data = self.reader.readline(size)
        data, bytesencoded = self.encode(data, self.errors)
        return data

    def readlines(self, sizehint=None):
        data = self.reader.read()
        data, bytesencoded = self.encode(data, self.errors)
        return data.splitlines(keepends=True)

    def __next__(self):
        """ Return the next decoded line from the input stream."""
        data = next(self.reader)
        data, bytesencoded = self.encode(data, self.errors)
        return data

    def __iter__(self):
        return self

    def write(self, data):
        data, bytesdecoded = self.decode(data, self.errors)
        return self.writer.write(data)

    def writelines(self, list):
        data = b''.join(list)
        data, bytesdecoded = self.decode(data, self.errors)
        return self.writer.write(data)

    def reset(self):
        self.reader.reset()
        self.writer.reset()

    def seek(self, offset, whence=0):
        # Seeks must be propagated to both the readers and writers
        # as they might need to reset their internal buffers.
        self.reader.seek(offset, whence)
        self.writer.seek(offset, whence)

    def __getattr__(self, name, getattr=getattr):
        """ Inherit all other methods from the underlying stream.
        """
        return getattr(self.stream, name)

    def __enter__(self):
        return self

    def __exit__(self, type, value, tb):
        self.stream.close()

    def __reduce_ex__(self, proto):
        raise TypeError("can't serialize %s" % self.__class__.__name__)

class _UTF8SigIncrementalEncoder(IncrementalEncoder):
    """utf-8-sig incremental encoder: emit the BOM exactly once (CPython
    ``encodings/utf_8_sig.py``). ``setstate(0)`` is how ``TextIOWrapper``
    suppresses the BOM when appending to a non-empty file."""

    def __init__(self, errors="strict"):
        super().__init__(errors)
        self.first = 1

    def encode(self, input, final=False):
        if self.first:
            self.first = 0
            return BOM_UTF8 + _codecs.utf_8_encode(input, self.errors)[0]
        return _codecs.utf_8_encode(input, self.errors)[0]

    def reset(self):
        super().reset()
        self.first = 1

    def getstate(self):
        return self.first

    def setstate(self, state):
        self.first = state


class _UTF8SigIncrementalDecoder(BufferedIncrementalDecoder):
    """utf-8-sig incremental decoder: strip a leading BOM once."""

    def __init__(self, errors="strict"):
        super().__init__(errors)
        self.first = 1

    def _buffer_decode(self, input, errors, final):
        if self.first:
            if len(input) < 3:
                if BOM_UTF8.startswith(input):
                    # Not enough data yet to decide; wait for more.
                    return ("", 0)
                self.first = 0
            else:
                self.first = 0
                if input[:3] == BOM_UTF8:
                    (output, consumed) = _codecs.utf_8_decode(input[3:], errors, final)
                    return (output, consumed + 3)
        return _codecs.utf_8_decode(input, errors, final)

    def reset(self):
        super().reset()
        self.first = 1

    def getstate(self):
        return (self.buffer, self.first)

    def setstate(self, state):
        (buffer, first) = state
        self.buffer = buffer
        self.first = first


class _UTF8SigStreamWriter(StreamWriter):
    """CPython ``encodings/utf_8_sig.py`` StreamWriter."""

    def reset(self):
        StreamWriter.reset(self)
        try:
            del self.encode
        except AttributeError:
            pass

    def encode(self, input, errors='strict'):
        self.encode = _utf_8_encode_stateless
        return _utf_8_sig_encode(input, errors)


class _UTF8SigStreamReader(StreamReader):
    """CPython ``encodings/utf_8_sig.py`` StreamReader."""

    def reset(self):
        StreamReader.reset(self)
        try:
            del self.decode
        except AttributeError:
            pass

    def decode(self, input, errors='strict'):
        if len(input) < 3:
            if BOM_UTF8.startswith(input):
                # not enough data to decide if this is a BOM
                # => try again on the next call
                return ("", 0)
        elif input[:3] == BOM_UTF8:
            self.decode = _codecs.utf_8_decode
            (output, consumed) = _codecs.utf_8_decode(input[3:], errors)
            return (output, consumed + 3)
        # (else) no BOM present
        self.decode = _codecs.utf_8_decode
        return _codecs.utf_8_decode(input, errors)


def _utf_8_encode_stateless(input, errors='strict'):
    return _codecs.utf_8_encode(input, errors)


def _utf_8_sig_codecinfo(name="utf-8-sig"):
    return CodecInfo(
        encode=_utf_8_sig_encode,
        decode=_utf_8_sig_decode,
        incrementalencoder=_UTF8SigIncrementalEncoder,
        incrementaldecoder=_UTF8SigIncrementalDecoder,
        streamreader=_UTF8SigStreamReader,
        streamwriter=_UTF8SigStreamWriter,
        name="utf-8-sig",
        _is_text_encoding=True,
    )


class _Utf16IncrementalEncoder(IncrementalEncoder):
    """utf-16 (auto-BOM) incremental encoder, ported from CPython's
    ``encodings/utf_16.py``. The BOM is emitted exactly once (in native byte
    order) on the first non-deferred ``encode``; every subsequent call uses the
    matching LE/BE encoder so the byte-order mark is never repeated. ``setstate``
    is how ``TextIOWrapper`` suppresses the BOM when appending/seeking into a
    non-empty stream (``test_io.test_seek_bom``/``test_encoded_writes``)."""

    def __init__(self, errors="strict"):
        super().__init__(errors)
        self.encoder = None

    def encode(self, input, final=False):
        if self.encoder is None:
            result = _codecs.utf_16_encode(input, self.errors)[0]
            if sys.byteorder == "little":
                self.encoder = _codecs.utf_16_le_encode
            else:
                self.encoder = _codecs.utf_16_be_encode
            return result
        return self.encoder(input, self.errors)[0]

    def reset(self):
        super().reset()
        self.encoder = None

    def getstate(self):
        # 2: byte order not yet emitted (BOM still pending); 0: BOM already out.
        return 2 if self.encoder is None else 0

    def setstate(self, state):
        if state:
            self.encoder = None
        elif sys.byteorder == "little":
            self.encoder = _codecs.utf_16_le_encode
        else:
            self.encoder = _codecs.utf_16_be_encode


class _Utf32IncrementalEncoder(IncrementalEncoder):
    """utf-32 (auto-BOM) incremental encoder, ported from CPython's
    ``encodings/utf_32.py`` — the utf-16 logic with the 4-byte codec."""

    def __init__(self, errors="strict"):
        super().__init__(errors)
        self.encoder = None

    def encode(self, input, final=False):
        if self.encoder is None:
            result = _codecs.utf_32_encode(input, self.errors)[0]
            if sys.byteorder == "little":
                self.encoder = _codecs.utf_32_le_encode
            else:
                self.encoder = _codecs.utf_32_be_encode
            return result
        return self.encoder(input, self.errors)[0]

    def reset(self):
        super().reset()
        self.encoder = None

    def getstate(self):
        return 2 if self.encoder is None else 0

    def setstate(self, state):
        if state:
            self.encoder = None
        elif sys.byteorder == "little":
            self.encoder = _codecs.utf_32_le_encode
        else:
            self.encoder = _codecs.utf_32_be_encode


class _Utf16IncrementalDecoder(BufferedIncrementalDecoder):
    """CPython ``encodings/utf_16.py`` IncrementalDecoder: BOM-sniffing on
    the first chunk, then the resolved LE/BE stateful decoder."""

    def __init__(self, errors='strict'):
        BufferedIncrementalDecoder.__init__(self, errors)
        self.decoder = None

    def _buffer_decode(self, input, errors, final):
        if self.decoder is None:
            (output, consumed, byteorder) = \
                _codecs.utf_16_ex_decode(input, errors, 0, final)
            if byteorder == -1:
                self.decoder = _codecs.utf_16_le_decode
            elif byteorder == 1:
                self.decoder = _codecs.utf_16_be_decode
            elif consumed >= 2:
                raise UnicodeDecodeError("utf-16", input, 0, 2,
                                         "Stream does not start with BOM")
            return (output, consumed)
        return self.decoder(input, self.errors, final)

    def reset(self):
        BufferedIncrementalDecoder.reset(self)
        self.decoder = None

    def getstate(self):
        # additional state info from the base class must be None here,
        # as it isn't passed along to the caller
        state = BufferedIncrementalDecoder.getstate(self)[0]
        # additional state info we pass to the caller:
        # 0: stream is in natural order for this platform
        # 1: stream is in unnatural order
        # 2: endianness hasn't been determined yet
        if self.decoder is None:
            return (state, 2)
        addstate = int((sys.byteorder == "big") !=
                       (self.decoder is _codecs.utf_16_be_decode))
        return (state, addstate)

    def setstate(self, state):
        # state[1] will be ignored by BufferedIncrementalDecoder.setstate()
        BufferedIncrementalDecoder.setstate(self, state)
        state = state[1]
        if state == 0:
            self.decoder = (_codecs.utf_16_be_decode
                            if sys.byteorder == "big"
                            else _codecs.utf_16_le_decode)
        elif state == 1:
            self.decoder = (_codecs.utf_16_le_decode
                            if sys.byteorder == "big"
                            else _codecs.utf_16_be_decode)
        else:
            self.decoder = None


class _Utf16StreamWriter(StreamWriter):
    """CPython ``encodings/utf_16.py`` StreamWriter."""

    def __init__(self, stream, errors='strict'):
        StreamWriter.__init__(self, stream, errors)
        self.encoder = None

    def reset(self):
        StreamWriter.reset(self)
        self.encoder = None

    def encode(self, input, errors='strict'):
        if self.encoder is None:
            result = _codecs.utf_16_encode(input, errors)
            if sys.byteorder == 'little':
                self.encoder = _codecs.utf_16_le_encode
            else:
                self.encoder = _codecs.utf_16_be_encode
            return result
        else:
            return self.encoder(input, errors)


class _Utf16StreamReader(StreamReader):
    """CPython ``encodings/utf_16.py`` StreamReader."""

    def reset(self):
        StreamReader.reset(self)
        try:
            del self.decode
        except AttributeError:
            pass

    def decode(self, input, errors='strict'):
        (object, consumed, byteorder) = \
            _codecs.utf_16_ex_decode(input, errors, 0, False)
        if byteorder == -1:
            self.decode = _codecs.utf_16_le_decode
        elif byteorder == 1:
            self.decode = _codecs.utf_16_be_decode
        elif consumed >= 2:
            raise UnicodeDecodeError("utf-16", input, 0, 2,
                                     "Stream does not start with BOM")
        return (object, consumed)


class _Utf32IncrementalDecoder(BufferedIncrementalDecoder):
    """CPython ``encodings/utf_32.py`` IncrementalDecoder."""

    def __init__(self, errors='strict'):
        BufferedIncrementalDecoder.__init__(self, errors)
        self.decoder = None

    def _buffer_decode(self, input, errors, final):
        if self.decoder is None:
            (output, consumed, byteorder) = \
                _codecs.utf_32_ex_decode(input, errors, 0, final)
            if byteorder == -1:
                self.decoder = _codecs.utf_32_le_decode
            elif byteorder == 1:
                self.decoder = _codecs.utf_32_be_decode
            elif consumed >= 4:
                raise UnicodeDecodeError("utf-32", input, 0, 4,
                                         "Stream does not start with BOM")
            return (output, consumed)
        return self.decoder(input, self.errors, final)

    def reset(self):
        BufferedIncrementalDecoder.reset(self)
        self.decoder = None

    def getstate(self):
        state = BufferedIncrementalDecoder.getstate(self)[0]
        if self.decoder is None:
            return (state, 2)
        addstate = int((sys.byteorder == "big") !=
                       (self.decoder is _codecs.utf_32_be_decode))
        return (state, addstate)

    def setstate(self, state):
        BufferedIncrementalDecoder.setstate(self, state)
        state = state[1]
        if state == 0:
            self.decoder = (_codecs.utf_32_be_decode
                            if sys.byteorder == "big"
                            else _codecs.utf_32_le_decode)
        elif state == 1:
            self.decoder = (_codecs.utf_32_le_decode
                            if sys.byteorder == "big"
                            else _codecs.utf_32_be_decode)
        else:
            self.decoder = None


class _Utf32StreamWriter(StreamWriter):
    """CPython ``encodings/utf_32.py`` StreamWriter."""

    def __init__(self, stream, errors='strict'):
        StreamWriter.__init__(self, stream, errors)
        self.encoder = None

    def reset(self):
        StreamWriter.reset(self)
        self.encoder = None

    def encode(self, input, errors='strict'):
        if self.encoder is None:
            result = _codecs.utf_32_encode(input, errors)
            if sys.byteorder == 'little':
                self.encoder = _codecs.utf_32_le_encode
            else:
                self.encoder = _codecs.utf_32_be_encode
            return result
        else:
            return self.encoder(input, errors)


class _Utf32StreamReader(StreamReader):
    """CPython ``encodings/utf_32.py`` StreamReader."""

    def reset(self):
        StreamReader.reset(self)
        try:
            del self.decode
        except AttributeError:
            pass

    def decode(self, input, errors='strict'):
        (object, consumed, byteorder) = \
            _codecs.utf_32_ex_decode(input, errors, 0, False)
        if byteorder == -1:
            self.decode = _codecs.utf_32_le_decode
        elif byteorder == 1:
            self.decode = _codecs.utf_32_be_decode
        elif consumed >= 4:
            raise UnicodeDecodeError("utf-32", input, 0, 4,
                                     "Stream does not start with BOM")
        return (object, consumed)


def _utf_16_decode_stateless(input, errors="strict"):
    return _codecs.utf_16_decode(input, errors, True)


def _utf_32_decode_stateless(input, errors="strict"):
    return _codecs.utf_32_decode(input, errors, True)


def _utf_16_codecinfo(name="utf-16"):
    return CodecInfo(
        encode=_codecs.utf_16_encode,
        decode=_utf_16_decode_stateless,
        incrementalencoder=_Utf16IncrementalEncoder,
        incrementaldecoder=_Utf16IncrementalDecoder,
        streamreader=_Utf16StreamReader,
        streamwriter=_Utf16StreamWriter,
        name=name,
        _is_text_encoding=True,
    )


def _utf_32_codecinfo(name="utf-32"):
    return CodecInfo(
        encode=_codecs.utf_32_encode,
        decode=_utf_32_decode_stateless,
        incrementalencoder=_Utf32IncrementalEncoder,
        incrementaldecoder=_Utf32IncrementalDecoder,
        streamreader=_Utf32StreamReader,
        streamwriter=_Utf32StreamWriter,
        name=name,
        _is_text_encoding=True,
    )


def _euc_jis_2004_codecinfo(name="euc_jis_2004"):
    # The codec's ~70 KB of packed tables are unpacked once at module import;
    # keep that cold until something actually asks for `euc_jis_2004`.
    import _codec_euc_jis_2004 as _ejis

    return _ejis.getregentry(name)


# Canonical names served by the `_codec_cjk_ext` frozen module (spelling
# variants funnel through `encodings.aliases` before this table is consulted).
_CJK_EXT_NAMES = {
    "hz": "hz",
    "johab": "johab",
    "shift_jis_2004": "shift_jis_2004",
    "shift_jisx0213": "shift_jisx0213",
    "iso2022_jp": "iso2022_jp",
    "iso2022_jp_1": "iso2022_jp_1",
    "iso2022_jp_2": "iso2022_jp_2",
    "iso2022_jp_2004": "iso2022_jp_2004",
    "iso2022_jp_3": "iso2022_jp_3",
    "iso2022_jp_ext": "iso2022_jp_ext",
    "iso2022_kr": "iso2022_kr",
}

# Canonical names served by the `_codec_cjk_dbcs` frozen module — the
# stateless double/multi-byte CJK codecs with CPython-parity tables
# (RFC 0050 WS3). The engine's `encoding_rs` backend must never claim
# these: WHATWG's indices diverge (its euc-kr IS cp949, its big5 carries
# HKSCS, its shift_jis IS cp932, ...).
_CJK_DBCS_NAMES = frozenset((
    "euc_kr", "cp949",
    "euc_jp", "cp932", "shift_jis",
    "gb2312", "gbk", "gb18030",
    "big5", "cp950", "big5hkscs",
))


# CPython's codec registry caches every successful lookup keyed by the
# normalised name (`interp->codec_search_cache`). Returning the *same*
# `CodecInfo` object across calls is observable: `test_io.test_illegal_decoder`
# mutates a looked-up codec (`swap_attr(quopri, 'incrementaldecoder', …)`) and
# relies on `TextIOWrapper`'s internal re-lookup seeing the change.
_CODEC_CACHE = {}


def lookup(encoding):
    # CPython's `_codecs.lookup` receives the name as a C string (the `s`
    # format code), which (1) requires ``str`` and (2) rejects an embedded NUL
    # with ``ValueError`` before any registry lookup — so e.g.
    # ``codecs.lookup('utf-8\0')`` / ``TextIOWrapper(b, encoding='utf-8\0')``
    # raise ``ValueError`` rather than ``LookupError`` (CPython
    # ``test_io.test_constructor`` / ``test_reconfigure_errors``).
    #
    # CPython also raises ``UnicodeEncodeError`` for a name containing a lone
    # surrogate (it can't be UTF-8-encoded for the C string). With WTF-8 ``str``
    # storage WeavePy now reproduces this faithfully: the lone-surrogate name
    # survives ``isinstance``/``.lower()``/``_normalise`` intact and the native
    # ``_codecs.lookup`` raises ``UnicodeEncodeError`` when it strict-UTF-8
    # encodes the codec name.
    if not isinstance(encoding, str):
        raise TypeError(
            f"lookup() argument must be str, not {type(encoding).__name__}"
        )
    if "\0" in encoding:
        raise ValueError("embedded null character")
    encoding = encoding.lower()
    # Explicit user/Rust-side registrations win and are always read fresh.
    if encoding in _USER_CODECS:
        return _USER_CODECS[encoding]
    if _normalise(encoding) in _USER_CODECS:
        return _USER_CODECS[_normalise(encoding)]
    cache_key = _normalise(encoding)
    cached = _CODEC_CACHE.get(cache_key)
    if cached is not None:
        return cached
    info = _lookup_uncached(encoding)
    _CODEC_CACHE[cache_key] = info
    return info


def _lookup_uncached(encoding):
    # CPython's `encodings.search_function` first maps spelling variants
    # through the `encodings.aliases` registry ("utf-16le" → "utf_16_le",
    # "windows-1251" → "cp1251", "646" → "ascii", …) and then resolves the
    # canonical name. Do the same before consulting any codec table.
    aliased = _alias_resolve(encoding)
    if aliased is not None and _normalise(aliased) != _normalise(encoding):
        return _lookup_uncached(aliased)
    norm = _normalise(encoding)
    if norm == "utf_8_sig":
        return _utf_8_sig_codecinfo()
    # The auto-BOM utf-16/utf-32 variants need a stateful incremental encoder
    # (BOM emitted once); the explicit `_le`/`_be` variants are BOM-free and use
    # the generic builtin path below.
    if norm in ("utf_16", "utf16", "u16"):
        return _utf_16_codecinfo()
    if norm in ("utf_32", "utf32", "u32"):
        return _utf_32_codecinfo()
    # The JIS X 0213:2004 `euc_jis_2004` CJK codec (and its aliases) — a faithful
    # port whose combining sequences make the incremental *encoder* stateful.
    # `encoding_rs` (the engine's CJK backend) doesn't carry it, so it lives in a
    # dedicated frozen module loaded on first use.
    if norm in ("euc_jis_2004", "euc_jis2004", "eucjis2004", "jisx0213"):
        return _euc_jis_2004_codecinfo()
    # JIS X 0213:2000 — CPython's `euc_jisx0213` shares the `_codecs_jp`
    # engine with `euc_jis_2004` (the 2004 revision only *adds* ten
    # characters); serve it from the same frozen port under its own name.
    if norm in ("euc_jisx0213", "eucjisx0213"):
        return _euc_jis_2004_codecinfo("euc_jisx0213")
    # The stateful CJK escape codecs (CPython's Modules/cjkcodecs): HZ-GB-2312,
    # the seven ISO-2022 variants, JOHAB and Shift_JIS-2004/X0213 live in a
    # dedicated frozen module bridging onto the euc_jp/euc_kr/gb2312 backends
    # and the euc_jis_2004 tables.
    if norm in _CJK_EXT_NAMES:
        import _codec_cjk_ext

        return _codec_cjk_ext.getregentry(_CJK_EXT_NAMES[norm])
    # The stateless CJK DBCS codecs (CPython's Modules/cjkcodecs) with
    # generated CPython-parity tables (RFC 0050 WS3).
    if norm in _CJK_DBCS_NAMES:
        import _codec_cjk_dbcs

        return _codec_cjk_dbcs.getregentry(norm)
    # `idna` (RFC 3490) and `punycode` (RFC 3492) are pure-Python codecs in
    # CPython's `encodings` package; WeavePy freezes just those two and resolves
    # them lazily here (the engine's native `_codecs.lookup` doesn't carry them).
    # `http.client`/`urllib` use `idna` to ASCII-encode non-ASCII hostnames, and
    # `encodings.idna` itself re-enters this lookup for `punycode`.
    if norm == "idna":
        import encodings.idna as _idna_mod
        return _idna_mod.getregentry()
    if norm == "punycode":
        import encodings.punycode as _punycode_mod
        return _punycode_mod.getregentry()
    # CPython's bootstrap search function imports `encodings.<name>` and uses
    # its `getregentry()`. WeavePy freezes only the modules that tests and
    # stdlib code reach into directly (ascii, utf_8, rot_13, base64_codec,
    # the on-demand codepages, …); those take precedence here so their
    # CodecInfo members are real module-level (hence picklable) objects.
    info = _frozen_encodings_lookup(norm)
    if info is not None:
        return info
    display = _DISPLAY_NAMES.get(norm, encoding)
    if encoding in _PURE_CODECS or _normalise(encoding) in _PURE_CODECS:
        key = encoding if encoding in _PURE_CODECS else _normalise(encoding)
        encode_fn, decode_fn = _PURE_CODECS[key]
        # `rot_13`/`hex`/… are binary "transform" codecs: CPython marks them
        # `_is_text_encoding=False`, so `io.TextIOWrapper(b, encoding="hex")`
        # raises `LookupError("… is not a text encoding")`
        # (`test_io.test_non_text_encoding_codecs_are_rejected`).
        return _make_codec(display, encode_fn, decode_fn, _is_text_encoding=False)
    if encoding in _BUILTIN_NAMES or _normalise(encoding) in _BUILTIN_NAMES:
        key = encoding if encoding in _BUILTIN_NAMES else _normalise(encoding)
        enc_name, dec_name = _BUILTIN_NAMES[key]
        encode_fn = getattr(_codecs, enc_name)
        raw_decode_fn = getattr(_codecs, dec_name)
        if dec_name in _STATEFUL_DECODE_FNS:
            def decode_fn(input, errors="strict", _raw=raw_decode_fn):
                return _raw(input, errors, True)
            return _make_codec(display, encode_fn, decode_fn,
                               partial_decode_fn=raw_decode_fn)
        return _make_codec(display, encode_fn, raw_decode_fn)
    # Generic fall-through via the engine's own lookup. `_codecs.lookup`
    # raises `LookupError` for an unknown name (CPython parity; some older
    # engines raised `ValueError`, so tolerate both). On a miss, defer to
    # any user-registered search functions (CPython's `codecs.register`
    # protocol — the search is called with the normalised name and returns
    # a `CodecInfo`/4-tuple or `None`). Builtins keep precedence; user
    # codecs like the test suite's `test_decoder`/`test_rot13` fill gaps.
    try:
        canonical = _codecs.lookup(encoding)
    except UnicodeError:
        # A codec name that can't be UTF-8-encoded (a lone surrogate) raises
        # ``UnicodeEncodeError`` (a ``ValueError`` subclass) — propagate it
        # rather than masking it as ``LookupError`` (``test_io.test_constructor``
        # ``encoding='\udcfe'`` for the ``_pyio`` variant).
        raise
    except (LookupError, ValueError):
        # CPython's codec registry bootstraps with `encodings.search_function`,
        # which imports `encodings.<name>` and returns its `getregentry()`. The
        # native `_codecs.lookup` covers the common encodings; the on-demand
        # single-byte codepages (cp037, cp737, …) live as frozen
        # `encodings.*` modules, resolved here before any user search func
        # (mirroring the bootstrap search's precedence).
        info = _frozen_encodings_lookup(_normalise(encoding))
        if info is not None:
            return info
        info = _search_registered(_normalise(encoding))
        if info is not None:
            return info
        raise LookupError("unknown encoding: " + encoding) from None
    def encode(s, errors="strict"):
        return _codecs.encode(s, canonical, errors)
    def decode(b, errors="strict"):
        return _codecs.decode(b, canonical, errors)
    # CPython names a codec by its `encodings.<module>` declared name, not the
    # engine's WHATWG label — post-alias `norm` IS the module key (e.g. a
    # `windows-1251` request alias-resolves to `cp1251` and reports as such).
    return _make_codec(_DISPLAY_NAMES.get(norm, norm), encode, decode)


def _frozen_encodings_lookup(norm):
    """Resolve a codec from a frozen ``encodings.<norm>`` module (CPython's
    ``encodings.search_function`` bootstrap). A name that isn't a plain
    module identifier, a missing module, or one without ``getregentry`` is a
    miss (``None``)."""
    if not norm or not norm.replace("_", "").isalnum():
        return None
    try:
        mod = __import__("encodings." + norm, fromlist=["getregentry"])
    except ImportError:
        return None
    getreg = getattr(mod, "getregentry", None)
    if getreg is None:
        return None
    try:
        return getreg()
    except Exception:
        return None


def _search_registered(name):
    """Run the registered search functions in order, returning the first
    non-``None`` result coerced to a :class:`CodecInfo`."""
    for search in _SEARCH_FUNCS:
        result = search(name)
        if result is not None:
            if not isinstance(result, CodecInfo):
                result = CodecInfo(*result)
            return result
    return None


def _note_codec_failure(exc, operation, encoding):
    """CPython `wrap_codec_error`: note an exception escaping a codec call
    with the codec it came from. Failures to attach the note are ignored."""
    try:
        exc.add_note("%s with %r codec failed" % (operation, encoding))
    except Exception:
        pass


def encode(obj, encoding="utf-8", errors="strict"):
    info = lookup(encoding)
    try:
        out, _ = info.encode(obj, errors)
    except BaseException as exc:
        _note_codec_failure(exc, "encoding", encoding)
        raise
    return out


def decode(obj, encoding="utf-8", errors="strict"):
    info = lookup(encoding)
    try:
        out, _ = info.decode(obj, errors)
    except BaseException as exc:
        _note_codec_failure(exc, "decoding", encoding)
        raise
    return out


# On CPython, `codecs.encode`/`decode` *are* the C builtins from
# `_codecs` (`from _codecs import *`), so they pickle by reference as
# `_codecs encode` — proto-0/1 `bytes` pickles embed exactly that
# GLOBAL (pickletools' disassembler_test checks the byte offsets).
# WeavePy's canonical implementations are these Python functions;
# attribute them to `_codecs` and install them there so
# `codecs.encode is _codecs.encode` and pickle's `save_global`
# identity check passes.
encode.__module__ = '_codecs'
encode.__qualname__ = 'encode'
decode.__module__ = '_codecs'
decode.__qualname__ = 'decode'
_codecs.encode = encode
_codecs.decode = decode


def register(search_function):
    """Register a search function. CPython's protocol calls it with
    a normalised encoding name and expects a `CodecInfo` (or
    `None`)."""
    if not callable(search_function):
        raise TypeError("argument must be callable")
    if search_function not in _SEARCH_FUNCS:
        _SEARCH_FUNCS.append(search_function)


def unregister(search_function):
    """Unregister a codec search function previously passed to
    :func:`register` and clear the registry's lookup cache (CPython
    3.10+ `_PyCodec_Unregister` semantics; no-op if never registered)."""
    try:
        _SEARCH_FUNCS.remove(search_function)
    except ValueError:
        return
    # CPython clears `interp->codec_search_cache` when the function was
    # registered, so a stale entry it served can't survive.
    _CODEC_CACHE.clear()


_SEARCH_FUNCS = []


def register_error(name, handler):
    if not callable(handler):
        raise TypeError("handler must be callable")
    _ERROR_HANDLERS[name] = handler


# ---------- built-in error handlers ----------
#
# Faithful ports of CPython's `Python/codecs.c` handlers. CPython's
# `PyObject_TypeCheck` looks at the *real* type (a `__class__`-faking
# instance is rejected with `TypeError`), and the `PyUnicode*Error_Get*`
# accessors validate the `object` attribute's type and clamp
# `start`/`end` — several tests poke exactly these edges
# (`test_codeccallbacks.test_fake_error_class` / `test_unicode*error`).


def _wrong_exception_type(exc):
    raise TypeError(
        f"don't know how to handle {type(exc).__name__} in error callback"
    )


def _real_type_check(exc, cls):
    # `PyObject_TypeCheck`: real inheritance, immune to `__class__` fakes.
    return cls in type(exc).__mro__


def _exc_fields(exc, kind):
    """`(object, start, end)` with CPython's getter validation/clamping.
    *kind* is ``'encode'``/``'translate'`` (str payload) or ``'decode'``
    (bytes payload)."""
    obj = exc.object
    if kind == "decode":
        if not isinstance(obj, (bytes, bytearray)):
            raise TypeError("object attribute must be bytes")
        obj = bytes(obj)
    else:
        if not isinstance(obj, str):
            raise TypeError("object attribute must be unicode")
    size = len(obj)
    start = exc.start
    end = exc.end
    if not isinstance(start, int) or not isinstance(end, int):
        raise TypeError("an integer is required")
    if start < 0:
        start = 0
    if start >= size:
        start = size - 1
    if end < 1:
        end = 1
    if end > size:
        end = size
    return obj, start, end


def _exc_kind(exc):
    if _real_type_check(exc, UnicodeEncodeError):
        return "encode"
    if _real_type_check(exc, UnicodeDecodeError):
        return "decode"
    if _real_type_check(exc, UnicodeTranslateError):
        return "translate"
    return None


def strict_errors(exc):
    """CPython built-in ``strict`` handler: re-raise."""
    if isinstance(exc, BaseException):
        raise exc
    raise TypeError("codec must pass exception instance")


def ignore_errors(exc):
    """CPython built-in ``ignore`` handler: drop the offending range."""
    kind = _exc_kind(exc)
    if kind is None:
        _wrong_exception_type(exc)
    _, _, end = _exc_fields(exc, kind)
    return ("", end)


def replace_errors(exc):
    """CPython built-in ``replace`` handler: '?' on encode, U+FFFD on
    decode/translate."""
    kind = _exc_kind(exc)
    if kind is None:
        _wrong_exception_type(exc)
    _, start, end = _exc_fields(exc, kind)
    if kind == "encode":
        return ("?" * (end - start), end)
    if kind == "decode":
        return ("\ufffd", end)
    return ("\ufffd" * (end - start), end)


def backslashreplace_errors(exc):
    """CPython built-in ``backslashreplace`` handler (encode *and*
    decode/translate)."""
    kind = _exc_kind(exc)
    if kind is None:
        _wrong_exception_type(exc)
    obj, start, end = _exc_fields(exc, kind)
    if kind == "decode":
        return ("".join(f"\\x{b:02x}" for b in obj[start:end]), end)
    parts = []
    for ch in obj[start:end]:
        cp = ord(ch)
        if cp < 0x100:
            parts.append(f"\\x{cp:02x}")
        elif cp < 0x10000:
            parts.append(f"\\u{cp:04x}")
        else:
            parts.append(f"\\U{cp:08x}")
    return ("".join(parts), end)


def xmlcharrefreplace_errors(exc):
    """CPython built-in ``xmlcharrefreplace`` handler (encode only)."""
    if _exc_kind(exc) != "encode":
        _wrong_exception_type(exc)
    obj, start, end = _exc_fields(exc, "encode")
    return ("".join(f"&#{ord(ch)};" for ch in obj[start:end]), end)


def namereplace_errors(exc):
    """CPython built-in ``namereplace`` handler (encode only)."""
    if _exc_kind(exc) != "encode":
        _wrong_exception_type(exc)
    obj, start, end = _exc_fields(exc, "encode")
    import unicodedata
    parts = []
    for ch in obj[start:end]:
        try:
            parts.append("\\N{%s}" % unicodedata.name(ch))
        except (KeyError, ValueError):
            cp = ord(ch)
            if cp < 0x100:
                parts.append(f"\\x{cp:02x}")
            elif cp < 0x10000:
                parts.append(f"\\u{cp:04x}")
            else:
                parts.append(f"\\U{cp:08x}")
    return ("".join(parts), end)


def _standard_encoding(encoding):
    """CPython ``get_standard_encoding``: `(family, bytelength)` where the
    family is one of utf-8 / utf-16-le / utf-16-be / utf-32-le / utf-32-be,
    or `(None, 0)` when unsupported. Byte-order-less names resolve to the
    platform (little-endian) order."""
    norm = encoding.lower().replace("_", "-")
    if norm in ("utf-8", "utf8", "cp65001"):
        return ("utf-8", 3)
    if norm in ("utf-16", "utf16", "u16", "utf-16-le", "utf-16le", "utf16le"):
        if norm in ("utf-16", "utf16", "u16") and sys.byteorder == "big":
            return ("utf-16-be", 2)
        return ("utf-16-le", 2)
    if norm in ("utf-16-be", "utf-16be", "utf16be"):
        return ("utf-16-be", 2)
    if norm in ("utf-32", "utf32", "u32", "utf-32-le", "utf-32le", "utf32le"):
        if norm in ("utf-32", "utf32", "u32") and sys.byteorder == "big":
            return ("utf-32-be", 4)
        return ("utf-32-le", 4)
    if norm in ("utf-32-be", "utf-32be", "utf32be"):
        return ("utf-32-be", 4)
    return (None, 0)


def surrogatepass_errors(exc):
    """CPython's (static) ``surrogatepass`` handler."""
    kind = _exc_kind(exc)
    if kind == "encode":
        obj, start, end = _exc_fields(exc, "encode")
        family, _ = _standard_encoding(exc.encoding)
        if family is None:
            raise exc
        out = bytearray()
        for ch in obj[start:end]:
            cp = ord(ch)
            if not 0xD800 <= cp <= 0xDFFF:
                raise exc
            if family == "utf-8":
                out += bytes([0xE0 | (cp >> 12),
                              0x80 | ((cp >> 6) & 0x3F),
                              0x80 | (cp & 0x3F)])
            elif family == "utf-16-le":
                out += cp.to_bytes(2, "little")
            elif family == "utf-16-be":
                out += cp.to_bytes(2, "big")
            elif family == "utf-32-le":
                out += cp.to_bytes(4, "little")
            else:
                out += cp.to_bytes(4, "big")
        return (bytes(out), end)
    if kind == "decode":
        obj, start, end = _exc_fields(exc, "decode")
        family, bytelength = _standard_encoding(exc.encoding)
        if family is None:
            raise exc
        cp = 0
        p = obj[start:start + bytelength]
        if len(p) == bytelength:
            if family == "utf-8":
                if (p[0] & 0xF0) == 0xE0 and (p[1] & 0xC0) == 0x80 \
                        and (p[2] & 0xC0) == 0x80:
                    cp = ((p[0] & 0x0F) << 12) + ((p[1] & 0x3F) << 6) \
                        + (p[2] & 0x3F)
            elif family == "utf-16-le":
                cp = int.from_bytes(p, "little")
            elif family == "utf-16-be":
                cp = int.from_bytes(p, "big")
            elif family == "utf-32-le":
                cp = int.from_bytes(p, "little")
            else:
                cp = int.from_bytes(p, "big")
        if not 0xD800 <= cp <= 0xDFFF:
            raise exc
        return (chr(cp), start + bytelength)
    _wrong_exception_type(exc)


def surrogateescape_errors(exc):
    """CPython's (static) ``surrogateescape`` handler (PEP 383)."""
    kind = _exc_kind(exc)
    if kind == "encode":
        obj, start, end = _exc_fields(exc, "encode")
        out = bytearray()
        for ch in obj[start:end]:
            cp = ord(ch)
            if not 0xDC80 <= cp <= 0xDCFF:
                # Not a UTF-8b surrogate — fail with the original error.
                raise exc
            out.append(cp - 0xDC00)
        return (bytes(out), end)
    if kind == "decode":
        obj, start, end = _exc_fields(exc, "decode")
        chars = []
        consumed = 0
        while consumed < 4 and consumed < end - start:
            b = obj[start + consumed]
            # Refuse to escape ASCII bytes.
            if b < 128:
                break
            chars.append(chr(0xDC00 + b))
            consumed += 1
        if not consumed:
            # Codec complained about an ASCII byte.
            raise exc
        return ("".join(chars), start + consumed)
    _wrong_exception_type(exc)


_BUILTIN_ERROR_HANDLERS = {
    "strict": strict_errors,
    "ignore": ignore_errors,
    "replace": replace_errors,
    "backslashreplace": backslashreplace_errors,
    "xmlcharrefreplace": xmlcharrefreplace_errors,
    "namereplace": namereplace_errors,
    "surrogateescape": surrogateescape_errors,
    "surrogatepass": surrogatepass_errors,
}


def lookup_error(name):
    if name in _ERROR_HANDLERS:
        return _ERROR_HANDLERS[name]
    if name in _BUILTIN_ERROR_HANDLERS:
        return _BUILTIN_ERROR_HANDLERS[name]
    raise LookupError(f"unknown error handler name '{name}'")


def getencoder(encoding):
    """The stateless ``encode`` callable for *encoding*."""
    return lookup(encoding).encode


def getdecoder(encoding):
    """The stateless ``decode`` callable for *encoding*."""
    return lookup(encoding).decode


def getincrementalencoder(encoding):
    """The ``IncrementalEncoder`` factory for *encoding*."""
    encoder = lookup(encoding).incrementalencoder
    if encoder is None:
        raise LookupError(encoding)
    return encoder


def getincrementaldecoder(encoding):
    """The ``IncrementalDecoder`` factory for *encoding*."""
    decoder = lookup(encoding).incrementaldecoder
    if decoder is None:
        raise LookupError(encoding)
    return decoder


def getreader(encoding):
    """The ``StreamReader`` factory for *encoding*."""
    return lookup(encoding).streamreader


def getwriter(encoding):
    """The ``StreamWriter`` factory for *encoding*."""
    return lookup(encoding).streamwriter


def iterencode(iterator, encoding, errors="strict", **kwargs):
    """Incrementally encode the strings from *iterator*."""
    encoder = getincrementalencoder(encoding)(errors, **kwargs)
    for input in iterator:
        output = encoder.encode(input)
        if output:
            yield output
    output = encoder.encode("", True)
    if output:
        yield output


def iterdecode(iterator, encoding, errors="strict", **kwargs):
    """Incrementally decode the bytes from *iterator*."""
    decoder = getincrementaldecoder(encoding)(errors, **kwargs)
    for input in iterator:
        output = decoder.decode(input)
        if output:
            yield output
    output = decoder.decode(b"", True)
    if output:
        yield output


# ---------- incremental codecs (function adapters) ----------


class _FuncIncrementalEncoder(IncrementalEncoder):
    """Generic incremental encoder over a stateless ``encode(input, errors)``
    callable. Adequate for the byte-per-character text codecs; stateful
    encodings (e.g. the utf-16 BOM) are handled by their own factories."""

    def __init__(self, encode, errors="strict"):
        super().__init__(errors)
        self._encode = encode

    def encode(self, input, final=False):
        if not input:
            return b""
        return self._encode(input, self.errors)[0]


class _FuncIncrementalDecoder(BufferedIncrementalDecoder):
    """Generic incremental decoder over a stateless ``decode(input, errors)``
    callable. Keeps a trailing partial multibyte sequence buffered until more
    data (or ``final``) arrives."""

    def __init__(self, decode, errors="strict"):
        super().__init__(errors)
        self._decode = decode

    def decode(self, input, final=False):
        # A str→str transform codec (rot_13) driven incrementally
        # (`codecs.iterdecode`): no byte buffering applies.
        if isinstance(input, str):
            return self._decode(input, self.errors)[0]
        return super().decode(input, final)

    def _buffer_decode(self, input, errors, final):
        if final or not input:
            return self._decode(input, errors)
        # Decode as much as possible, leaving a trailing partial sequence
        # (at most a few bytes for the variable-width encodings) buffered.
        for split in range(len(input), max(len(input) - 4, -1), -1):
            try:
                result, _ = self._decode(input[:split], errors)
            except (UnicodeDecodeError, ValueError):
                continue
            return (result, split)
        return ("", 0)


class _StatefulFuncIncrementalDecoder(BufferedIncrementalDecoder):
    """Incremental decoder over a *stateful* ``decode(input, errors, final)``
    callable (the `_codecs` UTF natives): the codec itself reports how many
    bytes it consumed, so no split-guessing is needed."""

    def __init__(self, decode, errors="strict"):
        super().__init__(errors)
        self._decode = decode

    def _buffer_decode(self, input, errors, final):
        return self._decode(input, errors, final)




# ---------- helpers for utf-8/utf-16 file IO ----------


def open(filename, mode='r', encoding=None, errors='strict', buffering=-1):
    """CPython ``codecs.open`` (vendored verbatim): open an encoded file
    and wrap it in a :class:`StreamReaderWriter`."""
    if encoding is not None and \
       'b' not in mode:
        # Force opening of the file in binary mode
        mode = mode + 'b'
    # Late-bound `builtins.open` (CPython does the same), so tests that
    # `mock.patch('builtins.open')` intercept this call.
    import builtins
    file = builtins.open(filename, mode, buffering)
    if encoding is None:
        return file

    try:
        info = lookup(encoding)
        srw = StreamReaderWriter(file, info.streamreader, info.streamwriter, errors)
        # Add attributes to simplify introspection
        srw.encoding = encoding
        return srw
    except:
        file.close()
        raise


def EncodedFile(file, data_encoding, file_encoding=None, errors='strict'):
    """CPython ``codecs.EncodedFile`` (vendored verbatim): wrap *file* in a
    :class:`StreamRecoder` translating between two encodings."""
    if file_encoding is None:
        file_encoding = data_encoding
    data_info = lookup(data_encoding)
    file_info = lookup(file_encoding)
    sr = StreamRecoder(file, data_info.encode, data_info.decode,
                       file_info.streamreader, file_info.streamwriter, errors)
    # Add attributes to simplify introspection
    sr.data_encoding = data_encoding
    sr.file_encoding = file_encoding
    return sr


# Deprecated (pre-Unicode-3.2) BOM aliases CPython still exports.
BOM32_LE = BOM_UTF16_LE
BOM32_BE = BOM_UTF16_BE
BOM64_LE = BOM_UTF32_LE
BOM64_BE = BOM_UTF32_BE


# CPython's exact `codecs.__all__` (3.13).
__all__ = ["register", "lookup", "open", "EncodedFile", "BOM", "BOM_BE",
           "BOM_LE", "BOM32_BE", "BOM32_LE", "BOM64_BE", "BOM64_LE",
           "BOM_UTF8", "BOM_UTF16", "BOM_UTF16_LE", "BOM_UTF16_BE",
           "BOM_UTF32", "BOM_UTF32_LE", "BOM_UTF32_BE",
           "CodecInfo", "Codec", "IncrementalEncoder", "IncrementalDecoder",
           "StreamReader", "StreamWriter",
           "StreamReaderWriter", "StreamRecoder",
           "getencoder", "getdecoder", "getincrementalencoder",
           "getincrementaldecoder", "getreader", "getwriter",
           "encode", "decode", "iterencode", "iterdecode",
           "strict_errors", "ignore_errors", "replace_errors",
           "xmlcharrefreplace_errors",
           "backslashreplace_errors", "namereplace_errors",
           "register_error", "lookup_error"]
