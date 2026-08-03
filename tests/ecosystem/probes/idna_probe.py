"""Ecosystem probe: idna — IDNA2008 encode/decode round-trips and the
error surface for invalid labels."""

import idna

# unicode -> ACE and back
assert idna.encode("bücher.example") == b"xn--bcher-kva.example"
assert idna.decode("xn--bcher-kva.example") == "bücher.example"

# a non-latin script round-trip
ace = idna.encode("例え.テスト")
assert ace == b"xn--r8jz45g.xn--zckzah", ace
assert idna.decode(ace) == "例え.テスト"

# already-ASCII passes through
assert idna.encode("example.com") == b"example.com"

# uts46 mapping folds case before encoding
assert idna.encode("BÜCHER.example", uts46=True) == b"xn--bcher-kva.example"

# invalid label raises IDNAError
try:
    idna.encode("xn--invalid-label-")
except idna.IDNAError:
    pass
else:
    raise AssertionError("IDNAError not raised")

print("idna ok", idna.__version__)
