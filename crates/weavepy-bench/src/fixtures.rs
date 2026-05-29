//! Discovery of fixtures embedded in this crate.
//!
//! Each fixture is a self-contained `.py` file that exports a
//! top-level `bench(n)` callable. The list below is the
//! authoritative set used by the runner and the CI gate; new
//! fixtures need to be both dropped on disk *and* added here so
//! the runner finds them.

use std::path::PathBuf;

/// The full set of fixtures the runner knows about. Order is
/// preserved in CLI output and in the JSON report.
pub const FIXTURES: &[&str] = &[
    "fannkuch",
    "nbody",
    "fib",
    "pidigits",
    "pyaes",
    "richards",
    "sumvm",
    "nested_loops",
    "jitloop",
];

/// Default per-fixture work parameter passed as `bench(n)`.
/// Picked to make a single iteration take ~10-100ms on CPython —
/// small enough to keep the bench job under a minute, large
/// enough to dwarf timer overhead.
pub fn default_work(name: &str) -> u32 {
    match name {
        "fannkuch" => 7,
        "nbody" => 200,
        "fib" => 28,
        "pidigits" => 100,
        "pyaes" => 50,
        "richards" => 1,
        "sumvm" => 50_000,
        "nested_loops" => 30,
        "jitloop" => 300,
        _ => 1,
    }
}

/// One discovered fixture (path + display name).
#[derive(Debug, Clone)]
pub struct Fixture {
    pub name: String,
    pub path: PathBuf,
    pub work: u32,
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
