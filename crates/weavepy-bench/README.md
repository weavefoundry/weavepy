# weavepy-bench

RFC 0058 — the `pyperformance`-shaped benchmark lane for WeavePy
(supersedes the RFC 0021 harness).

The harness times each fixture's `bench(n)` under **both** the built
`weavepy` binary and the host CPython, as subprocesses with an
identical `WEAVEPY_BENCH_WORK`. Fixtures self-time the bench region
with `time.perf_counter_ns()` and print `WEAVEPY_BENCH_NS=<int>`, so
startup / parse / import cost is excluded symmetrically (the dedicated
`startup` fixture measures full subprocess wall time instead).

The tracked baseline (`baselines/bench.json`) stores medians for both
interpreters plus the WeavePy/CPython **ratio** per fixture and the
suite geometric mean. `gate` compares ratios — host-independent,
unlike absolute nanoseconds — and fails on regressions beyond a
threshold, like the regrtest and ecosystem lanes' `--check`. CI runs
`gate --pct=25` on ubuntu + macos (the `bench` job).

The crate is excluded from `default-members` so `cargo build` /
`cargo test --workspace` doesn't pull it in. Opt in with
`-p weavepy-bench`.

## Usage

```bash
# The harness needs the binary under test next to it.
cargo build --release -p weavepy-cli -p weavepy-bench

# Run all fixtures, print a markdown report (ratio column + geomean).
cargo xbench run

# Compare current ratios against the baseline; exit non-zero on
# regression beyond 10% (default threshold).
cargo xbench gate
cargo xbench gate --pct=25

# Refresh the baseline JSON tracked at `baselines/bench.json`.
cargo xbench run --update-baseline

# Point at explicit interpreters.
cargo xbench run --weavepy=target/release/weavepy --python=python3.13

# Add a WEAVEPY_JIT=1 column (reported, never gated). The binary must
# be built with the tier-2 JIT compiled in:
cargo build --release -p weavepy-cli --features weavepy-cli/jit
cargo xbench run --jit

# Print the report as JSON instead of markdown.
cargo xbench run --json
```

## Adding a fixture

1. Drop `fixtures/foo.py`. The file must:
   - Define a `bench(n)` callable that runs the workload scaled by `n`.
   - End with the standard self-timing block (copy it from any
     fixture): read `WEAVEPY_BENCH_WORK`, time `bench(n)` with
     `time.perf_counter_ns()`, print `WEAVEPY_BENCH_NS=<int>`.
2. Add `"foo"` to `FIXTURES` in `src/fixtures.rs`.
3. Pick a `default_work(...)` value sized so the **CPython** leg takes
   ~25–65 ms.
4. Run `cargo xbench run --update-baseline` and inspect the diff
   before committing. The gate fails fixtures that have no baseline
   row, so the baseline refresh ships in the same change.
