//! RFC 0055 WS5 — the ecosystem conformance harness.
//!
//! Validates WeavePy against *real PyPI packages*, not just the CPython
//! test suite. Each manifest row describes a requirement set and a smoke
//! probe (a standalone Python file asserting real behaviour, not just
//! `import`). Per row the runner:
//!
//! 1. creates a scratch venv with the WeavePy binary under test
//!    (`weavepy -m venv <dir>`),
//! 2. installs the requirements through the in-tree `pip`
//!    (`python -m pip install …`; `--wheels DIR` switches to the offline
//!    `--no-index --find-links` lane, so CI can run against a cache
//!    populated by `tools/ecosystem_fetch.py`). Rows carrying a
//!    `no_binary` field (RFC 0062 WS2) force the named packages through
//!    pip's *sdist* path — a real C compile — via a loopback PEP 503
//!    index that only offers the source tarball (see [`local_index`]),
//! 3. runs the probe in a subprocess under a wall budget,
//! 4. optionally (RFC 0062 WS4, `--selftests`) runs the package's *own*
//!    pytest suite out of its pinned sdist, graded independently of the
//!    probe against `selftest_status` baseline rows,
//! 5. grades the outcome against `tests/ecosystem/expectations.toml`
//!    (per-OS `status_<os>` overrides land with RFC 0062 WS3) and
//!    writes `ecosystem.md` / `ecosystem.json` next to the regrtest
//!    reports.
//!
//! The harness is how the wave's headline claim is *stated*: the baseline
//! file says exactly which packages work, and a red row with a measured
//! reason is the next wave's worklist.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::Serialize;

/// Default per-row wall budget (venv + install + probe), in seconds.
pub const DEFAULT_ROW_TIMEOUT_SECS: u64 = 600;

/// Default wall budget for a row's self-test stage alone, in seconds.
pub const DEFAULT_SELFTEST_TIMEOUT_SECS: u64 = 600;

/// The per-OS override suffixes the expectations format accepts
/// (RFC 0062 WS3). Anything else after `status_`/`reason_` is a typo and
/// must fail the load rather than silently grade with the base value.
const KNOWN_OS_SUFFIXES: &[&str] = &["macos", "linux", "windows"];

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

/// Where a row's upstream suite runs from (RFC 0066 WS5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelftestMode {
    /// Fetch + extract the pinned `source` sdist and run pytest from its
    /// root — the RFC 0062 WS4 shape.
    #[default]
    Sdist,
    /// Run pytest from a neutral empty cwd against the *installed*
    /// package (`--pyargs`). For packages whose sdist cannot be tested
    /// unbuilt (numpy: the sdist tree would shadow the wheel and its
    /// C modules are only present post-build).
    Installed,
}

/// One `[packages.<name>.selftest]` sub-table (RFC 0062 WS4): run the
/// package's own pytest suite out of its pinned sdist after the probe
/// passes.
#[derive(Debug, Clone)]
pub struct SelftestSpec {
    /// Suite location strategy (RFC 0066 WS5): `sdist` (default) or
    /// `installed`.
    pub mode: SelftestMode,
    /// Exact-pinned pip requirement whose sdist carries the test suite
    /// (`attrs==26.1.0`). Pinning keeps the row hermetic and the offline
    /// cache deterministic. Required in `sdist` mode; forbidden in
    /// `installed` mode (the suite ships inside the row's own wheel, so
    /// the pin lives on the row requirement instead) unless `overlay`
    /// is present (the sdist then only donates its test subtree).
    pub source: Option<String>,
    /// Overlay staging (RFC 0075 WS9): `"<sdist subtree> -> <stage
    /// path>"`, installed mode only, requires `source`. For wheels that
    /// ship no tests (lxml) whose sdist also cannot run in place (the
    /// unbuilt source tree would shadow the installed package — the
    /// numpy problem, except here the suite *only* exists in the
    /// sdist). Builds a PYTHONPATH stage holding a copy of the
    /// installed top-level package with the sdist's test subtree
    /// grafted in, so `--pyargs <pkg>.tests` imports the real compiled
    /// package plus its upstream suite.
    pub overlay: Option<String>,
    /// Extra test-only requirements installed into the row's venv.
    pub requirements: Vec<String>,
    /// pytest target path inside the extracted sdist root (e.g.
    /// `tests`), whitespace-split into arguments so a row can carry
    /// collection-level flags (`--ignore=…` — needed when a test module
    /// fails at *import* time, which `--deselect` cannot reach).
    pub command: String,
    /// `pytest --deselect` arguments — the measured, enumerated escapes.
    /// Every entry carries an inline manifest comment naming the failure
    /// class.
    pub deselect: Vec<String>,
    /// Optional wall budget for the self-test stage alone.
    pub timeout_seconds: Option<u64>,
}

/// One `[packages.<name>]` row of `tests/ecosystem/manifest.toml`.
#[derive(Debug, Clone)]
pub struct ManifestRow {
    /// Row label (the section name).
    pub name: String,
    /// pip requirement strings, in install order.
    pub requirements: Vec<String>,
    /// Package names (comma/space separated in the manifest) forced to
    /// install from their sdist — the RFC 0062 WS2 source-build proof.
    pub no_binary: Vec<String>,
    /// Probe file path, relative to the manifest's directory.
    pub probe: String,
    /// Optional per-row override of the wall budget.
    pub timeout_seconds: Option<u64>,
    /// Optional upstream-test-suite spec (RFC 0062 WS4).
    pub selftest: Option<SelftestSpec>,
}

/// Parsed `manifest.toml`.
#[derive(Debug, Default)]
pub struct Manifest {
    pub rows: Vec<ManifestRow>,
    /// Directory the manifest was loaded from (probe paths resolve
    /// against it).
    pub base_dir: PathBuf,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self> {
        let body = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::from_body(&body, path.parent().unwrap_or(Path::new(".")))
            .map_err(|e| anyhow::anyhow!("failed to parse {}: {e:#}", path.display()))
    }

    fn from_body(body: &str, base_dir: &Path) -> Result<Self> {
        let tables = simple_tables::parse(body, "packages").map_err(|e| anyhow::anyhow!("{e}"))?;

        // Split main rows from `.selftest` sub-tables, preserving order.
        let mut rows = Vec::new();
        let mut selftests: BTreeMap<String, SelftestSpec> = BTreeMap::new();
        for (name, kv) in tables {
            if let Some(parent) = name.strip_suffix(".selftest") {
                selftests.insert(parent.to_owned(), Self::parse_selftest(parent, &kv)?);
                continue;
            }
            if let Some((parent, sub)) = name.split_once('.') {
                anyhow::bail!("[packages.{parent}.{sub}] unknown sub-table (only .selftest)");
            }
            let requirements: Vec<String> = kv
                .get("requirements")
                .and_then(simple_tables::Value::as_str)
                .map(|v| v.split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default();
            let no_binary = match kv.get("no_binary").and_then(simple_tables::Value::as_str) {
                Some(v) => {
                    let names: Vec<String> = v
                        .split([',', ' '])
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect();
                    // Typo protection: every no_binary name must match a
                    // row requirement, and that requirement must be
                    // exact-pinned so the sdist lane is deterministic.
                    for pkg in &names {
                        let req = requirements
                            .iter()
                            .find(|r| normalize_name(requirement_name(r)) == normalize_name(pkg))
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "[packages.{name}] no_binary {pkg:?} matches no requirement"
                                )
                            })?;
                        if parse_pinned_requirement(req).is_none() {
                            anyhow::bail!(
                                "[packages.{name}] no_binary requirement {req:?} must be \
                                 pinned with `==`"
                            );
                        }
                    }
                    names
                }
                None => Vec::new(),
            };
            let probe = kv
                .get("probe")
                .and_then(simple_tables::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("[packages.{name}] missing probe"))?;
            let timeout_seconds = parse_timeout(&kv, &format!("packages.{name}"))?;
            rows.push(ManifestRow {
                name,
                requirements,
                no_binary,
                probe,
                timeout_seconds,
                selftest: None,
            });
        }

        for (parent, spec) in selftests {
            let row = rows.iter_mut().find(|r| r.name == parent).ok_or_else(|| {
                anyhow::anyhow!("[packages.{parent}.selftest] has no [packages.{parent}] row")
            })?;
            row.selftest = Some(spec);
        }

        Ok(Self {
            rows,
            base_dir: base_dir.to_path_buf(),
        })
    }

    /// Keep only the `k`-th of `n` load-balanced shards (1-based) — the
    /// CI fan-out knob behind `ecosystem --shard K/N`. Deterministic
    /// longest-processing-time assignment: rows sorted by descending
    /// cost (name as tiebreak) each land in the currently-lightest bin,
    /// so for a fixed manifest + flag set every row lands in exactly
    /// one shard and the union of the shards is the full schedule. The
    /// cost proxy is the row's wall budget plus, when the selftest tier
    /// runs, its selftest budget: timeouts are the only committed
    /// runtime signal, and balance only needs the right order of
    /// magnitude (the heavy selftest rows carry explicit budgets).
    pub fn retain_shard(&mut self, k: usize, n: usize, selftests: bool) {
        assert!(n >= 1 && (1..=n).contains(&k), "shard {k}/{n} out of range");
        let cost = |row: &ManifestRow| -> u64 {
            let probe = row.timeout_seconds.unwrap_or(DEFAULT_ROW_TIMEOUT_SECS);
            let selftest = match (&row.selftest, selftests) {
                (Some(spec), true) => spec
                    .timeout_seconds
                    .unwrap_or(DEFAULT_SELFTEST_TIMEOUT_SECS),
                _ => 0,
            };
            probe + selftest
        };
        let mut order: Vec<usize> = (0..self.rows.len()).collect();
        order.sort_by(|&a, &b| {
            cost(&self.rows[b])
                .cmp(&cost(&self.rows[a]))
                .then_with(|| self.rows[a].name.cmp(&self.rows[b].name))
        });
        let mut loads = vec![0u64; n];
        let mut shard_of = vec![0usize; self.rows.len()];
        for idx in order {
            let lightest = (0..n).min_by_key(|&bin| loads[bin]).unwrap_or(0);
            loads[lightest] += cost(&self.rows[idx]);
            shard_of[idx] = lightest;
        }
        let mut i = 0;
        self.rows.retain(|_| {
            let keep = shard_of[i] == k - 1;
            i += 1;
            keep
        });
    }

    fn parse_selftest(parent: &str, kv: &simple_tables::Table) -> Result<SelftestSpec> {
        let section = format!("packages.{parent}.selftest");
        let mode = match kv.get("mode").and_then(simple_tables::Value::as_str) {
            None | Some("sdist") => SelftestMode::Sdist,
            Some("installed") => SelftestMode::Installed,
            Some(m) => anyhow::bail!("[{section}] bad mode {m:?} (sdist|installed)"),
        };
        let source = kv
            .get("source")
            .and_then(simple_tables::Value::as_str)
            .map(str::to_owned);
        let overlay = kv
            .get("overlay")
            .and_then(simple_tables::Value::as_str)
            .map(str::to_owned);
        if let Some(o) = &overlay {
            if mode != SelftestMode::Installed {
                anyhow::bail!("[{section}] overlay requires mode = \"installed\"");
            }
            if source.is_none() {
                anyhow::bail!("[{section}] overlay requires a pinned source sdist");
            }
            if o.split_once("->").is_none() {
                anyhow::bail!("[{section}] overlay must be \"<sdist subtree> -> <stage path>\"");
            }
        }
        match (mode, &source) {
            (SelftestMode::Sdist, None) => anyhow::bail!("[{section}] missing source"),
            (SelftestMode::Installed, Some(_)) if overlay.is_none() => anyhow::bail!(
                "[{section}] source is meaningless with mode = \"installed\" \
                 (pin the row requirement instead) unless overlay is set"
            ),
            _ => {}
        }
        if let Some(source) = &source {
            if parse_pinned_requirement(source).is_none() {
                anyhow::bail!("[{section}] source {source:?} must be pinned with `==`");
            }
        }
        let requirements = kv
            .get("requirements")
            .and_then(simple_tables::Value::as_str)
            .map(|v| v.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default();
        let command = kv
            .get("command")
            .and_then(simple_tables::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("[{section}] missing command"))?;
        let deselect = match kv.get("deselect") {
            Some(simple_tables::Value::List(items)) => items.clone(),
            Some(simple_tables::Value::Str(_)) => {
                anyhow::bail!("[{section}] deselect must be an array of strings")
            }
            None => Vec::new(),
        };
        let timeout_seconds = parse_timeout(kv, &section)?;
        Ok(SelftestSpec {
            mode,
            source,
            overlay,
            requirements,
            command,
            deselect,
            timeout_seconds,
        })
    }
}

