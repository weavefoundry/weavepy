# WeavePy

WeavePy is an experimental high-performance Python interpreter written in Rust,
designed to be a 100% compatible, drop-in replacement for CPython. The goal is
simple but ambitious: run existing Python code, packages, tools, and workflows
unchanged while dramatically improving execution speed, startup time, memory
usage, and runtime scalability. WeavePy treats CPython compatibility as the
baseline, not a stretch goal — using CPython's own test suite as a guiding
standard while exploring a modern Rust-based runtime architecture built for
aggressive optimization, native interoperability, and long-term performance
work.

> **Status: drop-in replacement for the documented CPython 3.13 surface,
> with a measured conformance baseline and a live C-extension entry point.**
> The bundled regression suite at `tests/regrtest/` covers the seven
> semantic groups exercised by `RFC 0027` (object model, iterators/
> generators/coroutines, numerics/strings/format, containers, exceptions/
> context, serialization/compression/codecs, and IO/OS/argparse/inspect/
> typing) and is green on `main`. `RFC 0028` adds the PEP 3118 buffer
> protocol, PEP 590 vectorcall, the full `PyType_FromSpec[WithBases]`
> slot surface, and a `_ndarray.c` C-extension fixture exercising the
> stack end-to-end. `RFC 0029` closes the loop: the `datetime` C-API,
> the full `PyCapsule` surface, keyword-aware `PyArg_ParseTupleAndKeywords`,
> property-aware descriptor dispatch in `tp_getset`, a numpy-shaped
> `_numpylike.c` fixture exercising `dtype`/ufuncs/buffer-protocol/
> reshape/`mask_select`/`PyDateTime`, a PEP 425 wheel-tag matcher in
> `_minipip` (so binary wheels resolve), and an end-to-end regression
> test that installs a binary wheel under a private prefix and imports
> the bundled extension through the regular `ExtensionFileLoader`
> path — proving the `numpy` install-and-run story works
> mechanically. `RFC 0030` ships the *pure-Python* drop-in surface:
> a real PyPI-compatible `pip` (PEP 440/503/508/425, dependency
> resolver, PEP 517 sdist builds, full CLI), a `numpy` facade with
> pure-Python fallback so `import numpy` works without compiling
> `_numpylike`, a bundled `pytest` + `pluggy` + `iniconfig` +
> `exceptiongroup` stack, and `sys.settrace` / `sys.setprofile` /
> `sys.monitoring` (PEP 669) + `tracemalloc` observability so
> debuggers, coverage tools, and profilers boot. `RFC 0031`
> closes the observability loop: the VM dispatcher actually
> *fires* the registered hooks (call / line / return / yield /
> exception for `settrace` + `setprofile`; the PEP 669 event
> table for `sys.monitoring`; `record_alloc` from container-
> construction opcodes for `tracemalloc`; PEP 578 audit dispatch
> at open / compile / exec / eval / import / marshal sites). The
> same commit lands PEP 684 sub-interpreters (`_xxsubinterpreters`
> + a high-level `interpreters` frontend with cross-interpreter
> channels), wires `pdb` / `bdb` on top of the now-firing
> `settrace`, and grows `_pytest` to handle `@pytest.mark.parametrize`
> Cartesian matrices, indirect fixtures, `request.addfinalizer`
> LIFO ordering, and per-scope (function / class / module /
> session) fixture caching. `RFC 0036` wires a real CPython 3.13
> `Lib/test/` checkout into the `regrtest` harness (`--cpython-dir`,
> crash-isolated `--mode subprocess`, `--jobs`) and rewrites the touched
> rows of `tests/regrtest/expectations.toml` from guesses to a **measured**
> baseline (`unexpected 0` on a fresh sweep). `RFC 0049` (wave 5)
> retires the curated allowlist as a scope mechanism: discovery now
> schedules **every** vendored `test_*.py` file and `test_*/` package
> (504 labels, up from 227), and `expectations.toml` is a measured
> whole-suite baseline — 226 of the 427 vendored-CPython labels pass
> under the sweep budget (plus all 77 bundled fixtures), and every red
> row carries a measured first-failure reason. The same
> wave landed the `SETUP_ANNOTATIONS` opcode (block-entry
> `__annotations__`, lazy type/module getsets), CPython-strict
> `bool()`/`__bool__`/`__len__` semantics, `str` argument-clinic arity
> across ~30 native methods, full-mapping-protocol `str.format_map`,
> saturating int shift semantics, code-object value equality
> (`code_richcompare`), `Py_ReprEnter`-style recursive-repr guards on
> dict views and the io stack (fixing two native stack overflows), a
> CPython-shaped `codeop`, verbatim `configparser`, and the six
> built-in `codecs` error-handler callables. Expect small breaking
> changes around the edges as the long tail catches up.
>
> `RFC 0033` makes WeavePy *introspectable like CPython*. It ships a
> CPython-faithful **code-object surface** (`co_code`, `co_linetable`
> (PEP 626), `co_exceptiontable`, `co_positions()` (PEP 657),
> `co_stacksize`, `co_qualname`, `co_lines()`, `replace()`, …) backed by
> a new `cpython_code` codec that re-encodes WeavePy's instruction stream
> into CPython 3.13's 16-bit `_Py_CODEUNIT` form (`EXTENDED_ARG` + inline
> `CACHE` entries). On top of that it lands the four introspection
> modules every serious tool reaches for — `import ast`, `import dis`,
> `import opcode`, `import symtable` — as frozen Python over thin Rust
> cores (`_ast`, `_symtable`), a `marshal` that serialises code objects
> byte-compatibly with CPython 3.13 (`TYPE_CODE` + `FLAG_REF` shared refs
> + exact 15-bit bigint digits), and real `.pyc` read/write under
> `__pycache__` using CPython's `b"\xf3\r\r\n"` magic + PEP 552 header
> (kept collision-safe by a distinct `weavepy-3.13` cache tag). Six
> bundled regrtests cross-check the whole surface against CPython 3.13.
>
> `RFC 0054` lands the **async wave**: a native `_asyncio` C-accelerator
> (the real `Future`/`Task` state machines, `current_task`/`all_tasks`,
> eager task factories, cancellation bookkeeping) that CPython's frozen
> `asyncio` adopts via its normal import hook, plus an OpenSSL-shaped
> `_ssl` over rustls — full `getpeercert()` X.509→dict parsing, SNI
> servername callbacks via a two-phase `rustls::server::Acceptor`
> handshake, server-side ALPN, session stats, options/verify_flags
> bitmasks (`VERIFY_X509_STRICT`, CRL checks), dual RSA/ECC certificate
> slots, encrypted PKCS#8 keys with password callbacks, per-message
> handshake callbacks, and TLS 1.3 post-handshake-auth emulation. The
> vendored `test_asyncio` package now grades as 31 per-submodule rows —
> **all 31 pass** over real loopback sockets, real subprocess transports,
> and rustls TLS — and the network tail graduates to measured `pass`
> rows: `test_ssl` (191 tests), `test_urllib2`, `test_poplib`, joining
> the already-green httplib/imaplib/ftplib/smtplib/socketserver family.
>
> `RFC 0055` is the **daily-driver wave**: the acceptance bar moves from
> "passes CPython's tests" to "runs real installed packages". A new
> `ecosystem` conformance lane (`tests/ecosystem/`) creates a scratch
> venv per manifest row with the WeavePy binary under test, installs
> real PyPI packages through the in-tree pip (online or fully offline
> via a wheel cache), runs a behaviour-asserting probe, and grades
> against a checked-in baseline — **all nine launch rows pass**: six,
> attrs, click, jinja2, requests, python-dateutil, typing_extensions,
> packaging, and *real* pytest (8.4). Getting there landed the mypyc
> C-API tail (charset_normalizer's compiled `.so` loads and runs),
> dependency-closure resolution in `pip install --no-index
> --find-links`, the 3.11+ `importlib.resources` package layout
> (`.abc`, `.readers`, `NamespaceLoader`), int-subclass `SystemExit`
> payloads (`sys.exit(pytest.ExitCode.OK)`), and site-packages
> precedence over the bundled third-party facades — `pip install
> packaging` (or pytest, numpy, …) now actually changes what `import
> packaging` returns. The same wave finished the CLI/REPL residuals
> (`test_cmd_line`, `test_repl`, `test_cmd_line_script` all pass).
>
> `RFC 0056` is the **modern-stack wave**: a faithful `_sqlite3` over
> rusqlite + verbatim `Lib/sqlite3`, real vendored expat behind
> `pyexpat`, the ctypes/mock/warnings residual burns, an abi3/PyO3
> surface audit (`_abi3check`), and an ecosystem matrix grown from 9
> to **27 rows — all pass**, offline from the wheel cache. Capstone:
> Django migrates against the new sqlite3 backend, runs ORM CRUD +
> `atomic()` rollback, and serves a request through `django.test.Client`.
> Net regrtest flips include `test_sqlite3`, `test_dbm_sqlite3`, the
> XML family (`test_pyexpat`/`test_sax`/`test_minidom`/`test_pulldom`/
> `test_plistlib`/`test_xml_etree*`), `test_htmlparser`,
> `test_unittest`/`test_doctest`/`test_warnings`, and
> `test_compileall`.
>
> `RFC 0057` is the **long-tail wave**: the measured whole-suite
> baseline moves from 418 to **496 of 543 `Lib/test` files passing**
> (+78 net flips, zero timeout rows, `unexpected 0`), with the
> ecosystem lane still 27/27 offline. The wave lands the
> comprehension-scope root-cause fix (the
> `test_listcomps`/`test_dictcomps`/`test_setcomps`/
> `test_named_expressions` quartet flips), exception `args` as a real
> slot, the slot-descriptor error taxonomy, `compile()` from AST with
> the `PyCF_*` flags (`test_ast` residual: 169F/80E → **1F**), frozen
> module specs + `AppleFrameworkLoader` (`test_import`/`test_types`
> now run end-to-end), a `_decimal` that passes the decTest corpus
> (`test_decimal` is a measured pass row), pickle protocol 5 with
> out-of-band `PickleBuffer` round-trips (`test_pickle`/
> `test_picklebuffer`/`test_pickletools` all pass), CPython-faithful
> pattern-match codegen + jump threading for trace-event exactness,
> and the retirement of every `timeout` row (`test_deque`/`test_mmap`/
> `test_weakref` pass under measured budgets). The re-baseline itself
> caught two engine bugs: a greedy TLS shutdown drain that could eat
> post-`close_notify` plaintext under load (intermittent `test_ssl`
> STARTTLS deadlock), and a `datetime_CAPI` stand-in shadowing the
> real capsule (segfaulting any extension doing `PyDateTime_IMPORT`,
> e.g. orjson) — both fixed and re-measured.

## Repository layout

This is a Cargo workspace organized along the classical interpreter pipeline.
Each crate owns one phase of execution and depends only on the phases before
it, so implementation work in any layer can proceed mostly in isolation.

```
weavepy/
├── Cargo.toml                  # workspace root (shared metadata, deps, lints)
├── rust-toolchain.toml         # pinned to stable + rustfmt + clippy
├── rustfmt.toml                # formatting rules
├── .cargo/config.toml          # workspace cargo aliases
├── crates/
│   ├── weavepy-lexer/          # Python source -> tokens
│   ├── weavepy-parser/         # tokens -> AST (re-exports the AST module)
│   ├── weavepy-compiler/       # AST   -> bytecode (CodeObject + opcodes)
│   ├── weavepy-vm/             # bytecode interpreter + object model
│   ├── weavepy/                # umbrella library: public Rust embedding API
│   ├── weavepy-cli/            # the `weavepy` binary, argv-compatible with `python`
│   └── weavepy-conformance/    # CPython-as-oracle harness (dev-only, not on crates.io)
├── conformance/
│   └── corpus/                 # in-tree Python fixtures graded against CPython
├── docs/
│   ├── ARCHITECTURE.md         # design overview + open questions
│   ├── CONFORMANCE.md          # how WeavePy is graded against CPython
│   └── rfcs/                   # design documents
└── .github/workflows/ci.yml    # fmt + clippy + tests on Linux/macOS/Windows + conformance
```

## Building

WeavePy targets stable Rust. The toolchain is pinned via `rust-toolchain.toml`,
so a fresh `rustup` install will pick up the right channel automatically.

```bash
# Build everything.
cargo build --workspace

# Run the test suite.
cargo test --workspace

# Lint and format checks (matches CI).
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Convenience aliases (defined in .cargo/config.toml).
cargo xtest
cargo xclippy
```

## Running

The CLI binary is named `weavepy` and aims to be argv-compatible with `python`.

```bash
# Inline source (mirrors `python -c`).
cargo run -p weavepy-cli -- -c "print('hello, weavepy')"

# Run a script file.
cargo run -p weavepy-cli -- path/to/script.py

# Print the version (mirrors `python -V`).
cargo run -p weavepy-cli -- --version
```

## CPython conformance

Compatibility is graded automatically. The `weavepy-conformance` crate
runs the host's `python3` as an oracle (tokenize, ast.parse + ast.dump,
compile + dis.dis) and reports per-phase agreement on a corpus of
Python fixtures. CI runs the harness on every PR and uploads the
report as an artifact.

