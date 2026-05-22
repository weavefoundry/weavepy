"""User-visible ``codecs`` module (RFC 0019).

The heavy lifting lives in `_codecs`. This module hosts the
public surface — `encode`, `decode`, `lookup`, `register`,
`register_error`, the `IncrementalEncoder`/`IncrementalDecoder`
shells, and the `BOM_*` constants.
"""

import _codecs

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
                 incrementalencoder=None, incrementaldecoder=None, name=None):
        self.encode = encode
        self.decode = decode
        self.streamreader = streamreader
        self.streamwriter = streamwriter
        self.incrementalencoder = incrementalencoder
        self.incrementaldecoder = incrementaldecoder
        self.name = name

    def __getitem__(self, idx):
        return (self.encode, self.decode, self.streamreader, self.streamwriter)[idx]

    def __iter__(self):
        return iter((self.encode, self.decode, self.streamreader, self.streamwriter))

    def __len__(self):
        return 4


def _make_codec(encoding, encode_fn, decode_fn):
    return CodecInfo(
        encode=encode_fn,
        decode=decode_fn,
        name=encoding,
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


def lookup(encoding):
    encoding = encoding.lower()
    if encoding in _USER_CODECS:
        return _USER_CODECS[encoding]
    if _normalise(encoding) in _USER_CODECS:
        return _USER_CODECS[_normalise(encoding)]
    if encoding in _BUILTIN_NAMES or _normalise(encoding) in _BUILTIN_NAMES:
        key = encoding if encoding in _BUILTIN_NAMES else _normalise(encoding)
        enc_name, dec_name = _BUILTIN_NAMES[key]
        encode_fn = getattr(_codecs, enc_name)
        decode_fn = getattr(_codecs, dec_name)
        return _make_codec(encoding, encode_fn, decode_fn)
    # Generic fall-through via the engine's own lookup.
    try:
        canonical = _codecs.lookup(encoding)
    except ValueError as e:
        raise LookupError(str(e)) from None
    def encode(s, errors="strict"):
        return _codecs.encode(s, canonical, errors)
    def decode(b, errors="strict"):
        return _codecs.decode(b, canonical, errors)
    return _make_codec(canonical, encode, decode)


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


# ---------- incremental codecs ----------


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
    pass


class BufferedIncrementalDecoder(IncrementalDecoder):
    pass


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
    "encode", "decode", "lookup", "register", "register_error",
    "lookup_error", "CodecInfo",
    "IncrementalEncoder", "IncrementalDecoder",
    "BufferedIncrementalEncoder", "BufferedIncrementalDecoder",
    "StreamReader", "StreamWriter", "StreamReaderWriter",
    "open",
]
