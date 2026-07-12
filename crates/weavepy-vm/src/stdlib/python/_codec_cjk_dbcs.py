"""WeavePy frozen port of CPython's stateless CJK DBCS codecs.

Covers the double/multi-byte codecs CPython implements in
``Modules/cjkcodecs`` whose mapping tables live in `_cjk_tables` (generated
by probing CPython 3.13 itself): ``euc_kr``, ``cp949``, ``euc_jp``,
``cp932``, ``shift_jis``, ``gb2312``, ``gbk``, ``gb18030``, ``big5``,
``cp950`` and ``big5hkscs``.

The state machines are line-by-line ports of ``_codecs_kr.c``
(EUC-KR 8-byte jamo make-up sequences), ``_codecs_jp.c`` (SS2/SS3 planes,
JIS X 0201 layers, cp932 PUA rows), ``_codecs_cn.c`` (GB18030 4-byte
linear forms) and ``_codecs_hk.c`` (HKSCS combining pairs — the one codec
here with a *stateful* encoder). Every cjkcodecs decode/encode error flags
exactly one byte/char (`return 1` throughout the C), except truncated
multi-byte tails at flush which span the remaining bytes with CPython's
"incomplete multibyte sequence" reason.
"""

from codecs import (
    CodecInfo,
    IncrementalEncoder,
    IncrementalDecoder,
    StreamReader,
    StreamWriter,
)

import _cjk_tables as _t
from _cjk_common import (
    ErrorsProperty,
    MbDecStateMixin,
    MbEncStateMixin,
    MbStreamReaderMixin,
    dec_handle as _dec_handle,
    enc_handle as _enc_handle,
)

_REASON = "illegal multibyte sequence"
_REASON_INCOMPLETE = "incomplete multibyte sequence"

_U16_UNDEF = 0xFFFF
_U24_UNDEF = 0xFFFFFF
_U24_PAIR = 0xFFFFFE


# ---------------------------------------------------------------------------
# packed-grid access + encode-map reconstruction
# ---------------------------------------------------------------------------

def _cell16(grid, lead, trail):
    if not (0x81 <= lead <= 0xFE and 0x40 <= trail <= 0xFE):
        return None
    off = ((lead - 0x81) * 191 + (trail - 0x40)) << 1
    v = grid[off] | (grid[off + 1] << 8)
    return None if v == _U16_UNDEF else v


def _cell24(grid, lead, trail):
    if not (0x81 <= lead <= 0xFE and 0x40 <= trail <= 0xFE):
        return None
    off = ((lead - 0x81) * 191 + (trail - 0x40)) * 3
    v = grid[off] | (grid[off + 1] << 8) | (grid[off + 2] << 16)
    return None if v == _U24_UNDEF else v


_ENC_MAPS = {}


def _enc_map(name):
    """{codepoint: bytes} — the *lowest (lead, trail)* preimage of the
    decode grid (the rule `tools/gen_cjk_dbcs_tables.py` diffed against),
    patched by the probed exceptions dict."""
    m = _ENC_MAPS.get(name)
    if m is not None:
        return m
    up = name.upper()
    grid = getattr(_t, up + "_DEC")
    cell = 3 if name == "big5hkscs" else 2
    m = {}
    idx = 0
    for li in range(126):
        for ti in range(191):
            if cell == 2:
                v = grid[idx] | (grid[idx + 1] << 8)
                undef = _U16_UNDEF
            else:
                v = grid[idx] | (grid[idx + 1] << 8) | (grid[idx + 2] << 16)
                undef = _U24_UNDEF
            idx += cell
            if v != undef and v != _U24_PAIR and v not in m:
                m[v] = bytes((0x81 + li, 0x40 + ti))
    ss3 = getattr(_t, up + "_DEC_SS3", None)
    if ss3 is not None:
        idx = 0
        for c2 in range(0xA1, 0xFF):
            for c3 in range(0xA1, 0xFF):
                v = ss3[idx] | (ss3[idx + 1] << 8)
                idx += 2
                if v != _U16_UNDEF and v not in m:
                    m[v] = bytes((0x8F, c2, c3))
    m.update(getattr(_t, up + "_ENC_EXC"))
    for cp in getattr(_t, up + "_ENC_REJECT"):
        m.pop(cp, None)
    _ENC_MAPS[name] = m
    return m


