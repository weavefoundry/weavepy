# CPython conformance

WeavePy's primary correctness criterion is *"does CPython do the same
thing?"* (see `docs/ARCHITECTURE.md` — "Compatibility strategy"). This
document describes the harness that turns that policy into a number.

## TL;DR

```bash
# Grade every corpus file across every phase.
cargo run -p weavepy-conformance -- run

# Grade only one phase.
cargo run -p weavepy-conformance -- diff tokens
cargo run -p weavepy-conformance -- diff ast
cargo run -p weavepy-conformance -- diff dis

# Run Lib/test/ files end-to-end and grade against the baseline; see
# "Stage B" below for the --cpython-dir / --mode / --jobs flags.
cargo run -p weavepy-conformance -- regrtest
```

Reports are written to `target/conformance/`:

- `report.md`   — human-readable summary plus per-file table.
- `report.json` — machine-readable; the artifact CI uploads on every run.

## Model: CPython as an oracle, per phase

CPython exposes a Python-level interface for every phase of its pipeline.
The harness invokes the host's `python3` as a subprocess and asks it for
the canonical output, then compares.

| WeavePy phase        | CPython oracle                 |
|----------------------|--------------------------------|
| `weavepy-lexer`      | `tokenize.tokenize`            |
| `weavepy-parser`     | `ast.parse` + `ast.dump`       |
| `weavepy-compiler`   | `compile` + `dis.dis`          |
| `weavepy-vm` (later) | running the script under `python3` |

Each phase reports one of five outcomes per file:

- **match** — canonical outputs are equal.
- **mismatch** — both sides succeeded but disagreed.
- **weavepy-error** — WeavePy raised an error (lex/parse/compile failure).
- **oracle-error** — CPython raised an error on the input (usually a
  broken fixture; the file is excluded from the match-rate denominator).
- **skipped** — phase is not yet wired up for this comparison; see "Where
  we are today" below.

## The corpus

Two sources, in priority order:

1. **In-tree fixtures** at `conformance/corpus/*.py`. Always present,
   intended for the inner dev loop. Each fixture isolates one feature so
   a regression points at a specific cause.
2. **Vendored CPython** at `vendor/cpython/Lib/test/` (optional). If
   checked out, a curated allowlist of files is added to the corpus
   (currently `test_tokenize.py`, `test_grammar.py`, `test_ast.py`).

The in-tree corpus is enough to validate the harness end-to-end. The
CPython submodule is what gives the harness real reach once the lexer
is implemented.

### Adding fixtures

In-tree fixtures live in `conformance/corpus/` and follow the convention
`<phase>_<feature>.py`. The CPython oracle must accept them without a
`SyntaxError` — a broken fixture is a corpus bug, not a WeavePy bug. See
`conformance/corpus/README.md` for details.

### Adding CPython as a submodule

When you want the wider CPython test corpus locally, run:

```bash
git submodule add -b v3.13.1 https://github.com/python/cpython.git vendor/cpython
git submodule update --init --depth=1 vendor/cpython
```

The harness picks up `vendor/cpython/Lib/test/` automatically on the next
run. CI deliberately does **not** clone the submodule — the in-tree
corpus is enough to track the front-of-pipeline metric without growing
the clone size on every PR.

## The oracle interpreter

By default the harness invokes `python3` on `$PATH`. Override it for one
run with `$WEAVEPY_PYTHON`:

```bash
WEAVEPY_PYTHON=/opt/cpython/3.13/bin/python3 \
  cargo run -p weavepy-conformance -- run
```

WeavePy currently tracks CPython 3.13. The harness uses whatever oracle
it's pointed at — using e.g. 3.12 will produce mismatches that aren't
about WeavePy. CI pins to 3.13.

## Where we are today

| phase  | status |
|--------|--------|
| tokens | live — full diff against `tokenize.tokenize` |
| ast    | live — graded diff against `ast.parse` + `ast.dump` |
| dis    | live — graded diff against `compile` + `dis.dis` |

All three phases are wired and graded. The `ast` and `dis` phases
compare WeavePy's **raw** parser/compiler IR (`parser::ast::dump_module`,
`CodeObject::format_dis`) against CPython, so their match rates are a
floor that climbs as the native pipeline converges on CPython's shapes —
they are not yet a perfect signal and the job stays non-blocking (see
"CI integration").

> Note: RFC 0033 additionally ships **CPython-faithful frozen drop-in
> modules** — `import ast`, `import dis`, `import opcode`,
> `import symtable`, plus `marshal`/`.pyc` and the `code` object `co_*`
> surface. Those are exercised as a *drop-in* (run real `dis.dis` /
> `ast.parse` *inside* WeavePy and diff against CPython) by the bundled
> regrtests, not by this raw-IR harness. Treat the two as complementary:
> this harness grades the native pipeline; the regrtests grade the
> user-visible module surface.

## Stage B: end-to-end regrtest runner

