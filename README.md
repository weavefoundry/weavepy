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
> debuggers, coverage tools, and profilers boot. The CPython
> `Lib/test/` allowlist remains an aspirational target — see
> `tests/regrtest/expectations.toml` for the per-test baseline.
> Expect small breaking changes around the edges as the long tail
> catches up.

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

# Run the (currently tiny) test suite.
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

> The above will currently run successfully but be a no-op, because the
> compiler and VM are stubs. The plumbing is wired end-to-end so each layer
> can be filled in independently.

## CPython conformance

Compatibility is graded automatically. The `weavepy-conformance` crate
runs the host's `python3` as an oracle (tokenize, ast.parse + ast.dump,
compile + dis.dis) and reports per-phase agreement on a corpus of
Python fixtures. CI runs the harness on every PR and uploads the
report as an artifact.

```bash
cargo run -p weavepy-conformance -- run            # all phases
cargo run -p weavepy-conformance -- diff tokens    # one phase
```

See [`docs/CONFORMANCE.md`](docs/CONFORMANCE.md) for the model, the
corpus layout, and how the harness will grow into a CPython
`regrtest`-style runner once the VM can execute Python.

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
