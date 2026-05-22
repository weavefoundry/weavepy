# RFC 0019: Numerics and serialization

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-22
- **Tracking issue**: TBD

## Summary

Close the gap between "modern Python — language + stdlib + asyncio + OS
interface + introspection — runs" (post RFC 0018) and "**real-world
Python apps that move data — numbers, archives, databases, pickles —
run**." After this RFC lands:

- The runtime gains **arbitrary-precision integers**. `2**1000`,
  `factorial(100)`, RSA-style modular exponentiation, and
  `int.from_bytes(b, 'big')` on multi-kilobyte inputs all work. The
  existing `i64` representation is preserved as the small-int fast
  path; values that don't fit are transparently promoted to a new
  `Object::Long` variant backed by `num_bigint::BigInt`.
- A real **`complex` number** type lands. `1+2j`, `complex(3, 4)`,
  the four arithmetic ops, `abs`, `conjugate`, `.real` / `.imag`,
  and the lexer's existing `j`/`J` suffix all dispatch to a new
  `Object::Complex` variant.
- The full **`int` / `float` / `bytes` method surface** that depends
  on bignum lands: `int.bit_length`, `int.bit_count`,
  `int.to_bytes`, `int.from_bytes`, `int.is_integer` (3.12+),
  `float.hex`, `float.fromhex`, `float.is_integer`,
  `float.as_integer_ratio`, `bytes.fromhex`, `bytearray.fromhex`,
  `bytes.hex(sep=...)`.
- A real **`struct`** module ships under `_struct` (Rust core).
  `pack`, `unpack`, `pack_into`, `unpack_from`, `iter_unpack`,
  `calcsize`, the `Struct` class, the full `<>=!@` byte-order
  prefix, and every CPython format character (`bBhHiIlLqQnNeEfdcs?`
  + the `P` pointer + `?` bool + repeat counts).
- A real **`codecs`** module: an encoding registry that resolves
  `utf-8`, `utf-16`, `utf-16-le`, `utf-16-be`, `utf-32`,
  `utf-32-le`, `utf-32-be`, `latin-1`, `iso-8859-1`, `ascii`,
  `cp1252`, `mbcs` (alias to cp1252 on Windows, latin-1
  elsewhere), `hex_codec`, `base64_codec`, `rot_13`, plus
  `unicode_escape` / `raw_unicode_escape`. Errors handlers:
  `strict`, `ignore`, `replace`, `backslashreplace`, `namereplace`,
  `xmlcharrefreplace`, `surrogateescape`, `surrogatepass`. The
  surface includes `encode` / `decode` / `lookup` /
  `register` / `register_error` / `lookup_error` / `BOM`-family
  constants / `IncrementalEncoder` / `IncrementalDecoder`.
- A real **`marshal`** module, capable of round-tripping every
  object type CPython 3.13's `pyc` format uses (`int`, `long`,
  `float`, `complex`, `str`, `bytes`, `tuple`, `list`, `dict`,
  `set`, `frozenset`, `code`, `None`, `True`, `False`, `Ellipsis`,
  `StopIteration`, `bytearray`, ASCII / interned-string variants).
  Wired into the import machinery so WeavePy now reads and writes
  `__pycache__/*.weavepy-3.13.pyc` files.
- **`py_compile`** and **`compileall`** ship as frozen Python
  modules on top of marshal.
- The full **compression stack**: `zlib` (extended from RFC 0017),
  plus new `gzip`, `bz2`, `lzma` Rust cores and frozen Python
  wrappers exposing the standard `GzipFile`/`BZ2File`/`LZMAFile`
  classes.
- **`zipfile`** and **`tarfile`** ship as frozen Python on top of
  `_struct` and the compression stack — enough to read and write
  the archives `pip` and the wheel/sdist toolchain produce.
