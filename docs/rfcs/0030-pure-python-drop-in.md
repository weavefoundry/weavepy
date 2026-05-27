# RFC 0030: pure-Python drop-in surface — `pip`, `numpy`, `pytest`, debugger hooks

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-27
- **Tracking issue**: TBD
- **Builds on**: RFC 0023 (drop-in parity), RFC 0028 (buffer protocol /
  vectorcall), RFC 0029 (numpy end-to-end)

## Summary

RFC 0029 wired the C-API tail that real `numpy.so` needs. RFC 0030
takes the parallel track: the **pure-Python** drop-in surface that
makes WeavePy boot a real-world Python workload *without* compiling
a native extension. Four threads land in this single commit:

1. **`pip` install path.** A self-hosted, PyPI-compatible installer
   built on a real PEP 440 / 503 / 508 / 425 stack, a depth-first
   dependency resolver, and a minimal PEP 517 build-backend driver
   that handles `pyproject.toml`-defined wheel builds. Surfaced as
   `import pip` (and `python -m pip`) with the standard subcommands
   (`install`, `download`, `wheel`, `freeze`, `list`, `show`,
   `uninstall`, `cache`, `check`, `config`, `search`).

2. **`numpy` facade with pure-Python fallback.** A frozen `numpy`
   module that prefers the bundled `_numpylike` C extension (RFC 0029)
   but falls back to a fully Python `NDArray` so `import numpy`
   succeeds even on a build without the native core. Includes
   `numpy.linalg`, `numpy.random`, `numpy.fft`, and `numpy.testing`
   submodules and the standard `np.array` / `np.zeros` / `np.arange`
   / `np.dot` / `@` matmul surface.

3. **`pytest` + `pluggy` + `iniconfig` + `exceptiongroup` shims.** A
   pytest-shaped test runner that drives `test_*` discovery,
   `@pytest.fixture`, `@pytest.mark.*`, `pytest.raises`, `pytest.warns`,
   `pytest.approx`, and `pytest.main(...)` returning the right
   `ExitCode`. Backed by a minimal `pluggy` plugin manager so third-
   party `conftest.py` files that register hooks don't crash.

4. **`sys.settrace` / `sys.setprofile` / `sys.monitoring` (PEP 669)
   observability + `tracemalloc`.** The hooks are observable: you can
   register them, read them back, and (for `tracemalloc`) call
   `take_snapshot()` to materialise per-allocation statistics. The
   actual line-by-line dispatch hook is gated behind RFC 0031 — the
   point of RFC 0030 is to make debuggers, coverage tools, and
   profilers boot far enough that the next layer of fixes can be
   data-driven against real workloads.

## Background

The README ranks the drop-in story in four bands:

> 1. **Lexer / parser / compiler.** Token-for-token compatible.
> 2. **Stdlib.** Mostly there; the long tail is the work.
> 3. **C extensions.** `numpy.so` boots; the long tail is the work.
> 4. **Drop-in workloads.** Pip, numpy, pytest, debuggers.

RFC 0023 and RFC 0029 covered (2) and (3). RFC 0030 closes (4) by
making the *pure-Python* spelling of each of those workloads work
out of the box. That matters for two reasons:

- A user who downloads WeavePy and types `pip install requests` from
  the REPL expects it to **install requests**, not fail at
  `import packaging.version`.
- The CI matrix has to cover both the C-extension fast path *and* the
  pure-Python fallback. Until RFC 0030, the fallback path was
  effectively untested because the modules didn't exist.

## Goals

- `import pip; pip.main(['install', 'requests'])` resolves the
  dependency graph against PyPI, picks the right wheel for the
  current interpreter (PEP 425 tag scoring), downloads, and unpacks
  into a writable `site-packages`. When no wheel matches it falls
  back to the sdist path and runs PEP 517 `build_wheel`.
- `import numpy as np; np.array([1, 2, 3]) @ np.array([[1], [2], [3]])`
  returns a 2D ndarray with the right shape and dtype, regardless of
  whether `_numpylike` is compiled.
- `import pytest; pytest.main([tmpdir])` discovers tests, runs them,
  prints a CPython-shaped summary, and returns the right `ExitCode`.
