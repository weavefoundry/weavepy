# RFC 0023: Drop-in parity — stdlib tail, IO hierarchy, HTTPS

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-25
- **Tracking issue**: TBD

## Summary

Close the long tail of CPython 3.13 surface that pure-Python scripts
routinely reach for, so a generic Python program — short of needing
real OS threads or a CPython C extension — runs unchanged under
`weavepy`. After this RFC lands, `weavepy script.py` is
indistinguishable from `python3 script.py` for the overwhelming
majority of scripts and pure-Python packages, including those that
talk HTTPS, walk the filesystem, pickle objects, or query the Unicode
database.

The RFC bundles three threads:

1. **Language gaps.** Walrus `:=`, PEP 695 type aliases / generic
   parameters, PEP 657 column tracebacks, PEP 701 nested f-strings,
   plus the handful of missing built-ins (`pow`, `breakpoint`, `help`,
   `copyright`, `license`, `memoryview`).
2. **Stdlib closure.** ~20 missing modules — `unicodedata`, `bisect`,
   `operator`, `copy`, `stat`, `posixpath`, `ntpath`, `genericpath`,
   `string`, `textwrap`, `atexit`, `numbers`, `statistics`, `mmap`,
   `_io`, `_string`, `_warnings`, `_random`, `_abc`, `_contextvars`,
   `_pickle`, `_locale`/`locale` — and an upgrade of the modules we
   already ship: a full `io` hierarchy (`IOBase`/`RawIOBase`/
   `BufferedIOBase`/`TextIOBase`/`FileIO`/`BufferedReader`/
   `BufferedWriter`/`BufferedRandom`/`TextIOWrapper`), an `os`
   that finally includes `chdir`/`scandir`/`walk`/`pipe`/`dup`/
   `fspath`/`stat_result`, real `pickle` for arbitrary objects, and
   arbitrary-precision `Decimal`.
3. **HTTPS.** A real `_ssl` engine backed by **rustls** + **rustls-
   native-certs**, wired through `ssl.SSLContext.wrap_socket`,
   `http.client.HTTPSConnection`, `urllib.request.urlopen`, and
   `asyncio.open_connection(ssl=…)` so that HTTPS works
   end-to-end.

Net diff: **~25–32K LOC** (Rust core + frozen Python + tests +
conformance).

## Motivation

After RFC 0020/0021/0022 the interpreter ran every language feature
we cared about and could load a hand-rolled C extension. But running
a *real* Python script still tripped on a thousand small papercuts —
missing `unicodedata`, no `io.TextIOWrapper`, `bisect` /
`operator` / `copy` not importable, `pickle` only for primitives,
HTTPS hard-coded to `NotImplementedError`. Every one of those bugs
takes ~20 lines to fix individually; bundling them is a multiplier
on conformance and a real "drop-in" milestone.

Concretely, the conformance smoke test for this RFC is the same
script under `python3` and `weavepy`:

```python
import unicodedata, bisect, copy, statistics, textwrap, mmap
import urllib.request, pickle
data = urllib.request.urlopen("https://example.com").read()
print(unicodedata.name("é"))
print(pickle.loads(pickle.dumps({"a": [1, 2, 3]})))
```

Today this fails on every line under `weavepy`. After RFC 0023 it
matches CPython byte-for-byte.

## CPython reference

We track **CPython 3.13.0** for surface and semantics. Specific
references:

- `Lib/io.py` and `Modules/_iomodule.c` for the IO hierarchy.
- `Lib/unicodedata.py` (a thin wrapper) + `Modules/unicodedata.c`
  for the Unicode character database. The actual data tables come
  from the `unicode-properties` Rust crate, which itself wraps the
  Unicode 16.0.0 data files.
- `Lib/copy.py`, `Lib/bisect.py`, `Lib/operator.py`,
  `Lib/textwrap.py`, `Lib/numbers.py`, `Lib/statistics.py`,
  `Lib/stat.py`, `Lib/posixpath.py`, `Lib/ntpath.py`,
  `Lib/genericpath.py`, `Lib/string.py`, `Lib/atexit.py`,
  `Lib/_pyio.py`.
- PEP 657 (column tracebacks), PEP 701 (PEP 701 f-strings),
  PEP 695 (type parameter syntax), PEP 572 (walrus).
- `Modules/_ssl.c` for the API shape; the implementation is rustls.

## Detailed design

### 1 — Language: walrus, PEP 695, PEP 657, PEP 701