```bash
cargo run -p weavepy-conformance -- run            # all phases
cargo run -p weavepy-conformance -- diff tokens    # one phase

# End-to-end: run CPython's own Lib/test/ files under WeavePy and grade
# against the measured baseline (RFC 0036).
cargo run -p weavepy-conformance -- regrtest \
    --cpython-dir vendor/cpython/Lib/test --mode subprocess --jobs 8

# Ecosystem lane: venv + pip install + probe per real PyPI package,
# graded against tests/ecosystem/expectations.toml (RFC 0055).
cargo run -p weavepy-conformance -- ecosystem                # online
python3 tools/ecosystem_fetch.py --dest target/ecosystem-wheels
cargo run -p weavepy-conformance -- ecosystem \
    --wheels target/ecosystem-wheels                         # offline
```

See [`docs/CONFORMANCE.md`](docs/CONFORMANCE.md) for the model, the
corpus layout, and the now-live CPython `regrtest`-style runner (RFC
0034 built it; RFC 0036 wired a real CPython 3.13 checkout into the CLI).

## Project goals

1. **Compatibility first.** CPython's behavior — including dark corners,
   PEP 8 grammar minutiae, and the reference C-API — is the spec. The CPython
   test suite is the acceptance harness. Performance work that breaks
   compatibility is rejected.
2. **Performance second, but seriously.** Once a feature is correct, the
   architecture should make it fast: tiered execution, inline caches,
   specialization, and a JIT are all on the long-term roadmap.
3. **Modern, safe foundation.** Written in safe Rust where possible, with
   `unsafe` confined to small, well-audited boundaries (object header layout,
   FFI to native extensions, etc.).
4. **Embeddable.** The `weavepy` crate is a library first; the `weavepy` CLI
   is just one consumer.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for development setup, coding
standards, and how to propose larger changes via the RFC process in
[`docs/rfcs/`](docs/rfcs/).

## License

WeavePy is dual-licensed under either of:

- Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))
- MIT License ([`LICENSE-MIT`](LICENSE-MIT))

at your option. This matches the rest of the Rust ecosystem, so contributions
to and from common Rust crates remain straightforward.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in WeavePy by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
