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

# Placeholder for the eventual end-to-end test runner; see "Stage B" below.
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

Once the VM can execute Python, the harness will gain a `regrtest` mode
that runs individual `Lib/test/test_*.py` files under WeavePy and
compares stdout/exit/exceptions against `python3`. That mode is gated
by an `expectations.toml` file listing currently-passing tests — CI
fails if outcomes drift in either direction (a previously-passing test
must not regress; a newly-passing test must be promoted).

Until then, the `regrtest` subcommand is a placeholder that explains
itself and exits cleanly.

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
