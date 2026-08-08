//! RFC 0058 — `weavepy-bench` v2.
//!
//! A `pyperformance`-shaped benchmark lane for WeavePy. Each fixture
//! is a self-contained `.py` file under `fixtures/` exposing a
//! top-level `bench(n)` callable. The runner executes each fixture as
//! a subprocess of **both** the built `weavepy` binary and the host
//! CPython with an identical `WEAVEPY_BENCH_WORK`, and each fixture
//! self-times its `bench(n)` region with `time.perf_counter_ns()`,
//! printing `WEAVEPY_BENCH_NS=<int>` — so process startup, parsing,
//! and imports are excluded from the loop metric. (The dedicated
//! `startup` fixture measures full subprocess wall time instead.)
//!
//! The tracked baseline (`baselines/bench.json`) stores WeavePy and
//! CPython medians plus the WeavePy/CPython **ratio** per fixture and
//! the suite geometric mean. `gate` compares ratios — which are
//! host-independent, unlike absolute nanoseconds — and fails on
//! regressions beyond a threshold, exactly like the regrtest and
//! ecosystem lanes' `--check`.
//!
//! ## Adding a fixture
//!
//! 1. Drop `fixtures/foo.py` with a `bench(n)` callable and the
//!    standard self-timing `__main__` block (copy any fixture).
//! 2. Add `"foo"` to [`fixtures::FIXTURES`] and a `default_work`
//!    entry sized so the CPython leg takes ~50–300 ms.
//! 3. Run `cargo run --release -p weavepy-bench -- run
//!    --update-baseline` and inspect the diff in
//!    `baselines/bench.json` before committing. The gate fails on
//!    fixtures that have no baseline row (the RFC 0049 "no
//!    unmeasured rows" rule applied to speed).

pub mod fixtures;
pub mod report;
pub mod runner;
pub mod stats;

pub use fixtures::{Fixture, FIXTURES};
pub use report::{Report, Row};
pub use runner::{run_one, run_suite, RunOpts};
pub use stats::{geometric_mean, mean, median, percentile, stddev};
