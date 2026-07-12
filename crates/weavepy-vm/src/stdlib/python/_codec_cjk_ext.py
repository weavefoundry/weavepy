"""WeavePy frozen port of CPython's stateful CJK escape codecs.

Covers the codecs CPython implements in ``Modules/cjkcodecs`` that have no
``encoding_rs`` backend: ``hz``, the seven ``iso2022_*`` variants, ``johab``,
and ``shift_jis_2004``/``shift_jisx0213``.

The double-byte charsets are bridged onto codecs WeavePy already carries:

- JIS X 0208  -> ``euc_jp``   (two-byte cells, both bytes | 0x80)
- KS X 1001   -> ``euc_kr``   (same EUC bridging; UHC-only cells rejected)
- GB 2312     -> ``gb2312``   (same; GBK-only cells rejected)
- JIS X 0213 planes 1/2 and JIS X 0212 -> the packed tables in
  ``_codec_euc_jis_2004`` (plane 2 rows split by row number: JIS X 0213
  plane 2 owns rows 1,3-5,8,12-15,78-94; JIS X 0212 owns the complement).

State machines (ISO-2022 designations/shifts, HZ ~{..~} runs, Shift_JIS
lead-byte splits) are line-by-line ports of ``_codecs_iso2022.c``,
``_codecs_cn.c``, ``_codecs_kr.c`` and ``_codecs_jp.c``.
"""

from codecs import (
    CodecInfo,
    IncrementalEncoder,
    IncrementalDecoder,
    StreamReader,
    StreamWriter,
)

import _codec_euc_jis_2004 as _jis
from _cjk_common import (
    ErrorsProperty,
    MbDecStateMixin,
    MbEncStateMixin,
    MbStreamReaderMixin,
    dec_handle,
    dec_setstate as _dec_setstate,
    enc_getstate as _enc_getstate,
    enc_handle,
    enc_setstate as _enc_setstate,
)

_ESC = 0x1B
_SO = 0x0E
_SI = 0x0F
_LF = 0x0A

_REASON = "illegal multibyte sequence"
_REASON_INCOMPLETE = "incomplete multibyte sequence"

# JIS X 0213:2000 emulation (emu_jisx0213_2000.h): characters added in the
# 2004 revision must be rejected, and U+9B1D moved plane between revisions.
_EMU2000_ENC_REJECT = frozenset(
    (0x9B1C, 0x4FF1, 0x525D, 0x541E, 0x5653, 0x59F8, 0x5C5B, 0x5E77, 0x7626, 0x7E6B)
)
_EMU2000_DEC_REJECT_P1 = frozenset(
    ((0x2E, 0x21), (0x2F, 0x7E), (0x4F, 0x54), (0x4F, 0x7E), (0x74, 0x27),
     (0x7E, 0x7A), (0x7E, 0x7B), (0x7E, 0x7C), (0x7E, 0x7D), (0x7E, 0x7E))
)

# JIS X 0213 plane 2 rows (1,3,4,5,8,12,13,14,15,78-94 -> +0x20); every other
# row of the merged euc_jis_2004 plane-2 table belongs to JIS X 0212.
_X0213_2_ROWS = frozenset((0x21, 0x23, 0x24, 0x25, 0x28, 0x2C, 0x2D, 0x2E, 0x2F))
_X0213_2_ROWS = _X0213_2_ROWS | frozenset(range(0x6E, 0x7F))


# Error-handler plumbing lives in `_cjk_common` (the exact
# multibytecodec.c encerror/decerror protocol).
_enc_handle = enc_handle
_dec_handle = dec_handle


# ---------------------------------------------------------------------------
# charset bridges
# ---------------------------------------------------------------------------

def _euc_cell_enc(ch, backend, rows=None):
    """Encode one char through an EUC-style backend; accept only a proper
    two-byte 94x94 cell (both bytes >= 0xA1). Returns (lead, trail) in the
    0x21..0x7E range or None."""
    try:
        b = ch.encode(backend)
    except ValueError:  # UnicodeEncodeError or the engine's ValueError
        return None
    if len(b) == 2 and b[0] >= 0xA1 and b[1] >= 0xA1:
        cell = (b[0] & 0x7F, b[1] & 0x7F)
        if rows is not None and cell[0] not in rows:
            return None
        return cell
    return None


def _euc_cell_dec(c1, c2, backend, rows=None):
    """Decode a 94x94 cell through an EUC-style backend -> str or None."""
    if rows is not None and c1 not in rows:
        return None
    try:
        s = bytes((c1 | 0x80, c2 | 0x80)).decode(backend)
    except ValueError:
        return None
    return s if len(s) == 1 else None


# CPython's jisx0208 map covers only the rows JIS X 0208 itself defines
# (1-8 symbols/kana, 16-84 kanji); the WHATWG index behind the euc_jp
# backend additionally carries the NEC row-13 and IBM row-89..92 extensions,
# which must stay invisible here.
_JISX0208_ROWS = frozenset(range(0x21, 0x29)) | frozenset(range(0x30, 0x75))
# GB 2312 defines rows 1-9 and 16-87; the gbk backend knows more.
_GB2312_ROWS = frozenset(range(0x21, 0x2A)) | frozenset(range(0x30, 0x78))


# --- JIS X 0213 tables (shared with the euc_jis_2004 frozen port) ----------

_P1_ENC = None  # {codepoint: (l, t)} plane 1 (merged 0208+0213-1)
_P2_ENC = None  # {codepoint: (l, t)} plane 2 (0213-2 rows only)
_X0212_ENC = None  # {codepoint: (l, t)} JIS X 0212 rows of the plane-2 table
_PAIR_ENC = None  # {(base, mark): (l, t)} combining pairs (plane 1)
_BASE_LONE = None  # {base: (l, t)} lone cell for each combining base