**Walrus `:=` → `NamedExpr`.** The token already exists in the
lexer; the AST already carries `ExprKind::NamedExpr`. We wire the
parser entry in `parse_or_test` to accept `<name> := <expr>` (with
the right associativity / precedence as defined in PEP 572) and
emit `NamedExpr { target, value }`. Compiler treats it as a
`StoreName`/`StoreFast` followed by a duplicate-top.

**PEP 695 generic syntax.** Three new forms:

- `type Alias = T` at module level — compiled as
  `LoadConst <TypeAliasObject>` + `StoreName Alias`.
- `def f[T, *Ts, **P](...)` — type parameters parsed and stored on
  the function's `__type_params__` attribute. Bodies see them as
  ordinary `TypeVar`/`TypeVarTuple`/`ParamSpec` objects.
- `class C[T]:` — same, on the class.

A new soft keyword `type` is recognised at statement position.
Type parameters borrow the syntax for `bound=`, `default=`, and
`*`/`**` prefix from PEP 695.

**PEP 657 — column tracebacks.** We extend the compiler's per-
instruction `linetable` to a parallel `column_table: Vec<(u32, u32)>`
storing the source byte range `(start_col, end_col)` of each
instruction's originating AST node. Tracebacks render the snippet
plus a `~~~^^^^~~~~` caret span when columns are available.

**PEP 701 — nested f-strings.** The lexer learns to re-enter
itself on every `{` inside an f-string; previously we closed the
outermost f-string on the first matching closing quote regardless
of bracket depth. This unlocks `f"{f'{x}'}"`,
`f"{ {1: 'one'} }"`, and backslashes in replacement fields.

### 2 — Missing built-ins and type objects

- `pow(base, exp[, mod])` — full three-arg with the CPython modular
  exponentiation fast path for ints.
- `breakpoint()` — looks up `PYTHONBREAKPOINT`, falls back to
  `pdb.set_trace()`.
- `help(obj)` — a thin shim that walks attrs and prints docstrings.
- `copyright`, `license`, `credits` — `_Printer` instances with the
  CPython text verbatim.
- `memoryview(obj)` — a real `MemoryView` runtime object backed by
  a slice into `bytes`/`bytearray` / future Buffer protocol objects.

The `BuiltinTypes` registry gains entries for `complex`, `slice`,
`memoryview`, `mappingproxy`, `dict_keys`, `dict_values`,
`dict_items`, `range_iterator`, `bytes_iterator`, `str_iterator`,
`list_iterator`, `tuple_iterator`, `set_iterator`, `dict_keyiterator`,
`dict_valueiterator`, `dict_itemiterator`. Existing values
(`Object::Complex`, `Object::Slice`) are wired so `isinstance(x,
complex)` and `isinstance(s, slice)` finally return `True`.

`dict.keys()`, `.values()`, `.items()` now return view objects of
the appropriate type instead of fresh lists. The views implement
`__len__`, `__iter__`, `__contains__`, and `__or__`/`__and__` etc.
on the keys view.

### 3 — Stdlib: new Rust cores

- **`unicodedata`** (~2K LOC) — `unicode-properties` crate wrapped
  in a `unicodedata` module surface: `name`, `lookup`, `category`,
  `bidirectional`, `combining`, `mirrored`, `decimal`, `digit`,
  `numeric`, `decomposition`, `normalize`, `is_normalized`, plus
  the `ucd_3_2_0` and `unidata_version` constants.
- **`_io`** (~3K LOC) — the C-level layer behind `io`. `_RawIOBase`
  is a Rust trait; concrete `FileIO` wraps a `std::fs::File`
  descriptor. `BufferedReader`/`Writer`/`Random` keep a `Vec<u8>`
  buffer per side. `TextIOWrapper` composes a buffered binary
  stream with a `codecs` codec for read/write.
- **`_string`** — `string.Formatter`'s field-name parser
  (`formatter_field_name_split`, `formatter_parser`). Tiny.
- **`_random`** — the Mersenne Twister state machine, exposed as
  `_random.Random`. The existing `random` Python module is
  rewritten to drive it.
- **`_warnings`** — the C-side of `warnings`: filters list,
  `warn`/`warn_explicit`/`filters_mutated`. Drives the existing
  frozen `warnings.py`.
- **`_pickle`** — a Rust accelerator for the protocol-4 pickle
  encoder/decoder. Falls back to Python `pickle` for unsupported
  shapes.
- **`mmap`** — `memmap2`-backed `mmap.mmap` objects with
  buffer-protocol exposure.
- **`_locale`** — `setlocale`/`getlocale`/`localeconv` calling into
  libc. The `locale.py` Python wrapper is shipped frozen.
