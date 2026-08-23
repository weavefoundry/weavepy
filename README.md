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
>
> `RFC 0060` is the **conformance endgame wave**: the measured
> whole-suite baseline moves to **515 of 548 files passing** (fail 27,
> error 0, skip 6, timeout 0, `unexpected 0`; +14 net flips), and the
> ecosystem lane grows to **29/29** with two capstones — **pandas**
> (binary wheel, real numpy underneath) and **FastAPI** (pydantic v2
> routes through `TestClient`). The wave lands the
> `_testcapi`/`_testinternalcapi` fixture surface (vectorcall fixture
> types un-gate `test_call`'s matrix, dict watchers, rare-event
> counters, frame probes, `normalize_path`, and an instruction-sequence
> assemble stage — `test_compiler_assemble` runs CPython-3.13 pseudo-op
> streams through to *executable* code objects), a Python-constructible
> `types.CodeType`, the full blake2/sha3/shake constructor surface,
> `sys.orig_argv` over the WTF-8 argv bridge, PEP 578 audit-hook
> blocking semantics, and retires the `test.libregrtest` shim for the
> verbatim package. The capstones caught real engine bugs — a
> `PyType_FromMetaclass` NULL-`tp_alloc` segfault, Cython's
> `PyType_CheckExact` rejecting the `_ImmutableTypeMeta` metaclass
> (retired for a truthful `Py_TPFLAGS_IMMUTABLETYPE`), `zoneinfo`
> restructured as a real package, `str`-subclass `__slots__`, and a
> `CALL_FUNCTION_EX` kwargs clone that defeated prompt reaping (an
> `SSLContext` leak under asyncio timeouts). Flips include `test_call`,
> `test_hashlib`, `test_re`, `test_ast`, `test_builtin`, `test_frame`,
> `test_zoneinfo`, and the fixture-gated introspection rows; the
> honestly-enumerated remainder (the `test_compile`/`test_peepholer`
> codegen-stage cluster, `test_capi`'s fixture fractal, and the
> unboxed-value identity legs of `test_marshal`) carries measured
> reasons in `expectations.toml`.
>
> `RFC 0067` (performance wave 5) makes the tier-2 Cranelift JIT the
> **default execution mode**: `cargo build -p weavepy-cli` now ships
> it, `WEAVEPY_JIT=0` restores the pure interpreter, and the bench
> gate measures the shipped configuration. The wave lands
> **native-to-native calls** — a compiled callee is entered directly
> with marshaled scalars (no interpreter frame, no argument binding,
> no guard re-resolution), with deopt/raise materializing the exact
> interpreter frame mid-flight — and the **native eval breaker**: a
> countdown poll at loop back edges and call sites that hands off the
> GIL, services signals/finalizers promptly, and revalidates burned-in
> globals so a cross-thread rebind (the spin-on-a-flag idiom) is
> observed within one stride. Measured on the committed macOS-aarch64
> baseline: suite geomean **3.33× CPython** (from wave 4's 8.04×),
> with `fib` under the default JIT 6.2× faster than interpreted
> (retiring wave 4's JIT call regression) and the loop kernels
> (`sumvm`/`nested_loops`/`jitloop`) at 0.05× — 20× faster than
> CPython.
>
> `RFC 0068` is **conformance zero — the final red-row burn**: the
> whole-suite sweep now grades **fail 0, error 0, timeout 0,
> unexpected 0** across all 550 labels (546 pass, the three principled
> skips — `test_embed`, `test_getpath`,
> `test_multiprocessing_fork`-on-macOS — and one `divergence` row:
> `test_marshal`'s two enumerated subtests assert marshal-loaded
> ints/floats are *new* objects by `id()`, unsatisfiable under the
> unboxed numeric model). The wave lands the codegen-stage surface —
> `_testinternalcapi.compiler_codegen` + `optimize_cfg` over the same
> flowgraph IR the production compiler emits through
> (`test_compiler_codegen`, `test_peepholer`, `test_compile`,
> `test_code`, `test_dis` flip) — tracing exactness
> (`test_sys_settrace`, `test_monitoring`, `test_trace`), the
> importlib machinery burn (CPython's real `_bootstrap`/
> `_bootstrap_external` frozen verbatim over the native `_imp`),
> real multi-process `weavepy -m test -j2` workers over the
> libregrtest JSON protocol (`test_regrtest` flips), the
> `test_socket` long tail (`sendmsg`/`recvmsg`/`SCM_RIGHTS`), and
> PEP 734 sub-interpreters end-to-end (`test_interpreters` and the
> `test__interpreters`/`test__interpchannels`/`test__interpqueues`
> trio, C-API-created interpreters included), while the skip-row
> audit graduates `test_locale`/`test_pdb`/`test_socket` to measured
> rows. The re-baseline caught real engine bugs, notably exhausted
> `FOR_ITER` temporaries skipping finalizers — CPython frees a
> temporary list's elements by refcount the instant the loop ends,
> and WeavePy's plain drop left `ChannelID.__del__` (and any
> `__del__` on a loop-consumed temporary) waiting for the next cyclic
> collection — and the sub-interpreter extension-compatibility
> ImportError being swallowed by the PyInit blanket retype.
>
> `RFC 0069` (performance wave 6) attacks the call-shaped remainder
> and burns the numpy crash census. Tier-2 lands **method-call
> lanes** (class-version-guarded `recv.method(args)` with a pinned
> receiver and native method entry), **float completion**
> (`math.sqrt`/`sin`/`cos`/`fabs` intrinsics with CPython's domain
> errors, float floor-div/mod, cross-block operand values for
> ternaries), and tier-1 gains **call-shape inline caches**
> (exact-positional, defaults, kwnames, bound-method) plus a
> **zero-allocation generator park/unpark** that holds the frame in
> the generator box across yields. Committed baseline: suite
> geomean **3.16× CPython** (3.05× measured on the dev host; from
> wave 5's 3.33×, 3.60× re-measured pre-wave on the same host), with
> `spectral_norm` 2.3× faster, `richards` 1.7×, `generators` 1.6×,
> and `call_overhead` 1.5× — the boxed-object fixtures
> (`float_math`, `deltablue`) carry to wave 7. The crash burn fixes
> **seven C-API crash classes** (C-recursion accounting with stack
> headroom guards, self-referential list crossing, `tp_mro`/
> `tp_basicsize` publication for VM subclasses of C types, borrowed
> instance-pointer pinning, foreign `len()` grounding,
> optional-probe error discipline, container-owned
> `PySequence_GetItem` references): all 12 previously-crashing
> `numpy._core` selftest modules now run **zero-crash**, with
> `test_hashtable` and `test_protocols` passing outright. The sweep
> stays at **unexpected 0**, catching two real WS4 regressions
> in-wave (generator frame identity across `await` for pdb, and the
> send dance's line-event discipline for `sys.settrace`).
>
> `RFC 0070` (performance wave 7) generalizes tier-2's single pinned
> receiver into a **nullable object lane**: any number of
> object-typed locals and stack values ride pin slots, `-1` encodes
> the `None` singleton, and `is None` / `is not None` fences lower
> to one integer compare — with **object-valued attribute loads and
> stores** (runtime pins, displaced-value prompt-reap discipline)
> and **`__slots__` attribute lanes** on top. **Native generator
> activations (v1)** land deopt-shaped: a yield writes the frame
> back and lets the interpreter execute the suspension
> (`JitStatus::Yielded`, never charged to the deopt budget), resumes
> re-enter natively at the next loop back edge's OSR entry, and a
> profitability gate admits only generator bodies with a yield-free
> inner loop — real work between suspensions — so yield-dense
> pipelines never pay the round trip. Committed baseline: suite
> geomean **3.11× CPython** (3.06× measured on the dev host), with
> `deltablue` 1.2× faster and `attr_access` 1.16×; the deferred
> object-lane consumers (class-call construction, object
> arguments/returns, iterator pipelines) carry to the next wave.

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
   specialization, and — since RFC 0067 — a Cranelift-backed tier-2 JIT
   that ships on by default (`WEAVEPY_JIT=0` opts out at runtime).
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
