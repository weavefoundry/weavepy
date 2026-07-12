#!/usr/bin/env python3.13
"""Generate WeavePy's UCD 15.1.0 tables by probing host CPython 3.13.

RFC 0050 WS4. CPython 3.13's `unicodedata` *is* UCD 15.1.0, so probing it
exhaustively (all 0x110000 code points, both the current database and the
`ucd_3_2_0` snapshot) reproduces `Modules/unicodedata_db.h` +
`Modules/unicodename_db.h` without re-implementing `makeunicodedata.py`.

Outputs (packed binary blobs under
`crates/weavepy-vm/src/stdlib/ucd/`, loaded via `include_bytes!`):

- `records.bin`     unique per-code-point property records (12 bytes each)
- `index1.bin`      block map: (0x110000 >> SHIFT) u16 block ids
- `index2.bin`      per-block record ids: blocks * (1 << SHIFT) u16s
- `index1_32.bin`,
  `index2_32.bin`   the same two-level index for the UCD 3.2.0 snapshot
                    (sharing `records.bin`'s record pool)
- `numeric.bin`     f64 numeric values (little-endian)
- `decomp.bin`      decomposition strings: u8 prefix-idx, u8 len, len*u24 cps
- `names.bin`       non-algorithmic names sorted by cp: u24 cp, u8 len, bytes
- `names32.bin`     bitmap of "assigned in UCD 3.2.0" (0x110000 bits)
- `comp.bin`        canonical composition pairs: u24 a, u24 b, u24 composed
- `case_records.bin` unique ctype/case records (14 bytes each: u16 flags +
                    4 u24 pool refs for lower/upper/title/casefold;
                    0xFFFFFF = identity)
- `case_index1.bin`,
  `case_index2.bin` two-level index over the case records
- `case_pool.bin`   case-mapping expansion pool: u8 len, len*u24 cps
- `aliases.bin`     NameAliases.txt entries (lookup-only): u24 cp, u8 len,
                    name bytes
- `seqs.bin`        NamedSequences.txt entries (lookup-only, multi-cp):
                    u8 n, n*u24 cps, u8 len, name bytes
- `meta.rs`         generated Rust consts (category/bidi/eaw/prefix string
                    pools, SHIFT, record count)

Record layout (12 bytes):
  0: category index        (into CATEGORIES)
  1: bidirectional index   (into BIDIRECTIONALS)
  2: combining class       (0..=254)
  3: mirrored              (0/1)
  4: east_asian_width idx  (into EASTASIANWIDTHS)
  5: decimal               (0xFF = none)
  6: digit                 (0xFF = none)
  7: numeric idx lo        (u16 into numeric.bin / 8; 0xFFFF = none)
  8: numeric idx hi
  9: decomp idx lo         (u24 byte offset into decomp.bin; 0xFFFFFF = none)
 10: decomp idx mid
 11: decomp idx hi
"""

import os
import struct
import sys
import unicodedata

assert sys.version_info[:2] == (3, 13), "must run under CPython 3.13"
assert unicodedata.unidata_version == "15.1.0", unicodedata.unidata_version

OUT_DIR = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "crates", "weavepy-vm", "src", "stdlib", "ucd",
)
SHIFT = 7
NCP = 0x110000

DB_CUR = unicodedata
DB_32 = unicodedata.ucd_3_2_0

# Decomposition tag prefixes (makeunicodedata.py DECOMPOSITION_PREFIXES).
DECOMP_PREFIXES = [
    "", "<noBreak>", "<compat>", "<super>", "<sub>", "<vertical>", "<wide>",
    "<narrow>", "<small>", "<square>", "<fraction>", "<font>", "<circle>",
    "<initial>", "<medial>", "<final>", "<isolated>",
]


def probe_record(db, cp, pools):
    ch = chr(cp)
    cat = db.category(ch)
    bidi = db.bidirectional(ch)
    comb = db.combining(ch)
    mirr = db.mirrored(ch)
    eaw = db.east_asian_width(ch)
    dec = db.decimal(ch, None)
    dig = db.digit(ch, None)
    num = db.numeric(ch, None)
    decomp = db.decomposition(ch)

    cats, bidis, eaws, nums, decomps = pools
    cat_i = intern_str(cats, cat)
    bidi_i = intern_str(bidis, bidi)
    eaw_i = intern_str(eaws, eaw)

    if num is None:
        num_i = 0xFFFF
    else:
        num_i = intern_str(nums, num)
        assert num_i < 0xFFFF

    if not decomp:
        dcp_i = 0xFFFFFF
    else:
        dcp_i = intern_decomp(decomps, decomp)
        assert dcp_i < 0xFFFFFF

    dec_b = 0xFF if dec is None else dec
    dig_b = 0xFF if dig is None else dig
    assert 0 <= comb <= 0xFF and mirr in (0, 1)
    return struct.pack(
        "<BBBBBBBH",
        cat_i, bidi_i, comb, mirr, eaw_i, dec_b, dig_b, num_i,
    ) + dcp_i.to_bytes(3, "little")


