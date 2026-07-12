"""Shared plumbing for WeavePy's frozen CJK codec ports.

Reproduces `Modules/cjkcodecs/multibytecodec.c`'s error-callback protocol
(`multibytecodec_encerror` / `multibytecodec_decerror`) so the three codec
modules (`_codec_cjk_dbcs`, `_codec_cjk_ext`, `_codec_euc_jis_2004`) behave
exactly like CPython's C classes:

- a custom handler must return a ``(str-or-bytes, int)`` tuple when encoding
  and a ``(str, int)`` tuple when decoding — anything else is a
  ``TypeError``;
- an out-of-bounds resume position is an ``IndexError`` (after negative
  wrap-around), not a silent clamp;
- ``errors`` on the incremental codecs is a getset (``del enc.errors``
  raises ``AttributeError`` — `test_incrementalencoder_del_segfault`);
- the multibyte stream readers accept ``None`` size hints and final-flush
  their decoder at EOF (a trailing partial sequence raises);
- ``getstate``/``setstate`` use the C layout: encoders pack
  ``[npending][pending as UTF-8][8 codec state bytes]`` into a
  little-endian int, decoders exchange ``(buffered bytes, state int)``.
"""

from codecs import StreamReader, lookup_error as _lookup_error

_REASON = "illegal multibyte sequence"
_REASON_INCOMPLETE = "incomplete multibyte sequence"

# multibytecodec.h: MAXENCPENDING = 2 UCS4 chars (8 UTF-8 bytes at the
# Python level), MAXDECPENDING = 8 bytes, sizeof(MULTIBYTECODEC_STATE.c) = 8.
MAXENCPENDING = 8
MAXDECPENDING = 8
STATE_SIZE = 8


def _backslash(ch):
    c = ord(ch)
    if c >= 0x10000:
        return "\\U%08x" % c
    if c >= 0x100:
        return "\\u%04x" % c
    return "\\x%02x" % c


def enc_handle(name, errors, s, start, end, reason=_REASON):
    """Resolve an unencodable run -> (replacement, new_index). The
    replacement may be ``str`` (must be re-encoded by the caller with
    strict errors) or ``bytes`` (emitted verbatim)."""
    if errors == "strict":
        raise UnicodeEncodeError(name, s, start, end, reason)
    if errors == "ignore":
        return ("", end)
    if errors == "replace":
        return ("?" * (end - start), end)
    if errors == "xmlcharrefreplace":
        return ("".join("&#%d;" % ord(c) for c in s[start:end]), end)
    if errors == "backslashreplace":
        return ("".join(_backslash(c) for c in s[start:end]), end)
    if errors == "namereplace":
        import unicodedata

        parts = []
        for c in s[start:end]:
            try:
                parts.append("\\N{%s}" % unicodedata.name(c))
            except ValueError:
                parts.append(_backslash(c))
        return ("".join(parts), end)
    handler = _lookup_error(errors)
    r = handler(UnicodeEncodeError(name, s, start, end, reason))
    if (
        not isinstance(r, tuple)
        or len(r) != 2
        or not isinstance(r[0], (str, bytes))
        or not isinstance(r[1], int)
    ):
        raise TypeError("encoding error handler must return (str, int) tuple")
    rep, newpos = r
    if newpos < 0:
        newpos += len(s)
    if newpos < 0 or newpos > len(s):
        raise IndexError(
            "position %d from error handler out of bounds" % newpos
        )
    return (rep, newpos)


def dec_handle(name, errors, data, start, end, reason):
    """Resolve an undecodable run -> (str, new_index)."""
    if errors == "strict":
        raise UnicodeDecodeError(name, bytes(data), start, end, reason)
    if errors == "ignore":
        return ("", end)
    if errors == "replace":
        return ("\ufffd", end)
    if errors == "backslashreplace":
        return ("".join("\\x%02x" % b for b in data[start:end]), end)
    handler = _lookup_error(errors)
    r = handler(UnicodeDecodeError(name, bytes(data), start, end, reason))
    if (
        not isinstance(r, tuple)
        or len(r) != 2
        or not isinstance(r[0], str)
        or not isinstance(r[1], int)
    ):
        raise TypeError("decoding error handler must return (str, int) tuple")
    rep, newpos = r
    if newpos < 0:
        newpos += len(data)
    if newpos < 0 or newpos > len(data):
        raise IndexError(
            "position %d from error handler out of bounds" % newpos
        )
    return (rep, newpos)


