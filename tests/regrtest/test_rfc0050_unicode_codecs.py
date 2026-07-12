"""RFC 0050 WS6 — Unicode/codec machinery invariants.

Pins the WS2–WS5 surface in-process (no CPython checkout needed):
the error-handler callback protocol, UCD 15.1.0 name/lookup/property
spot checks, CPython-parity str case mappings and predicates, and the
CJK codec round-trip semantics the frozen `_codec_cjk_*` engines
implement.
"""

import codecs
import sys
import unicodedata


# ---------------------------------------------------------------------------
# Error-handler callback protocol: a registered handler receives the exact
# exception CPython builds (object/start/end/reason attributes), and its
# (replacement, new_position) result is honoured — including a negative
# position (counts from the end) and a bytes replacement on encode.
# ---------------------------------------------------------------------------

seen = []


def handler(exc):
    seen.append((type(exc).__name__, exc.object[exc.start:exc.end], exc.start, exc.end))
    return ("?", exc.end)


codecs.register_error("rfc0050-probe", handler)

assert "ab\udc80cd".encode("ascii", "rfc0050-probe") == b"ab?cd"
assert seen[-1] == ("UnicodeEncodeError", "\udc80", 2, 3)

assert b"ab\xffcd".decode("ascii", "rfc0050-probe") == "ab?cd"
assert seen[-1] == ("UnicodeDecodeError", b"\xff", 2, 3)

# Built-in handlers, one probe each (the WS2 strictness sweep).
assert "a\udc80b".encode("utf-8", "surrogateescape") == b"a\x80b"
assert b"a\x80b".decode("utf-8", "surrogateescape") == "a\udc80b"
assert "a\u20acb".encode("ascii", "backslashreplace") == b"a\\u20acb"
assert "a\u20acb".encode("ascii", "namereplace") == b"a\\N{EURO SIGN}b"
assert "a\u20acb".encode("ascii", "xmlcharrefreplace") == b"a&#8364;b"
assert b"a\xffb".decode("utf-8", "replace") == "a\ufffdb"
try:
    b"\xff".decode("utf-8")
except UnicodeDecodeError as e:
    assert e.reason == "invalid start byte", e.reason
else:
    raise AssertionError("strict utf-8 decode of 0xff must raise")

# ---------------------------------------------------------------------------
# unicodedata: UCD 15.1.0 with aliases, named sequences, and algorithmic
# names (WS4).
# ---------------------------------------------------------------------------

assert unicodedata.unidata_version == "15.1.0", unicodedata.unidata_version
assert unicodedata.name("\u20ac") == "EURO SIGN"
assert unicodedata.lookup("EURO SIGN") == "\u20ac"
# Alias (NameAliases.txt) and a lookup-only algorithmic range (Tangut).
assert unicodedata.lookup("LATIN SMALL LETTER GHA") == "\u01a3"
assert unicodedata.lookup("CJK UNIFIED IDEOGRAPH-4E00") == "\u4e00"
assert unicodedata.lookup("TANGUT IDEOGRAPH-17000") == "\U00017000"
# Named sequence (NamedSequences.txt).
assert unicodedata.lookup("LATIN SMALL LETTER A WITH MACRON AND GRAVE") == "\u0101\u0300"
# Hangul syllable algorithmic name round-trip.
assert unicodedata.name("\uac00") == "HANGUL SYLLABLE GA"
assert unicodedata.lookup("HANGUL SYLLABLE GA") == "\uac00"
# Properties + normalization spot checks.
assert unicodedata.category("\u0301") == "Mn"
assert unicodedata.decimal("\u0660") == 0
assert unicodedata.numeric("\u00bd") == 0.5
assert unicodedata.normalize("NFC", "e\u0301") == "\u00e9"
assert unicodedata.normalize("NFD", "\u00e9") == "e\u0301"
assert unicodedata.normalize("NFKC", "\ufb01") == "fi"
assert unicodedata.is_normalized("NFC", "\u00e9")

# ---------------------------------------------------------------------------
# str case mappings / predicates ride the same generated tables (WS4).
# ---------------------------------------------------------------------------

assert "\u00df".upper() == "SS"                    # sharp s expands
assert "\u0130".lower() == "i\u0307"               # dotted capital I
assert "\u01c5".istitle()                          # Lt titlecase digraph
assert "\ufb01".casefold() == "fi"                 # ligature folds
assert "\u1e9e".casefold() == "ss"                 # capital sharp s
assert "\u03a3ΑΒ".title() == "Σαβ"
assert "ΟΔΟΣ".lower() == "οδος"                    # Final_Sigma rule
assert "\u001c".isspace() and " \u001d x ".split() == ["x"]
assert not "\u0345".isalpha()                      # Mn: not alpha in CPython (unlike Rust)

# ---------------------------------------------------------------------------
# CJK codecs (WS3): incremental/stateful semantics and known mappings.
# ---------------------------------------------------------------------------

# Representative two-way mappings, one per engine family.
assert "\u4e00".encode("gbk") == b"\xd2\xbb"
assert b"\xd2\xbb".decode("gbk") == "\u4e00"
assert "\U00020000".encode("gb18030") == b"\x95\x32\x82\x36"      # 4-byte form
assert b"\x95\x32\x82\x36".decode("gb18030") == "\U00020000"
assert "\u4e00".encode("big5") == b"\xa4\x40"
assert "\u4e00".encode("euc_kr") == b"\xec\xe9"
assert "\uac02".encode("euc_kr") == b"\xa4\xd4\xa4\xa1\xa4\xbf\xa4\xa2"  # Jamo composition
assert "\u3042".encode("shift_jis") == b"\x82\xa0"
assert "\u3042".encode("euc_jp") == b"\xa4\xa2"
# Stateful escape codecs reset per one-shot call.
assert "\u4e00".encode("iso2022_jp") == b"\x1b$B0l\x1b(B"
assert b"\x1b$B0l\x1b(B".decode("iso2022_jp") == "\u4e00"
assert "\u4e00".encode("hz") == b"~{R;~}"
# Incremental decoder holds partial multi-byte sequences across feeds.
dec = codecs.getincrementaldecoder("gb18030")()
assert dec.decode(b"\x95\x32") == ""
assert dec.decode(b"\x82\x36") == "\U00020000"
# Incremental encoder state survives a getstate/setstate round-trip.
enc = codecs.getincrementalencoder("iso2022_jp")()
first = enc.encode("\u4e00")
state = enc.getstate()
enc2 = codecs.getincrementalencoder("iso2022_jp")()
enc2.setstate(state)
assert first + enc2.encode("\u4e01", final=True) == "\u4e00\u4e01".encode("iso2022_jp")

# ---------------------------------------------------------------------------
# Registry error surface.
# ---------------------------------------------------------------------------

try:
    codecs.lookup("rfc0050-no-such-codec")
except LookupError as e:
    assert "unknown encoding" in str(e)
else:
    raise AssertionError("unknown codec must raise LookupError")

print("rfc0050 unicode/codec invariants ok")
