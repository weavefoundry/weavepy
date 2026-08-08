//! Discovery of fixtures embedded in this crate.
//!
//! Each fixture is a self-contained `.py` file that exports a
//! top-level `bench(n)` callable and self-times it in its
//! `__main__` block, printing `WEAVEPY_BENCH_NS=<int>` (RFC 0058
//! WS1). The list below is the authoritative set used by the runner
//! and the CI gate; new fixtures need to be both dropped on disk
//! *and* added here so the runner finds them.

use std::path::PathBuf;

/// The full set of fixtures the runner knows about. Order is
/// preserved in CLI output and in the JSON report.
pub const FIXTURES: &[&str] = &[
    // RFC 0021 originals.
    "fannkuch",
    "nbody",
    "fib",
    "pidigits",
    "pyaes",
    "richards",
    "sumvm",
    "nested_loops",
    "jitloop",
    // RFC 0058 additions — call/attr/subscript/str/dict shape
    // diversity so the suite can't be gamed by one fast path.
    "deltablue",
    "float_math",
    "spectral_norm",
    "json_bench",
    "str_methods",
    "dict_ops",
    "list_ops",
    "attr_access",
    "call_overhead",
    "generators",
    "startup",
];

/// Fixtures measured as full-subprocess wall time instead of the
/// self-timed `WEAVEPY_BENCH_NS` region. Startup cost *is* the
/// workload for these.
pub const WALL_CLOCK_FIXTURES: &[&str] = &["startup"];

/// Default per-fixture work parameter passed as `bench(n)`.
/// Picked to make a single iteration take ~50-300ms on CPython —
/// small enough to keep the bench job under a few minutes, large
/// enough to dwarf timer overhead and runner noise.
pub fn default_work(name: &str) -> u32 {
    match name {
        "fannkuch" => 100_000,
        "nbody" => 20_000,
        "fib" => 27,
        "pidigits" => 500_000,
        "pyaes" => 400,
        "richards" => 50_000,
        "sumvm" => 2_000_000,
        "nested_loops" => 120,
        "jitloop" => 1_000,
        "deltablue" => 50,
        "float_math" => 100_000,
        "spectral_norm" => 100,
        "json_bench" => 150,
        "str_methods" => 15_000,
        "dict_ops" => 100_000,
        "list_ops" => 10_000,
        "attr_access" => 200_000,
        "call_overhead" => 150_000,
        "generators" => 300_000,
        "startup" => 1,
        _ => 1,
    }
}

/// One discovered fixture (path + display name).
#[derive(Debug, Clone)]
pub struct Fixture {
    pub name: String,
    pub path: PathBuf,
    pub work: u32,
    /// Measure full subprocess wall time (startup fixtures) instead
    /// of the self-timed bench region.
    pub wall_clock: bool,
}

/// Resolve `fixtures/` next to the crate's `Cargo.toml`.
pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// Load all known fixtures, returning the ones that exist on disk.
/// Missing files are skipped silently so an in-flight rename
/// doesn't break the runner.
pub fn discover_fixtures() -> Vec<Fixture> {
    let dir = fixtures_dir();
    FIXTURES
        .iter()
        .filter_map(|name| {
            let path = dir.join(format!("{name}.py"));
            if path.exists() {
                Some(Fixture {
                    name: (*name).to_owned(),
                    path,
                    work: default_work(name),
                    wall_clock: WALL_CLOCK_FIXTURES.contains(name),
                })
            } else {
                None
            }
        })
        .collect()
}

/// Path to the baseline JSON tracked alongside the fixtures.
pub fn baseline_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("baselines")
        .join("bench.json")
}