fn parse_timeout(kv: &simple_tables::Table, section: &str) -> Result<Option<u64>> {
    match kv
        .get("timeout_seconds")
        .and_then(simple_tables::Value::as_str)
    {
        Some(v) => Ok(Some(v.parse::<u64>().map_err(|_| {
            anyhow::anyhow!("[{section}] bad timeout_seconds {v:?}")
        })?)),
        None => Ok(None),
    }
}

/// Base project name of a pip requirement string
/// (`attrs[dev]==26.1.0; marker` → `attrs`).
fn requirement_name(spec: &str) -> &str {
    let end = spec
        .find(|c: char| "<>=!~[;( ".contains(c))
        .unwrap_or(spec.len());
    &spec[..end]
}

/// PEP 503 name normalization — the same rule as the in-tree pip's
/// `_normalize` (runs of `-_.` collapse to `-`, lowercase).
fn normalize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut dash = false;
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            if !dash {
                out.push('-');
            }
            dash = true;
        } else {
            out.push(c.to_ascii_lowercase());
            dash = false;
        }
    }
    out
}

/// Split an exact-pinned requirement (`name==version`, extras allowed)
/// into `(name, version)`. Returns `None` for anything else — the sdist
/// lanes require exact pins.
fn parse_pinned_requirement(spec: &str) -> Option<(String, String)> {
    let (head, version) = spec.split_once("==")?;
    let version = version.trim();
    if version.is_empty() || version.contains(['<', '>', '!', '~', ',']) {
        return None;
    }
    let name = requirement_name(head.trim());
    if name.is_empty() {
        return None;
    }
    Some((name.to_owned(), version.to_owned()))
}

// ---------------------------------------------------------------------------
// Expectations
// ---------------------------------------------------------------------------

/// Expected status of one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RowStatus {
    Pass,
    Fail,
    Skip,
}

impl RowStatus {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "pass" => Some(Self::Pass),
            "fail" => Some(Self::Fail),
            "skip" => Some(Self::Skip),
            _ => None,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Skip => "skip",
        }
    }
}

/// One `[packages.<name>]` baseline row, already resolved against the
/// host OS (RFC 0062 WS3: `status_<os>`/`reason_<os>` override the base
/// keys; same scheme for the WS4 `selftest_status`/`selftest_reason`).
#[derive(Debug, Clone)]
pub struct ExpectationRow {
    pub status: RowStatus,
    pub reason: Option<String>,
    /// Expected self-test outcome. `None` means "no explicit row" — the
    /// grader defaults to `pass` whenever a manifest selftest spec
    /// exists.
    pub selftest_status: Option<RowStatus>,
    pub selftest_reason: Option<String>,
}

/// Parsed `expectations.toml` — `[packages.<name>] status = "…"` rows.
#[derive(Debug, Default)]
pub struct EcosystemExpectations {
    pub rows: BTreeMap<String, ExpectationRow>,
    /// RFC 0063 WS7: the OSes this baseline was *measured* on
    /// (top-level `measured_os = ["macos", "linux"]`, spelled like
    /// `std::env::consts::OS` — the same names the per-OS suffix keys
    /// use). On a host OS not in the stamp, a `--check` run still
    /// prints the full report and writes results, but unexpected rows
    /// are advisory (a NOTE line, exit 0) until a measured baseline
    /// for that OS lands and its name joins the stamp. `None` (no
    /// stamp) means "all OSes measured" — pre-RFC-0063 behaviour.
    pub measured_os: Option<Vec<String>>,
}

impl EcosystemExpectations {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let body = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::from_body(&body, std::env::consts::OS)
            .map_err(|e| anyhow::anyhow!("failed to parse {}: {e:#}", path.display()))
    }

    /// Parse and resolve against `host_os` (`std::env::consts::OS`
    /// spelling: `macos` / `linux` / `windows`). Split out from `load`
    /// so the override resolution is unit-testable per OS.
    fn from_body(body: &str, host_os: &str) -> Result<Self> {
        let measured_os = parse_measured_os(body)?;
        let tables = simple_tables::parse(body, "packages").map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut rows = BTreeMap::new();
        for (name, kv) in tables {
            validate_os_suffixes(&name, &kv)?;
            let status = resolve_for_os(&kv, "status", host_os)
                .and_then(RowStatus::parse)
                .ok_or_else(|| anyhow::anyhow!("[packages.{name}] missing/bad status"))?;
            let reason = resolve_for_os(&kv, "reason", host_os).map(str::to_owned);
            let selftest_status = match resolve_for_os(&kv, "selftest_status", host_os) {
                Some(s) => Some(RowStatus::parse(s).ok_or_else(|| {
                    anyhow::anyhow!("[packages.{name}] bad selftest_status {s:?}")
                })?),
                None => None,
            };
            let selftest_reason =
                resolve_for_os(&kv, "selftest_reason", host_os).map(str::to_owned);
            rows.insert(
                name,
                ExpectationRow {
                    status,
                    reason,
                    selftest_status,
                    selftest_reason,
                },
            );
        }
        Ok(Self { rows, measured_os })
    }

    /// RFC 0063 WS7: `true` when `host_os` has a measured baseline in
    /// this file — the `measured_os` stamp names it, or the file has no
    /// stamp at all (missing stamp ≡ "all OSes measured").
    pub fn os_is_measured(&self, host_os: &str) -> bool {
        match &self.measured_os {
            Some(stamp) => stamp.iter().any(|os| os == host_os),
            None => true,
        }
    }
}

/// Extract the top-level `measured_os = ["macos", "linux"]` stamp
/// (RFC 0063 WS7). Only the region *before* the first `[packages…]`
/// section header is scanned (TOML top-level keys must precede
/// sections). Single-line string arrays only — the stamp is a short
/// list of OS names, each validated against [`KNOWN_OS_SUFFIXES`] so a
/// typo is a load error rather than a silently-always-advisory gate.
fn parse_measured_os(body: &str) -> Result<Option<Vec<String>>> {
    for (lineno, raw) in body.lines().enumerate() {
        let line = simple_tables::strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            break;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() != "measured_os" {
            continue;
        }
        let v = v.trim();
        if !v.starts_with('[') || !v.ends_with(']') {
            anyhow::bail!(
                "line {}: measured_os must be a single-line array of strings",
                lineno + 1
            );
        }
        let names = simple_tables::parse_list(v, lineno).map_err(|e| anyhow::anyhow!("{e}"))?;
        for os in &names {
            if !KNOWN_OS_SUFFIXES.contains(&os.as_str()) {
                anyhow::bail!(
                    "unknown OS {os:?} in measured_os (expected one of: macos, linux, windows)"
                );
            }
        }
        return Ok(Some(names));
    }
    Ok(None)
}

