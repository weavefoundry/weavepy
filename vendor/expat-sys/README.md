# expat-sys (vendored)

Raw FFI bindings to the [expat](https://libexpat.github.io/) XML parser,
vendored for WeavePy's native `pyexpat` module (RFC 0056 WS3).

- Upstream: libexpat release **R_2_6_4** (expat 2.6.4),
  https://github.com/libexpat/libexpat/releases/tag/R_2_6_4
- `expat-2.6.4/` is the upstream `expat/` tree reduced to what compiles:
  `lib/` sources + headers and `COPYING`. Docs, tests, fuzzers, CMake/autoconf
  machinery, `xmlwf` and examples are dropped (the `lzma-sys` vendoring
  discipline).
- Built via `cc` in `build.rs` with `XML_NS`, `XML_DTD`, `XML_GE=1` and
  `XML_CONTEXT_BYTES=1024` — the same feature set CPython compiles its
  bundled expat with (see CPython `Modules/expat`).
- Entropy: `XML_POOR_ENTROPY` is defined only to satisfy expat's
  compile-time entropy-source requirement; WeavePy never relies on it —
  every parser is salted explicitly through `XML_SetHashSalt` with a
  process-random value before its first parse.