def intern_str(pool, value):
    idx = pool["map"].get(value)
    if idx is None:
        idx = len(pool["list"])
        pool["map"][value] = idx
        pool["list"].append(value)
    return idx


def intern_decomp(pool, decomp):
    """decomp is unicodedata.decomposition()'s text form, e.g.
    '<compat> 0020 0301' or '0041 0300'. Encode as (prefix_idx, cps)."""
    key = decomp
    idx = pool["map"].get(key)
    if idx is not None:
        return idx
    parts = decomp.split()
    if parts and parts[0].startswith("<"):
        prefix = parts[0]
        cps = [int(p, 16) for p in parts[1:]]
    else:
        prefix = ""
        cps = [int(p, 16) for p in parts]
    pfx_i = DECOMP_PREFIXES.index(prefix)
    assert len(cps) <= 0xFF
    blob = pool["blob"]
    idx = len(blob)
    blob.append(pfx_i)
    blob.append(len(cps))
    for c in cps:
        blob += c.to_bytes(3, "little")
    pool["map"][key] = idx
    return idx


def build_index(record_of_cp):
    """splitbins: two-level index over 0x110000 entries."""
    block_size = 1 << SHIFT
    blocks = {}
    index1 = []
    index2 = bytearray()
    for start in range(0, NCP, block_size):
        block = tuple(record_of_cp[start:start + block_size])
        bid = blocks.get(block)
        if bid is None:
            bid = len(blocks)
            blocks[block] = bid
            for rid in block:
                index2 += struct.pack("<H", rid)
        index1.append(bid)
    i1 = bytearray()
    for bid in index1:
        assert bid <= 0xFFFF
        i1 += struct.pack("<H", bid)
    return bytes(i1), bytes(index2)


ALGORITHMIC_PREFIXES = (
    "CJK UNIFIED IDEOGRAPH-",
    "CJK COMPATIBILITY IDEOGRAPH-",
    "HANGUL SYLLABLE ",
    "TANGUT IDEOGRAPH-",
    "KHITAN SMALL SCRIPT CHARACTER-",
    "NUSHU CHARACTER-",
)


def is_algorithmic(cp, name):
    for p in ALGORITHMIC_PREFIXES:
        if name.startswith(p):
            suffix = name[len(p):]
            if p == "HANGUL SYLLABLE ":
                return True
            if suffix == "%04X" % cp or suffix == "%X" % cp:
                return True
    return False


# Ctype/case flag bits (case_records.bin u16). Probed from str methods, so
# they encode CPython's *observed* semantics, not raw UCD properties.
FLAG_ALPHA = 1 << 0  # c.isalpha()
FLAG_DECIMAL = 1 << 1  # c.isdecimal()
FLAG_DIGIT = 1 << 2  # c.isdigit()
FLAG_NUMERIC = 1 << 3  # c.isnumeric()
FLAG_SPACE = 1 << 4  # c.isspace()
FLAG_LOWER = 1 << 5  # c.islower()   (Py_UNICODE_ISLOWER)
FLAG_UPPER = 1 << 6  # c.isupper()   (Py_UNICODE_ISUPPER)
FLAG_TITLE = 1 << 7  # Lt titlecase  (Py_UNICODE_ISTITLE)
FLAG_CASED = 1 << 8  # DerivedCoreProperties Cased
FLAG_CASE_IGNORABLE = 1 << 9  # DerivedCoreProperties Case_Ignorable
FLAG_ALNUM = 1 << 10  # c.isalnum()
FLAG_XID_START = 1 << 11  # c.isidentifier()
FLAG_XID_CONTINUE = 1 << 12  # ("_"+c).isidentifier()
FLAG_PRINTABLE = 1 << 13  # c.isprintable()


