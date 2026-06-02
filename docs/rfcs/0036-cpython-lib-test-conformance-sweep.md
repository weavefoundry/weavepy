# RFC 0036: Wiring a real CPython 3.13 checkout into the harness — the first `Lib/test/` conformance sweep

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-06-01
- **Tracking issue**: TBD
- **Builds on**: RFC 0034 (the CPython test suite as a live harness —
  `test.support` + `libregrtest`), RFC 0026/0027 (the regrtest runner +
  `expectations.toml` baseline), RFC 0033 (`ast`/`dis`/`marshal`
  introspection), RFC 0035 (faithful `re`/Unicode — the same
  "port CPython verbatim where behaviour is defined by CPython" ethos)

## Summary

RFC 0034 built the *machinery* to run CPython's own regression tests:
a frozen `test.support` package and a `libregrtest` shim. But the
`weavepy-conformance regrtest` **CLI** still only ran the small bundled
fixtures in-process, and every `cpython/Lib/test/*` row in
`tests/regrtest/expectations.toml` remained an **unverified guess**
(RFC 0034 named this exact problem; it just couldn't finish closing it
without a checkout to run against).

This RFC is the first **measured sweep**. Concretely it:

1. **Wires a real CPython 3.13 `Lib/test/` tree into the runner.** The
   `regrtest` subcommand grew `--cpython-dir`, `--mode
   {in-process,subprocess}`, `--jobs N`, `--weavepy <bin>`,
   `--all-cpython`, and `--stream`. The discovery/execution *library*
   already supported all of this (RFC 0026); this RFC exposes it on the
   command line so CI — and a developer with a local checkout — can
   point the harness at `vendor/cpython/Lib/test/` and run each test in
   a crash-isolated subprocess with a wall-clock timeout.
2. **Fixes the highest-leverage language/VM/stdlib gaps the sweep
   surfaced** — the bugs that were each blocking *whole files* from
   even importing or parsing (details below).
3. **Rewrites the touched `expectations.toml` rows from guesses to
   measured truth**, so the committed baseline is `--check`-clean
   against a fresh subprocess sweep (`unexpected 0`).

The throughline matches the rest of the project: where behaviour is
*defined by CPython*, port CPython (verbatim Python modules, the real
UCD name table) rather than re-approximate it.

## Motivation

The README's promise — "a drop-in replacement … using CPython's own
test suite as a guiding standard" — is only credible if we actually run
that suite and report honest numbers. RFC 0034 made import possible;
this RFC makes a run *happen*, and turns the result into a baseline the
next change is measured against.

Running even a couple dozen `Lib/test/` files immediately paid for
itself: each failure is a precise, real-world reproduction of an
incompatibility, and a handful of them were single bugs gating many
files at once (a parser that couldn't see `[*a, *b]`, a lexer that
choked on `1.`, classes with no `__doc__`). Fixing those is worth far
more than its line count.

## What shipped

### 1. Harness CLI (`crates/weavepy-conformance/src/bin/main.rs`)

The `regrtest` subcommand now threads the existing
`DiscoveryOptions`/`RunnerOptions`/`ExecutionMode` knobs through to the
command line:

```
weavepy-conformance regrtest \
    --cpython-dir vendor/cpython/Lib/test \
    --mode subprocess --jobs 8 --timeout 45
```

- `--cpython-dir DIR` overrides the `vendor/cpython/Lib/test/` →
  `vendor/cpython-tests/` auto-discovery (canonicalised so the child
  process and the `is_dir()` probe agree).
- `--mode subprocess` runs each test in a fresh `weavepy` child with a
  SIGKILL wall timer, so a stack overflow or `abort()` in one test is
  captured as a single failure instead of taking the runner down. The
  `weavepy` binary is resolved as `--weavepy`, then a sibling of the
  conformance binary, then `<workspace>/target/release/weavepy`.
- `--jobs N` fans tests across a thread pool; `--stream` prints each
  verdict as it lands; `--all-cpython` schedules every `test_*.py`
  under the directory (still graded against expectations).

### 2. Language / VM fixes (each unblocked ≥1 whole file)