# ---------------------------------------------------------------------------
# generic decode / encode cores
# ---------------------------------------------------------------------------

def _mb_decode(name, data, errors, final, single, needs2, cell2, special=None):
    """Port shape shared by every cjkcodecs DECODER: ASCII passthrough,
    optional single-byte layer, optional multi-byte special form, then the
    2-byte cell lookup. All error spans are 1 byte (the C `return 1`)."""
    out = []
    i = 0
    n = len(data)
    while i < n:
        c = data[i]
        if c < 0x80:
            out.append(chr(c))
            i += 1
            continue
        if single is not None:
            s = single(c)
            if s is not None:
                out.append(s)
                i += 1
                continue
        if special is not None:
            r = special(data, i, n)
            if r is not None:
                kind = r[0]
                if kind == "toofew":
                    if not final:
                        break
                    rep, i = _dec_handle(name, errors, data, i, n, _REASON_INCOMPLETE)
                    out.append(rep)
                    continue
                if kind == "out":
                    out.append(r[1])
                    i += r[2]
                    continue
                rep, i = _dec_handle(name, errors, data, i, i + r[1], _REASON)
                out.append(rep)
                continue
        if not needs2(c):
            rep, i = _dec_handle(name, errors, data, i, i + 1, _REASON)
            out.append(rep)
            continue
        if i + 1 >= n:
            if not final:
                break
            rep, i = _dec_handle(name, errors, data, i, n, _REASON_INCOMPLETE)
            out.append(rep)
            continue
        decoded = cell2(c, data[i + 1])
        if decoded is None:
            rep, i = _dec_handle(name, errors, data, i, i + 1, _REASON)
            out.append(rep)
            continue
        out.append(decoded)
        i += 2
    return ("".join(out), i)


def _mb_encode(name, s, errors, final, enc_char):
    """enc_char(c, c2, final) -> (bytes, chars_consumed), "toofew" or None."""
    out = bytearray()
    pending = ""
    i = 0
    n = len(s)
    while i < n:
        c = ord(s[i])
        if c < 0x80:
            out.append(c)
            i += 1
            continue
        c2 = ord(s[i + 1]) if i + 1 < n else None
        r = enc_char(c, c2, final)
        if r == "toofew":
            pending = s[i:]
            break
        if r is None:
            rep, newpos = _enc_handle(name, errors, s, i, i + 1)
            if isinstance(rep, bytes):
                out += rep
                i = newpos
                continue
            if rep:
                sub, _ = _mb_encode(name, rep, "strict", True, enc_char)
                out += sub
            i = newpos
            continue
        out += r[0]
        i += r[1]
    return (bytes(out), pending)


def _always2(c):
    return True


# ---------------------------------------------------------------------------
# EUC-KR (jamo make-up sequences) / CP949
# ---------------------------------------------------------------------------

_NONE = 127
_U2CGK_CHO = (
    0xA1, 0xA2, 0xA4, 0xA7, 0xA8, 0xA9, 0xB1, 0xB2,
    0xB3, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xBB,
    0xBC, 0xBD, 0xBE,
)
_U2CGK_JUNG = (
    0xBF, 0xC0, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6,
    0xC7, 0xC8, 0xC9, 0xCA, 0xCB, 0xCC, 0xCD, 0xCE,
    0xCF, 0xD0, 0xD1, 0xD2, 0xD3,
)
_U2CGK_JONG = (
    0xD4, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7,
    0xA9, 0xAA, 0xAB, 0xAC, 0xAD, 0xAE, 0xAF, 0xB0,
    0xB1, 0xB2, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xBA,
    0xBB, 0xBC, 0xBD, 0xBE,
)
_CGK2U_CHO = (
    0, 1, _NONE, 2, _NONE, _NONE, 3, 4,
    5, _NONE, _NONE, _NONE, _NONE, _NONE, _NONE, _NONE,
    6, 7, 8, _NONE, 9, 10, 11, 12,
    13, 14, 15, 16, 17, 18,
)
_CGK2U_JONG = (
    1, 2, 3, 4, 5, 6, 7, _NONE,
    8, 9, 10, 11, 12, 13, 14, 15,
    16, 17, _NONE, 18, 19, 20, 21, 22,
    _NONE, 23, 24, 25, 26, 27,
)