def probe_case_flags(cp):
    ch = chr(cp)
    flags = 0
    if ch.isalpha():
        flags |= FLAG_ALPHA
    if ch.isdecimal():
        flags |= FLAG_DECIMAL
    if ch.isdigit():
        flags |= FLAG_DIGIT
    if ch.isnumeric():
        flags |= FLAG_NUMERIC
    if ch.isspace():
        flags |= FLAG_SPACE
    if ch.islower():
        flags |= FLAG_LOWER
    if ch.isupper():
        flags |= FLAG_UPPER
    # str.istitle() on a single char is ISUPPER || ISTITLE; subtract ISUPPER.
    if ch.istitle() and not ch.isupper():
        flags |= FLAG_TITLE
    if ch.isalnum():
        flags |= FLAG_ALNUM
    if ch.isidentifier():
        flags |= FLAG_XID_START
    if ("_" + ch).isidentifier():
        flags |= FLAG_XID_CONTINUE
    if ch.isprintable():
        flags |= FLAG_PRINTABLE
    # Cased: str.title() run detection sets previous_is_cased from
    # _PyUnicode_IsCased(prev), so a following 'a' stays lowercase iff
    # the probed char is Cased.
    if (ch + "a").title().endswith("a"):
        flags |= FLAG_CASED
    else:
        assert (ch + "a").title().endswith("A"), hex(cp)
    # Case_Ignorable, via the Final_Sigma rule's after-scan (skip
    # ignorables, then check Cased):
    #   P1 = "AΣ<c>b": final iff <c> is neither ignorable nor cased
    #   P2 = "AΣ<c>":  final iff <c> is ignorable or not cased
    # so ignorable ⟺ (P1 non-final and P2 final).
    p1 = ("A\u03a3" + ch + "b").lower()[1]
    p2 = ("A\u03a3" + ch).lower()[1]
    assert p1 in "\u03c2\u03c3" and p2 in "\u03c2\u03c3", hex(cp)
    if p1 == "\u03c3" and p2 == "\u03c2":
        flags |= FLAG_CASE_IGNORABLE
    return flags


def probe_case_tables():
    """case_records.bin / case_index1.bin / case_index2.bin / case_pool.bin.

    Record (14 bytes): u16 flags, then u24 lower/upper/title/casefold refs.
    A ref < 0x110000 is a direct single code point; 0xFFFFFF means identity;
    anything else is 0x110000 + byte offset into case_pool.bin (u8 len,
    len*u24 cps) for multi-code-point expansions.
    """
    pool = bytearray()
    pool_map = {}

    def map_ref(cp, mapped):
        if mapped == chr(cp):
            return 0xFFFFFF
        cps = [ord(c) for c in mapped]
        if len(cps) == 1:
            assert cps[0] < 0x110000
            return cps[0]
        key = tuple(cps)
        off = pool_map.get(key)
        if off is None:
            off = len(pool)
            pool_map[key] = off
            pool.append(len(cps))
            for c in cps:
                pool.extend(c.to_bytes(3, "little"))
        ref = 0x110000 + off
        assert ref < 0xFFFFFF
        return ref

    records = {}
    record_list = []
    rids = []
    for cp in range(NCP):
        ch = chr(cp)
        rec = struct.pack("<H", probe_case_flags(cp))
        # ch.title() (the method on a 1-char string) applies ToTitleFull at
        # a word start, which is exactly the per-char titlecase mapping.
        for mapped in (ch.lower(), ch.upper(), ch.title(), ch.casefold()):
            rec += map_ref(cp, mapped).to_bytes(3, "little")
        rid = records.get(rec)
        if rid is None:
            rid = len(record_list)
            records[rec] = rid
            record_list.append(rec)
        rids.append(rid)
    assert len(record_list) <= 0xFFFF, len(record_list)
    i1, i2 = build_index(rids)
    return b"".join(record_list), i1, i2, bytes(pool)


DATA_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "data")


def probe_aliases_and_sequences():
    """`aliases.bin` + `seqs.bin` from the checked-in UCD 15.1.0 data files,
    with every entry verified against the host `unicodedata.lookup` (which
    compiles the same files into `unicodename_db.h`)."""
    aliases = bytearray()
    path = os.path.join(DATA_DIR, "NameAliases-15.1.0.txt")
    for line in open(path, encoding="utf-8"):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        cp_s, alias, _typ = line.split(";")
        cp = int(cp_s, 16)
        assert unicodedata.lookup(alias) == chr(cp), alias
        nb = alias.encode("ascii")
        assert len(nb) <= 0xFF
        aliases += cp.to_bytes(3, "little")
        aliases.append(len(nb))
        aliases += nb

    seqs = bytearray()
    path = os.path.join(DATA_DIR, "NamedSequences-15.1.0.txt")
    for line in open(path, encoding="utf-8"):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        name, cps_s = line.split(";")
        cps = [int(x, 16) for x in cps_s.split()]
        assert unicodedata.lookup(name) == "".join(map(chr, cps)), name
        nb = name.encode("ascii")
        assert len(nb) <= 0xFF and len(cps) <= 0xFF
        seqs.append(len(cps))
        for c in cps:
            seqs += c.to_bytes(3, "little")
        seqs.append(len(nb))
        seqs += nb
    return bytes(aliases), bytes(seqs)


