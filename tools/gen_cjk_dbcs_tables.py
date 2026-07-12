#!/usr/bin/env python3.13
"""Generate `_cjk_tables.py` — packed mapping tables for the CJK DBCS codecs.

Run with a real CPython 3.13 (`python3.13 tools/gen_cjk_dbcs_tables.py`); the
tables are *probed* from CPython's own cjkcodecs so WeavePy's pure-Python
ports (`_codec_cjk_dbcs.py`) reproduce the reference behaviour bit-for-bit,
including every NEC/IBM duplicate-row preference and decode-only cell.

Table model (per codec):

- decode grid: leads 0x81..0xFE x trails 0x40..0xFE, one u16 LE per cell
  (0xFFFF = unmapped). big5hkscs uses u24 LE cells (0xFFFFFF = unmapped,
  0xFFFFFE = decodes-to-pair, pairs listed separately) because it decodes
  into plane 2.
- euc_jp additionally carries a 94x94 u16 grid for the SS3 (0x8F) plane
  (JIS X 0212).
- encode side: reconstructed at runtime as the *lowest (lead, trail)*
  preimage of the decode grid, then patched by a small exceptions dict
  {codepoint: bytes} and a rejects set (cells that decode but never
  encode). Both are probed here by diffing `chr(cp).encode(codec)` against
  the reconstruction rule for every BMP (+SMP where relevant) code point.
- gb18030's 4-byte forms are algorithmic over a BMP-range table probed from
  encode results (runs of contiguous linear indices).

Multi-byte-result encodes are *not* table entries: euc_kr's 8-byte jamo
make-up sequences, gb18030's 4-byte forms and big5hkscs's combining pairs
are reproduced algorithmically by the runtime module.
"""

import base64
import sys

LEAD_LO, LEAD_HI = 0x81, 0xFE
TRAIL_LO, TRAIL_HI = 0x40, 0xFE
NLEAD = LEAD_HI - LEAD_LO + 1  # 126
NTRAIL = TRAIL_HI - TRAIL_LO + 1  # 191

U16_UNDEF = 0xFFFF
U24_UNDEF = 0xFFFFFF
U24_PAIR = 0xFFFFFE


def probe_decode_grid(codec, u24=False):
    """(grid bytearray, pairs dict {(lead, trail): (cp, cp)})."""
    cell = 3 if u24 else 2
    undef = U24_UNDEF if u24 else U16_UNDEF
    grid = bytearray()
    pairs = {}
    # Leads that decode standalone (shift_jis/cp932 kana etc.) are handled
    # algorithmically by the runtime; their grid rows stay unmapped.
    single_leads = set()
    for lead in range(LEAD_LO, LEAD_HI + 1):
        try:
            bytes((lead,)).decode(codec)
            single_leads.add(lead)
        except UnicodeDecodeError:
            pass
    for lead in range(LEAD_LO, LEAD_HI + 1):
        if lead in single_leads:
            grid += undef.to_bytes(cell, "little") * NTRAIL
            continue
        for trail in range(TRAIL_LO, TRAIL_HI + 1):
            v = undef
            try:
                s = bytes((lead, trail)).decode(codec)
                if len(s) == 1:
                    cp = ord(s)
                    assert cp <= (0x10FFFF if u24 else 0xFFFD), (codec, lead, trail, cp)
                    v = cp
                elif len(s) == 2:
                    assert u24, (codec, lead, trail, s)
                    pairs[(lead, trail)] = (ord(s[0]), ord(s[1]))
                    v = U24_PAIR
                else:
                    raise AssertionError((codec, lead, trail, s))
            except UnicodeDecodeError:
                pass
            grid += v.to_bytes(cell, "little")
    return grid, pairs


def probe_ss3_grid(codec):
    """euc_jp 0x8F plane: 94x94 u16 grid over (c2, c3) in 0xA1..0xFE."""
    grid = bytearray()
    for c2 in range(0xA1, 0xFF):
        for c3 in range(0xA1, 0xFF):
            v = U16_UNDEF
            try:
                s = bytes((0x8F, c2, c3)).decode(codec)
                assert len(s) == 1 and ord(s) <= 0xFFFD
                v = ord(s)
            except UnicodeDecodeError:
                pass
            grid += v.to_bytes(2, "little")
    return grid


