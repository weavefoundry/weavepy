# WeavePy

[![CI](https://github.com/weavefoundry/weavepy/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/weavefoundry/weavepy/actions/workflows/ci.yml)
[![Python 3.14](https://img.shields.io/badge/python-3.14-blue)](docs/CONFORMANCE.md)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

WeavePy is a Python 3.14 interpreter written in Rust. Its goal is to be a
drop-in replacement for CPython that runs existing code, packages, and tools
unchanged, and runs them faster. CPython's own test suite and real PyPI packages
are how it measures progress.

> [!NOTE]
> WeavePy is pre-release software (version 0.0.0). There are no published
> binaries yet, so you'll need to build it from source, and its behavior and
> interfaces can still change.

## Where it stands

| Area | Result | Measured |
| --- | --- | --- |
| CPython test suite | 464 of 468 `Lib/test` entries pass | November 2, 2026, on macOS |
| Real packages | 48 of 48 install and pass a behavior check | September 2026, on macOS and Linux; kept in CI |
| Warm speed | 0.85 times CPython's time | November 12, 2026, over 33 workloads |
| Peak memory | 1.13 times CPython's resident set, 1.21 times its footprint | November 12, 2026, over 33 workloads |

The sections below explain what these numbers cover and where they fall short.

## Quick start

You'll need [rustup](https://rustup.rs) and a C compiler. The Rust toolchain is
pinned in `rust-toolchain.toml`, so `rustup` selects it for you.

```bash
git clone https://github.com/weavefoundry/weavepy
cd weavepy
cargo build --release -p weavepy-cli
```

The release build uses fat LTO, so it's slow and memory-hungry. For day-to-day
work, build with `--profile quick` instead; the binary lands in `target/quick`.

The `weavepy` binary takes the same command-line arguments as `python`:

```bash
target/release/weavepy --version
target/release/weavepy -c "print('hello from WeavePy')"
target/release/weavepy path/to/script.py
```

Virtual environments and `pip` work as they do in CPython:

```bash
target/release/weavepy -m venv .venv
.venv/bin/python -m pip install requests
```

## What's included

- **Python 3.14.** WeavePy implements the language and standard library of
  CPython 3.14.7, and reports that version in `sys.version_info`.
- **Native extensions.** A CPython-compatible C API and ABI let compiled wheels
  such as NumPy, pandas, and lxml load and run.
- **Tiered execution.** Hot code moves from the interpreter to a baseline frame
  JIT and an optimizing JIT for numeric loops, both generating machine code
  through WeavePy's own one-pass code generator. Both are on by default.
  `WEAVEPY_FRAME_JIT=0` turns off the former and `WEAVEPY_JIT=0` the latter.
- **Experimental free threading.** `-X gil=0` or `PYTHON_GIL=0` runs without the
  GIL. See the [design note](docs/FREETHREADING.md) for its current limits.
- **Embedding.** The `weavepy` crate exposes a Rust API, starting with
  `weavepy::run_source`.

## Compatibility

Two test lanes grade WeavePy, each against a checked-in baseline.

**CPython's test suite.** A full local sweep at commit `b16a51a7` scheduled 468
`Lib/test` entries, and 464 passed, with no failures or timeouts. Of the other
four:

- **Three are deliberate divergences.** `test_capi`, `test_embed`, and
  `test_marshal` have enumerated cases that WeavePy doesn't aim to satisfy. For
  example, unboxed integers and floats don't have distinct `id()` values.
- **One is skipped,** as CPython skips it: `test_multiprocessing_fork` on macOS.

This sweep needs a CPython source tree, so it runs locally. CI gates every change
on WeavePy's 587 bundled regression tests instead.

**Real packages.** The ecosystem lane creates a virtual environment for each
package, installs it, and runs a behavior check. All 48 rows pass on macOS and
Linux, including Django, Flask, FastAPI, NumPy, SciPy, pandas, scikit-learn,
Matplotlib, PyTorch, Polars, SQLAlchemy, pydantic, cryptography, gevent, and
pytest. CI runs this lane on every change.

See [Conformance](docs/CONFORMANCE.md) for how the grading works.

## Performance

These ratios compare WeavePy with CPython 3.14.8, so lower is better. They're
the geometric mean of 33 realistic workloads, measured on November 12, 2026, at
`d1f48c5c` on an Intel Core i9 Mac.

| Measure | WeavePy / CPython |
| ---: | ---: |
| Warm time | 0.85 |
| Cold time (first call, including compilation) | 0.95 |
| Whole process (start-up, import, four calls) | 0.90 |
| Start-up (`-c pass`) | 0.70 |
| Start-up of a script that imports a module of its own | 0.75 |
| Peak resident memory | 1.13 |
| Peak memory footprint | 1.21 |

Warm time is well under CPython's overall, but results vary widely by workload:
WeavePy is ahead on 22 of the 33. `scimark` (0.52), `pickle_roundtrip` (0.55),
and `json_roundtrip` and `unpack_sequence` (0.62) lead, while `async_tree`
(1.28), `context_managers` (1.14), `collections_mix` (1.09), and `deepcopy`
(1.07) still trail CPython. CPython's
own times move by a few percent between sessions, and single workloads by more,
so treat any one ratio as approximate. (The start-up of a script with a module
of its own was last measured at `11c293b4`.)

The suite isn't the whole picture. Over 153 single operations
(`tools/pybench/microops.py`) WeavePy retires about as many instructions as
CPython overall (a geometric mean of 0.97 before the latest fixes), with wide
spread: integer and float arithmetic, plain calls and attribute reads run in a
small fraction of CPython's instructions, while `zip`, `Enum` lookups, dict
copies and calls through `*args` still take one and a half to two and a half
times as many. The [progress note](docs/PERFORMANCE-PROGRESS.md) lists them.

Code that the JIT compiles fully can run far faster. The numeric loops in the
[bench gate fixtures](docs/PERFORMANCE-CPYTHON-PARITY.md) took 5% to 8% of
CPython's time when that report was measured.

These figures come from profile-guided builds. A plain `cargo build --release`
skips that step and won't match them. To reproduce them (you'll need
`rustup component add llvm-tools`):

```bash
tools/pgo/build.sh
python3 tools/pybench/run.py --weavepy target/pgo/use/release/weavepy --cpython python3.14
```

See [Performance progress](docs/PERFORMANCE-PROGRESS.md) for the latest
measurements and the work behind them.

## Known limitations

- **No releases.** Building from source is the only way to get WeavePy today.
- **Memory.** WeavePy uses more memory than CPython: 1.12 times its resident
  set and 1.22 times its footprint (the memory it dirties) on these
  workloads, and the gap on Linux is larger than the macOS figures above. The
  resident-set figure relies on a profile-guided build, which releases the
  code pages a program is done with once its imports are over; a plain
  release build does that only in processes that run for more than a tenth
  of a second.
- **Imports and warm-up.** Importing large parts of the standard library costs
  about 1.4 times CPython's instructions; a faster start-up hides most of it
  in short scripts. Cold time is a little under CPython's overall, but some
  workloads, such as `async_tree` and `logging_mix`, still start slower until
  the JIT warms up.
- **Object-heavy code.** Programs built from many small methods on class
  hierarchies can run slower than on CPython: the `deltablue` bench fixture
  takes about 1.5 times CPython's time.
- **Windows.** WeavePy builds and is tested on Windows, but the CPython and
  package baselines are measured on macOS and Linux. On Windows, those lanes
  report results without blocking.
- **Package self-tests.** The behavior checks above are narrower than a package's
  own test suite. Among the packages graded that way, NumPy and lxml are still
  recorded as failing. Each row in the
  [baseline](tests/ecosystem/expectations.toml) explains why.

## Project layout

WeavePy is a Cargo workspace organized along the interpreter pipeline.

| Crate | Purpose |
| --- | --- |
| `weavepy-lexer` | Turns Python source into tokens |
| `weavepy-parser` | Turns tokens into an AST |
| `weavepy-compiler` | Compiles the AST to bytecode |
| `weavepy-vm` | Runs the interpreter, object model, frame JIT, and native standard library modules |
| `weavepy-jit` | Implements the optimizing JIT for numeric loops |
| `weavepy-codegen` | Generates the JITs' machine code (x86-64 and AArch64) |
| `weavepy-capi` | Provides the C API for native extensions |
| `weavepy-pylib` | Packages the runtime as a CPython-ABI shared library |
| `weavepy-version` | Defines the CPython version WeavePy implements |
| `weavepy` | Provides the Rust embedding API |
| `weavepy-cli` | Builds the `weavepy` binary |
| `weavepy-conformance` | Grades WeavePy against CPython and real packages |
| `weavepy-bench` | Runs the benchmark gate |
| `weavepy-dist` | Builds and checks relocatable distribution artifacts |

## Development

```bash
cargo xtest                                  # Run the workspace tests.
cargo xclippy                                # Run the lints that CI runs.
cargo fmt --all -- --check                   # Check formatting.
python3 tools/check_repository_artifacts.py  # Check the artifact policy.
```

To run the bundled regression tests after a release build:

```bash
target/release/weavepy regrtest --mode subprocess --workers 4 --timeout 60
```

To run CPython's suite, fetch its 3.14.7 `Lib` directory, which isn't checked in.
Unset `PYTHONDONTWRITEBYTECODE` first if your shell sets it, since some tests need
bytecode caches:

```bash
mkdir -p vendor/cpython314
curl -L https://github.com/python/cpython/archive/refs/tags/v3.14.7.tar.gz \
    | tar -xz -C vendor/cpython314 --strip-components=1 cpython-3.14.7/Lib
cargo run --profile quick -p weavepy-conformance -- regrtest \
    --cpython-dir vendor/cpython314/Lib/test --all-cpython --mode subprocess --jobs 4
```

To run the package lane offline:

```bash
python3 tools/ecosystem_fetch.py --dest target/ecosystem-wheels
cargo run --profile quick -p weavepy-conformance -- ecosystem --wheels target/ecosystem-wheels
```

## Documentation

- [Architecture](docs/ARCHITECTURE.md): the interpreter's design
- [Conformance](docs/CONFORMANCE.md): how WeavePy is graded against CPython
- [Performance progress](docs/PERFORMANCE-PROGRESS.md): current numbers and work
- [Free threading](docs/FREETHREADING.md): the `-X gil=0` design
- [RFCs](docs/rfcs/): the design record and history of each major change

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for development setup, coding standards,
and how to propose larger changes through the RFC process.

## License

WeavePy is dual-licensed under either of:

- Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))
- MIT License ([`LICENSE-MIT`](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in WeavePy by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