/// RFC 0063 WS7 — resolve the `--check` gate against the `measured_os`
/// stamp for the current host. Returns `true` when unexpected rows
/// should fail the run (measured host); on an unmeasured host it prints
/// a clearly-labelled advisory NOTE instead and returns `false`, so the
/// caller exits 0 with the full report/artifacts already written.
pub fn strict_gate_blocks(
    expectations: &EcosystemExpectations,
    summary: &EcosystemSummary,
) -> bool {
    strict_gate_blocks_for_os(expectations, summary, std::env::consts::OS)
}

/// Host-OS-explicit seam for [`strict_gate_blocks`], unit-testable on
/// every platform.
fn strict_gate_blocks_for_os(
    expectations: &EcosystemExpectations,
    summary: &EcosystemSummary,
    host_os: &str,
) -> bool {
    if summary.unexpected == 0 {
        return false;
    }
    if expectations.os_is_measured(host_os) {
        return true;
    }
    eprintln!(
        "NOTE: {host_os} is not in measured_os; {} unexpected result(s) reported, \
         gate is advisory until a measured baseline lands (RFC 0063)",
        summary.unexpected
    );
    false
}

/// Resolve `<base>_<host_os>` over plain `<base>` (RFC 0062 WS3).
fn resolve_for_os<'t>(kv: &'t simple_tables::Table, base: &str, host_os: &str) -> Option<&'t str> {
    kv.get(&format!("{base}_{host_os}"))
        .and_then(simple_tables::Value::as_str)
        .or_else(|| kv.get(base).and_then(simple_tables::Value::as_str))
}

/// Reject `status_freebsd`-style typos at load time. Only keys with one
/// of the overridable bases are checked; free-form keys (`notes`) stay
/// legal.
fn validate_os_suffixes(name: &str, kv: &simple_tables::Table) -> Result<()> {
    // Longest bases first so `selftest_status_x` is attributed to
    // `selftest_status`, not misparsed via a shorter base.
    const BASES: &[&str] = &["selftest_status", "selftest_reason", "status", "reason"];
    for key in kv.keys() {
        for base in BASES {
            if let Some(suffix) = key.strip_prefix(&format!("{base}_")) {
                if !KNOWN_OS_SUFFIXES.contains(&suffix) {
                    anyhow::bail!(
                        "[packages.{name}] unknown OS suffix in {key:?} \
                         (expected one of: macos, linux, windows)"
                    );
                }
                break;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

/// Knobs for one harness run.
#[derive(Debug)]
pub struct EcosystemOptions {
    /// The WeavePy binary the scratch venvs are built from.
    pub weavepy: PathBuf,
    /// Offline wheel cache (`pip install --no-index --find-links`).
    pub wheels: Option<PathBuf>,
    /// Default per-row wall budget.
    pub timeout: Duration,
    /// Run the RFC 0062 WS4 self-test tier after each passing probe.
    pub selftests: bool,
    /// Keep the scratch venvs around for post-mortem.
    pub keep_venvs: bool,
    /// Scratch root for venvs (defaults to a temp dir).
    pub scratch_dir: PathBuf,
    /// Extra env for probes (e.g. cert dir for local-TLS probes).
    pub probe_env: Vec<(String, String)>,
}

/// The stage a row failed in (also the reason prefix in reports).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FailStage {
    Venv,
    Install,
    Probe,
    Timeout,
}

/// Result of running one manifest row.
#[derive(Debug, Serialize)]
pub struct RowReport {
    pub name: String,
    pub status: RowStatus,
    pub expected: Option<RowStatus>,
    /// True when `status` *or* `selftest` disagrees with the baseline.
    pub unexpected: bool,
    pub stage: Option<FailStage>,
    pub reason: Option<String>,
    /// Self-test outcome (`None` = no spec / probe failed / tier off).
    pub selftest: Option<RowStatus>,
    pub selftest_expected: Option<RowStatus>,
    pub selftest_reason: Option<String>,
    pub duration_secs: f64,
}

/// Whole-run summary.
#[derive(Debug, Serialize)]
pub struct EcosystemSummary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub unexpected: usize,
    pub selftest_passed: usize,
    pub selftest_failed: usize,
    pub selftest_skipped: usize,
}

impl EcosystemSummary {
    pub fn from_reports(reports: &[RowReport]) -> Self {
        let count = |s: RowStatus| reports.iter().filter(|r| r.status == s).count();
        let st_count = |s: RowStatus| reports.iter().filter(|r| r.selftest == Some(s)).count();
        Self {
            total: reports.len(),
            passed: count(RowStatus::Pass),
            failed: count(RowStatus::Fail),
            skipped: count(RowStatus::Skip),
            unexpected: reports.iter().filter(|r| r.unexpected).count(),
            selftest_passed: st_count(RowStatus::Pass),
            selftest_failed: st_count(RowStatus::Fail),
            selftest_skipped: st_count(RowStatus::Skip),
        }
    }
}

/// What [`run_row`] measured, before grading against expectations.
struct RowOutcome {
    status: RowStatus,
    stage: Option<FailStage>,
    reason: Option<String>,
    selftest: Option<RowStatus>,
    selftest_reason: Option<String>,
}

impl RowOutcome {
    fn fail(stage: FailStage, reason: String) -> Self {
        Self {
            status: RowStatus::Fail,
            stage: Some(stage),
            reason: Some(reason),
            selftest: None,
            selftest_reason: None,
        }
    }
}

/// Run every manifest row serially (installs are network/disk heavy; a
/// serial run keeps the output readable and the load predictable).
pub fn run_all(
    manifest: &Manifest,
    expectations: &EcosystemExpectations,
    opts: &EcosystemOptions,
) -> Vec<RowReport> {
    let mut reports = Vec::with_capacity(manifest.rows.len());
    for row in &manifest.rows {
        let exp_row = expectations.rows.get(&row.name);
        let expected = exp_row.map(|e| e.status);
        if expected == Some(RowStatus::Skip) {
            let reason = exp_row.and_then(|e| e.reason.clone());
            eprintln!("[ecosystem] {} … skip (baseline)", row.name);
            reports.push(RowReport {
                name: row.name.clone(),
                status: RowStatus::Skip,
                expected,
                unexpected: false,
                stage: None,
                reason,
                selftest: None,
                selftest_expected: None,
                selftest_reason: None,
                duration_secs: 0.0,
            });
            continue;
        }

        // The self-test tier grades independently of the probe: an
        // absent baseline row defaults to expected-pass (same discipline
        // as probe rows), and an explicit `selftest_status = "skip"`
        // keeps the stage from running at all.
        let selftest_expected = if opts.selftests {
            row.selftest.as_ref().map(|_| {
                exp_row
                    .and_then(|e| e.selftest_status)
                    .unwrap_or(RowStatus::Pass)
            })
        } else {
            None
        };
        let run_selftest = matches!(selftest_expected, Some(s) if s != RowStatus::Skip);

        eprintln!("[ecosystem] {} …", row.name);
        let started = Instant::now();
        let mut outcome = run_row(manifest, row, opts, run_selftest);
        let duration = started.elapsed();
        if selftest_expected == Some(RowStatus::Skip) {
            outcome.selftest = Some(RowStatus::Skip);
            outcome.selftest_reason = exp_row.and_then(|e| e.selftest_reason.clone());
        }
        let probe_diverged = match expected {
            Some(exp) => exp != outcome.status,
            // An unlisted row is treated as expected-pass: a red row must
            // be baselined explicitly with a measured reason.
            None => outcome.status != RowStatus::Pass,
        };
        // The self-test only grades when the stage actually ran (a probe
        // failure already flags the row; skipping double-jeopardy keeps
        // the report readable).
        let selftest_diverged = match (outcome.selftest, selftest_expected) {
            (Some(actual), Some(exp)) => actual != exp,
            _ => false,
        };
        let unexpected = probe_diverged || selftest_diverged;
        let selftest_note = match outcome.selftest {
            Some(s) => format!(", selftest {}", s.as_str()),
            None => String::new(),
        };
        eprintln!(
            "[ecosystem] {} → {}{}{} ({:.1}s)",
            row.name,
            outcome.status.as_str(),
            selftest_note,
            if unexpected { " [UNEXPECTED]" } else { "" },
            duration.as_secs_f64(),
        );
        reports.push(RowReport {
            name: row.name.clone(),
            status: outcome.status,
            expected,
            unexpected,
            stage: outcome.stage,
            reason: outcome.reason,
            selftest: outcome.selftest,
            selftest_expected,
            selftest_reason: outcome.selftest_reason,
            duration_secs: duration.as_secs_f64(),
        });
    }
    reports
}

fn run_row(
    manifest: &Manifest,
    row: &ManifestRow,
    opts: &EcosystemOptions,
    run_selftest: bool,
) -> RowOutcome {
    let budget = row
        .timeout_seconds
        .map(Duration::from_secs)
        .unwrap_or(opts.timeout);
    let deadline = Instant::now() + budget;

    let venv_dir = opts.scratch_dir.join(format!("venv-{}", row.name));
    let _ = fs::remove_dir_all(&venv_dir);
    let cleanup = ScratchCleanup {
        dir: venv_dir.clone(),
        keep: opts.keep_venvs,
    };

    // 1. venv
    let out = run_with_deadline(
        Command::new(&opts.weavepy)
            .args(["-m", "venv"])
            .arg(&venv_dir),
        deadline,
    );
    match out {
        Ok(o) if o.success => {}
        Ok(o) => {
            return RowOutcome::fail(
                FailStage::Venv,
                trim_reason(&format!("venv creation failed: {}", o.tail())),
            )
        }
        Err(TimedOut) => {
            return RowOutcome::fail(
                FailStage::Timeout,
                "venv creation exceeded the wall budget".to_owned(),
            )
        }
    }

    let python = venv_python(&venv_dir);

    // 2. install — requirements matched by `no_binary` go through the
    //    sdist lane (a loopback index that only offers the tarball, so
    //    pip's own wheel-missing fallback compiles it from source);
    //    everything else through the normal wheel lane first, so build
    //    prerequisites like setuptools are in place before the compile.
    let (sdist_reqs, wheel_reqs): (Vec<&String>, Vec<&String>) =
        row.requirements.iter().partition(|r| {
            row.no_binary
                .iter()
                .any(|p| normalize_name(p) == normalize_name(requirement_name(r)))
        });
    if !wheel_reqs.is_empty() {
        let mut cmd = Command::new(&python);
        cmd.args(["-m", "pip", "install", "--quiet"]);
        if let Some(wheels) = &opts.wheels {
            cmd.arg("--no-index").arg("--find-links").arg(wheels);
        }
        cmd.args(&wheel_reqs);
        match run_with_deadline(&mut cmd, deadline) {
            Ok(o) if o.success => {}
            Ok(o) => {
                return RowOutcome::fail(
                    FailStage::Install,
                    trim_reason(&format!("pip install failed: {}", o.tail())),
                )
            }
            Err(TimedOut) => {
                return RowOutcome::fail(
                    FailStage::Timeout,
                    "pip install exceeded the wall budget".to_owned(),
                )
            }
        }
    }
    for spec in &sdist_reqs {
        if let Err(outcome) = install_from_sdist(spec, &python, &venv_dir, opts, deadline) {
            return outcome;
        }
    }

    // 3. probe
    let probe_path = manifest.base_dir.join(&row.probe);
    let mut cmd = Command::new(&python);
    cmd.arg(&probe_path);
    for (k, v) in &opts.probe_env {
        cmd.env(k, v);
    }
    match run_with_deadline(&mut cmd, deadline) {
        Ok(o) if o.success => {}
        Ok(o) => {
            return RowOutcome::fail(
                FailStage::Probe,
                trim_reason(&format!("probe failed: {}", o.tail())),
            )
        }
        Err(TimedOut) => {
            return RowOutcome::fail(
                FailStage::Timeout,
                "probe exceeded the wall budget".to_owned(),
            )
        }
    }

    // 4. self-test (RFC 0062 WS4) — its own wall budget, graded
    //    independently of the probe.
    let (selftest, selftest_reason) = match (run_selftest, &row.selftest) {
        (true, Some(spec)) => {
            let (status, reason) = run_selftest_stage(spec, &python, &venv_dir, opts);
            (Some(status), reason)
        }
        _ => (None, None),
    };

    drop(cleanup);
    RowOutcome {
        status: RowStatus::Pass,
        stage: None,
        reason: None,
        selftest,
        selftest_reason,
    }
}

/// Force `spec` (an exact-pinned requirement) through pip's sdist install
/// path — the RFC 0062 WS2 source-build proof.
///
/// The in-tree pip has no `--no-binary` flag (its `--only-binary` is the
/// *opposite* toggle), and `pip install <path>` only accepts `.whl`
/// files. What it *does* support is `--index-url`, plus a genuine sdist
/// fallback whenever the index offers no compatible wheel. So the
/// harness obtains the sdist itself and serves it from a loopback
/// PEP 503 index that carries nothing else — pip then walks its real
/// `sdist → _pep517 → setuptools → cc` lane end-to-end.
fn install_from_sdist(
    spec: &str,
    python: &Path,
    venv_dir: &Path,
    opts: &EcosystemOptions,
    deadline: Instant,
) -> std::result::Result<(), RowOutcome> {
    let sdist_dir = venv_dir.join("sdist-cache");
    if let Err(e) = fs::create_dir_all(&sdist_dir) {
        return Err(RowOutcome::fail(
            FailStage::Install,
            format!("failed to create {}: {e}", sdist_dir.display()),
        ));
    }
    let sdist = obtain_sdist(spec, python, opts, &sdist_dir, deadline).map_err(|e| {
        RowOutcome::fail(
            FailStage::Install,
            trim_reason(&format!("sdist fetch failed: {e}")),
        )
    })?;
    // The in-tree pip spools index downloads to a temp file named with
    // `os.path.splitext(label)[1]`, which reduces `*.tar.gz` to a bare
    // `.gz` that `_pep517.extract_sdist` rejects. Serving the tarball
    // under the equivalent `.tgz` name — accepted by both pip's index
    // matcher and the extractor — keeps the flow inside pip's
    // supported surface.
    if let Some(stem) = sdist
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".tar.gz"))
    {
        let tgz = sdist.with_file_name(format!("{stem}.tgz"));
        if let Err(e) = fs::rename(&sdist, &tgz) {
            return Err(RowOutcome::fail(
                FailStage::Install,
                format!("failed to rename {} to .tgz: {e}", sdist.display()),
            ));
        }
    }

    let index = local_index::LocalIndex::serve(sdist_dir).map_err(|e| {
        RowOutcome::fail(
            FailStage::Install,
            format!("local index failed to start: {e}"),
        )
    })?;
    let mut cmd = Command::new(python);
    cmd.args([
        "-m",
        "pip",
        "install",
        "--quiet",
        "--no-deps",
        "--index-url",
    ])
    .arg(index.url())
    .arg(spec);
    match run_with_deadline(&mut cmd, deadline) {
        Ok(o) if o.success => Ok(()),
        Ok(o) => Err(RowOutcome::fail(
            FailStage::Install,
            trim_reason(&format!("sdist install failed: {}", o.tail())),
        )),
        Err(TimedOut) => Err(RowOutcome::fail(
            FailStage::Timeout,
            "sdist install exceeded the wall budget".to_owned(),
        )),
    }
}