The `regrtest` subcommand runs individual `Lib/test/test_*.py` files
end-to-end through WeavePy and grades each against
`tests/regrtest/expectations.toml`. It is **live** (RFC 0026/0034 built
the runner + `test.support`; RFC 0036 wired a real CPython checkout into
the CLI). A test is graded `pass`/`fail`/`error`/`skip`/`timeout` (plus
the `divergence` expectation below), and the baseline gates CI in both
directions: a previously-passing test must not regress, and a file that
starts passing must be promoted.

### The `divergence` status (RFC 0068 WS7)

`status = "divergence"` is reserved for rows where a test asserts
behavior **provably unsatisfiable under WeavePy's documented object
model** — not a gap to burn down, but a deliberate design choice the
suite happens to observe. The bar is strict:

- The row must enumerate the exact unittest ids it is allowed to fail
  in a mandatory `divergence_tests` list, plus a `reason` explaining
  why the behavior is unsatisfiable (both are hard load errors when
  missing).
- The runner grades the row `divergence` only when the observed
  failure set **equals** the enumerated list — every listed id fails
  and nothing else does. A row that starts passing, or failing a
  different set, is `unexpected` and blocks CI exactly like a `pass`
  row regressing.
- `divergence` is counted separately from `fail` in the sweep summary.

The budget is deliberately tiny — **one row**: `test_marshal`, whose
`InstancingTestCase.testInt`/`testFloat` assert that version-2 marshal
loads create `id()`-distinct instances. WeavePy's unboxed int/float
representation (the RFC 0058+ performance arc) derives `id()` from the
value, so two equal unboxed ints *are* the same object identity-wise;
abandoning unboxing to satisfy two identity asserts is rejected.

```bash
# Run the curated allowlist against a vendored CPython 3.13 Lib/test/,
# one crash-isolated subprocess per test, 8 in parallel:
cargo run -p weavepy-conformance -- regrtest \
    --cpython-dir vendor/cpython/Lib/test \
    --mode subprocess --jobs 8 --timeout 45

# Refresh the baseline after an intentional change (grade without gating):
cargo run -p weavepy-conformance -- regrtest --mode subprocess --no-check
```

Key flags (the discovery/execution library has supported these since
RFC 0026; RFC 0036 exposes them on the CLI):

- `--cpython-dir DIR` — point at any CPython `Lib/test/` tree, overriding
  the `vendor/cpython/Lib/test/` → `vendor/cpython-tests/` autodiscovery.
- `--mode subprocess` — run each test in a fresh `weavepy` child with a
  SIGKILL wall timer, so a stack overflow / `abort()` is captured as a
  single failure instead of taking the runner down. (`--mode in-process`
  is faster for the bundled fixtures but not crash-safe.)
- `--jobs N` — fan tests across `N` worker threads; `--stream` prints
  each verdict as it lands; `--all-cpython` schedules every `test_*.py`
  in the directory (still graded against expectations).

The committed baseline is **measured, not guessed** (RFC 0036): a fresh
subprocess sweep reports `unexpected 0`. Each `cpython/Lib/test/*` row
carries a `reason` that, where the file fails, quotes the measured first
failure so the gap is concrete.

As of RFC 0068 the burn is complete: the whole-suite sweep grades
**fail 0, error 0, timeout 0, unexpected 0** across all 550 labels —
**546 pass**, three principled skips (`test_embed` and `test_getpath`
exercise CPython build artifacts that don't exist for a Rust
interpreter; `test_multiprocessing_fork` is skipped on macOS exactly
as CPython does), and one `divergence` row (`test_marshal`: two
enumerated `InstancingTestCase` ids assert that version-2 loads mint
*new* int/float objects by `id()`, unsatisfiable under WeavePy's
unboxed numeric model — see the status description above). The
ecosystem lane stands at **36/36 passing rows** offline (pandas and
FastAPI capstones included, self-tests green); the `gevent` stretch
row stays a measured fail per RFC 0066.

## CI integration

A `conformance` job runs on every push and pull request. It:

1. Installs Python 3.13 via `actions/setup-python`.
2. Builds and runs `weavepy-conformance run`.
3. Appends the Markdown report to the GitHub Actions job summary.
4. Uploads `target/conformance/` as an artifact named
   `conformance-report`.

The job is marked `continue-on-error: true` so it does **not** block PR
merges — the `ast`/`dis` raw-IR match rates are still a climbing floor,
and a blocking gate would amount to noise until the native pipeline
converges. The blocking signal lives in the separate **`regrtest`** job
(`cargo run -p weavepy-cli -- regrtest`), which gates on
`tests/regrtest/expectations.toml`; this `conformance` job is promoted to
blocking via a follow-up PR once its floor is meaningful.

## Why a separate crate?

The harness depends on `weavepy` and on quite a bit of host-side
tooling (`serde_json`, `walkdir`, a subprocess `python3`). Keeping it
out of the pipeline crates avoids contaminating their dependency
footprint, and `publish = false` means it never reaches crates.io.
It's also excluded from `default-members`, so `cargo build` and
`cargo test` (without `--workspace`) stay light; CI and contributors
opt in explicitly with `-p weavepy-conformance`.
