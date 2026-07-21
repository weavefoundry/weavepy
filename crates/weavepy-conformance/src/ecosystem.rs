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
//!    populated by `tools/ecosystem_fetch.py`),
//! 3. runs the probe in a subprocess under a wall budget,
//! 4. grades the outcome against `tests/ecosystem/expectations.toml` and
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

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

/// One `[packages.<name>]` row of `tests/ecosystem/manifest.toml`.
#[derive(Debug, Clone)]
pub struct ManifestRow {
    /// Row label (the section name).
    pub name: String,
    /// pip requirement strings, in install order.
    pub requirements: Vec<String>,
    /// Probe file path, relative to the manifest's directory.
    pub probe: String,
    /// Optional per-row override of the wall budget.
    pub timeout_seconds: Option<u64>,
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
        let tables = simple_tables::parse(&body, "packages")
            .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;
        let mut rows = Vec::new();
        for (name, kv) in tables {
            let requirements = kv
                .get("requirements")
                .map(|v| v.split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default();
            let probe = kv
                .get("probe")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("[packages.{name}] missing probe"))?;
            let timeout_seconds =
                match kv.get("timeout_seconds") {
                    Some(v) => Some(v.parse::<u64>().map_err(|_| {
                        anyhow::anyhow!("[packages.{name}] bad timeout_seconds {v:?}")
                    })?),
                    None => None,
                };
            rows.push(ManifestRow {
                name,
                requirements,
                probe,
                timeout_seconds,
            });
        }
        Ok(Self {
            rows,
            base_dir: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
        })
    }
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

/// Parsed `expectations.toml` — `[packages.<name>] status = "…"` rows.
#[derive(Debug, Default)]
pub struct EcosystemExpectations {
    pub rows: BTreeMap<String, (RowStatus, Option<String>)>,
}

impl EcosystemExpectations {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let body = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let tables = simple_tables::parse(&body, "packages")
            .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;
        let mut rows = BTreeMap::new();
        for (name, kv) in tables {
            let status = kv
                .get("status")
                .and_then(|s| RowStatus::parse(s))
                .ok_or_else(|| anyhow::anyhow!("[packages.{name}] missing/bad status"))?;
            rows.insert(name, (status, kv.get("reason").cloned()));
        }
        Ok(Self { rows })
    }
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
    /// True when `status` disagrees with a recorded expectation.
    pub unexpected: bool,
    pub stage: Option<FailStage>,
    pub reason: Option<String>,
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
}

impl EcosystemSummary {
    pub fn from_reports(reports: &[RowReport]) -> Self {
        Self {
            total: reports.len(),
            passed: reports
                .iter()
                .filter(|r| r.status == RowStatus::Pass)
                .count(),
            failed: reports
                .iter()
                .filter(|r| r.status == RowStatus::Fail)
                .count(),
            skipped: reports
                .iter()
                .filter(|r| r.status == RowStatus::Skip)
                .count(),
            unexpected: reports.iter().filter(|r| r.unexpected).count(),
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
        let expected = expectations.rows.get(&row.name).map(|(s, _)| *s);
        if expected == Some(RowStatus::Skip) {
            let reason = expectations
                .rows
                .get(&row.name)
                .and_then(|(_, r)| r.clone());
            eprintln!("[ecosystem] {} … skip (baseline)", row.name);
            reports.push(RowReport {
                name: row.name.clone(),
                status: RowStatus::Skip,
                expected,
                unexpected: false,
                stage: None,
                reason,
                duration_secs: 0.0,
            });
            continue;
        }
        eprintln!("[ecosystem] {} …", row.name);
        let started = Instant::now();
        let (status, stage, reason) = run_row(manifest, row, opts);
        let duration = started.elapsed();
        let unexpected = match expected {
            Some(exp) => exp != status,
            // An unlisted row is treated as expected-pass: a red row must
            // be baselined explicitly with a measured reason.
            None => status != RowStatus::Pass,
        };
        eprintln!(
            "[ecosystem] {} → {}{} ({:.1}s)",
            row.name,
            status.as_str(),
            if unexpected { " [UNEXPECTED]" } else { "" },
            duration.as_secs_f64(),
        );
        reports.push(RowReport {
            name: row.name.clone(),
            status,
            expected,
            unexpected,
            stage,
            reason,
            duration_secs: duration.as_secs_f64(),
        });
    }
    reports
}

fn run_row(
    manifest: &Manifest,
    row: &ManifestRow,
    opts: &EcosystemOptions,
) -> (RowStatus, Option<FailStage>, Option<String>) {
    let budget = row
        .timeout_seconds
        .map(Duration::from_secs)
        .unwrap_or(opts.timeout);
    let deadline = Instant::now() + budget;

    let venv_dir = opts.scratch_dir.join(format!("venv-{}", row.name));
    let _ = fs::remove_dir_all(&venv_dir);
    let cleanup = VenvCleanup {
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
            return (
                RowStatus::Fail,
                Some(FailStage::Venv),
                Some(trim_reason(&format!("venv creation failed: {}", o.tail()))),
            )
        }
        Err(TimedOut) => {
            return (
                RowStatus::Fail,
                Some(FailStage::Timeout),
                Some("venv creation exceeded the wall budget".to_owned()),
            )
        }
    }

