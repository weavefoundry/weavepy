# weavepy-bench

RFC 0058 — the `pyperformance`-shaped benchmark lane for WeavePy
(supersedes the RFC 0021 harness).

The harness times each fixture's `bench(n)` under **both** the built
`weavepy` binary and the host CPython, as subprocesses with an
identical `WEAVEPY_BENCH_WORK`. Fixtures self-time the bench region
with `time.perf_counter_ns()` and print `WEAVEPY_BENCH_NS=<int>`, so
startup / parse / import cost is excluded symmetrically (the dedicated
`startup` fixture measures full subprocess wall time instead).

Baselines are tracked **per platform** (RFC 0062 WS3) as
`baselines/bench-{os}-{arch}.json`, resolved against the host's
`std::env::consts::{OS, ARCH}` — e.g. `bench-macos-aarch64.json`,
`bench-linux-x86_64.json`. Each file stores medians for both
interpreters plus the WeavePy/CPython **ratio** per fixture, the
suite geometric mean, and the platform it was measured on. `gate`
compares ratios — host-independent, unlike absolute nanoseconds —
and fails on regressions beyond a threshold, like the regrtest and
ecosystem lanes' `--check`. CI runs `gate --pct=25` on ubuntu +
macos (the `bench` job).

Two per-platform guardrails:

- If the host has **no** baseline file, `gate` fails with a message
  naming the missing `bench-{os}-{arch}.json` — pass
  `--allow-missing-baseline` to turn that into an advisory note with
  exit 0 (what CI does on platforms whose baseline hasn't been
  measured and committed yet).
- Each baseline records the platform it was measured on, and `gate`
  refuses a baseline whose recorded platform mismatches the host —
  copying `bench-macos-aarch64.json` to `bench-linux-x86_64.json`
  cannot silently gate Linux against macOS ratios.

The crate is excluded from `default-members` so `cargo build` /
`cargo test --workspace` doesn't pull it in. Opt in with
`-p weavepy-bench`.

## Usage

```bash
# The harness needs the binary under test next to it.
cargo build --release -p weavepy-cli -p weavepy-bench

# Run all fixtures, print a markdown report (ratio column + geomean).
cargo xbench run

# Compare current ratios against the host platform's baseline; exit
# non-zero on regression beyond 10% (default threshold).
cargo xbench gate
cargo xbench gate --pct=25

# Advisory mode: exit 0 with a note when the host platform has no
# committed baseline (a present-but-foreign baseline still fails).
cargo xbench gate --allow-missing-baseline

# Refresh the host platform's baseline JSON tracked at
# `baselines/bench-{os}-{arch}.json`.
cargo xbench run --update-baseline

# Point at explicit interpreters.
cargo xbench run --weavepy=target/release/weavepy --python=python3.13

# Add a WEAVEPY_JIT=0 interpreter-only column (reported, never gated).
# The default binary ships with the tier-2 JIT on (RFC 0067), so the
# gated WeavePy column already measures the JIT:
cargo xbench run --interp

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
   row, so the baseline refresh ships in the same change. (This only
   refreshes the *host* platform's file; other platforms' baselines
   are refreshed on their own hardware.)

## Refreshing the baseline

A committed baseline `ratio` is the **acceptance envelope**, not the
best measurement the baseline host ever produced. CI's shared runners
measure ratios up to ~25% above a quiet baseline host on
interpreter-bound fixtures — about the entire gate threshold — so a
refresh that adopts tighter host numbers on fixtures a change didn't
touch silently converts cross-machine skew into gate flakes.

When refreshing after a perf change, inspect the per-fixture ratio
diff and:

- **Keep the old (looser) ratio** for fixtures the change doesn't
  affect — never tighten their gate as a side effect.
- **Adopt the new ratio** for fixtures the change genuinely improved
  (that ratchet is the point of the refresh), leaving headroom for
  the ratios CI actually reports on its runners.
- The stored `geomean_ratio` should be recomputed from the committed
  per-row ratios; the geomean gate is the suite-level ratchet that
  catches a broad regression even under loose per-fixture envelopes.