- **Lexer — trailing-dot float literals.** `1.`, `2.+3.`, `[1.]`,
  `1.e3` now tokenise as floats (the dot binds to the number, exactly
  as in CPython's tokenizer). Was a `SyntaxError` that blocked
  `test_math`/`test_complex` and friends.
  (`crates/weavepy-lexer/src/scanner.rs`)
- **Parser + compiler — PEP 448 unpacking in displays.** `[*a, *b]`,
  `(*a, *b)`, `{*a, *b}` lower to incremental
  `append`/`extend`/`add`/`update` builds (robust against a shadowed
  `list`/`set`). Was `SyntaxError: unexpected token … Star`.
  (`crates/weavepy-parser/src/parser.rs`,
  `crates/weavepy-compiler/src/lib.rs`)
- **Parser — `\N{NAME}` named Unicode escapes.** `"\N{BULLET}"` →
  `"•"`, resolved against the **full UCD name table** (new
  `unicode_names2` dependency). This is what made `test_textwrap` go
  green (its non-breaking-space cases depend on it).
- **VM — class `__doc__`.** A class body's leading string literal is
  now `STORE_NAME`'d as `__doc__` (and every class gets `__doc__ =
  None` otherwise) instead of raising `AttributeError`. The class-body
  `co_consts[0]` slot holds the qualname, so — unlike a function — this
  needs an explicit store. Fixes anything reading `type(self).__doc__`
  (e.g. `contextlib._GeneratorContextManager`).
  (`crates/weavepy-compiler/src/lib.rs`, `crates/weavepy-vm/src/lib.rs`)
- **VM — PEP 487 dunder hooks.** `__init_subclass__` and
  `__class_getitem__` written as plain `def`s become implicit
  classmethods at class creation (`string.Template` exercises this at
  import). (`crates/weavepy-vm/src/lib.rs`)
- **VM — native iterators are iterable.** `iter(it) is it` and passing
  a native iterator to a plain builtin (`dict.fromkeys`, `set`, …)
  drains it instead of raising `'iterator' object is not iterable`.
  (`crates/weavepy-vm/src/object.rs`)
- **Builtins — `zip()` with no args** returns an empty iterator instead
  of spinning forever; **`__debug__`** is now a builtin constant
  (`True`). (`crates/weavepy-vm/src/builtins.rs`)
- **`sys` struct-sequences.** `sys.float_info` / `int_info` /
  `hash_info` answer attribute access (`sys.float_info.max`) via a
  namespace object, and `sys.float_repr_style == "short"` exists.
  (`crates/weavepy-vm/src/stdlib/sys.rs`)

### 3. Stdlib fidelity (frozen Python, ported verbatim where possible)

- `string` and `platform` frozen **verbatim** from CPython 3.13;
  `textwrap` replaced with the verbatim module.
- `contextlib` gained `ContextDecorator` and decorator support for the
  `@contextmanager` result.
- `collections._count_elements` (the pure-Python `Counter` fallback).
- `unicodedata.name` / `.lookup` now use the full UCD name table
  (`'a'` → `LATIN SMALL LETTER A`, `'•'` → `BULLET`) rather than the
  previous hand-rolled approximation.
- `test.support` gained the helpers the sweep reached for:
  `open_urlresource`, `SuppressCrashReport`, `bigaddrspacetest`,
  `skip_if_pgo_task`, `Py_TRACE_REFS`, `requires_mac_ver`,
  `linked_to_musl`, `no_color`/`force_not_colorized[_test_class]`, plus
  the `test.support.numbers` and `test.support.testcase` submodules.

### 4. Measured baseline + regression guard

`tests/regrtest/expectations.toml` rows for the swept files are now
**measured**, not guessed: `test_textwrap` and `test_datetime` flipped
to `pass`, `test_bigaddrspace` was added as `pass`, the two genuinely
slow/hanging files (`test_unicodedata`, `test_queue`) became honest
`skip`s, and ~18 previously-unlisted allowlist files got rows whose
`reason` quotes the *measured* first failure. A new bundled fixture,
`tests/regrtest/test_rfc0036_dropin.py`, asserts every language/VM fix
above and passes under both WeavePy and CPython 3.13.

## Results

A full subprocess sweep of the curated allowlist against the vendored
CPython 3.13 `Lib/test/` (`--mode subprocess --jobs 8`):

```
184 total — pass 56 / fail 104 / error 0 / skip 24 / timeout 0 — unexpected 0
```

`unexpected 0` under the default `--check` is the headline: the
committed baseline now matches a fresh run exactly, so any future
regression (or improvement) shows up as a single divergent row instead
of being lost in a sea of guesses. `test_math` is a missing-fixture
away from green (its lone error is the absent `mathdata/ieee754.txt`
doctest data file, which Homebrew's CPython strips).

## Non-goals / follow-ups

The sweep surfaced more gaps than one slice should swallow; these are
captured as measured `fail` rows for the next pass:

- **UCD version pin.** The engine's `unicode-properties` /
  `unicode-normalization` crates ship Unicode 16.0.0; CPython 3.13
  pins 15.1.0. Reconciling them (and budgeting the full
  `NormalizationTest` sweep) is what stands between us and a green
  `test_unicodedata`.
- **Recursion limit.** `test_exceptions` aborts the process on its
  infinite-recursion case — WeavePy needs a Python-level recursion
  guard (`sys.setrecursionlimit`) that raises `RecursionError` before
  the native stack overflows.
- **`FOR_ITER` edge** still trips `test_complex`; **non-ASCII
  identifiers** (PEP 3131) and **f-string backslashes / nested quotes**
  (PEP 701) block `test_unicode_identifiers` / `test_codecs` /
  `test_fstring`; **`collections.abc`**, **`locale`/`encodings`**,
  **`cmath`**, **`pydoc`**, and **`calendar`** are the module-level
  dependencies gating the largest remaining clusters.
- **`Fraction(str)`** should accept decimal literals (`'1.2'`); a
  signal/thread info object still exposes a dict where CPython exposes
  a struct-sequence (`test_threadsignals`).