def probe_lookup_only_ranges():
    """Hex-suffixed algorithmic ranges that `unicodedata.lookup` (and `\\N`)
    accept but `name()` does not generate — Tangut, in UCD 15.1.0."""
    runs = []
    for prefix in ALGORITHMIC_PREFIXES:
        if prefix == "HANGUL SYLLABLE ":
            continue
        for cp in range(NCP):
            if DB_CUR.name(chr(cp), None) is not None:
                continue
            try:
                got = unicodedata.lookup("%s%X" % (prefix, cp))
            except KeyError:
                continue
            assert got == chr(cp)
            if runs and runs[-1][0] == prefix and runs[-1][2] == cp - 1:
                runs[-1][2] = cp
            else:
                runs.append([prefix, cp, cp])
    return runs


def main():
    os.makedirs(OUT_DIR, exist_ok=True)
    pools = (
        {"map": {}, "list": []},  # categories
        {"map": {}, "list": []},  # bidirectionals
        {"map": {}, "list": []},  # east asian widths
        {"map": {}, "list": []},  # numeric strings
        {"map": {}, "blob": bytearray()},  # decompositions
    )
    records = {}
    record_list = []

    def record_id(db, cp):
        rec = probe_record(db, cp, pools)
        rid = records.get(rec)
        if rid is None:
            rid = len(record_list)
            records[rec] = rid
            record_list.append(rec)
        return rid

    print("probing current database...", flush=True)
    rid_cur = [record_id(DB_CUR, cp) for cp in range(NCP)]
    print("probing ucd_3_2_0 snapshot...", flush=True)
    rid_32 = [record_id(DB_32, cp) for cp in range(NCP)]
    assert len(record_list) <= 0xFFFF, len(record_list)

    print("building indexes...", flush=True)
    i1, i2 = build_index(rid_cur)
    i1_32, i2_32 = build_index(rid_32)

    print("probing names...", flush=True)
    names = bytearray()
    n_names = n_algo = 0
    assigned32 = bytearray(NCP // 8)
    algo_runs = []  # (prefix, start, end) inclusive, hex-suffixed names
    hangul_runs = []  # (start, end) inclusive
    for cp in range(NCP):
        ch = chr(cp)
        name = DB_CUR.name(ch, None)
        if name is not None:
            if is_algorithmic(cp, name):
                n_algo += 1
                if name.startswith("HANGUL SYLLABLE "):
                    if hangul_runs and hangul_runs[-1][1] == cp - 1:
                        hangul_runs[-1][1] = cp
                    else:
                        hangul_runs.append([cp, cp])
                else:
                    prefix = name[: name.rindex("-") + 1]
                    if (
                        algo_runs
                        and algo_runs[-1][0] == prefix
                        and algo_runs[-1][2] == cp - 1
                    ):
                        algo_runs[-1][2] = cp
                    else:
                        algo_runs.append([prefix, cp, cp])
            else:
                nb = name.encode("ascii")
                assert len(nb) <= 0xFF
                names += cp.to_bytes(3, "little")
                names.append(len(nb))
                names += nb
                n_names += 1
        if DB_32.name(ch, None) is not None:
            assigned32[cp >> 3] |= 1 << (cp & 7)

    print("probing canonical composition pairs...", flush=True)
    comp = bytearray()
    n_comp = 0
    for cp in range(NCP):
        ch = chr(cp)
        d = DB_CUR.decomposition(ch)
        if not d or d.startswith("<"):
            continue
        parts = [int(p, 16) for p in d.split()]
        if len(parts) != 2:
            continue
        # Primary composite iff NFC of the decomposition recomposes to it.
        seq = "".join(map(chr, parts))
        if unicodedata.normalize("NFC", seq) == ch:
            comp += parts[0].to_bytes(3, "little")
            comp += parts[1].to_bytes(3, "little")
            comp += cp.to_bytes(3, "little")
            n_comp += 1

    print("probing case/ctype tables...", flush=True)
    case_records, case_i1, case_i2, case_pool = probe_case_tables()

    print("probing aliases + named sequences...", flush=True)
    aliases_blob, seqs_blob = probe_aliases_and_sequences()

    print("probing lookup-only algorithmic ranges...", flush=True)
    lookup_only_runs = probe_lookup_only_ranges()

    print("writing blobs...", flush=True)
    numeric_blob = bytearray()
    for s in pools[3]["list"]:
        numeric_blob += struct.pack("<d", float(s))

    blobs = {
        "case_records.bin": case_records,
        "case_index1.bin": case_i1,
        "case_index2.bin": case_i2,
        "case_pool.bin": case_pool,
        "aliases.bin": aliases_blob,
        "seqs.bin": seqs_blob,
        "records.bin": b"".join(record_list),
        "index1.bin": i1,
        "index2.bin": i2,
        "index1_32.bin": i1_32,
        "index2_32.bin": i2_32,
        "numeric.bin": bytes(numeric_blob),
        "decomp.bin": bytes(pools[4]["blob"]),
        "names.bin": bytes(names),
        "names32.bin": bytes(assigned32),
        "comp.bin": bytes(comp),
    }
    for fname, blob in blobs.items():
        with open(os.path.join(OUT_DIR, fname), "wb") as f:
            f.write(blob)
        print(f"  {fname}: {len(blob):,} bytes")

    def rust_str_array(name, values):
        items = ", ".join('"%s"' % v for v in values)
        return f"pub const {name}: &[&str] = &[{items}];\n"

    with open(os.path.join(OUT_DIR, "meta.rs"), "w") as f:
        f.write(
            "// Generated by tools/gen_ucd_tables.py from CPython 3.13\n"
            "// (UCD 15.1.0). Do not edit.\n\n"
        )
        f.write(f"pub const SHIFT: u32 = {SHIFT};\n")
        f.write(f"pub const N_RECORDS: usize = {len(record_list)};\n")
        f.write(rust_str_array("CATEGORIES", pools[0]["list"]))
        f.write(rust_str_array("BIDIRECTIONALS", pools[1]["list"]))
        f.write(rust_str_array("EASTASIANWIDTHS", pools[2]["list"]))
        f.write(rust_str_array("DECOMP_PREFIXES", DECOMP_PREFIXES))
        f.write(
            "/// Hex-suffixed algorithmic name ranges "
            "(prefix, first cp, last cp).\n"
        )
        f.write("pub const ALGO_RANGES: &[(&str, u32, u32)] = &[\n")
        for prefix, lo, hi in algo_runs:
            f.write(f'    ("{prefix}", 0x{lo:X}, 0x{hi:X}),\n')
        f.write("];\n")
        f.write("/// HANGUL SYLLABLE ranges (first cp, last cp).\n")
        f.write("pub const HANGUL_RANGES: &[(u32, u32)] = &[\n")
        for lo, hi in hangul_runs:
            f.write(f"    (0x{lo:X}, 0x{hi:X}),\n")
        f.write("];\n")
        f.write(
            "/// Hex-suffixed ranges `lookup()`/`\\N` accept but `name()` "
            "does not generate.\n"
        )
        f.write("pub const LOOKUP_ONLY_RANGES: &[(&str, u32, u32)] = &[\n")
        for prefix, lo, hi in lookup_only_runs:
            f.write(f'    ("{prefix}", 0x{lo:X}, 0x{hi:X}),\n')
        f.write("];\n")
        f.write("// Ctype/case flag bits (case_records.bin u16).\n")
        for fname_, fval in [
            ("ALPHA", FLAG_ALPHA), ("DECIMAL", FLAG_DECIMAL),
            ("DIGIT", FLAG_DIGIT), ("NUMERIC", FLAG_NUMERIC),
            ("SPACE", FLAG_SPACE), ("LOWER", FLAG_LOWER),
            ("UPPER", FLAG_UPPER), ("TITLE", FLAG_TITLE),
            ("CASED", FLAG_CASED), ("CASE_IGNORABLE", FLAG_CASE_IGNORABLE),
            ("ALNUM", FLAG_ALNUM), ("XID_START", FLAG_XID_START),
            ("XID_CONTINUE", FLAG_XID_CONTINUE),
            ("PRINTABLE", FLAG_PRINTABLE),
        ]:
            f.write(f"pub const FLAG_{fname_}: u16 = 0x{fval:04X};\n")

    print(
        f"done: {len(record_list)} records, {n_names} names "
        f"(+{n_algo} algorithmic), {n_comp} composition pairs"
    )


if __name__ == "__main__":
    main()