- A real **`sqlite3`** module backed by `rusqlite` (SQLite bundled
  in the binary so there's no system dependency). `connect`,
  `Connection`, `Cursor`, `Row`, prepared statements with `?`/
  named parameters, `executemany`, `executescript`, `commit` /
  `rollback`, transaction modes, type adapters/converters,
  `register_adapter` / `register_converter`, the standard error
  hierarchy.
- A real **`decimal`** module (Rust core via `rust_decimal` for
  the hot path, frozen Python for the user-visible class) and a
  frozen **`fractions`** module on top of bignum.
- A real **`pickle`** + `copyreg` + `shelve` trio (frozen Python).
  Protocols 0–5; the `dispatch_table`, `Pickler`/`Unpickler`
  classes, `loads`/`dumps`/`load`/`dump`, the `__reduce__` /
  `__reduce_ex__` / `__getstate__` / `__setstate__` protocol.
- 17 new corpus fixtures gating the surface above.

The combination is what the project calls "Option A" in the
roadmap: drop in `pickle.dumps(2**1000)`, `with gzip.open("x.gz",
"rb") as f`, `sqlite3.connect("db.db")`, or a `zipfile.ZipFile(...)`
loop and have it work.

## Motivation

After RFC 0018, the language was effectively complete and the
introspection / test infrastructure was in place. What was *not*
in place was every real-world data-interchange path:

- Anything past `2**62` silently overflowed. RSA, hashing
  helpers, `int.from_bytes(b'...', 'big')` for any multi-byte
  input, all of `Lib/test/test_long.py`, half of
  `Lib/test/test_int.py`, every test case that constructs a
  large literal — all broken.
- `pickle.dumps(...)` raised `ImportError`, so anything that
  cached intermediate results, multiprocessing's `spawn`-style
  IPC story, the `concurrent.futures` round-trip, and every test
  that round-trips data through an in-memory file all fell over.
- `import gzip` raised `ImportError`. So did `import bz2`,
  `import lzma`, `import zipfile`, `import tarfile`, `import
  sqlite3`, `import struct`, `import codecs`, `import decimal`,
  `import fractions`. The list of unavailable stdlib modules was
  longer than the list of available ones if you measured by
  surface size.
- `pip` itself unzips wheels (`zipfile`), un-tars sdists
  (`tarfile`), and writes pickled cache files. None of those
  paths worked.
- The CPython `__pycache__` invariant — that source files
  compile to `.pyc` on first import and import-time on
  subsequent runs reads the cached bytecode if mtimes match —
  did not exist. WeavePy compiled every imported source on every
  run.
- The conformance harness's "Stage B" — running CPython
  `Lib/test/test_*.py` files unmodified — was blocked on the
  same surface, since the test files themselves use bignum,
  pickle, struct, gzip, and the rest constantly.

Down-tree, this RFC unblocks:

- A **Stage B** conformance run that includes most of CPython's
  `Lib/test/test_long.py`, `test_format.py`, `test_pickle.py`,
  `test_struct.py`, `test_decimal.py`, `test_fractions.py`,
  `test_zipfile.py`, `test_tarfile.py`, `test_gzip.py`,
  `test_bz2.py`, `test_lzma.py`, `test_sqlite3*.py`.
- `pip install` for pure-Python wheels (no C extension yet, but
  the wheel format / dependency resolution / cache layer is now
  WeavePy-runnable).
- `multiprocessing` over `pickle`-based IPC (the next concurrency
  RFC depends on `pickle` working).
- The C-extension story (RFC 0020-ish): wheels are zipfiles, so
  `zipfile` is a precondition; cached bytecode is `marshal`-
  produced, so `marshal` is too.

## CPython reference

This RFC tracks **CPython 3.13**:

- **Bignum** — `Objects/longobject.c`, the language reference
  §"Numeric Types — int, float, complex". The PEP 3127 prefix
  rules (`0b`/`0o`/`0x`) and the `_` digit-separator rule.
- **Complex** — `Objects/complexobject.c` plus the language
  reference. Rectangular form only; polar conversions live in
  `cmath` and are out of scope.
- **`struct`** — `Modules/_struct.c` plus the language reference
  "Format Strings" table.
- **`codecs`** — `Lib/codecs.py`, `Modules/_codecsmodule.c`,
  `Lib/encodings/*`. We follow the public `lookup` /
  `register` / `register_error` / `lookup_error` surface and the
  built-in error handlers.
- **`marshal`** — `Python/marshal.c`. We implement the version-4
  format (CPython 3.4+) plus the version-5 *short references*
  optimisation that 3.8+ uses by default.
- **`pickle`** — `Lib/pickle.py`, `Lib/copyreg.py`,
  PEP 3154 (protocol 4) + PEP 574 (protocol 5 with out-of-band
  buffers). We implement the Python pickler/unpickler; CPython's
  C accelerator (`_pickle`) is not provided.
- **`gzip`** / **`bz2`** / **`lzma`** — `Lib/gzip.py`,
  `Lib/bz2.py`, `Lib/lzma.py`. The Python wrappers track CPython;
  the underlying engines come from `flate2`, `bzip2`, and
  `lzma-rs`.
- **`zipfile`** / **`tarfile`** — `Lib/zipfile.py`,
  `Lib/tarfile.py`. We ship a substantial subset; see "Drawbacks"
  for what's out of scope.
- **`sqlite3`** — `Modules/_sqlite/`. We back it with `rusqlite`,
  which links bundled SQLite (no system dep).
- **`decimal`** — `Modules/_decimal/` and `Lib/_pydecimal.py`. We
  ship a Rust accelerator (`_decimal`) for the common path plus
  a frozen Python wrapper that exposes the public `Decimal` /
  `Context` / `getcontext` surface.
- **`fractions`** — `Lib/fractions.py`. Pure-Python on top of
  bignum.
- **`runpy` `__pycache__` invariant** — PEP 3147 / PEP 488. Cache
  filenames look like `module.weavepy-3.13.pyc` to avoid
  colliding with CPython's own `module.cpython-313.pyc` entries
  in shared trees.

We deliberately do **not** track:

- **Decimal arbitrary-precision contexts** — we expose a 96-bit
  decimal via `rust_decimal` and accept that very-high-precision
  computations (`getcontext().prec = 50`) are clamped. Real
  arbitrary-precision decimal is a follow-up RFC.
- **`pickle` C accelerator (`_pickle`)** — pure Python is fast
  enough for the common case.
- **`zipfile.ZipFile.testzip` over deflate64 / zip64-only
  archives at extreme sizes**. The 4 GiB / 65535-entry zip64
  envelope is supported; pathological corner cases are not.
- **`tarfile.PAX` extended header subtleties** — we read and
  write `USTAR` and `PAX` headers; the long list of optional
  PAX records is approximate.
- **`sqlite3` extension loading** (`enable_load_extension`).
  Bundled SQLite ships without the loadable-extension entry
  points.
- **`codecs.StreamReader` / `StreamWriter` / `StreamRecoder`**
  — the file-oriented codec wrappers. The most-common path
  (`io.TextIOWrapper(..., encoding=...)`) is what the runtime
  uses; the explicit codec-stream classes are a follow-up.

## Detailed design

See the implementation. The shape is:

- **bignum**: `Object::Long(Rc<BigInt>)` is added alongside the
  existing `Object::Int(i64)`. All arithmetic checks for overflow
  and promotes; comparison/hash/eq treat `Int(0)` and `Long(0)`
  as identical; `int(...)` parsing produces the smaller of the
  two when possible.
- **complex**: `Object::Complex(Rc<PyComplex>)`. Lexer's
  existing `j`/`J` suffix gates a new constant kind; compiler
  emits `Constant::Complex(real, imag)`; `complex_to_object`
  builds the variant.
- **struct / codecs / marshal**: each is a standalone Rust
  module under `crates/weavepy-vm/src/stdlib/`. They depend on
  the bignum + complex paths above; the marshal format
  references both directly.
- **compression**: `_gzip`, `_bz2`, `_lzma` are Rust cores;
  `gzip`, `bz2`, `lzma` are frozen Python wrappers that build
  the file class on top of the existing `PyFile` machinery.
- **archives**: `zipfile` and `tarfile` are frozen Python that
  build on `struct` + the compression stack.
- **sqlite3**: Rust core via `rusqlite` (with `bundled` feature
  so SQLite ships in-binary), exposing `_sqlite3.connect` and
  the row/cursor/connection types. A frozen `sqlite3.py` adds
  the user-visible exception hierarchy and the converter/adapter
  registry.
- **decimal / fractions**: Rust `_decimal` core wrapping
  `rust_decimal::Decimal`, frozen `decimal.py` for the
  `Decimal` class and context machinery; frozen `fractions.py`
  on top of bignum.
- **pickle / copyreg / shelve**: pure-Python on top of
  `_struct` + bignum + the existing object model.
- **__pycache__**: `import.rs` gains a cache lookup that, given
  a source filename, computes the cache path
  (`__pycache__/<name>.weavepy-3.13.pyc`), reads it via
  `marshal.loads` if mtime matches, and writes a fresh one via
  `marshal.dumps` after compilation otherwise.

### Object model

```rust
pub enum Object {
    // ... existing variants ...
    Long(Rc<BigInt>),
    Complex(Rc<PyComplex>),
}

#[derive(Clone, Copy, Debug)]
pub struct PyComplex {
    pub real: f64,
    pub imag: f64,
}
```

Helpers in a new `pyint` module convert freely between
`Object::Int(i64)`, `Object::Long(Rc<BigInt>)`, and `BigInt`.
Construction goes through `Object::int_from_bigint(b)`, which
demotes to the small-int variant when `b` fits in `i64`.

### Bignum arithmetic

`binary_op` in `weavepy-vm/src/lib.rs` is restructured around
checked arithmetic on i64 with bignum fallback:

```rust
(O::Int(x), O::Int(y), B::Add) => match x.checked_add(*y) {
    Some(r) => Ok(O::Int(r)),
    None => Ok(int_from_bigint(BigInt::from(*x) + BigInt::from(*y))),
},
```

`(Long, Long)`, `(Int, Long)`, and `(Long, Int)` cases delegate
to `BigInt` arithmetic. Float coercion (`(Long, Float)`) goes
through `bigint_to_f64_lossy`.

Comparison treats `Int` and `Long` as the same type for the
purposes of `<`/`==`/`hash` — `Int(0).hash() == Long(0).hash()`,
and `Int(0) < Long(1)` returns `true`. Hash uses CPython's
modulo-`2**61 - 1` formula so hashes match across the
representation boundary.

### Errors

| Bignum / serialization error | Python exception |
|------------------------------|------------------|
| Negative shift count | `ValueError` |
| `int.to_bytes(...)` length too small | `OverflowError` |
| `int.from_bytes(b, byteorder)` invalid byteorder | `ValueError` |
| `struct.pack` value out of range | `struct.error` (alias for OverflowError below 3.4) |
| `struct.unpack` buffer too short | `struct.error` |
| `pickle.UnpicklingError` on bad opcode | `pickle.UnpicklingError` |
| `pickle.PicklingError` on un-pickleable | `pickle.PicklingError` |
| `marshal` malformed input | `ValueError("bad marshal data")` |
| `gzip.BadGzipFile` on truncated header | `gzip.BadGzipFile` |
| `lzma.LZMAError` on corrupt stream | `lzma.LZMAError` |
| `zipfile.BadZipFile` on bad CRC / EOCD | `zipfile.BadZipFile` |
| `tarfile.ReadError` on bad block | `tarfile.ReadError` |
| `sqlite3` driver error | `sqlite3.OperationalError` / `IntegrityError` / etc. |

## Drawbacks

- **Rc<BigInt> per long.** Every `Long` is a heap allocation.
  CPython interns small ints (`-5..=256`) as immortal singletons
  and we get the same observable behaviour because our small-int
  fast path is `Object::Int(i64)`. For *medium* ints (those
  fitting in i64 but not in CPython's small-int cache) we still
  match by value. For the *large* case, every operation
  allocates a fresh `BigInt`. Future work: arena-allocated
  bignums, NaN-boxed small ints inside Long for the just-bigger-
  than-i64 range.
- **No arbitrary-precision decimal.** `rust_decimal` is 96-bit
  fixed-precision. CPython's `decimal` module is true
  arbitrary-precision. For scientific or financial code that
  requires `getcontext().prec = 50`, behaviour diverges; the
  divergence is documented and tracked. A pure-Python decimal
  on top of bignum is the straightforward follow-up.
- **`zipfile` / `tarfile` are smaller than CPython's.** The
  full ZIP64 / PAX surface is wired but the long tail of
  optional metadata (extra fields beyond `Zip64ExtendedInformation`,
  PAX records beyond size/path/mtime) is approximated.
- **`sqlite3` ships bundled SQLite (no system dep).** Binary
  size grows by ~1 MB. Loadable extensions are not supported.
- **`pickle` is the pure-Python pickler.** Faster `_pickle` C
  accelerator is a follow-up.
- **`__pycache__` cache invalidates on source mtime.** A real
  source-hash mode (PEP 552) is not yet wired; the timestamp
  mode that CPython 3.7+ uses by default works here.
- **`marshal` writes a WeavePy-flavoured magic number.** The
  bytecode CPython produces is not byte-compatible with ours
  (different opcodes), so a CPython `.pyc` will not load under
  WeavePy and vice versa. The cache-filename suffix is
  `.weavepy-3.13.pyc` to avoid collisions in shared trees.
- **`codecs` does not load on-disk codec modules.** CPython
  walks `Lib/encodings/` lazily; we ship a fixed registry. New
  encodings can be registered via `codecs.register(...)` at
  runtime.

## Alternatives

- **Replace `Object::Int(i64)` with `Object::Int(Rc<BigInt>)`
  unconditionally.** Cleaner type but every small-int op pays
  for an allocation. Rejected.
- **Tagged-pointer object model first (RFC 0002), then bignum
  on top.** Tempting but huge — the object-model rework is its
  own multi-month commit. Bignum is independently valuable;
  ship it now and let the model rework subsume the
  representation choice later.
- **Wrap CPython's actual `Lib/pickle.py` / `Lib/zipfile.py` /
  `Lib/tarfile.py` vendored unmodified.** Rejected for now: those
  modules use built-in inheritance (`class ZipFile(...)` and a
  long chain of `__slots__` + descriptor patterns) and assume
  `_pickle` / `_struct` are loaded. Our re-implementations track
  the surface; the divergence on private internals is
  intentional and small.
- **Use `lzma-sys` (system liblzma) instead of `lzma-rs`.**
  Faster but adds a system dependency. We pick `lzma-rs` for
  pure-Rust portability; the perf gap is acceptable for now.
- **Use `bzip2` (libbz2 binding) instead of `bzip2` with the
  `static` feature.** Same trade-off; we pick the static-bundled
  feature so there's no system dep.
- **Skip `decimal`.** Rejected: many applications use `Decimal`
  for currency arithmetic and any test corpus that touches
  finance code reaches for it.

## Prior art

- **CPython 3.13** — the conformance target.
- **PyPy** — ships unmodified `Lib/pickle.py`, `Lib/zipfile.py`,
  `Lib/tarfile.py`, etc. on top of a bignum integer that's
  cached for small values. Their decimal is a Rust-ish (RPython)
  port of CPython's `_decimal`; ours is `rust_decimal`-backed.
- **RustPython** — uses `num-bigint` for ints; ships a similar
  Rust+Python split for compression and archives. Their
  `sqlite3` is also `rusqlite`-bundled.
- **MicroPython** — small int + GMP-lite bignum; no
  zipfile/tarfile/sqlite3/decimal. Useful comparison for "the
  minimum viable serialization stack."

## Unresolved questions

- **Bignum hash compatibility.** We follow CPython's `2**61 - 1`
  prime modulus for hashes of arbitrary-size ints. The host
  platform's pointer size doesn't change the hash output (we do
  the math on `i128`).
- **Pickle protocol 5 out-of-band buffers.** The `PickleBuffer`
  class is shipped, but the buffer-aware pickler integration
  with `multiprocessing` shared memory is a follow-up.
- **`sqlite3.Row` factory chains.** We support
  `connection.row_factory = Row` and `lambda c, r: dict(...)`
  patterns; the deeper factory protocol used by some ORMs is
  approximate.
- **`marshal` format-version selection.** We default to writing
  format 4. CPython 3.13 defaults to format 4 too; format 5
  (with reference deduplication) is supported on read but not
  yet on write.

## Future work

- **Arbitrary-precision `decimal`** on top of bignum — replaces
  the 96-bit `rust_decimal` core for users who need it.
- **`_pickle` C-shaped accelerator** in Rust — drop-in for the
  pure-Python pickler when speed matters.
- **PEP 552 source-hash `__pycache__` mode** — alternative to
  the mtime mode shipped here.
- **Loadable `sqlite3` extensions** — gated on a build flag that
  re-enables the entry points in bundled SQLite.
- **Native bignum representation** — small-Long inline, arena
  allocation, NaN-boxed pointer for a tagged-pointer Object
  model (RFC 0002).
- **`zoneinfo`** — IANA timezone database, the natural follow-up
  to RFC 0018's `datetime`. Out of scope here; the `tzdata`
  package would need to ship as frozen data.