def grid_lowest_preimage(grid, u24=False):
    """{cp: (lead, trail)} — first (lowest) cell wins, scanning in
    (lead, trail) order. This is the runtime reconstruction rule."""
    cell = 3 if u24 else 2
    undef = U24_UNDEF if u24 else U16_UNDEF
    inv = {}
    idx = 0
    for lead in range(LEAD_LO, LEAD_HI + 1):
        for trail in range(TRAIL_LO, TRAIL_HI + 1):
            v = int.from_bytes(grid[idx : idx + cell], "little")
            idx += cell
            if v != undef and v != U24_PAIR and v not in inv:
                inv[v] = (lead, trail)
    return inv


def probe_encode(codec, cp_ranges):
    """{cp: bytes or None}; None = raises. Multi-char results are skipped
    (handled algorithmically)."""
    out = {}
    for lo, hi in cp_ranges:
        for cp in range(lo, hi):
            if 0xD800 <= cp <= 0xDFFF:
                continue
            try:
                out[cp] = chr(cp).encode(codec)
            except UnicodeEncodeError:
                out[cp] = None
    return out


def diff_encode(codec, enc_probe, inv, extra_rule=None):
    """(exceptions {cp: bytes}, rejects set, algorithmic {cp: bytes}).

    `extra_rule(cp)` may return the bytes the runtime rule would produce for
    code points outside the grid inverse (e.g. algorithmic layers); return
    None for "rule says unencodable"."""
    exceptions = {}
    rejects = set()
    algorithmic = {}
    for cp, got in sorted(enc_probe.items()):
        rule = None
        if cp < 0x80:
            rule = bytes((cp,))
        elif extra_rule is not None:
            rule = extra_rule(cp)
        if rule is None and cp in inv:
            rule = bytes(inv[cp])
        if got == rule:
            continue
        if got is None:
            if rule is not None:
                rejects.add(cp)
            continue
        if len(got) > 2:
            # euc_kr jamo (8), gb18030 4-byte: algorithmic in the runtime.
            algorithmic[cp] = got
            continue
        exceptions[cp] = got
    return exceptions, rejects, algorithmic


def gb18030_ranges(enc_probe):
    """[(unicode_first, unicode_last, linear_base)] for BMP 4-byte forms,
    probed from contiguous runs. Linear index over (b1-0x81, b2-0x30,
    b3-0x81, b4-0x30) with mixed radix 10/1260."""
    ranges = []
    cur = None  # (first, last, base)
    for cp in range(0x80, 0x10000):
        got = enc_probe.get(cp)
        if got is None or len(got) != 4:
            continue
        b1, b2, b3, b4 = got
        assert 0x81 <= b1 <= 0x84, hex(cp)
        lin = ((b1 - 0x81) * 10 + (b2 - 0x30)) * 1260 + (b3 - 0x81) * 10 + (b4 - 0x30)
        if cur is not None and cp == cur[1] + 1 and lin == cur[2] + (cp - cur[0]):
            cur = (cur[0], cp, cur[2])
        else:
            if cur is not None:
                ranges.append(cur)
            cur = (cp, cp, lin)
    if cur is not None:
        ranges.append(cur)
    return ranges


def b64_lines(data, indent="    "):
    enc = base64.b64encode(bytes(data)).decode("ascii")
    lines = [enc[i : i + 96] for i in range(0, len(enc), 96)]
    return "\n".join(f'{indent}"{line}"' for line in lines)


def fmt_dict_bytes(d):
    items = ", ".join(f"0x{cp:X}: {v!r}" for cp, v in sorted(d.items()))
    return "{" + items + "}"


def fmt_set(s):
    if not s:
        return "frozenset()"
    return "frozenset((" + ", ".join(f"0x{cp:X}" for cp in sorted(s)) + ",))"