/// Run one row's upstream test suite (RFC 0062 WS4): fetch + extract the
/// pinned sdist, install the test-only requirements into the same venv,
/// and run pytest from the sdist root. `mode = "installed"` (RFC 0066
/// WS5) skips the sdist stages and runs pytest from a neutral empty cwd
/// against the installed package (`--pyargs`). Exit 0 grades `pass`;
/// anything else is a `fail` carrying the tail of the pytest output.
fn run_selftest_stage(
    spec: &SelftestSpec,
    python: &Path,
    venv_dir: &Path,
    opts: &EcosystemOptions,
) -> (RowStatus, Option<String>) {
    let budget = spec
        .timeout_seconds
        .unwrap_or(DEFAULT_SELFTEST_TIMEOUT_SECS);
    let deadline = Instant::now() + Duration::from_secs(budget);
    let fail = |msg: String| (RowStatus::Fail, Some(trim_reason_to(&msg, 4000)));

    let scratch = venv_dir.join("selftest");
    if let Err(e) = fs::create_dir_all(&scratch) {
        return fail(format!("failed to create {}: {e}", scratch.display()));
    }

    // 1.+2. locate the suite: extract the pinned sdist (sdist mode), or
    // an empty neutral cwd so pytest collects nothing but the `--pyargs`
    // target from site-packages (installed mode — a source tree in cwd
    // would shadow the built wheel).
    let suite_cwd = match spec.mode {
        SelftestMode::Installed => {
            let neutral = scratch.join("cwd");
            if let Err(e) = fs::create_dir_all(&neutral) {
                return fail(format!("failed to create {}: {e}", neutral.display()));
            }
            neutral
        }
        SelftestMode::Sdist => {
            let source = spec.source.as_deref().expect("sdist mode carries source");
            match fetch_and_extract_sdist(source, python, opts, &scratch, "src", deadline) {
                Ok(d) => d,
                Err(e) => return fail(e),
            }
        }
    };

    // 2b. overlay staging (RFC 0075 WS9): graft the sdist's test subtree
    // onto a copy of the *installed* package inside a PYTHONPATH stage,
    // so `--pyargs <pkg>.tests` imports the real compiled package plus
    // the upstream suite the wheel doesn't ship (lxml).
    let mut stage_pythonpath: Option<PathBuf> = None;
    if let Some(overlay) = &spec.overlay {
        let (from, to) = overlay
            .split_once("->")
            .map(|(a, b)| (a.trim(), b.trim()))
            .expect("overlay format validated at parse time");
        let source = spec.source.as_deref().expect("overlay carries source");
        let sdist_root = match fetch_and_extract_sdist(
            source,
            python,
            opts,
            &scratch,
            "overlay-src",
            deadline,
        ) {
            Ok(d) => d,
            Err(e) => return fail(e),
        };
        let top_pkg = match to.split(['/', '\\']).next().filter(|s| !s.is_empty()) {
            Some(p) => p,
            None => return fail(format!("overlay stage path {to:?} has no top package")),
        };
        let purelib = match venv_purelib(python, deadline) {
            Ok(p) => p,
            Err(e) => return fail(format!("overlay: cannot locate site-packages: {e}")),
        };
        let stage = scratch.join("stage");
        if let Err(e) = copy_dir_recursive(&purelib.join(top_pkg), &stage.join(top_pkg)) {
            return fail(format!("overlay: staging installed {top_pkg}: {e}"));
        }
        if let Err(e) = copy_dir_recursive(&sdist_root.join(from), &stage.join(to)) {
            return fail(format!("overlay: grafting sdist {from}: {e}"));
        }
        stage_pythonpath = Some(stage);
    }

    // 3. test-only requirements into the same venv
    if !spec.requirements.is_empty() {
        let mut cmd = Command::new(python);
        cmd.args(["-m", "pip", "install", "--quiet"]);
        if let Some(wheels) = &opts.wheels {
            cmd.arg("--no-index").arg("--find-links").arg(wheels);
        }
        cmd.args(&spec.requirements);
        match run_with_deadline(&mut cmd, deadline) {
            Ok(o) if o.success => {}
            Ok(o) => return fail(format!("selftest deps install failed: {}", o.tail())),
            Err(TimedOut) => {
                return fail("selftest deps install exceeded the selftest budget".to_owned())
            }
        }
    }

    // 4. the suite itself, from the sdist root so relative test paths
    //    and conftest discovery match upstream's own invocation (or the
    //    neutral cwd in installed mode). The venv python must be spawned
    //    by *absolute* path — with `current_dir` set, a scratch-relative
    //    program path would resolve against the suite cwd and fail to
    //    spawn — but NOT canonicalized: resolving the `bin/python`
    //    symlink lands on the base `weavepy` binary and loses the venv
    //    identity (pyvenv.cfg discovery is executable-path based).
    let python = std::path::absolute(python).unwrap_or_else(|_| python.to_path_buf());
    let mut cmd = Command::new(&python);
    cmd.args(["-m", "pytest"])
        .args(spec.command.split_whitespace())
        .args(["-q", "-p", "no:cacheprovider"]);
    for d in &spec.deselect {
        cmd.arg("--deselect").arg(d);
    }
    cmd.current_dir(&suite_cwd);
    if let Some(stage) = &stage_pythonpath {
        cmd.env("PYTHONPATH", stage);
    }
    for (k, v) in &opts.probe_env {
        cmd.env(k, v);
    }
    match run_with_deadline(&mut cmd, deadline) {
        Ok(o) if o.success => (RowStatus::Pass, None),
        Ok(o) => fail(format!("selftest failed: {}", o.tail_n(50))),
        Err(TimedOut) => fail(format!("selftest exceeded the {budget}s selftest budget")),
    }
}

