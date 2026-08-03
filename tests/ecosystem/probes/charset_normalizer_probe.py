"""Ecosystem probe: charset-normalizer (standalone) — encoding detection
on real byte payloads (the wheel is mypyc-compiled, so this also
exercises the C-API surface)."""

from charset_normalizer import from_bytes
from charset_normalizer.version import __version__

# clean UTF-8 with multi-byte characters
res = from_bytes("naïve café — résumé".encode("utf-8")).best()
assert res is not None
assert res.encoding in ("utf_8", "utf-8"), res.encoding
assert "café" in str(res)

# latin-1 payload decodes to the original text
res = from_bytes("héllo wörld".encode("latin-1")).best()
assert res is not None
assert "llo w" in str(res)

# utf-16 with BOM
res = from_bytes("hello utf16".encode("utf-16")).best()
assert res is not None
assert str(res) == "hello utf16"

# pure ASCII
res = from_bytes(b"plain ascii text").best()
assert res is not None and str(res) == "plain ascii text"

print("charset-normalizer ok", __version__)