    let python = venv_python(&venv_dir);

    // 2. install
    if !row.requirements.is_empty() {
        let mut cmd = Command::new(&python);
        cmd.args(["-m", "pip", "install", "--quiet"]);
        if let Some(wheels) = &opts.wheels {
            cmd.arg("--no-index").arg("--find-links").arg(wheels);
        }
        cmd.args(&row.requirements);
        match run_with_deadline(&mut cmd, deadline) {
            Ok(o) if o.success => {}
            Ok(o) => {
                return (
                    RowStatus::Fail,
                    Some(FailStage::Install),
                    Some(trim_reason(&format!("pip install failed: {}", o.tail()))),
                )
            }
            Err(TimedOut) => {
                return (
                    RowStatus::Fail,
                    Some(FailStage::Timeout),
                    Some("pip install exceeded the wall budget".to_owned()),
                )
            }
        }
    }

    // 3. probe
    let probe_path = manifest.base_dir.join(&row.probe);
    let mut cmd = Command::new(&python);
    cmd.arg(&probe_path);
    for (k, v) in &opts.probe_env {
        cmd.env(k, v);
    }
    let result = match run_with_deadline(&mut cmd, deadline) {
        Ok(o) if o.success => (RowStatus::Pass, None, None),
        Ok(o) => (
            RowStatus::Fail,
            Some(FailStage::Probe),
            Some(trim_reason(&format!("probe failed: {}", o.tail()))),
        ),
        Err(TimedOut) => (
            RowStatus::Fail,
            Some(FailStage::Timeout),
            Some("probe exceeded the wall budget".to_owned()),
        ),
    };
    drop(cleanup);
    result
}

/// RAII scratch-venv removal (skipped with `--keep-venvs`).
struct VenvCleanup {
    dir: PathBuf,
    keep: bool,
}

impl Drop for VenvCleanup {
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
    combined: String,
}

impl CmdOutput {
    /// Last ~12 lines of output — enough to state a reason without
    /// pasting an install log into the baseline.
    fn tail(&self) -> String {
        let lines: Vec<&str> = self.combined.lines().collect();
        let start = lines.len().saturating_sub(12);
        lines[start..].join("\n")
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
                    combined: "wait failed".to_owned(),
                });
            }
        }
    }
}

fn trim_reason(s: &str) -> String {
    const MAX: usize = 2000;
    if s.len() <= MAX {
        return s.to_owned();
    }
    let mut cut = MAX;
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
    let _ = writeln!(out);
    let _ = writeln!(out, "| package | status | expected | time | reason |");
    let _ = writeln!(out, "|---|---|---|---|---|");
    for r in reports {
        let reason = r
            .reason
            .as_deref()
            .unwrap_or("")
            .replace('|', "\\|")
            .replace('\n', " ⏎ ");
        let reason = if reason.len() > 200 {
            let mut cut = 200;
            while !reason.is_char_boundary(cut) {
                cut -= 1;
            }
            format!("{}…", &reason[..cut])
        } else {
            reason
        };
        let _ = writeln!(
            out,
            "| {} | {}{} | {} | {:.1}s | {} |",
            r.name,
            r.status.as_str(),
            if r.unexpected { " ⚠" } else { "" },
            r.expected.map(|e| e.as_str()).unwrap_or("—"),
            r.duration_secs,
            reason,
        );
    }
    out
}

// ---------------------------------------------------------------------------
// Minimal TOML-subset parser (same dialect as the regrtest baseline:
// `[prefix."name"]` sections of `key = "value"` pairs, `#` comments).
// ---------------------------------------------------------------------------

mod simple_tables {
    use std::collections::BTreeMap;

    type Table = BTreeMap<String, String>;

    /// Parse `[<prefix>.<name>]` sections into `(name, key→value)` rows,
    /// preserving section order.
    pub(super) fn parse(body: &str, prefix: &str) -> Result<Vec<(String, Table)>, String> {
        let mut rows: Vec<(String, Table)> = Vec::new();
        let mut current: Option<String> = None;
        let mut table = Table::new();

        for (lineno, raw) in body.lines().enumerate() {
            let line = strip_comment(raw).trim().to_owned();
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
            let (k, v) = parse_kv(&line, lineno)?;
            if current.is_some() {
                table.insert(k, v);
            }
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

    fn parse_kv(line: &str, lineno: usize) -> Result<(String, String), String> {
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| format!("line {}: expected key = value", lineno + 1))?;
        Ok((k.trim().to_owned(), strip_quotes(v.trim()).to_owned()))
    }

    fn strip_comment(line: &str) -> &str {
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
}