def _build_tables():
    global _P1_ENC, _P2_ENC, _X0212_ENC, _PAIR_ENC, _BASE_LONE
    if _P1_ENC is not None:
        return
    p1 = {}
    dec1 = _jis._DEC1
    for idx in range(len(dec1)):
        v = dec1[idx]
        if v >= 0x110000:
            continue
        cell = (0x21 + idx // 94, 0x21 + idx % 94)
        if cell == (0x22, 0x32):
            # euc_jis_2004 decodes 1-2-18 as U+FF5E; the layered
            # jisx0208/jisx0213 view used by iso2022/shift_jis gives U+007E,
            # and neither maps FF5E back on encode.
            continue
        p1.setdefault(v, cell)
    p2 = {}
    x0212 = {}
    dec2 = _jis._DEC2
    for idx in range(len(dec2)):
        v = dec2[idx]
        if v >= 0x110000:
            continue
        cell = (0x21 + idx // 94, 0x21 + idx % 94)
        if cell[0] in _X0213_2_ROWS:
            p2.setdefault(v, cell)
        else:
            x0212.setdefault(v, cell)
    pair = {}
    lone = {}
    for k, cp_pair in _jis._COMB.items():
        cell = (0x21 + k // 94, 0x21 + k % 94)
        pair[cp_pair] = cell
    for base in _jis._COMB_BASES:
        b = _jis._ENC.get(chr(base))
        if b is not None and len(b) == 2:
            lone[base] = (b[0] & 0x7F, b[1] & 0x7F)
    _P1_ENC, _P2_ENC, _X0212_ENC, _PAIR_ENC, _BASE_LONE = p1, p2, x0212, pair, lone


def _x0213_p1_dec(c1, c2, y2000):
    """JIS X 0213 plane-1 layered decode (jisx0208 first) -> str or None."""
    if y2000 and (c1, c2) in _EMU2000_DEC_REJECT_P1:
        return None
    if c1 == 0x22 and c2 == 0x32:
        return "\x7e"
    if not (0x21 <= c1 <= 0x7E and 0x21 <= c2 <= 0x7E):
        return None
    idx = (c1 - 0x21) * 94 + (c2 - 0x21)
    v = _jis._DEC1[idx]
    if v == 0xFFFFFE:
        a, b = _jis._COMB[idx]
        return chr(a) + chr(b)
    if v >= 0x110000:
        return None
    return chr(v)


def _x0213_p2_dec(c1, c2, y2000):
    """JIS X 0213 plane-2 decode -> str or None (0212 rows excluded)."""
    if y2000 and c1 == 0x7D and c2 == 0x3B:
        return "\u9b1d"
    if c1 not in _X0213_2_ROWS or not (0x21 <= c2 <= 0x7E):
        return None
    v = _jis._DEC2[(c1 - 0x21) * 94 + (c2 - 0x21)]
    if v >= 0x110000:
        return None
    return chr(v)


def _x0212_dec(c1, c2):
    if c1 in _X0213_2_ROWS or not (0x21 <= c1 <= 0x7E and 0x21 <= c2 <= 0x7E):
        return None
    v = _jis._DEC2[(c1 - 0x21) * 94 + (c2 - 0x21)]
    if v >= 0x110000:
        return None
    return chr(v)


# ---------------------------------------------------------------------------
# ISO-2022 designation charsets
# ---------------------------------------------------------------------------
#
# Each charset is a tuple: (dbcs, letter, width, encoder, decoder)
#   encoder(c, c2, final) -> None (unmappable), "toofew",
#       (cell, 1) or (cell, 2)  -- cell is an int byte (width 1) or
#       an (l, t) tuple (width 2); the second item is chars consumed.
#   decoder(bytes...) -> str or None. ``None`` encoder/decoder marks a
#   decode-only (G2) or dummy charset.

def _cs_ascii_dummy():
    return (False, "B", 1, None, None)


def _enc_jisx0208(c, c2, final):
    if c > 0xFFFF:
        return None
    cell = _euc_cell_enc(chr(c), "euc_jp", _JISX0208_ROWS)
    return (cell, 1) if cell is not None else None


def _dec_jisx0208(c1, c2):
    return _euc_cell_dec(c1, c2, "euc_jp", _JISX0208_ROWS)


def _enc_jisx0212(c, c2, final):
    if c > 0xFFFF:
        return None
    _build_tables()
    cell = _X0212_ENC.get(c)
    return (cell, 1) if cell is not None else None


def _dec_jisx0212(c1, c2):
    return _x0212_dec(c1, c2)


def _enc_ksx1001(c, c2, final):
    if c > 0xFFFF:
        return None
    cell = _euc_cell_enc(chr(c), "euc_kr")
    return (cell, 1) if cell is not None else None


def _dec_ksx1001(c1, c2):
    return _euc_cell_dec(c1, c2, "euc_kr")


def _enc_gb2312(c, c2, final):
    if c > 0xFFFF:
        return None
    cell = _euc_cell_enc(chr(c), "gb2312", _GB2312_ROWS)
    return (cell, 1) if cell is not None else None


def _dec_gb2312(c1, c2):
    return _euc_cell_dec(c1, c2, "gb2312", _GB2312_ROWS)


def _enc_jisx0201_r(c, c2, final):
    if c == 0x00A5:
        return (0x5C, 1)
    if c == 0x203E:
        return (0x7E, 1)
    return None


def _dec_jisx0201_r(c1):
    if c1 == 0x5C:
        return "\u00a5"
    if c1 == 0x7E:
        return "\u203e"
    if c1 <= 0x7F:
        return chr(c1)
    return None


def _enc_jisx0201_k(c, c2, final):
    if 0xFF61 <= c <= 0xFF9F:
        return (c - 0xFEC0 - 0x80, 1)
    return None


def _dec_jisx0201_k(c1):
    if 0x21 <= c1 <= 0x5F:
        return chr(0xFF40 + c1)
    return None


def _x0213_encoder(c, c2, final, y2000, plane, paironly):
    """Port of jisx0213_encoder + the plane/paironly wrappers."""
    _build_tables()
    if c > 0xFFFF:
        if paironly or (c >> 16) != 0x2:
            return None
        if y2000 and c == 0x20B9F:
            return None
        cell = (_P1_ENC if plane == 1 else _P2_ENC).get(c)
        return (cell, 1) if cell is not None else None
    if y2000:
        if c in _EMU2000_ENC_REJECT:
            return None
        if c == 0x9B1D:
            return ((0x7D, 0x3B), 1) if plane == 2 else None
    if c in _jis._COMB_BASES:
        if c2 is None:
            if not final:
                return "toofew"
            # flush: paironly never encodes a lone base (ilength == -1 path)
            if paironly or plane != 1:
                return None
            cell = _BASE_LONE.get(c)
            return (cell, 1) if cell is not None else None
        cell = _PAIR_ENC.get((c, c2))
        if cell is not None:
            return (cell, 2) if plane == 1 else None
        if paironly or plane != 1:
            return None
        cell = _BASE_LONE.get(c)
        return (cell, 1) if cell is not None else None
    if paironly:
        return None
    cell = (_P1_ENC if plane == 1 else _P2_ENC).get(c)
    return (cell, 1) if cell is not None else None


def _mk_x0213_charset(letter, y2000, plane, paironly):
    def enc(c, c2, final):
        return _x0213_encoder(c, c2, final, y2000, plane, paironly)

    def dec(c1, c2):
        if plane == 1:
            return _x0213_p1_dec(c1, c2, y2000)
        return _x0213_p2_dec(c1, c2, y2000)

    return (True, letter, 2, enc, dec)


_CS_JISX0208 = (True, "B", 2, _enc_jisx0208, _dec_jisx0208)
_CS_JISX0208_O = (True, "@", 2, _enc_jisx0208, _dec_jisx0208)
_CS_JISX0212 = (True, "D", 2, _enc_jisx0212, _dec_jisx0212)
_CS_KSX1001 = (True, "C", 2, _enc_ksx1001, _dec_ksx1001)
_CS_GB2312 = (True, "A", 2, _enc_gb2312, _dec_gb2312)
_CS_JISX0201_R = (False, "J", 1, _enc_jisx0201_r, _dec_jisx0201_r)
_CS_JISX0201_K = (False, "I", 1, _enc_jisx0201_k, _dec_jisx0201_k)
_CS_ISO8859_1 = (False, "A", 1, None, None)
_CS_ISO8859_7 = (False, "F", 1, None, None)
_CS_X0213_2000_1 = _mk_x0213_charset("O", True, 1, False)
_CS_X0213_2000_1_PAIR = _mk_x0213_charset("O", True, 1, True)
_CS_X0213_2000_2 = _mk_x0213_charset("P", True, 2, False)
_CS_X0213_2004_1 = _mk_x0213_charset("Q", False, 1, False)
_CS_X0213_2004_1_PAIR = _mk_x0213_charset("Q", False, 1, True)
_CS_X0213_2004_2 = _mk_x0213_charset("P", False, 2, False)

# iso2022_config flags
_NO_SHIFT = 0x01
_USE_G2 = 0x02
_USE_JISX0208_EXT = 0x04

# name -> (flags, ((charset, plane), ...))
_ISO2022_CONFIGS = {
    "iso2022_kr": (0, ((_CS_KSX1001, 1),)),
    "iso2022_jp": (
        _NO_SHIFT | _USE_JISX0208_EXT,
        ((_CS_JISX0208, 0), (_CS_JISX0201_R, 0), (_CS_JISX0208_O, 0)),
    ),
    "iso2022_jp_1": (
        _NO_SHIFT | _USE_JISX0208_EXT,
        ((_CS_JISX0208, 0), (_CS_JISX0212, 0), (_CS_JISX0201_R, 0),
         (_CS_JISX0208_O, 0)),
    ),
    "iso2022_jp_2": (
        _NO_SHIFT | _USE_G2 | _USE_JISX0208_EXT,
        ((_CS_JISX0208, 0), (_CS_JISX0212, 0), (_CS_KSX1001, 0),
         (_CS_GB2312, 0), (_CS_JISX0201_R, 0), (_CS_JISX0208_O, 0),
         (_CS_ISO8859_1, 2), (_CS_ISO8859_7, 2)),
    ),
    "iso2022_jp_2004": (
        _NO_SHIFT | _USE_JISX0208_EXT,
        ((_CS_X0213_2004_1_PAIR, 0), (_CS_JISX0208, 0),
         (_CS_X0213_2004_1, 0), (_CS_X0213_2004_2, 0)),
    ),
    "iso2022_jp_3": (
        _NO_SHIFT | _USE_JISX0208_EXT,
        ((_CS_X0213_2000_1_PAIR, 0), (_CS_JISX0208, 0),
         (_CS_X0213_2000_1, 0), (_CS_X0213_2000_2, 0)),
    ),
    "iso2022_jp_ext": (
        _NO_SHIFT | _USE_JISX0208_EXT,
        ((_CS_JISX0208, 0), (_CS_JISX0212, 0), (_CS_JISX0201_R, 0),
         (_CS_JISX0201_K, 0), (_CS_JISX0208_O, 0)),
    ),
}

_ASCII_MARK = (False, "B")


def _charset_key(cs):
    return (cs[0], cs[1])


# ---------------------------------------------------------------------------
# ISO-2022 encoder / decoder cores
# ---------------------------------------------------------------------------

class _Iso2022State:
    """Mutable G0..G3 designations + shift/esc-throughout flags.

    ``getstate`` packs each designation slot into one byte exactly like
    ``_codecs_iso2022.c``: the designation final character, OR'd with 0x80
    for a multibyte charset; 0 means "never designated" (which is why a
    fresh *encoder* re-emits ``ESC ( B`` after ``setstate(0)`` even though
    its natural initial G0 is ASCII)."""

    __slots__ = ("g", "shifted", "escthrough")

    def __init__(self, decoder=False):
        # ENCODER_INIT designates G0/G1 only; DECODER_INIT also sets G2.
        self.g = [
            _ASCII_MARK,
            _ASCII_MARK,
            _ASCII_MARK if decoder else None,
            None,
        ]
        self.shifted = False
        self.escthrough = False


def _mark_byte(mark):
    if mark is None:
        return 0
    return ord(mark[1]) | (0x80 if mark[0] else 0)


def _byte_mark(b):
    if b == 0:
        return None
    return (bool(b & 0x80), chr(b & 0x7F))


def _iso2022_encode_core(name, s, errors, final, state):
    flags, desigs = _ISO2022_CONFIGS[name]
    out = bytearray()
    pending = ""
    chars = s
    i = 0
    n = len(chars)
    while i < n:
        c = ord(chars[i])
        if c < 0x80:
            if state.g[0] != _ASCII_MARK:
                out += b"\x1b(B"
                state.g[0] = _ASCII_MARK
            if state.shifted:
                out.append(_SI)
                state.shifted = False
            out.append(c)
            i += 1
            continue

        c2 = ord(chars[i + 1]) if i + 1 < n else None
        encoded = None
        consumed = 1
        hit = None
        toofew = False
        for cs, plane in desigs:
            enc = cs[3]
            if enc is None:
                continue
            r = enc(c, c2, final)
            if r == "toofew":
                toofew = True
                break
            if r is not None:
                encoded, consumed = r
                hit = (cs, plane)
                break
        if toofew:
            pending = chars[i:]
            break
        if hit is None:
            end = i + 1
            rep, newpos = _enc_handle(name, errors, chars, i, end)
            if isinstance(rep, bytes):
                out += rep
                i = newpos
                continue
            if rep:
                # Replacement text is encoded through this same state machine
                # with strict errors (CPython re-encodes the callback result
                # via the codec and errors out if it is unencodable).
                sub, _ = _iso2022_encode_core(name, rep, "strict", True, state)
                out += sub
            i = newpos
            continue

        cs, plane = hit
        dbcs, letter, width = cs[0], cs[1], cs[2]
        mark = (dbcs, letter)
        if plane == 0:
            if state.shifted:
                out.append(_SI)
                state.shifted = False
            if state.g[0] != mark:
                if width == 1:
                    out += b"\x1b(" + letter.encode("ascii")
                elif mark == (True, "B"):
                    out += b"\x1b$B"
                else:
                    out += b"\x1b$(" + letter.encode("ascii")
                state.g[0] = mark
        else:  # plane 1 (iso2022_kr)
            if state.g[1] != mark:
                if width == 1:
                    out += b"\x1b)" + letter.encode("ascii")
                else:
                    out += b"\x1b$)" + letter.encode("ascii")
                state.g[1] = mark
            if not state.shifted:
                out.append(_SO)
                state.shifted = True

        if width == 1:
            out.append(encoded)
        else:
            out.append(encoded[0])
            out.append(encoded[1])
        i += consumed

    if final:
        if state.shifted:
            out.append(_SI)
            state.shifted = False
        if state.g[0] != _ASCII_MARK:
            out += b"\x1b(B"
            state.g[0] = _ASCII_MARK
    return (bytes(out), pending)


_IS_ESCEND = frozenset(range(ord("A"), ord("Z") + 1)) | {ord("@")}
_ISO2022ESC_2ND = frozenset(b"()$.&")


def _iso2022_process_esc(name, data, i, final, state):
    """Port of iso2022processesc. Returns ('ok', consumed), ('toofew', 0) or
    ('err', length)."""
    flags, desigs = _ISO2022_CONFIGS[name]
    n = len(data)
    esclen = 0
    j = 1
    while j < 16:
        if i + j >= n:
            if not final:
                return ("toofew", 0)
            return ("err", n - i)
        b = data[i + j]
        if b in _IS_ESCEND:
            esclen = j + 1
            break
        if (flags & _USE_JISX0208_EXT) and i + j + 1 < n \
                and b == 0x26 and data[i + j + 1] == 0x40:
            j += 2
        j += 1

    if esclen == 0:
        return ("err", 1)
    if esclen == 3:
        if data[i + 1] == 0x24:  # '$'
            charset = (True, chr(data[i + 2]))
            slot = 0
        else:
            charset = (False, chr(data[i + 2]))
            if data[i + 1] == 0x28:
                slot = 0
            elif data[i + 1] == 0x29:
                slot = 1
            elif (flags & _USE_G2) and data[i + 1] == 0x2E:
                slot = 2
            else:
                return ("err", 3)
    elif esclen == 4:
        if data[i + 1] != 0x24:
            return ("err", 4)
        charset = (True, chr(data[i + 3]))
        if data[i + 2] == 0x28:
            slot = 0
        elif data[i + 2] == 0x29:
            slot = 1
        else:
            return ("err", 4)
    elif esclen == 6:
        if (flags & _USE_JISX0208_EXT) and data[i + 3] == _ESC \
                and data[i + 4] == 0x24 and data[i + 5] == 0x42:
            charset = (True, "B")
            slot = 0
        else:
            return ("err", 6)
    else:
        return ("err", esclen)

    if charset != _ASCII_MARK:
        for cs, _plane in desigs:
            if _charset_key(cs) == charset:
                break
        else:
            return ("err", esclen)
    state.g[slot] = charset
    return ("ok", esclen)


def _iso2022_process_g2(data, i, state):
    """Port of iso2022processg2 (SS2, ESC N x). Returns str or None."""
    b = data[i + 2]
    g2 = state.g[2]
    if g2 == (False, "A"):  # iso8859-1
        if b < 0x80:
            return chr(b + 0x80)
        return None
    if g2 == (False, "F"):  # iso8859-7
        try:
            return bytes((b | 0x80,)).decode("iso8859_7")
        except UnicodeDecodeError:
            return None
    if g2 == _ASCII_MARK:
        if b & 0x80:
            return None
        return chr(b)
    return None


def _iso2022_decode_core(name, data, errors, final, state):
    flags, desigs = _ISO2022_CONFIGS[name]
    out = []
    i = 0
    n = len(data)
    while i < n:
        c = data[i]

        if state.escthrough:
            # Non-iso2022 escape sequence: pass bytes through until the
            # sequence terminator.
            out.append(chr(c))
            i += 1
            if c in _IS_ESCEND:
                state.escthrough = False
            continue

        if c == _ESC:
            if i + 1 >= n:
                if not final:
                    break
                rep, i = _dec_handle(name, errors, data, i, n, _REASON_INCOMPLETE)
                out.append(rep)
                continue
            if data[i + 1] in _ISO2022ESC_2ND:
                status, length = _iso2022_process_esc(name, data, i, final, state)
                if status == "toofew":
                    break
                if status == "err":
                    rep, i = _dec_handle(name, errors, data, i, i + length, _REASON)
                    out.append(rep)
                    continue
                i += length
                continue
            if (flags & _USE_G2) and data[i + 1] == 0x4E:  # 'N' (SS2)
                if i + 2 >= n:
                    if not final:
                        break
                    rep, i = _dec_handle(name, errors, data, i, n, _REASON_INCOMPLETE)
                    out.append(rep)
                    continue
                decoded = _iso2022_process_g2(data, i, state)
                if decoded is None:
                    rep, i = _dec_handle(name, errors, data, i, i + 3, _REASON)
                    out.append(rep)
                    continue
                out.append(decoded)
                i += 3
                continue
            out.append("\x1b")
            state.escthrough = True
            i += 1
            continue

        if c == _SI and not (flags & _NO_SHIFT):
            state.shifted = False
            i += 1
            continue
        if c == _SO and not (flags & _NO_SHIFT):
            state.shifted = True
            i += 1
            continue
        if c == _LF:
            state.shifted = False
            out.append("\n")
            i += 1
            continue
        if c < 0x20:
            out.append(chr(c))
            i += 1
            continue
        if c >= 0x80:
            rep, i = _dec_handle(name, errors, data, i, i + 1, _REASON)
            out.append(rep)
            continue

        charset = state.g[1] if state.shifted else state.g[0]
        if charset == _ASCII_MARK:
            out.append(chr(c))
            i += 1
            continue

        for cs, _plane in desigs:
            if _charset_key(cs) == charset:
                break
        else:  # pragma: no cover - designation validation forbids this
            rep, i = _dec_handle(name, errors, data, i, i + 1, _REASON)
            out.append(rep)
            continue

        width = cs[2]
        dec = cs[4]
        if i + width > n:
            if not final:
                break
            rep, i = _dec_handle(name, errors, data, i, n, _REASON_INCOMPLETE)
            out.append(rep)
            continue
        if dec is None:
            decoded = None
        elif width == 2:
            decoded = dec(data[i], data[i + 1])
        else:
            decoded = dec(data[i])
        if decoded is None:
            rep, i = _dec_handle(name, errors, data, i, i + width, _REASON)
            out.append(rep)
            continue
        out.append(decoded)
        i += width

    return ("".join(out), i)


# ---------------------------------------------------------------------------
# HZ (HZ-GB-2312) encoder / decoder cores
# ---------------------------------------------------------------------------

def _hz_encode_core(s, errors, final, state):
    """state: [in_gb_mode]"""
    out = bytearray()
    i = 0
    n = len(s)
    while i < n:
        c = ord(s[i])
        if c < 0x80:
            if state[0]:
                out += b"~}"
                state[0] = 0
            out.append(c)
            if c == 0x7E:
                out.append(0x7E)
            i += 1
            continue
        cell = _euc_cell_enc(s[i], "gb2312", _GB2312_ROWS) if c <= 0xFFFF else None
        if cell is None:
            rep, newpos = _enc_handle("hz", errors, s, i, i + 1)
            if isinstance(rep, bytes):
                out += rep
                i = newpos
                continue
            if rep:
                sub, _ = _hz_encode_core(rep, "strict", True, state)
                out += sub
            i = newpos
            continue
        if not state[0]:
            out += b"~{"
            state[0] = 1
        out.append(cell[0])
        out.append(cell[1])
        i += 1
    if final and state[0]:
        out += b"~}"
        state[0] = 0
    return (bytes(out), "")


def _hz_decode_core(data, errors, final, state):
    out = []
    i = 0
    n = len(data)
    while i < n:
        c = data[i]
        if c == 0x7E:  # '~'
            if i + 1 >= n:
                if not final:
                    break
                rep, i = _dec_handle("hz", errors, data, i, n, _REASON_INCOMPLETE)
                out.append(rep)
                continue
            c2 = data[i + 1]
            if c2 == 0x7E and not state[0]:
                out.append("~")
            elif c2 == 0x7B and not state[0]:
                state[0] = 1
            elif c2 == 0x0A and not state[0]:
                pass  # line continuation
            elif c2 == 0x7D and state[0]:
                state[0] = 0
            else:
                rep, i = _dec_handle("hz", errors, data, i, i + 1, _REASON)
                out.append(rep)
                continue
            i += 2
            continue
        if c & 0x80:
            rep, i = _dec_handle("hz", errors, data, i, i + 1, _REASON)
            out.append(rep)
            continue
        if not state[0]:
            out.append(chr(c))
            i += 1
            continue
        if i + 1 >= n:
            if not final:
                break
            rep, i = _dec_handle("hz", errors, data, i, n, _REASON_INCOMPLETE)
            out.append(rep)
            continue
        decoded = _euc_cell_dec(c, data[i + 1], "gb2312", _GB2312_ROWS)
        if decoded is None:
            # cjkcodecs decoders flag one byte per error (`return 1`)
            rep, i = _dec_handle("hz", errors, data, i, i + 1, _REASON)
            out.append(rep)
            continue
        out.append(decoded)
        i += 2
    return ("".join(out), i)


# ---------------------------------------------------------------------------
# JOHAB encoder / decoder (stateless; port of _codecs_kr.c)
# ---------------------------------------------------------------------------

_JOHAB_U2IDX_CHO = (
    0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
    0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
    0x10, 0x11, 0x12, 0x13, 0x14,
)
_JOHAB_U2IDX_JUNG = (
    0x03, 0x04, 0x05, 0x06, 0x07,
    0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
    0x12, 0x13, 0x14, 0x15, 0x16, 0x17,
    0x1A, 0x1B, 0x1C, 0x1D,
)
_JOHAB_U2IDX_JONG = (
    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
    0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
    0x10, 0x11, 0x13, 0x14, 0x15, 0x16, 0x17,
    0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D,
)
_JOHAB_U2JAMO = (
    0x8841, 0x8C41, 0x8444, 0x9041, 0x8446, 0x8447, 0x9441,
    0x9841, 0x9C41, 0x844A, 0x844B, 0x844C, 0x844D, 0x844E, 0x844F,
    0x8450, 0xA041, 0xA441, 0xA841, 0x8454, 0xAC41, 0xB041, 0xB441,
    0xB841, 0xBC41, 0xC041, 0xC441, 0xC841, 0xCC41, 0xD041, 0x8461,
    0x8481, 0x84A1, 0x84C1, 0x84E1, 0x8541, 0x8561, 0x8581, 0x85A1,
    0x85C1, 0x85E1, 0x8641, 0x8661, 0x8681, 0x86A1, 0x86C1, 0x86E1,
    0x8741, 0x8761, 0x8781, 0x87A1,
)

_J_FILL = 0xFD
_J_NONE = 0xFF
_JOHAB_IDX_CHO = (
    _J_NONE, _J_FILL, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05,
    0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D,
    0x0E, 0x0F, 0x10, 0x11, 0x12, _J_NONE, _J_NONE, _J_NONE,
    _J_NONE, _J_NONE, _J_NONE, _J_NONE, _J_NONE, _J_NONE, _J_NONE, _J_NONE,
)
_JOHAB_IDX_JUNG = (
    _J_NONE, _J_NONE, _J_FILL, 0x00, 0x01, 0x02, 0x03, 0x04,
    _J_NONE, _J_NONE, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A,
    _J_NONE, _J_NONE, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10,
    _J_NONE, _J_NONE, 0x11, 0x12, 0x13, 0x14, _J_NONE, _J_NONE,
)
_JOHAB_IDX_JONG = (
    _J_NONE, _J_FILL, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06,
    0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
    0x0F, 0x10, _J_NONE, 0x11, 0x12, 0x13, 0x14, 0x15,
    0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, _J_NONE, _J_NONE,
)
_JOHAB_JAMO_CHO = (
    _J_NONE, _J_FILL, 0x31, 0x32, 0x34, 0x37, 0x38, 0x39,
    0x41, 0x42, 0x43, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4A, 0x4B, 0x4C, 0x4D, 0x4E, _J_NONE, _J_NONE, _J_NONE,
    _J_NONE, _J_NONE, _J_NONE, _J_NONE, _J_NONE, _J_NONE, _J_NONE, _J_NONE,
)
_JOHAB_JAMO_JUNG = (
    _J_NONE, _J_NONE, _J_FILL, 0x4F, 0x50, 0x51, 0x52, 0x53,
    _J_NONE, _J_NONE, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59,
    _J_NONE, _J_NONE, 0x5A, 0x5B, 0x5C, 0x5D, 0x5E, 0x5F,
    _J_NONE, _J_NONE, 0x60, 0x61, 0x62, 0x63, _J_NONE, _J_NONE,
)
_JOHAB_JAMO_JONG = (
    _J_NONE, _J_FILL, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36,
    0x37, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x3F,
    0x40, 0x41, _J_NONE, 0x42, 0x44, 0x45, 0x46, 0x47,
    0x48, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, _J_NONE, _J_NONE,
)


def _johab_encode_char(c):
    """-> (b1, b2) or None."""
    if 0xAC00 <= c <= 0xD7A3:
        v = c - 0xAC00
        code = (
            0x8000
            | (_JOHAB_U2IDX_CHO[v // 588] << 10)
            | (_JOHAB_U2IDX_JUNG[(v // 28) % 21] << 5)
            | _JOHAB_U2IDX_JONG[v % 28]
        )
        return (code >> 8, code & 0xFF)
    if 0x3131 <= c <= 0x3163:
        code = _JOHAB_U2JAMO[c - 0x3131]
        return (code >> 8, code & 0xFF)
    if c > 0xFFFF:
        return None
    cell = _euc_cell_enc(chr(c), "euc_kr")
    if cell is None:
        return None
    c1, c2 = cell
    if ((0x21 <= c1 <= 0x2C) or (0x4A <= c1 <= 0x7D)) and 0x21 <= c2 <= 0x7E:
        t1 = (c1 - 0x21 + 0x1B2) if c1 < 0x4A else (c1 - 0x21 + 0x197)
        t2 = ((0x5E if (t1 & 1) else 0) + (c2 - 0x21)) & 0xFF
        b1 = t1 >> 1
        b2 = t2 + 0x31 if t2 < 0x4E else t2 + 0x43
        return (b1 & 0xFF, b2 & 0xFF)
    return None


def _johab_decode_cell(c, c2):
    """-> str or None (c is the lead byte, 0x80..0xFF)."""
    if c < 0xD8:
        c_cho = (c >> 2) & 0x1F
        c_jung = ((c << 3) | (c2 >> 5)) & 0x1F
        c_jong = c2 & 0x1F
        i_cho = _JOHAB_IDX_CHO[c_cho]
        i_jung = _JOHAB_IDX_JUNG[c_jung]
        i_jong = _JOHAB_IDX_JONG[c_jong]
        if _J_NONE in (i_cho, i_jung, i_jong):
            return None
        if i_cho == _J_FILL:
            if i_jung == _J_FILL:
                if i_jong == _J_FILL:
                    return "\u3000"
                return chr(0x3100 | _JOHAB_JAMO_JONG[c_jong])
            if i_jong == _J_FILL:
                return chr(0x3100 | _JOHAB_JAMO_JUNG[c_jung])
            return None
        if i_jung == _J_FILL:
            if i_jong == _J_FILL:
                return chr(0x3100 | _JOHAB_JAMO_CHO[c_cho])
            return None
        return chr(
            0xAC00 + i_cho * 588 + i_jung * 28 + (0 if i_jong == _J_FILL else i_jong)
        )
    # KS X 1001 non-hangul area
    if (c == 0xDF or c > 0xF9 or c2 < 0x31
            or (0x80 <= c2 < 0x91) or (c2 & 0x7F) == 0x7F
            or (c == 0xDA and 0xA1 <= c2 <= 0xD3)):
        return None
    t1 = (2 * (c - 0xD9)) & 0xFF if c < 0xE0 else (2 * c - 0x197) & 0xFF
    t2 = (c2 - 0x31) & 0xFF if c2 < 0x91 else (c2 - 0x43) & 0xFF
    t1 = (t1 + (0 if t2 < 0x5E else 1) + 0x21) & 0xFF
    t2 = ((t2 if t2 < 0x5E else t2 - 0x5E) + 0x21) & 0xFF
    return _euc_cell_dec(t1, t2, "euc_kr")


def _johab_encode_core(s, errors, final, state):
    out = bytearray()
    i = 0
    n = len(s)
    while i < n:
        c = ord(s[i])
        if c < 0x80:
            out.append(c)
            i += 1
            continue
        pair = _johab_encode_char(c)
        if pair is None:
            rep, newpos = _enc_handle("johab", errors, s, i, i + 1)
            if isinstance(rep, bytes):
                out += rep
                i = newpos
                continue
            if rep:
                sub, _ = _johab_encode_core(rep, "strict", True, state)
                out += sub
            i = newpos
            continue
        out.append(pair[0])
        out.append(pair[1])
        i += 1
    return (bytes(out), "")


def _johab_decode_core(data, errors, final, state):
    out = []
    i = 0
    n = len(data)
    while i < n:
        c = data[i]
        if c < 0x80:
            out.append(chr(c))
            i += 1
            continue
        if i + 1 >= n:
            if not final:
                break
            rep, i = _dec_handle("johab", errors, data, i, n, _REASON_INCOMPLETE)
            out.append(rep)
            continue
        decoded = _johab_decode_cell(c, data[i + 1])
        if decoded is None:
            # cjkcodecs decoders flag one byte per error (`return 1`)
            rep, i = _dec_handle("johab", errors, data, i, i + 1, _REASON)
            out.append(rep)
            continue
        out.append(decoded)
        i += 2
    return ("".join(out), i)


# ---------------------------------------------------------------------------
# Shift_JIS-2004 / Shift_JISX0213 (port of _codecs_jp.c shift_jis_2004)
# ---------------------------------------------------------------------------

def _sjis2004_cell_to_bytes(plane, c1, c2):
    """Pack a plane/cell into the Shift_JIS lead/trail byte pair."""
    c2 -= 0x21
    if plane == 2:
        c1 |= 0x80
        if c1 >= 0xEE:
            c1 -= 0x87
        elif c1 >= 0xAC or c1 == 0xA8:
            c1 -= 0x49
        else:
            c1 -= 0x43
    else:
        c1 -= 0x21
    if c1 & 1:
        c2 += 0x5E
    c1 >>= 1
    b1 = c1 + (0x81 if c1 < 0x1F else 0xC1)
    b2 = c2 + (0x40 if c2 < 0x3F else 0x41)
    return (b1, b2)


def _sjis2004_encode_char(c, c2, final, y2000):
    """-> ('single', byte), ('cell', plane, c1, c2, consumed), 'toofew',
    or None."""
    _build_tables()
    # JIS X 0201 layer
    if c < 0x80 and c != 0x5C and c != 0x7E:
        return ("single", c)
    if c == 0x00A5:
        return ("single", 0x5C)
    if c == 0x203E:
        return ("single", 0x7E)
    if 0xFF61 <= c <= 0xFF9F:
        return ("single", c - 0xFEC0)
    if c <= 0xFFFF:
        if y2000:
            if c in _EMU2000_ENC_REJECT:
                return None
            if c == 0x9B1D:
                return ("cell", 2, 0x7D, 0x3B, 1)
        if c in _jis._COMB_BASES:
            if c2 is None:
                if not final:
                    return "toofew"
                cell = _BASE_LONE.get(c)
                if cell is None:
                    return None
                return ("cell", 1, cell[0], cell[1], 1)
            cell = _PAIR_ENC.get((c, c2))
            if cell is not None:
                return ("cell", 1, cell[0], cell[1], 2)
            cell = _BASE_LONE.get(c)
            if cell is None:
                return None
            return ("cell", 1, cell[0], cell[1], 1)
        if c == 0x5C:
            return ("cell", 1, 0x21, 0x40, 1)
        if c == 0x7E:
            return ("cell", 1, 0x22, 0x32, 1)
        if c in (0xFF3C, 0xFF5E):
            # euc_jis_2004 maps these to cells 1-1-32 / 1-2-18; the layered
            # Shift_JIS-2004 view gives those cells to '\\' and '~'.
            return None
        cell = _P1_ENC.get(c)
        if cell is not None:
            return ("cell", 1, cell[0], cell[1], 1)
        cell = _P2_ENC.get(c)
        if cell is not None:
            return ("cell", 2, cell[0], cell[1], 1)
        cell = _X0212_ENC.get(c)
        if cell is not None:
            # JIS X 0212 codes are abandoned by shift_jis_2004
            return None
        return None
    if (c >> 16) == 0x2:
        if y2000 and c == 0x20B9F:
            return None
        cell = _P1_ENC.get(c)
        if cell is not None:
            return ("cell", 1, cell[0], cell[1], 1)
        cell = _P2_ENC.get(c)
        if cell is not None:
            return ("cell", 2, cell[0], cell[1], 1)
    return None


def _sjis2004_encode_core(name, s, errors, final, state, y2000):
    out = bytearray()
    pending = ""
    i = 0
    n = len(s)
    while i < n:
        c = ord(s[i])
        c2 = ord(s[i + 1]) if i + 1 < n else None
        r = _sjis2004_encode_char(c, c2, final, y2000)
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
                sub, _ = _sjis2004_encode_core(name, rep, "strict", True, state, y2000)
                out += sub
            i = newpos
            continue
        if r[0] == "single":
            out.append(r[1])
            i += 1
        else:
            _tag, plane, c1, cc2, consumed = r
            b1, b2 = _sjis2004_cell_to_bytes(plane, c1, cc2)
            out.append(b1)
            out.append(b2)
            i += consumed
    return (bytes(out), pending)


def _sjis2004_decode_core(name, data, errors, final, state, y2000):
    out = []
    i = 0
    n = len(data)
    while i < n:
        c = data[i]
        # JIS X 0201 layer
        if c < 0x5C:
            out.append(chr(c))
            i += 1
            continue
        if c == 0x5C:
            out.append("\u00a5")
            i += 1
            continue
        if c < 0x7E:
            out.append(chr(c))
            i += 1
            continue
        if c == 0x7E:
            out.append("\u203e")
            i += 1
            continue
        if c == 0x7F:
            out.append("\x7f")
            i += 1
            continue
        if 0xA1 <= c <= 0xDF:
            out.append(chr(0xFEC0 + c))
            i += 1
            continue
        if (0x81 <= c <= 0x9F) or (0xE0 <= c <= 0xFC):
            if i + 1 >= n:
                if not final:
                    break
                rep, i = _dec_handle(name, errors, data, i, n, _REASON_INCOMPLETE)
                out.append(rep)
                continue
            b2 = data[i + 1]
            if b2 < 0x40 or (0x7E < b2 < 0x80) or b2 > 0xFC:
                # cjkcodecs decoders flag one byte per error (`return 1`)
                rep, i = _dec_handle(name, errors, data, i, i + 1, _REASON)
                out.append(rep)
                continue
            c1 = c - 0x81 if c < 0xE0 else c - 0xC1
            c2 = b2 - 0x40 if b2 < 0x80 else b2 - 0x41
            c1 = 2 * c1 + (0 if c2 < 0x5E else 1)
            c2 = (c2 if c2 < 0x5E else c2 - 0x5E) + 0x21
            if c1 < 0x5E:  # plane 1
                if y2000 and (c1 + 0x21, c2) in _EMU2000_DEC_REJECT_P1:
                    # EMULATE_JISX0213_2000_DECODE_INVALID flags both bytes
                    rep, i = _dec_handle(name, errors, data, i, i + 2, _REASON)
                    out.append(rep)
                    continue
                if c1 == 0 and c2 == 0x40:
                    # shift_jis_2004 exposes the raw jisx0208 value for cell
                    # 1-1-32 (U+005C); euc/iso2022 decoders override it to
                    # U+FF3C before consulting the tables.
                    decoded = "\\"
                else:
                    decoded = _x0213_p1_dec(c1 + 0x21, c2, y2000)
            else:  # plane 2
                if c1 >= 0x67:
                    c1 += 0x07
                elif c1 >= 0x63 or c1 == 0x5F:
                    c1 -= 0x37
                else:
                    c1 -= 0x3D
                decoded = _x0213_p2_dec(c1, c2, y2000)
            if decoded is None:
                rep, i = _dec_handle(name, errors, data, i, i + 1, _REASON)
                out.append(rep)
                continue
            out.append(decoded)
            i += 2
            continue
        rep, i = _dec_handle(name, errors, data, i, i + 1, _REASON)
        out.append(rep)
    return ("".join(out), i)


# ---------------------------------------------------------------------------
# codec class factories
# ---------------------------------------------------------------------------

class _Iso2022IncrementalEncoder(ErrorsProperty, IncrementalEncoder):
    name = None

    def __init__(self, errors="strict"):
        super().__init__(errors)
        self._state = _Iso2022State()
        self._pending = ""

    def encode(self, input, final=False):
        out, self._pending = _iso2022_encode_core(
            self.name, self._pending + input, self.errors, final, self._state
        )
        return out

    def reset(self):
        # ENCODER_RESET emits the shift-in/ASCII re-designation bytes, but
        # they have nowhere to go from reset(); discard state like CPython's
        # incremental wrapper does on reset-without-output.
        self._state = _Iso2022State()
        self._pending = ""

    def getstate(self):
        st = self._state
        statebytes = bytes((
            _mark_byte(st.g[0]),
            _mark_byte(st.g[1]),
            _mark_byte(st.g[2]),
            _mark_byte(st.g[3]),
            1 if st.shifted else 0,
            0, 0, 0,
        ))
        return _enc_getstate(self._pending, statebytes)

    def setstate(self, state):
        pending, sb = _enc_setstate(state)
        st = _Iso2022State()
        st.g[0] = _byte_mark(sb[0])
        st.g[1] = _byte_mark(sb[1])
        st.g[2] = _byte_mark(sb[2])
        st.g[3] = _byte_mark(sb[3])
        st.shifted = bool(sb[4] & 1)
        self._state = st
        self._pending = pending


class _Iso2022IncrementalDecoder(ErrorsProperty, IncrementalDecoder):
    name = None

    def __init__(self, errors="strict"):
        super().__init__(errors)
        self._state = _Iso2022State(decoder=True)
        self._buffer = b""

    def decode(self, input, final=False):
        data = self._buffer + bytes(input)
        result, consumed = _iso2022_decode_core(
            self.name, data, self.errors, final, self._state
        )
        self._buffer = data[consumed:]
        return result

    def reset(self):
        self._state = _Iso2022State(decoder=True)
        self._buffer = b""

    def getstate(self):
        st = self._state
        packed = int.from_bytes(
            bytes((
                _mark_byte(st.g[0]),
                _mark_byte(st.g[1]),
                _mark_byte(st.g[2]),
                _mark_byte(st.g[3]),
                (1 if st.shifted else 0) | (2 if st.escthrough else 0),
            )),
            "little",
        )
        return (self._buffer, packed)

    def setstate(self, state):
        buf, flags = _dec_setstate(state, self.name)
        sb = flags.to_bytes(8, "little")
        st = _Iso2022State(decoder=True)
        st.g[0] = _byte_mark(sb[0])
        st.g[1] = _byte_mark(sb[1])
        st.g[2] = _byte_mark(sb[2])
        st.g[3] = _byte_mark(sb[3])
        st.shifted = bool(sb[4] & 1)
        st.escthrough = bool(sb[4] & 2)
        self._state = st
        self._buffer = buf


class _HzIncrementalEncoder(ErrorsProperty, IncrementalEncoder):
    def __init__(self, errors="strict"):
        super().__init__(errors)
        self._state = [0]

    def encode(self, input, final=False):
        out, _ = _hz_encode_core(input, self.errors, final, self._state)
        return out

    def reset(self):
        self._state = [0]

    def getstate(self):
        # state.c[0] carries the ~{ shift flag; hz has no pending chars.
        return _enc_getstate("", bytes((self._state[0], 0, 0, 0, 0, 0, 0, 0)))

    def setstate(self, state):
        _pending, sb = _enc_setstate(state)
        self._state = [1 if sb[0] else 0]


class _HzIncrementalDecoder(ErrorsProperty, IncrementalDecoder):
    def __init__(self, errors="strict"):
        super().__init__(errors)
        self._state = [0]
        self._buffer = b""

    def decode(self, input, final=False):
        data = self._buffer + bytes(input)
        result, consumed = _hz_decode_core(data, self.errors, final, self._state)
        self._buffer = data[consumed:]
        return result

    def reset(self):
        self._state = [0]
        self._buffer = b""

    def getstate(self):
        return (self._buffer, self._state[0])

    def setstate(self, state):
        buf, flags = _dec_setstate(state, "hz")
        self._buffer = buf
        self._state = [1 if (flags & 0xFF) else 0]


class _StatelessIncrementalEncoder(ErrorsProperty, MbEncStateMixin,
                                   IncrementalEncoder):
    _core = None
    name = None
    _pending = ""

    def encode(self, input, final=False):
        out, _ = type(self)._core(self._pending + input, self.errors, final, None)
        self._pending = ""
        return out

    def reset(self):
        self._pending = ""


class _BufferedIncrementalDecoder(ErrorsProperty, MbDecStateMixin,
                                  IncrementalDecoder):
    _core = None
    name = None

    def __init__(self, errors="strict"):
        super().__init__(errors)
        self._buffer = b""
        self._flags = 0

    def decode(self, input, final=False):
        data = self._buffer + bytes(input)
        result, consumed = type(self)._core(data, self.errors, final, None)
        self._buffer = data[consumed:]
        return result

    def reset(self):
        self._buffer = b""


class _Sjis2004IncrementalEncoder(ErrorsProperty, MbEncStateMixin,
                                  IncrementalEncoder):
    name = None
    y2000 = False

    def __init__(self, errors="strict"):
        super().__init__(errors)
        self._pending = ""

    def encode(self, input, final=False):
        out, self._pending = _sjis2004_encode_core(
            self.name, self._pending + input, self.errors, final, None, self.y2000
        )
        return out

    def reset(self):
        self._pending = ""


class _Sjis2004IncrementalDecoder(ErrorsProperty, MbDecStateMixin,
                                  IncrementalDecoder):
    name = None
    y2000 = False

    def __init__(self, errors="strict"):
        super().__init__(errors)
        self._buffer = b""
        self._flags = 0

    def decode(self, input, final=False):
        data = self._buffer + bytes(input)
        result, consumed = _sjis2004_decode_core(
            self.name, data, self.errors, final, None, self.y2000
        )
        self._buffer = data[consumed:]
        return result

    def reset(self):
        self._buffer = b""


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

    if name in _ISO2022_CONFIGS:
        enc_cls = type(
            "_IncEnc_" + name, (_Iso2022IncrementalEncoder,), {"name": name}
        )
        dec_cls = type(
            "_IncDec_" + name, (_Iso2022IncrementalDecoder,), {"name": name}
        )

        def encode(input, errors="strict", _name=name):
            out, _pending = _iso2022_encode_core(
                _name, input, errors, True, _Iso2022State()
            )
            return (out, len(input))

        def decode(input, errors="strict", _name=name):
            data = _check_decode_input(_name, input)
            text, _consumed = _iso2022_decode_core(
                _name, data, errors, True, _Iso2022State(decoder=True)
            )
            return (text, len(data))

    elif name == "hz":
        enc_cls = _HzIncrementalEncoder
        dec_cls = _HzIncrementalDecoder

        def encode(input, errors="strict"):
            out, _ = _hz_encode_core(input, errors, True, [0])
            return (out, len(input))

        def decode(input, errors="strict"):
            data = _check_decode_input("hz", input)
            text, _consumed = _hz_decode_core(data, errors, True, [0])
            return (text, len(data))

    elif name == "johab":
        enc_cls = type(
            "_IncEnc_johab",
            (_StatelessIncrementalEncoder,),
            {"name": "johab", "_core": staticmethod(_johab_encode_core)},
        )
        dec_cls = type(
            "_IncDec_johab",
            (_BufferedIncrementalDecoder,),
            {"name": "johab", "_core": staticmethod(_johab_decode_core)},
        )

        def encode(input, errors="strict"):
            out, _ = _johab_encode_core(input, errors, True, None)
            return (out, len(input))

        def decode(input, errors="strict"):
            data = _check_decode_input("johab", input)
            text, _consumed = _johab_decode_core(data, errors, True, None)
            return (text, len(data))

    elif name in ("shift_jis_2004", "shift_jisx0213"):
        y2000 = name == "shift_jisx0213"
        enc_cls = type(
            "_IncEnc_" + name,
            (_Sjis2004IncrementalEncoder,),
            {"name": name, "y2000": y2000},
        )
        dec_cls = type(
            "_IncDec_" + name,
            (_Sjis2004IncrementalDecoder,),
            {"name": name, "y2000": y2000},
        )

        def encode(input, errors="strict", _name=name, _y=y2000):
            out, _pending = _sjis2004_encode_core(_name, input, errors, True, None, _y)
            return (out, len(input))

        def decode(input, errors="strict", _name=name, _y=y2000):
            data = _check_decode_input(_name, input)
            text, _consumed = _sjis2004_decode_core(_name, data, errors, True, None, _y)
            return (text, len(data))

    else:
        raise LookupError("unknown encoding: " + name)

    # Stream classes wrap the incremental machinery so designation/shift
    # state survives chunked reads/writes (codecs.StreamReader calls decode
    # as a plain function otherwise, losing ISO-2022 designations across
    # chunk boundaries).
    class _Writer(StreamWriter):
        def __init__(self, stream, errors="strict"):
            super().__init__(stream, errors)
            self._encoder = enc_cls(errors)

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
            self._decoder = dec_cls(errors)

        def decode(self, input, errors="strict"):
            self._decoder.errors = errors
            text = self._decoder.decode(bytes(input), False)
            # Everything is either consumed or held inside the incremental
            # decoder, so report the full input as consumed.
            return (text, len(input))

        def reset(self):
            super().reset()
            dec = getattr(self, "_decoder", None)
            if dec is not None:
                dec.reset()

    _Writer.__name__ = "_StreamWriter_" + name
    _Reader.__name__ = "_StreamReader_" + name

    info = CodecInfo(
        encode=encode,
        decode=decode,
        incrementalencoder=enc_cls,
        incrementaldecoder=dec_cls,
        streamreader=_Reader,
        streamwriter=_Writer,
        name=name,
        _is_text_encoding=True,
    )
    _REGENTRY_CACHE[name] = info
    return info