- `import sys; sys.settrace(hook); sys.gettrace() is hook` round-trips.
- `import tracemalloc; tracemalloc.take_snapshot().statistics('lineno')`
  returns a list of objects with `count` / `size` / `traceback`
  attributes that match the CPython surface.
- `cargo run --release -p weavepy-cli -- regrtest --mode subprocess
  --workers 4 --timeout 60` reports 0 unexpected failures.

## Non-goals

- Bit-for-bit numerical parity with upstream `numpy`. The pure-Python
  backend is correct to ~1e-9 on the operations RFC 0030 ships;
  workloads that need IEEE-perfect `BLAS` should pick the C extension.
- A real CPython `_pytest` reimplementation. The shim is enough for
  `pytest.main([dir])` against simple test files; complex fixtures
  (parametrize matrices, indirect requests, fixture finalisers
  with `request.addfinalizer`) are deferred.
- Wiring `sys.settrace` / `sys.setprofile` line events into the VM
  hot path. That's RFC 0031 — the cost on tight arithmetic loops
  needs profiling first.
- Bit-for-bit `pip` parity. RFC 0030 ships a *compatible* installer
  whose plan and output match real `pip` for the common cases, but
  doesn't try to replicate every flag, warning, or edge-case spinner.

## Design

### 1. `_packaging` — PEP 440 / 503 / 508 / 425

A single Python module at
`crates/weavepy-vm/src/stdlib/python/_packaging.py` implements:

