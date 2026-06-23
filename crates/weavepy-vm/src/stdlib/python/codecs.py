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


def _make_codec(encoding, encode_fn, decode_fn, _is_text_encoding=True):
    # Build generic incremental factories on top of the stateless
    # (encode, decode) pair so `codecs.getincremental*` work for the
    # built-in codecs without a bespoke class each. The stream
    # reader/writer factories are intentionally left unset: a faithful
    # `StreamReader`/`StreamWriter` is part of the deferred codecs wave,
    # and a half-built one is worse than `None` (callers fall back).
    def _mk_incremental_encoder(errors="strict"):
        return _FuncIncrementalEncoder(encode_fn, errors)

    def _mk_incremental_decoder(errors="strict"):
        return _FuncIncrementalDecoder(decode_fn, errors)

    return CodecInfo(
        encode=encode_fn,
        decode=decode_fn,
        incrementalencoder=_mk_incremental_encoder,
        incrementaldecoder=_mk_incremental_decoder,
        name=encoding,
        _is_text_encoding=_is_text_encoding,
    )


_BUILTIN_NAMES = {
    "utf-8": ("utf_8_encode", "utf_8_decode"),
    "utf_8": ("utf_8_encode", "utf_8_decode"),
    "utf8": ("utf_8_encode", "utf_8_decode"),
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


def _normalise(name):
    return name.replace("-", "_").replace(" ", "_").lower()


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
    if isinstance(b, bytes):
        b = b.decode("ascii")
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
                    (output, consumed) = _codecs.utf_8_decode(input[3:], errors)
                    return (output, consumed + 3)
        return _codecs.utf_8_decode(input, errors)

    def reset(self):
        super().reset()
        self.first = 1

    def getstate(self):
        return (self.buffer, self.first)

    def setstate(self, state):
        (buffer, first) = state
        self.buffer = buffer
        self.first = first


def _utf_8_sig_codecinfo(name="utf-8-sig"):
    return CodecInfo(
        encode=_utf_8_sig_encode,
        decode=_utf_8_sig_decode,
        incrementalencoder=_UTF8SigIncrementalEncoder,
        incrementaldecoder=_UTF8SigIncrementalDecoder,
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


def _utf_16_codecinfo(name="utf-16"):
    dec = _codecs.utf_16_decode
    return CodecInfo(
        encode=_codecs.utf_16_encode,
        decode=dec,
        incrementalencoder=_Utf16IncrementalEncoder,
        incrementaldecoder=lambda errors="strict": _FuncIncrementalDecoder(dec, errors),
        name=name,
        _is_text_encoding=True,
    )


def _utf_32_codecinfo(name="utf-32"):
    dec = _codecs.utf_32_decode
    return CodecInfo(
        encode=_codecs.utf_32_encode,
        decode=dec,
        incrementalencoder=_Utf32IncrementalEncoder,
        incrementaldecoder=lambda errors="strict": _FuncIncrementalDecoder(dec, errors),
        name=name,
        _is_text_encoding=True,
    )


def _euc_jis_2004_codecinfo():
    # The codec's ~70 KB of packed tables are unpacked once at module import;
    # keep that cold until something actually asks for `euc_jis_2004`.
    import _codec_euc_jis_2004 as _ejis

    return _ejis.getregentry("euc_jis_2004")


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
    norm = _normalise(encoding)
    if norm == "utf_8_sig":
        return _utf_8_sig_codecinfo(encoding)
    # The auto-BOM utf-16/utf-32 variants need a stateful incremental encoder
    # (BOM emitted once); the explicit `_le`/`_be` variants are BOM-free and use
    # the generic builtin path below.
    if norm in ("utf_16", "utf16", "u16"):
        return _utf_16_codecinfo(encoding)
    if norm in ("utf_32", "utf32", "u32"):
        return _utf_32_codecinfo(encoding)
    # The JIS X 0213:2004 `euc_jis_2004` CJK codec (and its aliases) — a faithful
    # port whose combining sequences make the incremental *encoder* stateful.
    # `encoding_rs` (the engine's CJK backend) doesn't carry it, so it lives in a
    # dedicated frozen module loaded on first use.
    if norm in ("euc_jis_2004", "euc_jis2004", "eucjis2004", "jisx0213"):
        return _euc_jis_2004_codecinfo()
    if encoding in _PURE_CODECS or _normalise(encoding) in _PURE_CODECS:
        key = encoding if encoding in _PURE_CODECS else _normalise(encoding)
        encode_fn, decode_fn = _PURE_CODECS[key]
        # `rot_13`/`hex`/… are binary "transform" codecs: CPython marks them
        # `_is_text_encoding=False`, so `io.TextIOWrapper(b, encoding="hex")`
        # raises `LookupError("… is not a text encoding")`
        # (`test_io.test_non_text_encoding_codecs_are_rejected`).
        return _make_codec(encoding, encode_fn, decode_fn, _is_text_encoding=False)
    if encoding in _BUILTIN_NAMES or _normalise(encoding) in _BUILTIN_NAMES:
        key = encoding if encoding in _BUILTIN_NAMES else _normalise(encoding)
        enc_name, dec_name = _BUILTIN_NAMES[key]
        encode_fn = getattr(_codecs, enc_name)
        decode_fn = getattr(_codecs, dec_name)
        return _make_codec(encoding, encode_fn, decode_fn)
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
        info = _search_registered(_normalise(encoding))
        if info is not None:
            return info
        raise LookupError("unknown encoding: " + encoding) from None
    def encode(s, errors="strict"):
        return _codecs.encode(s, canonical, errors)
    def decode(b, errors="strict"):
        return _codecs.decode(b, canonical, errors)
    return _make_codec(canonical, encode, decode)


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


def encode(obj, encoding="utf-8", errors="strict"):
    info = lookup(encoding)
    out, _ = info.encode(obj, errors)
    return out


def decode(obj, encoding="utf-8", errors="strict"):
    info = lookup(encoding)
    out, _ = info.decode(obj, errors)
    return out


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
    :func:`register` (no-op if it was never registered). Mirrors
    CPython 3.10+ `codecs.unregister`."""
    try:
        _SEARCH_FUNCS.remove(search_function)
    except ValueError:
        return


_SEARCH_FUNCS = []


def register_error(name, handler):
    if not callable(handler):
        raise TypeError("handler must be callable")
    _ERROR_HANDLERS[name] = handler


def lookup_error(name):
    if name in _ERROR_HANDLERS:
        return _ERROR_HANDLERS[name]
    if name in {"strict", "ignore", "replace", "backslashreplace",
                "namereplace", "xmlcharrefreplace", "surrogateescape",
                "surrogatepass"}:
        # Built-in handlers are implemented in `_codecs`. We hand
        # back a passthrough sentinel since the user call-back path
        # is only used for *explicit* lookup_error() invocations.
        def passthrough(exc):  # noqa
            raise exc
        return passthrough
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


class StreamReader:
    def __init__(self, stream, errors="strict"):
        self.stream = stream
        self.errors = errors

    def read(self, size=-1, chars=-1, firstline=False):
        data = self.stream.read() if size < 0 else self.stream.read(size)
        return data.decode(getattr(self, "encoding", "utf-8"), self.errors) if isinstance(data, bytes) else data

    def readline(self, size=-1):
        return self.stream.readline(size)

    def readlines(self, sizehint=-1):
        return self.stream.readlines(sizehint)


class StreamWriter:
    def __init__(self, stream, errors="strict"):
        self.stream = stream
        self.errors = errors

    def write(self, s):
        return self.stream.write(s)

    def writelines(self, lines):
        for line in lines:
            self.write(line)


class StreamReaderWriter:
    def __init__(self, stream, Reader, Writer, errors="strict"):
        self.stream = stream
        self.reader = Reader(stream, errors)
        self.writer = Writer(stream, errors)

    def read(self, size=-1):
        return self.reader.read(size)

    def write(self, data):
        return self.writer.write(data)


class _FuncStreamReader(StreamReader):
    """Generic ``StreamReader`` over a stateless ``decode`` callable."""

    def __init__(self, decode, stream, errors="strict"):
        StreamReader.__init__(self, stream, errors)
        self._decode = decode

    def read(self, size=-1, chars=-1, firstline=False):
        data = self.stream.read() if size < 0 else self.stream.read(size)
        if isinstance(data, str):
            return data
        return self._decode(data, self.errors)[0]


class _FuncStreamWriter(StreamWriter):
    """Generic ``StreamWriter`` over a stateless ``encode`` callable."""

    def __init__(self, encode, stream, errors="strict"):
        StreamWriter.__init__(self, stream, errors)
        self._encode = encode

    def write(self, s):
        return self.stream.write(self._encode(s, self.errors)[0])


# ---------- helpers for utf-8/utf-16 file IO ----------


_builtin_open = open


def open(filename, mode="rb", encoding=None, errors="strict", buffering=-1):
    """Open a file with codec wrapping. Falls through to the builtin `open`."""
    if "b" not in mode:
        mode = mode + "b"
    f = _builtin_open(filename, mode)
    if encoding is None:
        return f
    info = lookup(encoding)
    f.encoding = encoding
    f.errors = errors

    class _Wrap:
        def __init__(self, raw):
            self.raw = raw

        def read(self, n=-1):
            data = self.raw.read(n)
            if isinstance(data, bytes):
                text, _ = info.decode(data, errors)
                return text
            return data

        def write(self, s):
            data, _ = info.encode(s, errors)
            return self.raw.write(data)

        def close(self):
            self.raw.close()

        def __enter__(self):
            return self

        def __exit__(self, *exc):
            self.close()
            return False

    return _Wrap(f)


# Default error handlers.
def strict_errors(exc):
    raise exc


def ignore_errors(exc):
    return ("", getattr(exc, "end", 0))


def replace_errors(exc):
    return ("\ufffd", getattr(exc, "end", 0))


_ERROR_HANDLERS["strict"] = strict_errors
_ERROR_HANDLERS["ignore"] = ignore_errors
_ERROR_HANDLERS["replace"] = replace_errors


__all__ = [
    "BOM", "BOM_UTF8", "BOM_UTF16", "BOM_UTF16_BE", "BOM_UTF16_LE",
    "BOM_UTF32", "BOM_UTF32_BE", "BOM_UTF32_LE", "BOM_BE", "BOM_LE",
    "encode", "decode", "lookup", "register", "unregister",
    "register_error", "lookup_error", "CodecInfo",
    "getencoder", "getdecoder", "getincrementalencoder",
    "getincrementaldecoder", "getreader", "getwriter",
    "iterencode", "iterdecode",
    "IncrementalEncoder", "IncrementalDecoder",
    "BufferedIncrementalEncoder", "BufferedIncrementalDecoder",
    "StreamReader", "StreamWriter", "StreamReaderWriter",
    "open",
]