/// Fetch the pinned `source` sdist and extract it under
/// `scratch/<subdir>`, returning the single root directory it unpacks
/// to. Shared by the sdist-mode suite location and overlay staging.
fn fetch_and_extract_sdist(
    source: &str,
    python: &Path,
    opts: &EcosystemOptions,
    scratch: &Path,
    subdir: &str,
    deadline: Instant,
) -> std::result::Result<PathBuf, String> {
    let sdist = obtain_sdist(source, python, opts, scratch, deadline)
        .map_err(|e| format!("selftest sdist fetch failed: {e}"))?;
    let extract_dir = scratch.join(subdir);
    fs::create_dir_all(&extract_dir)
        .map_err(|e| format!("failed to create {}: {e}", extract_dir.display()))?;
    let out = run_with_deadline(
        Command::new("tar")
            .arg("-xzf")
            .arg(&sdist)
            .arg("-C")
            .arg(&extract_dir),
        deadline,
    );
    match out {
        Ok(o) if o.success => {}
        Ok(o) => return Err(format!("sdist extract failed: {}", o.tail())),
        Err(TimedOut) => return Err("sdist extract exceeded the selftest budget".to_owned()),
    }
    single_subdir(&extract_dir).ok_or_else(|| {
        format!(
            "sdist {} did not extract to a single root directory",
            sdist.display()
        )
    })
}

/// The venv's `site-packages` directory, asked of the venv interpreter
/// itself (`sysconfig`) so platform layout differences don't leak here.
fn venv_purelib(python: &Path, deadline: Instant) -> std::result::Result<PathBuf, String> {
    let mut cmd = Command::new(python);
    cmd.args([
        "-c",
        "import sysconfig; print(sysconfig.get_paths()['purelib'])",
    ]);
    match run_with_deadline(&mut cmd, deadline) {
        Ok(o) if o.success => {
            // stdout and stderr are drained into one stream; the probe
            // prints exactly one line, so take the last non-empty one.
            let p = o
                .combined
                .lines()
                .rev()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or_default()
                .to_owned();
            if p.is_empty() {
                return Err("sysconfig returned an empty purelib".to_owned());
            }
            Ok(PathBuf::from(p))
        }
        Ok(o) => Err(format!("sysconfig probe failed: {}", o.tail())),
        Err(TimedOut) => Err("sysconfig probe exceeded the selftest budget".to_owned()),
    }
}

/// Minimal recursive directory copy (symlinks followed; the staged
/// trees are plain package dirs).
fn copy_dir_recursive(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_dir_recursive(&src, &dst)?;
        } else {
            fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// The single directory an sdist extracts to (`<name>-<version>/`).
fn single_subdir(dir: &Path) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    (dirs.len() == 1).then(|| dirs.remove(0))
}

/// In-venv sdist downloader. The in-tree pip's `download` subcommand only
/// fetches wheels (no `--no-binary`), so the online lane asks PyPI's JSON
/// API directly through the venv's own urllib — same interpreter, same
/// TLS stack pip itself uses.
const SDIST_FETCH_SCRIPT: &str = r#"
import json, os, sys, urllib.request

name, version, dest = sys.argv[1], sys.argv[2], sys.argv[3]
url = "https://pypi.org/pypi/{}/{}/json".format(name, version)
with urllib.request.urlopen(url) as resp:
    data = json.load(resp)
entry = next((u for u in data["urls"] if u["packagetype"] == "sdist"), None)
if entry is None:
    sys.exit("no sdist published for {}=={}".format(name, version))
target = os.path.join(dest, entry["filename"])
with urllib.request.urlopen(entry["url"]) as resp, open(target, "wb") as f:
    f.write(resp.read())
print(target)
"#;

/// Materialize the sdist for an exact-pinned requirement into `dest`.
///
/// Offline (`--wheels DIR`): the tarball must already be in the cache
/// (`tools/ecosystem_fetch.py` downloads sdists for `no_binary` and
/// selftest rows). Online: fetched via [`SDIST_FETCH_SCRIPT`].
fn obtain_sdist(
    spec: &str,
    python: &Path,
    opts: &EcosystemOptions,
    dest: &Path,
    deadline: Instant,
) -> std::result::Result<PathBuf, String> {
    let (name, version) = parse_pinned_requirement(spec)
        .ok_or_else(|| format!("requirement {spec:?} must be pinned with `==`"))?;

    if let Some(wheels) = &opts.wheels {
        let found = find_sdist_in_dir(wheels, &name, &version)
            .ok_or_else(|| format!("{name}-{version}.tar.gz not in {}", wheels.display()))?;
        let target = dest.join(found.file_name().unwrap_or_default());
        return match fs::copy(&found, &target) {
            Ok(_) => Ok(target),
            Err(e) => Err(format!("failed to copy {}: {e}", found.display())),
        };
    }

    let mut cmd = Command::new(python);
    cmd.arg("-c")
        .arg(SDIST_FETCH_SCRIPT)
        .arg(&name)
        .arg(&version)
        .arg(dest);
    match run_with_deadline(&mut cmd, deadline) {
        Ok(o) if o.success => {
            let path = o
                .combined
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .map(|l| PathBuf::from(l.trim()))
                .filter(|p| p.is_file())
                .ok_or_else(|| format!("sdist download reported no path: {}", o.tail()))?;
            Ok(path)
        }
        Ok(o) => Err(format!("sdist download failed: {}", o.tail())),
        Err(TimedOut) => Err("sdist download exceeded the wall budget".to_owned()),
    }
}

/// Find `<name>-<version>.tar.gz` (PEP 503-normalized name match) in a
/// wheel-cache directory.
fn find_sdist_in_dir(dir: &Path, name: &str, version: &str) -> Option<PathBuf> {
    let want = normalize_name(name);
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let fname = entry.file_name();
        let fname = fname.to_string_lossy();
        let Some(stem) = fname.strip_suffix(".tar.gz") else {
            continue;
        };
        let Some((base, ver)) = stem.rsplit_once('-') else {
            continue;
        };
        if normalize_name(base) == want && ver.eq_ignore_ascii_case(version) {
            return Some(entry.path());
        }
    }
    None
}

/// RAII scratch-dir removal (skipped with `--keep-venvs`).
struct ScratchCleanup {
    dir: PathBuf,
    keep: bool,
}

impl Drop for ScratchCleanup {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

fn venv_python(venv_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        venv_dir.join("Scripts").join("python.exe")
    } else {
        venv_dir.join("bin").join("python")
    }
}

struct TimedOut;

struct CmdOutput {
    success: bool,
    /// Exit code when the child terminated normally (`None` = signal).
    code: Option<i32>,
    combined: String,
}

impl CmdOutput {
    /// Last ~12 lines of output — enough to state a reason without
    /// pasting an install log into the baseline.
    fn tail(&self) -> String {
        self.tail_n(12)
    }

    fn tail_n(&self, n: usize) -> String {
        let lines: Vec<&str> = self.combined.lines().collect();
        let start = lines.len().saturating_sub(n);
        let tail = lines[start..].join("\n");
        if tail.trim().is_empty() {
            // A silent nonzero exit (e.g. a crash) would otherwise
            // produce an empty, undebuggable reason.
            return match self.code {
                Some(c) => format!("(no output; exit code {c})"),
                None => "(no output; killed by signal)".to_owned(),
            };
        }
        tail
    }
}

/// Run `cmd`, killing the child when `deadline` passes.
///
/// Both pipes are drained on background threads *while the child runs* —
/// waiting to read until after exit deadlocks any child that writes more
/// than the OS pipe buffer (~64 KB; a chatty pip failure log easily
/// does), which would then masquerade as a timeout.
fn run_with_deadline(cmd: &mut Command, deadline: Instant) -> Result<CmdOutput, TimedOut> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Ok(CmdOutput {
                success: false,
                code: None,
                combined: format!("failed to spawn: {e}"),
            })
        }
    };
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_string(&mut s);
            }
            s
        })
    };
    let out_thread = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err_thread = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let join = |t: std::thread::JoinHandle<String>| t.join().unwrap_or_default();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut combined = join(out_thread);
                let err = join(err_thread);
                if !err.is_empty() {
                    if !combined.is_empty() {
                        combined.push('\n');
                    }
                    combined.push_str(&err);
                }
                return Ok(CmdOutput {
                    success: status.success(),
                    code: status.code(),
                    combined,
                });
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    // Reap the drain threads so they don't leak.
                    let _ = join(out_thread);
                    let _ = join(err_thread);
                    return Err(TimedOut);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = join(out_thread);
                let _ = join(err_thread);
                return Ok(CmdOutput {
                    success: false,
                    code: None,
                    combined: "wait failed".to_owned(),
                });
            }
        }
    }
}

