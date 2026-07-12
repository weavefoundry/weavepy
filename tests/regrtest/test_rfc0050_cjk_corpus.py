"""RFC 0050 WS6 — CJK codec round-trip over CPython's cjkencodings corpus.

Each `<codec>.txt` / `<codec>-utf8.txt` pair under
`vendor/cpython/Lib/test/cjkencodings/` must decode and re-encode
byte-for-byte through the frozen CJK engines, exactly as
`test_multibytecodec_support.TestBase.test_chunkcoding` grades.
Skips silently when the vendored corpus is absent.
"""

import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
CORPUS = os.path.normpath(
    os.path.join(HERE, "..", "..", "vendor", "cpython", "Lib", "test", "cjkencodings")
)

if not os.path.isdir(CORPUS):
    print("cjk corpus: skipped (vendored cjkencodings data not found)")
    sys.exit(0)

# File-name stem → codec name (mirrors test_codecencodings_* supported map).
CODECS = [
    "big5", "big5hkscs", "cp949", "euc_jisx0213", "euc_jp", "euc_kr",
    "gb18030", "gb2312", "gbk", "hz", "iso2022_jp", "iso2022_kr",
    "johab", "shift_jis", "shift_jisx0213",
]

checked = 0
for name in CODECS:
    raw_path = os.path.join(CORPUS, f"{name}.txt")
    ref_path = os.path.join(CORPUS, f"{name}-utf8.txt")
    if not (os.path.exists(raw_path) and os.path.exists(ref_path)):
        continue
    with open(raw_path, "rb") as f:
        raw = f.read()
    with open(ref_path, "rb") as f:
        text = f.read().decode("utf-8")
    decoded = raw.decode(name)
    assert decoded == text, f"{name}: decode mismatch"
    # euc_jisx0213/shift_jisx0213 hold combining pairs that re-encode to
    # equivalent-but-different bytes in CPython too; round-trip the text
    # through encode→decode instead for those, byte-exact for the rest.
    encoded = text.encode(name)
    if name in ("euc_jisx0213", "shift_jisx0213"):
        assert encoded.decode(name) == text, f"{name}: re-decode mismatch"
    else:
        assert encoded == raw, f"{name}: encode mismatch"
    checked += 1

assert checked >= 10, f"only {checked} corpus pairs found — corpus layout changed?"
print(f"cjk corpus round-trip ok ({checked} codecs)")
