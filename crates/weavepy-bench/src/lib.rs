//! RFC 0021 — `weavepy-bench`.
//!
//! A `pyperformance`-shaped microbench harness for WeavePy. Each
//! fixture is a self-contained `.py` file under `fixtures/` that
//! exposes a single top-level callable `bench(N)` performing some
//! workload `N` times. The runner times each fixture under
//! WeavePy (in-process) and the host's CPython (subprocess), and
//! emits a JSON report comparing the two. CI compares the report
//! against [`fixtures::BASELINE`] and fails on regressions over a
//! configurable threshold.
//!
//! ## Adding a fixture
//!
//! 1. Drop `fixtures/foo.py` containing a `bench(n)` callable.
//! 2. Add `"foo"` to [`fixtures::FIXTURES`].
//! 3. Run `cargo run -p weavepy-bench -- run --update-baseline`
//!    to refresh the baseline JSON. Inspect the diff in
//!    `baselines/bench.json` before committing.

pub mod fixtures;
pub mod report;
pub mod runner;
pub mod stats;

pub use fixtures::{Fixture, FIXTURES};
pub use report::{Report, Row};
pub use runner::{run_one, run_suite, RunOpts};
pub use stats::{mean, median, percentile, stddev};