- **`_abc`** — Rust-backed registry of ABC virtual subclasses and
  the cache that `isinstance(obj, ABC)` uses. Plugs into the
  existing frozen `abc.py`.
- **`_contextvars`** — `ContextVar` / `Context` / `copy_context`
  / `Token` implemented over a thread-local stack of dicts.

### 4 — Stdlib: new frozen Python modules

- **`bisect`** — straight reference implementation; backed by the
  `_bisect` Rust accelerator (added because `statistics` needs it
  on the hot path).
- **`operator`** — every operator function, plus `attrgetter`,
  `itemgetter`, `methodcaller`. Pure Python.
- **`copy`** — full `copy`/`deepcopy` with `_copy_dispatch`,
  `__copy__`/`__deepcopy__`/`__reduce_ex__` hooks.
- **`stat`** — `S_IFREG`, `S_ISDIR`, etc., bit-identical to CPython.
- **`posixpath`/`ntpath`/`genericpath`** — three explicit
  modules. `os.path` aliases to the platform-appropriate one at
  import time.
- **`textwrap`** — `wrap`, `fill`, `shorten`, `indent`, `dedent`,
  `TextWrapper`.
- **`atexit`** — Python wrapper over a tiny Rust core that
  registers an exit handler on `Interpreter::shutdown`.
- **`numbers`** — the ABCs `Number`, `Complex`, `Real`, `Rational`,
  `Integral`, plus `register()` calls for the built-in numeric
  types.
- **`statistics`** — `mean`, `median`, `mode`, `pstdev`, `pvariance`,
  `stdev`, `variance`, `harmonic_mean`, `geometric_mean`,
  `quantiles`, `correlation`, `covariance`, `linear_regression`,
  `NormalDist`.

### 5 — IO hierarchy rewrite

Today `open()` returns a flat `PyFile` and `io` exposes only
`StringIO`/`BytesIO`. We rewrite:

```
io.IOBase
├── io.RawIOBase
│   └── io.FileIO            ← Rust-backed raw file descriptor
├── io.BufferedIOBase
│   ├── io.BufferedReader    ← Rust buffer + RawIOBase delegate
│   ├── io.BufferedWriter
│   └── io.BufferedRandom
└── io.TextIOBase
    ├── io.TextIOWrapper     ← codec + BufferedIOBase delegate
    └── io.StringIO
```

`open(path, mode="r", encoding=None, errors=None, newline=None)`
returns a properly layered object — `FileIO → BufferedReader →
TextIOWrapper` for `"r"`, etc. The existing `PyFile` is retained
as a fast path for ASCII-mode pure-binary use cases.

### 6 — `os` completeness

`os` gains: `chdir`, `getcwdb`, `scandir` (returning real
`DirEntry` objects), `walk` (generator), `pipe`, `pipe2`, `dup`,
`dup2`, `fspath`, `fstat`, real `stat_result` (a `_struct_seq`),
`getlogin`, `getuid`/`getgid`/`getppid`, `WIFEXITED`/`WEXITSTATUS`/…,
proper mutable `environ` whose mutations call `setenv(3)`, and
`PathLike` protocol on the import path.

### 7 — Real HTTPS via rustls

`ssl_mod.rs` is rewritten:

- **`ssl.SSLContext`** — wraps a `rustls::ClientConfig` /
  `ServerConfig`.
- **`ssl.SSLContext.wrap_socket(sock, server_hostname=…)`** —
  returns a real `SSLSocket` that drives a
  `rustls::ClientConnection` over the underlying `_socket.socket`.
- **`ssl.create_default_context()`** — picks up
  `rustls-native-certs` and the Mozilla CA bundle.
- **`ssl.SSLError`** real exception type.
- **HTTPS in urllib/http.client** — the `_ssl` engine flows through
  unchanged because the higher-level modules call
  `context.wrap_socket`.
- **asyncio TLS** — `loop.start_tls()` and the `ssl=` kwarg on
  `open_connection`/`start_server` now drive the same engine.

We intentionally **do not** support every CPython `ssl` knob:
`set_ciphers`, custom verify callbacks, `set_default_verify_paths`,
and OCSP stapling are accepted but mostly inert. The set we support
is enough for HTTPS to PyPI / GitHub / any standard certbot-issued
host.

### 8 — Other surface

- **PEP 420 namespace packages.** `import.find_source` falls back
  to a directory without `__init__.py` and sets `__path__` to the
  set of directories that contributed.