def enc_getstate(pending, statebytes=b""):
    """Pack an incremental encoder's state exactly like
    ``MultibyteIncrementalEncoder.getstate``: a little-endian int of
    ``[len(pending-utf8)][pending-utf8][codec state bytes]``."""
    u8 = pending.encode("utf-8")
    return int.from_bytes(bytes((len(u8),)) + u8 + bytes(statebytes), "little")


def enc_setstate(state):
    """Unpack ``setstate`` input -> (pending_str, statebytes).
    Mirrors the C validation: int-only argument, unsigned and bounded
    (OverflowError), pending capped at MAXENCPENDING (UnicodeError) and
    strictly decoded as UTF-8 (UnicodeDecodeError)."""
    if not isinstance(state, int):
        raise TypeError(
            "setstate() argument must be int, not %s" % type(state).__name__
        )
    raw = state.to_bytes(1 + MAXENCPENDING + STATE_SIZE, "little")
    npending = raw[0]
    if npending > MAXENCPENDING:
        raise UnicodeError("pending buffer too large")
    pending = raw[1:1 + npending].decode("utf-8")
    return (pending, raw[1 + npending:1 + npending + STATE_SIZE])


def dec_setstate(state, name):
    """Unpack decoder ``setstate`` input -> (buffer_bytes, flags_int)."""
    if not isinstance(state, tuple):
        raise TypeError(
            "setstate() argument must be tuple, not %s" % type(state).__name__
        )
    if (
        len(state) != 2
        or type(state[0]) is not bytes
        or not isinstance(state[1], int)
    ):
        raise TypeError("setstate(): illegal state argument")
    buf, flags = state
    flags.to_bytes(STATE_SIZE, "little")  # OverflowError parity with C
    if len(buf) > MAXDECPENDING:
        raise UnicodeDecodeError(
            name, buf, 0, len(buf), "pending buffer too large"
        )
    return (buf, flags)


class MbEncStateMixin:
    """C-layout ``getstate``/``setstate`` for encoders whose only state is a
    pending string held in ``self._pending`` (the codec state bytes stay
    zero)."""

    def getstate(self):
        return enc_getstate(self._pending)

    def setstate(self, state):
        pending, _statebytes = enc_setstate(state)
        self._pending = pending


class MbDecStateMixin:
    """C-layout ``getstate``/``setstate`` for decoders whose only state is a
    buffered byte tail in ``self._buffer``; the state int is carried
    verbatim (CPython round-trips the raw ``state.c`` bytes)."""

    name = None
    _flags = 0

    def getstate(self):
        return (self._buffer, self._flags)

    def setstate(self, state):
        self._buffer, self._flags = dec_setstate(state, self.name)


class ErrorsProperty:
    """CPython's multibyte codec objects expose ``errors`` as a getset:
    it can be read and assigned but not deleted."""

    @property
    def errors(self):
        return self._errors

    @errors.setter
    def errors(self, value):
        self._errors = value


class MbStreamReaderMixin:
    """CPython's MultibyteStreamReader accepts ``None`` size hints
    (``read(None)`` = read everything) and finalizes the decoder at EOF, so
    a dangling partial multibyte sequence raises (`test_bug1728403`); the
    generic ``codecs.StreamReader`` does neither."""

    def _decode_flush(self):
        """Final-decode whatever the reader is still holding at EOF."""
        dec = getattr(self, "_decoder", None)
        if dec is not None:
            return dec.decode(b"", True)
        return ""

    def read(self, size=-1, chars=-1, firstline=False):
        # codecs.StreamReader.read with two changes: a None size hint reads
        # everything, and hitting EOF final-flushes the decoder.
        if size is None:
            size = -1
        if self.linebuffer:
            self.charbuffer = self._empty_charbuffer.join(self.linebuffer)
            self.linebuffer = None
        if chars < 0:
            chars = size
        while True:
            if chars >= 0 and len(self.charbuffer) >= chars:
                break
            if size < 0:
                newdata = self.stream.read()
            else:
                newdata = self.stream.read(size)
            data = self.bytebuffer + newdata
            if data:
                try:
                    newchars, decodedbytes = self.decode(data, self.errors)
                except UnicodeDecodeError as exc:
                    if not firstline:
                        raise
                    newchars, decodedbytes = self.decode(
                        data[: exc.start], self.errors
                    )
                    lines = newchars.splitlines(keepends=True)
                    if len(lines) <= 1:
                        raise
                self.bytebuffer = data[decodedbytes:]
                self.charbuffer += newchars
            if not newdata:
                self.charbuffer += self._decode_flush()
                break
        if chars < 0:
            result = self.charbuffer
            self.charbuffer = self._empty_charbuffer
        else:
            result = self.charbuffer[:chars]
            self.charbuffer = self.charbuffer[chars:]
        return result