def main():
    out = []
    out.append('"""Packed CJK DBCS mapping tables (generated; do not edit).')
    out.append("")
    out.append("Generated by `tools/gen_cjk_dbcs_tables.py` by probing CPython")
    out.append(f"{sys.version.split()[0]}'s cjkcodecs. See `_codec_cjk_dbcs` for the codec")
    out.append('state machines that consume these tables."""')
    out.append("")
    out.append("import base64 as _b64")
    out.append("")
    out.append(f"LEAD_LO, TRAIL_LO, NTRAIL = 0x{LEAD_LO:X}, 0x{TRAIL_LO:X}, {NTRAIL}")
    out.append("")
    out.append("def _g(s):")
    out.append("    return _b64.b64decode(s)")
    out.append("")

    stats = []

    def emit_codec(name, codec, u24=False, enc_ranges=((0x80, 0x10000),), extra_rule=None,
                   ss3=False):
        grid, pairs = probe_decode_grid(codec, u24=u24)
        inv = grid_lowest_preimage(grid, u24=u24)
        ss3_grid = None
        if ss3:
            ss3_grid = probe_ss3_grid(codec)
            # SS3 cells encode as 0x8F + cell; fold into the rule via extra_rule
            ss3_inv = {}
            idx = 0
            for c2 in range(0xA1, 0xFF):
                for c3 in range(0xA1, 0xFF):
                    v = int.from_bytes(ss3_grid[idx : idx + 2], "little")
                    idx += 2
                    if v != U16_UNDEF and v not in ss3_inv:
                        ss3_inv[v] = bytes((0x8F, c2, c3))
            prev_rule = extra_rule

            def rule(cp):
                if prev_rule is not None:
                    r = prev_rule(cp)
                    if r is not None:
                        return r
                if cp in inv:
                    return bytes(inv[cp])
                return ss3_inv.get(cp)

            extra_rule_eff = rule
        else:
            extra_rule_eff = extra_rule

        enc_probe = probe_encode(codec, enc_ranges)
        exceptions, rejects, algorithmic = diff_encode(codec, enc_probe, inv, extra_rule_eff)
        stats.append(
            f"{name}: grid={len(grid)}B pairs={len(pairs)} exc={len(exceptions)} "
            f"rej={len(rejects)} alg={len(algorithmic)}"
        )
        up = name.upper()
        out.append(f"# ---- {codec} ----")
        out.append(f"{up}_DEC = _g(")
        out.append(b64_lines(grid))
        out.append(")")
        if ss3_grid is not None:
            out.append(f"{up}_DEC_SS3 = _g(")
            out.append(b64_lines(ss3_grid))
            out.append(")")
        if pairs:
            items = ", ".join(
                f"(0x{l:X}, 0x{t:X}): (0x{a:X}, 0x{b:X})" for (l, t), (a, b) in sorted(pairs.items())
            )
            out.append(f"{up}_DEC_PAIRS = {{{items}}}")
        out.append(f"{up}_ENC_EXC = {fmt_dict_bytes(exceptions)}")
        out.append(f"{up}_ENC_REJECT = {fmt_set(rejects)}")
        out.append("")
        return enc_probe, algorithmic

    # --- kr ---
    emit_codec("euc_kr", "euc_kr")  # 8-byte jamo handled algorithmically
    emit_codec("cp949", "cp949")
    # johab reuses the euc_kr (ksx1001) grid algorithmically.

    # --- jp ---
    emit_codec("euc_jp", "euc_jp", ss3=True)

    def sjis_kana_rule(cp):
        if 0xFF61 <= cp <= 0xFF9F:
            return bytes((cp - 0xFEC0,))
        return None

    emit_codec("shift_jis", "shift_jis", extra_rule=sjis_kana_rule)

    def cp932_rule(cp):
        if 0xFF61 <= cp <= 0xFF9F:
            return bytes((cp - 0xFEC0,))
        if cp == 0xF8F0:
            return b"\xa0"
        if 0xF8F1 <= cp <= 0xF8F3:
            return bytes((cp - 0xF8F1 + 0xFD,))
        if 0xE000 <= cp < 0xE758:
            c1 = (cp - 0xE000) // 188
            c2 = (cp - 0xE000) % 188
            return bytes((c1 + 0xF0, c2 + 0x40 if c2 < 0x3F else c2 + 0x41))
        return None

    emit_codec("cp932", "cp932", extra_rule=cp932_rule)

    # --- cn ---
    emit_codec("gb2312", "gb2312")
    emit_codec("gbk", "gbk")
    enc_probe, alg = emit_codec(
        "gb18030", "gb18030", enc_ranges=((0x80, 0x10000),)
    )
    ranges = gb18030_ranges(enc_probe)
    items = ", ".join(f"(0x{a:X}, 0x{b:X}, {c})" for a, b, c in ranges)
    out.append(f"GB18030_RANGES = ({items})")
    out.append("")

    # --- tw / hk ---
    emit_codec("big5", "big5")
    emit_codec("cp950", "cp950")
    emit_codec(
        "big5hkscs", "big5hkscs", u24=True,
        enc_ranges=((0x80, 0x10000), (0x20000, 0x30000)),
    )

    text = "\n".join(out) + "\n"
    dest = "crates/weavepy-vm/src/stdlib/python/_cjk_tables.py"
    with open(dest, "w") as f:
        f.write(text)
    print(f"wrote {dest}: {len(text)} bytes")
    for s in stats:
        print(" ", s)


if __name__ == "__main__":
    main()