fn trim_reason(s: &str) -> String {
    trim_reason_to(s, 2000)
}

fn trim_reason_to(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut cut = max;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &s[..cut])
}

// ---------------------------------------------------------------------------
// Report rendering
// ---------------------------------------------------------------------------

pub fn report_to_markdown(reports: &[RowReport]) -> String {
    use std::fmt::Write;
    let summary = EcosystemSummary::from_reports(reports);
    let mut out = String::new();
    let _ = writeln!(out, "# Ecosystem conformance");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "{} rows — {} pass / {} fail / {} skip ({} unexpected)",
        summary.total, summary.passed, summary.failed, summary.skipped, summary.unexpected,
    );
    let selftests_ran =
        summary.selftest_passed + summary.selftest_failed + summary.selftest_skipped;
    if selftests_ran > 0 {
        let _ = writeln!(
            out,
            "selftests: {} pass / {} fail / {} skip",
            summary.selftest_passed, summary.selftest_failed, summary.selftest_skipped,
        );
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "| package | status | expected | selftest | time | reason |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|");
    for r in reports {
        let mut reason = r.reason.clone().unwrap_or_default();
        if let Some(st_reason) = &r.selftest_reason {
            if !reason.is_empty() {
                reason.push_str(" ⏎ ");
            }
            reason.push_str("selftest: ");
            reason.push_str(st_reason);
        }
        let reason = reason.replace('|', "\\|").replace('\n', " ⏎ ");
        let reason = if reason.len() > 200 {
            let mut cut = 200;
            while !reason.is_char_boundary(cut) {
                cut -= 1;
            }
            format!("{}…", &reason[..cut])
        } else {
            reason
        };
        let selftest_cell = match r.selftest {
            Some(s) => format!(
                "{}{}",
                s.as_str(),
                if r.selftest_expected.is_some() && r.selftest != r.selftest_expected {
                    " ⚠"
                } else {
                    ""
                }
            ),
            None => "—".to_owned(),
        };
        let _ = writeln!(
            out,
            "| {} | {}{} | {} | {} | {:.1}s | {} |",
            r.name,
            r.status.as_str(),
            if r.unexpected { " ⚠" } else { "" },
            r.expected.map(|e| e.as_str()).unwrap_or("—"),
            selftest_cell,
            r.duration_secs,
            reason,
        );
    }
    out
}

// ---------------------------------------------------------------------------
// Loopback PEP 503 index
// ---------------------------------------------------------------------------

/// A tiny PEP 503 "simple index" served from a local directory over a
/// loopback socket, alive for the duration of one `pip install`.
///
/// This is how the harness forces the in-tree pip down its *sdist*
/// install path (RFC 0062 WS2): the CLI has no `--no-binary` flag, but
/// `--index-url` is a supported knob and pip falls back to an sdist
/// whenever the index offers no compatible wheel — an index that only
/// carries the tarball is exactly that situation, with zero private-API
/// reach-ins.
mod local_index {
    use std::io::{Read as _, Write as _};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    pub(super) struct LocalIndex {
        addr: SocketAddr,
        stop: Arc<AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl LocalIndex {
        pub(super) fn serve(dir: PathBuf) -> std::io::Result<Self> {
            let listener = TcpListener::bind(("127.0.0.1", 0))?;
            let addr = listener.local_addr()?;
            // Non-blocking accept so the thread can observe the stop
            // flag instead of parking in `accept()` forever.
            listener.set_nonblocking(true)?;
            let stop = Arc::new(AtomicBool::new(false));
            let stop_thread = Arc::clone(&stop);
            let handle = std::thread::spawn(move || {
                while !stop_thread.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = handle_conn(stream, &dir);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        Err(_) => break,
                    }
                }
            });
            Ok(Self {
                addr,
                stop,
                handle: Some(handle),
            })
        }

