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
| ast    | oracle runs; WeavePy side reports **skipped** until the parser emits `ast.dump`-shaped output |
| dis    | oracle runs; WeavePy side reports **skipped** until the compiler emits `dis`-shaped output |

The skipped phases are deliberate: we do not pretend to be measuring
something we can't measure yet. As soon as WeavePy can emit comparable
output for a phase, the runner switches that phase from `Skipped` to a
real diff in a single PR. The oracle infrastructure for all three
phases is wired up today, so we know it works.

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
merges — today's baseline is 0% by design, and a blocking gate would
amount to noise until the harness has a meaningful floor. Once the
lexer's first real commit moves the tokens phase well above zero, the
job is promoted to blocking via a follow-up PR.

## Why a separate crate?

The harness depends on `weavepy` and on quite a bit of host-side
tooling (`serde_json`, `walkdir`, a subprocess `python3`). Keeping it
out of the pipeline crates avoids contaminating their dependency
footprint, and `publish = false` means it never reaches crates.io.
It's also excluded from `default-members`, so `cargo build` and
`cargo test` (without `--workspace`) stay light; CI and contributors
opt in explicitly with `-p weavepy-conformance`.