| Class / function | PEP | Notes |
|---|---|---|
| `Version` | 440 | Local-version segment, epoch, pre/post/dev releases, lexicographic comparison via sortable-tuple keys (no float infinities — every key element is wrapped so heterogeneous keys compare safely). |
| `Specifier` / `SpecifierSet` | 440 | Operators `==`, `!=`, `===`, `<`, `<=`, `>`, `>=`, `~=`, with `==1.4.*` wildcard support. |
| `Requirement` | 508 | Name / extras / specifier / marker / URL parsing. |
| `Marker` | 508 | `python_version >= "3.10" and sys_platform == "linux"`-shaped marker grammar with full Boolean composition. |
| `WheelTag` / `parse_wheel_filename` / `compatible_tags` / `wheel_is_compatible` / `wheel_score` | 425 | Wheel-name parsing, candidate ranking with platform-tag matching. |
| `canonicalize_name` | 503 | PEP 503 name normalisation (`Foo.Bar_Baz` → `foo-bar-baz`). |
| `default_environment()` | 508 | Marker variables (`python_version`, `sys_platform`, `platform_machine`, etc.) computed from `sys` / `os.uname` with sensible fallbacks (WeavePy doesn't yet have the `platform` module). |

The `packaging` namespace is exposed via a thin facade
(`packaging.__init__.py` plus `packaging.version`,
`packaging.specifiers`, `packaging.requirements`, `packaging.markers`,
`packaging.utils`, `packaging.tags`) so user code that does
`from packaging.version import Version` resolves to the same classes.

### 2. `_pip_resolver` — dependency resolution

A depth-first resolver (`Resolver.resolve(requirements)`) walks the
graph one project at a time:

1. Look up candidates on the index (callback `lookup(name) -> [(filename, url), ...]`).
2. Filter to PEP 425-compatible wheels, scored by tag specificity.
3. Pick the best candidate satisfying the active `SpecifierSet`.
4. Download (callback `downloader(url) -> bytes`), pull METADATA,
   apply environment marker evaluation to `Requires-Dist`.
5. Recurse with the union of new requirements; raise
   `ResolutionError` on conflict.

The callbacks are injection points: the production path wires them
to `_https` and the PyPI simple index; tests inject in-memory wheel
catalogues so the resolver is exercised without network I/O.
`parse_pep723(source)` parses inline-metadata blocks per PEP 723.

### 3. `_pep517` — sdist build driver

When no compatible wheel is available, `_minipip` falls back to the
sdist path. `_pep517` implements the minimum subset of PEP 517 needed
to drive a `pyproject.toml`-defined `build_wheel`:

- `extract_sdist(blob, dest)` — unpacks a `.tar.gz` sdist.
- `_load_pyproject(path)` — parses `[build-system]` (TOML).
- `_import_backend(name)` — dynamically imports the configured
  backend (e.g. `setuptools.build_meta`).
- `build_wheel(srcdir, wheel_dir)` / `build_sdist(srcdir, sdist_dir)`
  — calls the backend's hook and returns the produced filename.
- Pure-Python `_fallback_build_wheel` for projects with no
  `build-system` table — discovers packages under the source tree
  and emits a flat wheel with METADATA + WHEEL + RECORD.

### 4. `_minipip` — the pip CLI

`crates/weavepy-vm/src/stdlib/python/_minipip.py` orchestrates the
above. The CLI surface mirrors real pip:

```
pip install [options] <requirement> ...
pip download [options] <requirement> ...
pip wheel [options] <requirement> ...
pip uninstall <pkg> ...
pip list [--format=columns|freeze|json]
pip show <pkg>
pip freeze
pip cache (purge|info|dir)
pip check
pip config (list|get|set|unset|edit) <key> [value]
pip search <query>
```

Frozen into the runtime as both `_minipip` and `pip` so
`python -m pip install foo` works.

### 5. `numpy` facade

`numpy/__init__.py` (frozen at
`crates/weavepy-vm/src/stdlib/python/numpy_init.py`) tries
`import _numpylike` first; on `ImportError` it loads `_numpy_pure`.
The facade publishes a single `_CORE_KIND` flag (`'native'` or
`'pure-python'`) so users can branch on the backend, but the array
constructors, arithmetic, reductions, broadcasting, dtype, linear
algebra, RNG, and FFT all work the same shape either way.

The pure-Python fallback at
`crates/weavepy-vm/src/stdlib/python/_numpy_pure.py` carries a
complete `NDArray` class with shape/dtype/ndim, ravel/reshape/
transpose, the arithmetic operators (including `__matmul__`),
indexing/slicing, reductions (`sum`, `prod`, `mean`, `min`, `max`,
`argmin`, `argmax`), `dot`, and the constructor surface (`array`,
`zeros`, `ones`, `empty`, `arange`, `concatenate`).

### 6. `pytest`, `pluggy`, `iniconfig`, `exceptiongroup`

- `_pluggy.py` — `HookspecMarker`, `HookimplMarker`, `PluginManager`,
  `HookCaller`. Just enough to register plugins, call hooks, and
  inspect the relay.
- `_pytest.py` — exposed as `pytest` in the module table. Provides
  test discovery (`test_*.py` / `Test*` / `test_*`), `@pytest.fixture`,
  `@pytest.mark.*`, `pytest.raises` / `warns` / `skip` / `fail` /
  `xfail` / `approx`, a node hierarchy (`Collector` → `Module` →
  `Class` → `Item`), and a `Session` runner that prints CPython-shaped
  output and returns the right `ExitCode` (`OK` / `TESTS_FAILED` /
  `NO_TESTS_COLLECTED` / `USAGE_ERROR` / `INTERNAL_ERROR`).
- `iniconfig_mod.py` — `IniConfig` for parsing `pytest.ini` /
  `setup.cfg` `[tool:pytest]` sections.
- `exceptiongroup_mod.py` — backport shim for `BaseExceptionGroup` /
  `ExceptionGroup` (PEP 654). WeavePy already implements
  `ExceptionGroup` natively; this module just re-exports the
  built-in name so packages that `from exceptiongroup import …`
  resolve.

### 7. `sys.settrace`, `sys.setprofile`, `sys.monitoring`, `tracemalloc`

A new `crates/weavepy-vm/src/trace.rs` holds per-thread
`TRACE_HOOK` / `PROFILE_HOOK` / `MONITORING_TOOLS` cells.
`sys.settrace(fn)` / `sys.setprofile(fn)` store the hook;
`sys.gettrace()` / `sys.getprofile()` read it back.
`sys.monitoring` (PEP 669) exposes the tool-id reservation API
(`use_tool_id`, `free_tool_id`, `get_tool`, `set_events`,
`get_events`, `register_callback`) plus the full `events` namespace
(`PY_START`, `PY_RETURN`, `LINE`, `EXCEPTION_HANDLED`, …) with
the standard bit masks.

`tracemalloc` (Rust module at
`crates/weavepy-vm/src/stdlib/tracemalloc_real.rs`) backs
`start()` / `stop()` / `is_tracing()` / `get_traced_memory()` /
`take_snapshot()` / `clear_traces()` / `reset_peak()`. Snapshots
carry per-`(filename, lineno)` allocation counts and bytes; the
returned object exposes `.statistics(key_type)` which returns a list
of `SimpleNamespace`-shaped records with `.count` / `.size` /
`.traceback`. The runtime allocator integration is gated behind
RFC 0031 — RFC 0030 ships the observable API surface so the next
layer of work has a stable target.

### 8. Built-in compatibility fixes

Two CPython-conformance gaps blocked the work above and got fixed
along the way:

- `str.startswith` / `str.endswith` / `bytes.startswith` /
  `bytes.endswith` now accept a *tuple* of prefixes/suffixes in
  addition to a single string. The Rust implementation in
  `crates/weavepy-vm/src/builtins.rs` got `str_match_prefix_suffix` /
  `bytes_match_prefix_suffix` helpers that walk the tuple and apply
  the optional `start` / `end` arguments correctly.
- The `math` module gained the long tail of CPython functions
  consumed by `numpy` and `statistics`: `fsum`, `prod`, `hypot`,
  `dist`, `expm1`, `log1p`, `ldexp`, `frexp`, `modf`, `comb`, `perm`,
  `remainder`, `nextafter`, `ulp`, `erf`, `erfc`, `gamma`, `lgamma`,
  `isqrt`, `cbrt`, `exp2`, `atanh`, `asinh`, `acosh`. `collect_numbers`
  and `object_to_f64` helpers normalize the argument shapes.

## Test plan

Five drop-in regression tests bundled into
`tests/regrtest/` and exercised by the regrtest harness in
subprocess mode:

| Test | What it covers |
|---|---|
| `test_packaging_pep440.py` | Version comparison, specifier matching, requirement parsing, marker evaluation, name canonicalization, wheel filename / tag parsing. |
| `test_numpy_dropin.py` | Constructors, arithmetic, reductions, matmul, reshape, dtype, linalg, random, fft, constants — runs against whichever backend the build has. |
| `test_pytest_dropin.py` | `raises`, `warns`, `approx`, `skip`/`xfail` markers, fixture decoration, end-to-end `pytest.main` against discovered tests (passing, failing, empty). |
| `test_pip_install_resolution.py` | Resolver against in-memory wheel catalogues, conflict detection, METADATA parsing, PEP 723 inline metadata, end-to-end local-wheel install via `_minipip._install_wheel`. |
| `test_sys_settrace_dropin.py` | `sys.settrace` / `sys.setprofile` round-trip, `tracemalloc` lifecycle, `sys.monitoring` constants + tool-id registration. |

CI gate: the regrtest job (`mode=subprocess`, `workers=4`,
`timeout=60`) reports 0 unexpected failures.

## Open questions

1. **Line-event firing in the VM dispatcher.** RFC 0031. The
   bookkeeping is here; the dispatch hook still needs to land
   without regressing the arithmetic micro-benchmarks more than
   1–2 %.
2. **`numpy.linalg` fallback accuracy.** The pure-Python fallback
   uses naive Gauss-Jordan for `inv` / `det`; pathological condition
   numbers diverge. Documented as a known limitation; production
   workloads should compile `_numpylike`.
3. **`pip install` against private indexes / auth.** RFC 0030 wires
   the public PyPI happy path; basic-auth / token / netrc are
   deferred to a follow-up.
4. **`pytest` parametrize matrices and complex fixtures.** The
   shim handles the common cases but doesn't expand parametrize
   matrices yet. Tracked as RFC 0032.

## Acceptance criteria

- `cargo fmt --all -- --check` — clean.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — clean.
- `cargo test --workspace --all-targets --all-features` — green.
- `cargo test --workspace --doc` — green.
- `cargo run --release -p weavepy-cli -- regrtest --mode subprocess
  --workers 4 --timeout 60` — 39/39 pass, 0 unexpected.
- All five drop-in regression tests pass under regrtest.

## Rollout

This RFC lands as a single commit (~9 K LOC of Rust + Python).
Subsequent work plugs:

- RFC 0031: line-event firing in the dispatcher.
- RFC 0032: pytest parametrize + complex fixture surface.
- RFC 0033: PyPI auth + private index support in `_minipip`.