- **`sys.implementation`** is a `SimpleNamespace`-shaped object
  (we model it as a `module`-like attribute container) with
  `name='weavepy'`, `version=(0,0,0,'final',0)`, `hexversion`,
  `cache_tag='weavepy-3.13'`.
- **Signal delivery.** The eval loop checks
  `signal_mod::pending()` every `Resume` (function entry) and on
  the back-edge of `JumpBackward`. Pending signals raise
  `KeyboardInterrupt` / `SystemExit` as appropriate.

## Implementation status (post-merge)

| Item | Status |
|------|--------|
| Walrus parser | ✅ |
| PEP 695 type aliases / params | ✅ |
| PEP 657 column tracebacks | ✅ |
| PEP 701 nested f-strings | ✅ |
| `pow`/`breakpoint`/`help`/`copyright`/`license`/`memoryview` | ✅ |
| `complex`/`slice`/`memoryview`/`mappingproxy` types | ✅ |
| dict view types | ✅ |
| `unicodedata` | ✅ |
| `_io` + full hierarchy | ✅ |
| `_string` | ✅ |
| `_random` core | ✅ |
| `_warnings` | ✅ |
| `_pickle` | ✅ |
| `mmap` | ✅ |
| `_locale` + `locale` | ✅ |
| `_abc` | ✅ |
| `_contextvars` | ✅ |
| `bisect`, `operator`, `copy`, `stat`, `textwrap`, `atexit`, `numbers`, `statistics` | ✅ |
| `posixpath`, `ntpath`, `genericpath` | ✅ |
| `os.chdir`/`walk`/`scandir`/`pipe`/`dup`/`fspath`/`stat_result`/`environ` | ✅ |
| `pickle` for arbitrary objects | ✅ |
| Arbitrary-precision `Decimal` | ✅ |
| `rustls` `_ssl` + HTTPS through urllib/http.client/asyncio | ✅ |
| PEP 420 namespace packages | ✅ |
| `sys.implementation` namespace | ✅ |
| Signal delivery in eval loop | ✅ |
| Conformance corpus expansion | ✅ |

## Drawbacks

- The IO hierarchy rewrite is a one-time cost on every script
  startup: text-mode `open()` now goes through three Python objects
  instead of one Rust struct. The hot ASCII fast path retains the
  flat `PyFile` shape behind the `TextIOWrapper` constructor for
  this reason, but a careful benchmark will still see a small
  regression on `open(path).read()`. We measured ~5% on the
  conformance fixtures and accepted it.
- `rustls` does not implement TLS 1.0/1.1, weak ciphers, or
  arbitrary cipher selection. Any code path that relies on those
  is broken; we document the intentional divergence.
- `unicodedata` ships an embedded ~600KB UCD blob; the binary grows
  by ~3MB after compression and LTO.

## Alternatives

1. **Ship CPython `Lib/` and link against it.** Would close every
   stdlib gap in one move, but explicitly excluded by RFC 0020 —
   we want our own frozen tree so we control determinism and
   build size.
2. **`native-tls` instead of `rustls`.** Better cipher coverage,
   but pulls a C dependency and changes the platform-specific cert
   discovery story. We picked `rustls` to stay pure-Rust.
3. **Skip PEP 657 columns.** Easy to skip and we could land
   everything else without it. We chose to include it because most
   third-party error-rendering libraries (rich, traceback2) already
   look for it.

## Prior art

- **PyPy** ships a forked `_pyio.py` and a flat `_io` shim in
  RPython. Their split between `_io` (C side) and `io` (Python
  side) is the same one we adopt.
- **MicroPython** elides `unicodedata` for binary-size reasons.
  We can't because real scripts call `.normalize()`.
- **GraalPy** delegates `_ssl` to Java's TLS stack. Same shape;
  we pick rustls instead of Java.

## Unresolved questions

- Should `_ssl` support TLS 1.0 for legacy server compatibility?
  Currently no; `rustls` doesn't. If a real user hits this, we
  add a feature-gated `native-tls` backend.
- `os.scandir` returns `DirEntry` objects whose `stat()` calls
  re-stat. CPython caches; we don't yet, because the cache lifetime
  rules are subtle and we'd rather start correct.
- `mmap` on Windows uses `CreateFileMappingW` semantics; we ship
  `memmap2` which abstracts that, but the surface differs from
  CPython in edge cases (anonymous mmap size, `MAP_FIXED`). We
  document and move on.

## Future work

- **RFC 0024** — real OS threads + GIL.
- **RFC 0025** — cycle GC, real weakrefs.
- **RFC 0026** — numpy on the C-API foundation from RFC 0022.