        pub(super) fn url(&self) -> String {
            format!("http://{}/", self.addr)
        }
    }

    impl Drop for LocalIndex {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }

    fn handle_conn(mut stream: TcpStream, dir: &Path) -> std::io::Result<()> {
        // BSD-family accepted sockets inherit the listener's
        // non-blocking flag; reads want to block (with a cap).
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = stream.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 64 * 1024 {
                break;
            }
        }
        let request = String::from_utf8_lossy(&buf);
        let path = request
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_owned();

        let (status, ctype, body): (&str, &str, Vec<u8>) = if path.ends_with('/') {
            // Any project page lists every artifact in the directory;
            // pip's own filename normalization filters by name.
            let mut html = String::from("<!DOCTYPE html><html><body>\n");
            if let Ok(entries) = std::fs::read_dir(dir) {
                for e in entries.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    html.push_str(&format!("<a href=\"/files/{name}\">{name}</a><br>\n"));
                }
            }
            html.push_str("</body></html>\n");
            ("200 OK", "text/html", html.into_bytes())
        } else if let Some(fname) = path.strip_prefix("/files/") {
            // Basename only — no path traversal out of the sdist dir.
            let fname = fname.rsplit('/').next().unwrap_or(fname);
            match std::fs::read(dir.join(fname)) {
                Ok(bytes) => ("200 OK", "application/octet-stream", bytes),
                Err(_) => ("404 Not Found", "text/plain", b"not found".to_vec()),
            }
        } else {
            ("404 Not Found", "text/plain", b"not found".to_vec())
        };

        let header = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(header.as_bytes())?;
        stream.write_all(&body)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Minimal TOML-subset parser (same dialect as the regrtest baseline:
// `[prefix."name"]` sections of `key = "value"` pairs, `#` comments —
// extended for RFC 0062 with `key = ["a", "b"]` string arrays, which may
// span lines so deselect entries can carry inline reason comments).
// ---------------------------------------------------------------------------

mod simple_tables {
    use std::collections::BTreeMap;

    /// A parsed value: a bare string or an array of strings.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) enum Value {
        Str(String),
        List(Vec<String>),
    }

    impl Value {
        pub(super) fn as_str(&self) -> Option<&str> {
            match self {
                Value::Str(s) => Some(s),
                Value::List(_) => None,
            }
        }
    }

    pub(super) type Table = BTreeMap<String, Value>;

    /// Parse `[<prefix>.<name>]` sections into `(name, key→value)` rows,
    /// preserving section order. Sub-table headers keep their dotted
    /// name (`[packages.x.selftest]` → `"x.selftest"`).
    pub(super) fn parse(body: &str, prefix: &str) -> Result<Vec<(String, Table)>, String> {
        let mut rows: Vec<(String, Table)> = Vec::new();
        let mut current: Option<String> = None;
        let mut table = Table::new();
        // A `key = [` array whose closing bracket hasn't arrived yet.
        let mut pending: Option<(String, String, usize)> = None;

        for (lineno, raw) in body.lines().enumerate() {
            let line = strip_comment(raw).trim().to_owned();
            if let Some((key, mut acc, start_line)) = pending.take() {
                acc.push(' ');
                acc.push_str(&line);
                if brackets_balanced(&acc) {
                    let items = parse_list(&acc, start_line)?;
                    if current.is_some() {
                        table.insert(key, Value::List(items));
                    }
                } else {
                    pending = Some((key, acc, start_line));
                }
                continue;
            }
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                flush(&mut rows, current.take(), &mut table);
                let header = &line[1..line.len() - 1];
                let name = header
                    .strip_prefix(prefix)
                    .and_then(|r| r.strip_prefix('.'))
                    .ok_or_else(|| format!("line {}: unknown section [{header}]", lineno + 1))?;
                current = Some(strip_quotes(name.trim()).to_owned());
                continue;
            }
            let (k, v) = line
                .split_once('=')
                .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
                .ok_or_else(|| format!("line {}: expected key = value", lineno + 1))?;
            if v.starts_with('[') {
                if brackets_balanced(&v) {
                    let items = parse_list(&v, lineno)?;
                    if current.is_some() {
                        table.insert(k, Value::List(items));
                    }
                } else {
                    pending = Some((k, v, lineno));
                }
                continue;
            }
            if current.is_some() {
                table.insert(k, Value::Str(unescape(strip_quotes(&v))));
            }
        }
        if let Some((key, _, start_line)) = pending {
            return Err(format!(
                "line {}: unterminated array for key {key:?}",
                start_line + 1
            ));
        }
        flush(&mut rows, current, &mut table);
        Ok(rows)
    }

    fn flush(rows: &mut Vec<(String, Table)>, section: Option<String>, table: &mut Table) {
        if let Some(name) = section {
            rows.push((name, std::mem::take(table)));
        } else {
            table.clear();
        }
    }

    /// Bracket balance outside quoted strings — decides whether a
    /// multi-line array has closed yet. Brackets *inside* strings (e.g.
    /// pytest parametrize ids like `test_x[a-b]`) don't count.
    fn brackets_balanced(s: &str) -> bool {
        let mut depth = 0i64;
        let mut in_str = false;
        for c in s.chars() {
            match c {
                '"' => in_str = !in_str,
                '[' if !in_str => depth += 1,
                ']' if !in_str => depth -= 1,
                _ => {}
            }
        }
        depth == 0
    }

    /// Parse a balanced `[ "a", "b" ]` string-array literal.
    pub(super) fn parse_list(s: &str, lineno: usize) -> Result<Vec<String>, String> {
        let inner = s
            .trim()
            .strip_prefix('[')
            .and_then(|t| t.strip_suffix(']'))
            .ok_or_else(|| format!("line {}: malformed array {s:?}", lineno + 1))?;
        let mut items = Vec::new();
        let mut element = String::new();
        let mut in_str = false;
        for c in inner.chars() {
            match c {
                '"' => {
                    in_str = !in_str;
                    element.push(c);
                }
                ',' if !in_str => {
                    push_element(&mut items, &mut element, lineno)?;
                }
                _ => element.push(c),
            }
        }
        push_element(&mut items, &mut element, lineno)?;
        Ok(items)
    }

    fn push_element(
        items: &mut Vec<String>,
        element: &mut String,
        lineno: usize,
    ) -> Result<(), String> {
        let trimmed = element.trim();
        if !trimmed.is_empty() {
            if !(trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2) {
                return Err(format!(
                    "line {}: array element {trimmed:?} must be a quoted string",
                    lineno + 1
                ));
            }
            items.push(unescape(strip_quotes(trimmed)));
        }
        element.clear();
        Ok(())
    }

    pub(super) fn strip_comment(line: &str) -> &str {
        // A `#` inside a quoted string stays; the baseline dialect only
        // uses full-line or trailing comments outside quotes.
        let mut in_str = false;
        for (i, c) in line.char_indices() {
            match c {
                '"' => in_str = !in_str,
                '#' if !in_str => return &line[..i],
                _ => {}
            }
        }
        line
    }

    fn strip_quotes(s: &str) -> &str {
        s.strip_prefix('"')
            .and_then(|t| t.strip_suffix('"'))
            .unwrap_or(s)
    }

    /// Decode the TOML basic-string escapes the baseline dialect needs.
    /// Pytest parametrize ids may contain a literal backslash-n (pytest's
    /// own escaping of a newline in a parameter), written `\\n` in the
    /// manifest. Unknown escapes pass through verbatim.
    fn unescape(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- manifest parsing ---------------------------------------------------

    const MANIFEST_BODY: &str = r#"
[packages.markupsafe_sdist]
requirements = "setuptools markupsafe==3.0.3"
no_binary = "markupsafe"
probe = "probes/markupsafe_sdist_probe.py"

[packages.attrs]
requirements = "attrs"
probe = "probes/attrs_probe.py"

[packages.attrs.selftest]
source = "attrs==26.1.0"
requirements = "pytest hypothesis"
command = "tests"
deselect = [
    "tests/test_mypy.py",            # needs mypy plugin machinery
    "tests/test_abc.py::test_x[a-b]",# parametrized id with brackets
]
timeout_seconds = 900
"#;

    #[test]
    fn manifest_parses_no_binary_and_selftest() {
        let m = Manifest::from_body(MANIFEST_BODY, Path::new(".")).unwrap();
        assert_eq!(m.rows.len(), 2);

        let sdist_row = &m.rows[0];
        assert_eq!(sdist_row.name, "markupsafe_sdist");
        assert_eq!(sdist_row.no_binary, vec!["markupsafe"]);
        assert!(sdist_row.selftest.is_none());

        let attrs = &m.rows[1];
        assert!(attrs.no_binary.is_empty());
        let st = attrs.selftest.as_ref().expect("selftest spec");
        assert_eq!(st.mode, SelftestMode::Sdist);
        assert_eq!(st.source.as_deref(), Some("attrs==26.1.0"));
        assert_eq!(st.requirements, vec!["pytest", "hypothesis"]);
        assert_eq!(st.command, "tests");
        assert_eq!(
            st.deselect,
            vec!["tests/test_mypy.py", "tests/test_abc.py::test_x[a-b]"]
        );
        assert_eq!(st.timeout_seconds, Some(900));
    }

    /// The RFC 0075 WS9 overlay shape (lxml): installed mode carrying a
    /// pinned `source` whose sdist donates the test subtree.
    #[test]
    fn manifest_parses_overlay_selftest() {
        let body = r#"
[packages.lxml]
requirements = "lxml"
probe = "p.py"

[packages.lxml.selftest]
mode = "installed"
source = "lxml==6.1.2"
overlay = "src/lxml/tests -> lxml/tests"
requirements = "pytest"
command = "--pyargs lxml.tests"
"#;
        let m = Manifest::from_body(body, Path::new(".")).unwrap();
        let st = m.rows[0].selftest.as_ref().expect("selftest spec");
        assert_eq!(st.mode, SelftestMode::Installed);
        assert_eq!(st.source.as_deref(), Some("lxml==6.1.2"));
        assert_eq!(st.overlay.as_deref(), Some("src/lxml/tests -> lxml/tests"));
    }

    #[test]
    fn manifest_rejects_overlay_without_source() {
        let body = r#"
[packages.x]
requirements = "x"
probe = "p.py"

[packages.x.selftest]
mode = "installed"
overlay = "src/tests -> x/tests"
command = "--pyargs x.tests"
"#;
        let err = Manifest::from_body(body, Path::new(".")).unwrap_err();
        assert!(err.to_string().contains("overlay requires"), "{err}");
    }

    #[test]
    fn manifest_rejects_overlay_in_sdist_mode() {
        let body = r#"
[packages.x]
requirements = "x"
probe = "p.py"

[packages.x.selftest]
source = "x==1.0"
overlay = "src/tests -> x/tests"
command = "tests"
"#;
        let err = Manifest::from_body(body, Path::new(".")).unwrap_err();
        assert!(err.to_string().contains("mode = \"installed\""), "{err}");
    }

    #[test]
    fn manifest_rejects_unpinned_no_binary() {
        let body = r#"
[packages.x]
requirements = "markupsafe"
no_binary = "markupsafe"
probe = "p.py"
"#;
        let err = Manifest::from_body(body, Path::new(".")).unwrap_err();
        assert!(err.to_string().contains("pinned"), "{err}");
    }

    #[test]
    fn manifest_rejects_no_binary_without_requirement() {
        let body = r#"
[packages.x]
requirements = "setuptools"
no_binary = "markupsafe"
probe = "p.py"
"#;
        let err = Manifest::from_body(body, Path::new(".")).unwrap_err();
        assert!(err.to_string().contains("matches no requirement"), "{err}");
    }

    #[test]
    fn manifest_rejects_unknown_subtable() {
        let body = r#"
[packages.x]
requirements = "attrs"
probe = "p.py"

[packages.x.extras]
key = "v"
"#;
        let err = Manifest::from_body(body, Path::new(".")).unwrap_err();
        assert!(err.to_string().contains("unknown sub-table"), "{err}");
    }

    #[test]
    fn manifest_rejects_orphan_selftest() {
        let body = r#"
[packages.x.selftest]
source = "x==1.0"
command = "tests"
"#;
        let err = Manifest::from_body(body, Path::new(".")).unwrap_err();
        assert!(err.to_string().contains("has no [packages.x] row"), "{err}");
    }

    #[test]
    fn manifest_rejects_unpinned_selftest_source() {
        let body = r#"
[packages.x]
requirements = "attrs"
probe = "p.py"

[packages.x.selftest]
source = "attrs>=26"
command = "tests"
"#;
        let err = Manifest::from_body(body, Path::new(".")).unwrap_err();
        assert!(err.to_string().contains("pinned"), "{err}");
    }

    #[test]
    fn manifest_parses_installed_mode_selftest() {
        let body = r#"
[packages.numpy]
requirements = "numpy==2.5.2"
probe = "p.py"

[packages.numpy.selftest]
mode = "installed"
requirements = "pytest hypothesis"
command = "--pyargs numpy._core"
"#;
        let m = Manifest::from_body(body, Path::new(".")).unwrap();
        let st = m.rows[0].selftest.as_ref().expect("selftest spec");
        assert_eq!(st.mode, SelftestMode::Installed);
        assert_eq!(st.source, None);
        assert_eq!(st.command, "--pyargs numpy._core");
    }

    #[test]
    fn manifest_rejects_installed_mode_with_source() {
        let body = r#"
[packages.x]
requirements = "attrs"
probe = "p.py"

[packages.x.selftest]
mode = "installed"
source = "attrs==26.1.0"
command = "--pyargs attrs"
"#;
        let err = Manifest::from_body(body, Path::new(".")).unwrap_err();
        assert!(err.to_string().contains("meaningless"), "{err}");
    }

    #[test]
    fn manifest_rejects_unknown_selftest_mode() {
        let body = r#"
[packages.x]
requirements = "attrs"
probe = "p.py"

[packages.x.selftest]
mode = "wheelhouse"
command = "tests"
"#;
        let err = Manifest::from_body(body, Path::new(".")).unwrap_err();
        assert!(err.to_string().contains("bad mode"), "{err}");
    }

    #[test]
    fn no_binary_accepts_comma_and_space_separation() {
        let body = r#"
[packages.x]
requirements = "markupsafe==3.0.3 wrapt==2.3.0"
no_binary = "markupsafe, wrapt"
probe = "p.py"
"#;
        let m = Manifest::from_body(body, Path::new(".")).unwrap();
        assert_eq!(m.rows[0].no_binary, vec!["markupsafe", "wrapt"]);
    }

    #[test]
    fn strings_decode_backslash_escapes() {
        // Pytest parametrize ids can contain a literal backslash-n
        // (pytest's escaping of a newline parameter), written `\\n`.
        let body = r#"
[packages.x]
requirements = "click==8.4.2"
probe = "p.py"

[packages.x.selftest]
source = "click==8.4.2"
requirements = "pytest==8.4.2"
command = "tests"
deselect = ["tests/test_chain.py::test_pipeline[args0-foo\\nbar-expect0]"]
"#;
        let m = Manifest::from_body(body, Path::new(".")).unwrap();
        let st = m.rows[0].selftest.as_ref().unwrap();
        assert_eq!(
            st.deselect,
            vec![r"tests/test_chain.py::test_pipeline[args0-foo\nbar-expect0]"]
        );
    }

    // -- requirement helpers --------------------------------------------

    #[test]
    fn requirement_helpers() {
        assert_eq!(requirement_name("attrs[dev]==26.1.0; extra"), "attrs");
        assert_eq!(normalize_name("Python_dateutil.x"), "python-dateutil-x");
        assert_eq!(
            parse_pinned_requirement("MarkupSafe==3.0.3"),
            Some(("MarkupSafe".to_owned(), "3.0.3".to_owned()))
        );
        assert_eq!(parse_pinned_requirement("markupsafe>=3"), None);
        assert_eq!(parse_pinned_requirement("markupsafe"), None);
    }

    // -- expectations: per-OS override resolution (RFC 0062 WS3) ---------

    #[test]
    fn expectations_os_override_resolution_order() {
        let body = r#"
[packages.x]
status = "pass"
status_linux = "fail"
reason_linux = "linux-only divergence"
selftest_status = "pass"
selftest_status_linux = "fail"
selftest_reason_linux = "suite red on linux"
"#;
        // On linux the suffixed keys win…
        let exp = EcosystemExpectations::from_body(body, "linux").unwrap();
        let row = &exp.rows["x"];
        assert_eq!(row.status, RowStatus::Fail);
        assert_eq!(row.reason.as_deref(), Some("linux-only divergence"));
        assert_eq!(row.selftest_status, Some(RowStatus::Fail));
        assert_eq!(row.selftest_reason.as_deref(), Some("suite red on linux"));

        // …while every other OS resolves to the base keys.
        let exp = EcosystemExpectations::from_body(body, "macos").unwrap();
        let row = &exp.rows["x"];
        assert_eq!(row.status, RowStatus::Pass);
        assert_eq!(row.reason, None);
        assert_eq!(row.selftest_status, Some(RowStatus::Pass));
        assert_eq!(row.selftest_reason, None);
    }

    #[test]
    fn expectations_suffix_only_row_needs_base_status() {
        // A row with only `status_linux` has no status on other hosts —
        // that's a load error there (the base key is required).
        let body = r#"
[packages.x]
status_linux = "fail"
"#;
        assert!(EcosystemExpectations::from_body(body, "linux").is_ok());
        let err = EcosystemExpectations::from_body(body, "macos").unwrap_err();
        assert!(err.to_string().contains("missing/bad status"), "{err}");
    }

    #[test]
    fn expectations_reject_unknown_os_suffix() {
        for key in [
            "status_freebsd",
            "reason_osx",
            "selftest_status_win32",
            "selftest_reason_darwin",
        ] {
            let body = format!("[packages.x]\nstatus = \"pass\"\n{key} = \"fail\"\n");
            let err = EcosystemExpectations::from_body(&body, "macos").unwrap_err();
            assert!(
                err.to_string().contains("unknown OS suffix"),
                "{key}: {err}"
            );
        }
    }

    #[test]
    fn expectations_keep_free_form_keys_legal() {
        let body = r#"
[packages.x]
status = "pass"
notes = "prose for humans"
"#;
        let exp = EcosystemExpectations::from_body(body, "macos").unwrap();
        assert_eq!(exp.rows["x"].status, RowStatus::Pass);
    }

    #[test]
    fn expectations_selftest_status_default_is_absent() {
        let body = "[packages.x]\nstatus = \"pass\"\n";
        let exp = EcosystemExpectations::from_body(body, "macos").unwrap();
        // The pass-if-spec-exists default is applied at grading time,
        // not load time — an absent key stays absent here.
        assert_eq!(exp.rows["x"].selftest_status, None);
    }

    // -- measured_os stamp (RFC 0063 WS7) ---------------------------------

    #[test]
    fn expectations_measured_os_stamp_parses() {
        let body = r#"
# header comment
measured_os = ["macos", "linux"]

[packages.x]
status = "pass"
"#;
        let exp = EcosystemExpectations::from_body(body, "macos").unwrap();
        assert_eq!(
            exp.measured_os,
            Some(vec!["macos".to_owned(), "linux".to_owned()])
        );
        // Rows still parse as before.
        assert_eq!(exp.rows["x"].status, RowStatus::Pass);
    }

    #[test]
    fn expectations_missing_stamp_means_all_measured() {
        let body = "[packages.x]\nstatus = \"pass\"\n";
        let exp = EcosystemExpectations::from_body(body, "windows").unwrap();
        assert_eq!(exp.measured_os, None);
        for host in ["macos", "linux", "windows"] {
            assert!(exp.os_is_measured(host), "host {host}");
        }
    }

    #[test]
    fn expectations_stamp_resolves_per_host() {
        let body = "measured_os = [\"macos\", \"linux\"]\n";
        let exp = EcosystemExpectations::from_body(body, "windows").unwrap();
        assert!(exp.os_is_measured("macos"));
        assert!(exp.os_is_measured("linux"));
        assert!(!exp.os_is_measured("windows"));
    }

    #[test]
    fn expectations_stamp_rejects_unknown_os() {
        let err =
            EcosystemExpectations::from_body("measured_os = [\"darwin\"]\n", "macos").unwrap_err();
        assert!(err.to_string().contains("unknown OS"), "{err}");
    }

    #[test]
    fn expectations_stamp_only_read_from_top_level() {
        // A `measured_os` key *inside* a section is not the stamp — it
        // stays an ignored free-form row key.
        let body = r#"
[packages.x]
status = "pass"
measured_os = ["macos"]
"#;
        let exp = EcosystemExpectations::from_body(body, "windows").unwrap();
        assert_eq!(exp.measured_os, None);
        assert!(exp.os_is_measured("windows"));
    }

    // -- measured_os advisory gate (RFC 0063 WS7) --------------------------

    fn summary_with_unexpected(n: usize) -> EcosystemSummary {
        EcosystemSummary {
            total: n,
            passed: 0,
            failed: n,
            skipped: 0,
            unexpected: n,
            selftest_passed: 0,
            selftest_failed: 0,
            selftest_skipped: 0,
        }
    }

    #[test]
    fn gate_blocks_on_measured_host_and_advises_elsewhere() {
        let exp = EcosystemExpectations {
            measured_os: Some(vec!["macos".to_owned(), "linux".to_owned()]),
            ..EcosystemExpectations::default()
        };
        assert!(strict_gate_blocks_for_os(
            &exp,
            &summary_with_unexpected(1),
            "linux"
        ));
        assert!(!strict_gate_blocks_for_os(
            &exp,
            &summary_with_unexpected(1),
            "windows"
        ));
        // No unexpected rows → never blocks, measured or not.
        assert!(!strict_gate_blocks_for_os(
            &exp,
            &summary_with_unexpected(0),
            "linux"
        ));
    }

    #[test]
    fn gate_blocks_everywhere_without_stamp() {
        let exp = EcosystemExpectations::default();
        for host in ["macos", "linux", "windows"] {
            assert!(
                strict_gate_blocks_for_os(&exp, &summary_with_unexpected(1), host),
                "host {host}"
            );
        }
    }

    fn shard_fixture() -> Manifest {
        let row = |name: &str, timeout: Option<u64>, selftest: Option<u64>| ManifestRow {
            name: name.to_owned(),
            requirements: Vec::new(),
            no_binary: Vec::new(),
            probe: "probe.py".to_owned(),
            timeout_seconds: timeout,
            selftest: selftest.map(|t| SelftestSpec {
                mode: SelftestMode::Installed,
                source: None,
                overlay: None,
                requirements: Vec::new(),
                command: "tests".to_owned(),
                deselect: Vec::new(),
                timeout_seconds: Some(t),
            }),
        };
        Manifest {
            rows: vec![
                row("scipy", None, Some(14400)),
                row("pillow", None, Some(7200)),
                row("six", None, None),
                row("numpy", None, Some(2400)),
                row("aiohttp", Some(1200), None),
            ],
            base_dir: PathBuf::from("."),
        }
    }

    /// The `--shard K/N` partition: every row lands in exactly one
    /// shard, the union of the shards is the full schedule, and the
    /// budget-heaviest row is isolated from the next-heaviest.
    #[test]
    fn shards_partition_every_row_exactly_once() {
        const N: usize = 3;
        let full: Vec<String> = shard_fixture()
            .rows
            .iter()
            .map(|r| r.name.clone())
            .collect();
        let mut seen = Vec::new();
        for k in 1..=N {
            let mut m = shard_fixture();
            m.retain_shard(k, N, true);
            assert!(!m.rows.is_empty(), "shard {k}/{N} is empty");
            seen.extend(m.rows.iter().map(|r| r.name.clone()));
            let names: Vec<&str> = m.rows.iter().map(|r| r.name.as_str()).collect();
            assert!(
                !(names.contains(&"scipy") && names.contains(&"pillow")),
                "two heaviest rows share shard {k}/{N}: {names:?}"
            );
        }
        seen.sort();
        let mut expected = full;
        expected.sort();
        assert_eq!(seen, expected, "shards must partition the schedule");
    }

    /// Without `--selftests` the selftest budgets must not skew the
    /// balance — a probe-only lane weighs scipy like any other row.
    #[test]
    fn shard_cost_ignores_selftests_when_disabled() {
        let mut m = shard_fixture();
        m.retain_shard(1, 2, false);
        // aiohttp (1200s probe budget) is the heaviest probe-only row;
        // LPT places it first, into shard 1.
        assert!(m.rows.iter().any(|r| r.name == "aiohttp"));
    }
}