def _euckr_special(data, i, n):
    """KS X 1001:1998 make-up sequence: A4 D4 A4 cho A4 jung A4 jong."""
    if data[i] != 0xA4 or i + 1 >= n or data[i + 1] != 0xD4:
        return None
    if i + 8 > n:
        return ("toofew",)
    if data[i + 2] != 0xA4 or data[i + 4] != 0xA4 or data[i + 6] != 0xA4:
        return ("err", 1)
    c = data[i + 3]
    cho = _CGK2U_CHO[c - 0xA1] if 0xA1 <= c <= 0xBE else _NONE
    c = data[i + 5]
    jung = (c - 0xBF) if 0xBF <= c <= 0xD3 else _NONE
    c = data[i + 7]
    if c == 0xD4:
        jong = 0
    elif 0xA1 <= c <= 0xBE:
        jong = _CGK2U_JONG[c - 0xA1]
    else:
        jong = _NONE
    if cho == _NONE or jung == _NONE or jong == _NONE:
        return ("err", 1)
    return ("out", chr(0xAC00 + cho * 588 + jung * 28 + jong), 8)


def _euckr_enc_char(c, c2, final):
    b = _enc_map("euc_kr").get(c)
    if b is not None:
        return (b, 1)
    if 0xAC00 <= c <= 0xD7A3:
        # CP949-extension syllable -> 8-byte jamo make-up sequence.
        v = c - 0xAC00
        return (
            bytes(
                (
                    0xA4, 0xD4,
                    0xA4, _U2CGK_CHO[v // 588],
                    0xA4, _U2CGK_JUNG[(v // 28) % 21],
                    0xA4, _U2CGK_JONG[v % 28],
                )
            ),
            1,
        )
    return None


# ---------------------------------------------------------------------------
# EUC-JP (SS3 plane) / Shift_JIS / CP932
# ---------------------------------------------------------------------------

def _eucjp_special(data, i, n):
    if data[i] != 0x8F:
        return None
    if i + 3 > n:
        return ("toofew",)
    c2, c3 = data[i + 1], data[i + 2]
    if 0xA1 <= c2 <= 0xFE and 0xA1 <= c3 <= 0xFE:
        off = ((c2 - 0xA1) * 94 + (c3 - 0xA1)) << 1
        ss3 = _t.EUC_JP_DEC_SS3
        v = ss3[off] | (ss3[off + 1] << 8)
        if v != _U16_UNDEF:
            return ("out", chr(v), 3)
    return ("err", 1)


def _sjis_single(c):
    if 0xA1 <= c <= 0xDF:
        return chr(0xFEC0 + c)
    return None


def _sjis_needs2(c):
    return (0x81 <= c <= 0x9F) or (0xE0 <= c <= 0xEA)


def _sjis_enc_char(c, c2, final):
    if 0xFF61 <= c <= 0xFF9F:
        return (bytes((c - 0xFEC0,)), 1)
    b = _enc_map("shift_jis").get(c)
    return (b, 1) if b is not None else None


def _cp932_single(c):
    if c == 0x80:
        return "\x80"
    if c == 0xA0:
        return "\uf8f0"
    if 0xA1 <= c <= 0xDF:
        return chr(0xFEC0 + c)
    if c >= 0xFD:
        return chr(0xF8F1 - 0xFD + c)
    return None


def _cp932_enc_char(c, c2, final):
    if c == 0x80:
        return (b"\x80", 1)
    if 0xFF61 <= c <= 0xFF9F:
        return (bytes((c - 0xFEC0,)), 1)
    if c == 0xF8F0:
        return (b"\xa0", 1)
    if 0xF8F1 <= c <= 0xF8F3:
        return (bytes((c - 0xF8F1 + 0xFD,)), 1)
    b = _enc_map("cp932").get(c)
    if b is not None:
        return (b, 1)
    if 0xE000 <= c < 0xE758:
        c1 = (c - 0xE000) // 188
        t = (c - 0xE000) % 188
        return (bytes((c1 + 0xF0, t + 0x40 if t < 0x3F else t + 0x41)), 1)
    return None


# ---------------------------------------------------------------------------
# GB18030 4-byte linear forms
# ---------------------------------------------------------------------------

def _gb18030_special(data, i, n):
    if i + 1 >= n:
        return None  # generic path: needs2 -> toofew
    c2 = data[i + 1]
    if not (0x30 <= c2 <= 0x39):
        return None
    if i + 4 > n:
        return ("toofew",)
    c, c3, c4 = data[i], data[i + 2], data[i + 3]
    if not (0x81 <= c <= 0xFE and 0x81 <= c3 <= 0xFE and 0x30 <= c4 <= 0x39):
        return ("err", 1)
    a = c - 0x81
    if a < 4:  # U+0080 - U+FFFF
        lseq = ((a * 10 + (c2 - 0x30)) * 1260 + (c3 - 0x81) * 10 + (c4 - 0x30))
        if lseq < 39420:
            for first, last, base in _t.GB18030_RANGES:
                if base <= lseq <= base + (last - first):
                    return ("out", chr(first - base + lseq), 4)
    elif a >= 15:  # U+10000 - U+10FFFF
        lseq = 0x10000 + ((a - 15) * 10 + (c2 - 0x30)) * 1260 \
            + (c3 - 0x81) * 10 + (c4 - 0x30)
        if lseq <= 0x10FFFF:
            return ("out", chr(lseq), 4)
    return ("err", 1)


def _gb18030_enc_char(c, c2, final):
    b = _enc_map("gb18030").get(c)
    if b is not None:
        return (b, 1)
    if c >= 0x10000:
        tc = c - 0x10000
        b4 = tc % 10 + 0x30
        tc //= 10
        b3 = tc % 126 + 0x81
        tc //= 126
        b2 = tc % 10 + 0x30
        tc //= 10
        return (bytes((tc + 0x90, b2, b3, b4)), 1)
    if c >= 0xD800 and c <= 0xDFFF:
        return None
    for first, last, base in _t.GB18030_RANGES:
        if first <= c <= last:
            tc = c - first + base
            b4 = tc % 10 + 0x30
            tc //= 10
            b3 = tc % 126 + 0x81
            tc //= 126
            b2 = tc % 10 + 0x30
            tc //= 10
            return (bytes((tc + 0x81, b2, b3, b4)), 1)
    return None


# ---------------------------------------------------------------------------
# BIG5HKSCS (combining pairs; the encoder is stateful)
# ---------------------------------------------------------------------------

_HK_PAIRENC = {  # ((c >> 4) | (c2 >> 3)) & 3 -> DBCS code
    (0x00CA, 0x0304): b"\x88\x62",
    (0x00CA, 0x030C): b"\x88\x64",
    (0x00EA, 0x0304): b"\x88\xa3",
    (0x00EA, 0x030C): b"\x88\xa5",
}


def _hk_cell2(c, c2):
    v = _cell24(_t.BIG5HKSCS_DEC, c, c2)
    if v is None:
        return None
    if v == _U24_PAIR:
        a, b = _t.BIG5HKSCS_DEC_PAIRS[(c, c2)]
        return chr(a) + chr(b)
    return chr(v)


def _hk_enc_char(c, c2, final):
    if c in (0x00CA, 0x00EA):
        if c2 is None and not final:
            return "toofew"
        if c2 in (0x0304, 0x030C):
            return (_HK_PAIRENC[(c, c2)], 2)
        return (b"\x88\x66" if c == 0x00CA else b"\x88\xa7", 1)
    b = _enc_map("big5hkscs").get(c)
    return (b, 1) if b is not None else None


# ---------------------------------------------------------------------------
# codec registry
# ---------------------------------------------------------------------------

def _table_enc_char(name):
    m = _enc_map(name)

    def enc_char(c, c2, final, _m=m):
        b = _m.get(c)
        return (b, 1) if b is not None else None

    return enc_char


def _grid_cell2(name):
    grid = getattr(_t, name.upper() + "_DEC")

    def cell2(c, c2, _grid=grid):
        v = _cell16(_grid, c, c2)
        return chr(v) if v is not None else None

    return cell2


def _codec_spec(name):
    """(single, needs2, cell2, special, enc_char) for `name`."""
    if name in ("euc_kr", "cp949", "gb2312", "gbk", "big5", "cp950"):
        single = None
        needs2 = _always2
        cell2 = _grid_cell2(name)
        special = _euckr_special if name == "euc_kr" else None
        enc_char = _euckr_enc_char if name == "euc_kr" else _table_enc_char(name)
        return (single, needs2, cell2, special, enc_char)
    if name == "euc_jp":
        return (None, _always2, _grid_cell2(name), _eucjp_special,
                _table_enc_char(name))
    if name == "shift_jis":
        return (_sjis_single, _sjis_needs2, _grid_cell2(name), None,
                _sjis_enc_char)
    if name == "cp932":
        return (_cp932_single, _always2, _grid_cell2(name), None,
                _cp932_enc_char)
    if name == "gb18030":
        return (None, _always2, _grid_cell2(name), _gb18030_special,
                _gb18030_enc_char)
    if name == "big5hkscs":
        return (None, _always2, _hk_cell2, None, _hk_enc_char)
    raise LookupError("unknown encoding: " + name)


def _check_decode_input(name, input):
    if not isinstance(input, (bytes, bytearray, memoryview)):
        raise TypeError(
            "decode() argument 'data' must be a bytes-like object, not %s"
            % type(input).__name__
        )
    return bytes(input)


_REGENTRY_CACHE = {}


def getregentry(name):
    info = _REGENTRY_CACHE.get(name)
    if info is not None:
        return info

    single, needs2, cell2, special, enc_char = _codec_spec(name)

    def encode(input, errors="strict", _name=name):
        out, _pending = _mb_encode(_name, input, errors, True, enc_char)
        return (out, len(input))

    def decode(input, errors="strict", _name=name):
        data = _check_decode_input(_name, input)
        text, _consumed = _mb_decode(
            _name, data, errors, True, single, needs2, cell2, special
        )
        return (text, len(data))

    class _IncEnc(ErrorsProperty, MbEncStateMixin, IncrementalEncoder):
        # Only big5hkscs is genuinely stateful (pair lookahead); the
        # pending-tail pattern is a no-op for every other codec.
        def __init__(self, errors="strict"):
            super().__init__(errors)
            self._pending = ""

        def encode(self, input, final=False):
            out, self._pending = _mb_encode(
                name, self._pending + input, self.errors, final, enc_char
            )
            return out

        def reset(self):
            self._pending = ""

    class _IncDec(ErrorsProperty, MbDecStateMixin, IncrementalDecoder):
        def __init__(self, errors="strict"):
            super().__init__(errors)
            self._buffer = b""
            self._flags = 0

        def decode(self, input, final=False):
            data = self._buffer + bytes(input)
            result, consumed = _mb_decode(
                name, data, self.errors, final, single, needs2, cell2, special
            )
            self._buffer = data[consumed:]
            return result

        def reset(self):
            self._buffer = b""

    class _Writer(StreamWriter):
        def __init__(self, stream, errors="strict"):
            super().__init__(stream, errors)
            self._encoder = _IncEnc(errors)

        def encode(self, input, errors="strict"):
            self._encoder.errors = errors
            return (self._encoder.encode(input, False), len(input))

        def reset(self):
            super().reset()
            enc = getattr(self, "_encoder", None)
            if enc is not None:
                try:
                    tail = enc.encode("", True)
                except UnicodeEncodeError:
                    tail = b""
                if tail:
                    self.stream.write(tail)
                enc.reset()

    class _Reader(MbStreamReaderMixin, StreamReader):
        def __init__(self, stream, errors="strict"):
            super().__init__(stream, errors)
            self._decoder = _IncDec(errors)

        def decode(self, input, errors="strict"):
            self._decoder.errors = errors
            text = self._decoder.decode(bytes(input), False)
            # Unconsumed tail bytes are buffered inside the incremental
            # decoder, so report the full input as consumed.
            return (text, len(input))

        def reset(self):
            super().reset()
            dec = getattr(self, "_decoder", None)
            if dec is not None:
                dec.reset()

    _IncEnc.__name__ = "_IncEnc_" + name
    _IncDec.__name__ = "_IncDec_" + name
    _IncDec.name = name
    _Writer.__name__ = "_StreamWriter_" + name
    _Reader.__name__ = "_StreamReader_" + name

    info = CodecInfo(
        encode=encode,
        decode=decode,
        incrementalencoder=_IncEnc,
        incrementaldecoder=_IncDec,
        streamreader=_Reader,
        streamwriter=_Writer,
        name=name,
        _is_text_encoding=True,
    )
    _REGENTRY_CACHE[name] = info
    return info
