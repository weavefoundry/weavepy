//! Regression test runner — drive individual `test_*.py` files end-to-end
//! through WeavePy and grade them against a checked-in baseline.
//!
//! Two test pools are recognised:
//!
//! 1. **Bundled regression tests** under `tests/regrtest/` in the repo
//!    root. These are small, hand-curated fixtures that exercise the
//!    Rust↔Python boundary. They should all pass on `main`; a
//!    regression breaks CI.
//! 2. **CPython `Lib/test/`** when `vendor/cpython/` is checked out as
//!    a submodule. The full CPython test suite is enormous so we
//!    operate off an allowlist that grows organically as features
//!    light up. Status per test is tracked in
//!    `tests/regrtest/expectations.toml`.
//!
//! Each test is graded with one of [`TestStatus`]:
//!
//! - `Pass`   — script ran to completion without an uncaught exception.
//! - `Fail`   — uncaught exception escaped the script.
//! - `Error`  — pre-execution failure (parse/compile/IO).
//! - `Skip`   — the expectations file marked the test as `skip`.
//! - `Timeout`— exceeded the per-test wall budget.
//!
//! The runner is single-threaded; tests share no global state because each
//! test gets a fresh [`weavepy::vm::Interpreter`]. Concurrency could be
//! layered on later but isn't worth the complexity yet.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use weavepy::{InterpreterFlags, RunOptions};

/// Outcome of one regression test run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestStatus {
    Pass,
    Fail,
    Error,
    Skip,
    Timeout,
}

impl TestStatus {
    pub fn label(self) -> &'static str {
        match self {
            TestStatus::Pass => "pass",
            TestStatus::Fail => "fail",
            TestStatus::Error => "error",
            TestStatus::Skip => "skip",
            TestStatus::Timeout => "timeout",
        }
    }

    /// `true` when the run was a successful execution from the runner's
    /// point of view. Equivalent to `==Pass`, but spelled out so callers
    /// reading the source don't have to remember the convention.
    pub fn is_passing(self) -> bool {
        self == TestStatus::Pass
    }
}

/// Per-test record produced by the runner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestReport {
    /// Stable label, e.g. `"bundled/test_basic.py"`.
    pub label: String,
    pub status: TestStatus,
    /// Wall-clock execution time. `None` for `Skip` (we never ran it).
    pub duration_ms: Option<u128>,
    /// Free-form diagnostic detail (truncated stderr/traceback).
    pub detail: Option<String>,
    /// Status the expectations file demanded. `None` ≡ no expectation.
    pub expected: Option<TestStatus>,
}

impl TestReport {
    /// `true` iff the observed status matches the expected one. When no
    /// expectation is declared, anything but `Fail`/`Error`/`Timeout`
    /// counts as a pass (i.e. new tests default to "expect pass").
    pub fn matches_expectation(&self) -> bool {
        match self.expected {
            Some(exp) => exp == self.status,
            None => self.status == TestStatus::Pass || self.status == TestStatus::Skip,
        }
    }
}

/// Aggregated counts for a single regrtest run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegrtestSummary {
    pub total: usize,
    pub pass: usize,
    pub fail: usize,
    pub error: usize,
    pub skip: usize,
    pub timeout: usize,
    /// Tests whose observed status differed from the expectations file —
    /// the regressions that should block CI.
    pub unexpected: usize,
}

impl RegrtestSummary {
    pub fn from_reports(reports: &[TestReport]) -> Self {
        let mut s = Self::default();
        for r in reports {
            s.total += 1;
            match r.status {
                TestStatus::Pass => s.pass += 1,
                TestStatus::Fail => s.fail += 1,
                TestStatus::Error => s.error += 1,
                TestStatus::Skip => s.skip += 1,
                TestStatus::Timeout => s.timeout += 1,
            }
            if !r.matches_expectation() {
                s.unexpected += 1;
            }
        }
        s
    }
}

/// Expectations file format. Keyed by stable test label.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Expectations {
    /// Per-test expectations.
    #[serde(default)]
    pub tests: BTreeMap<String, ExpectedEntry>,
    /// Per-test wall-clock budget. Honoured only when present.
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedEntry {
    pub status: TestStatus,
    /// Human-readable reason (free-form), e.g. "blocked on UnpackEx".
    #[serde(default)]
    pub reason: Option<String>,
}

impl Expectations {
    /// Parse a TOML expectations file. Missing/empty file ≡ "everything
    /// should pass."
    pub fn load(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let body = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let parsed: Expectations = simple_toml::parse(&body)
            .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;
        Ok(parsed)
    }

    pub fn get(&self, label: &str) -> Option<TestStatus> {
        self.tests.get(label).map(|e| e.status)
    }
}

/// A single bundled test file scheduled for execution.
#[derive(Debug, Clone)]
pub struct RegrtestFile {
    pub path: PathBuf,
    pub label: String,
}

/// Discover regrtest files under `workspace_root`.
///
/// Returns the bundled tests in `tests/regrtest/` plus, when present,
/// the CPython `Lib/test/` files from the allowlist in
/// [`CPYTHON_REGRTEST_INCLUDE`].
pub fn discover(workspace_root: &Path) -> Vec<RegrtestFile> {
    let mut out = Vec::new();

    let bundled = workspace_root.join("tests").join("regrtest");
    if bundled.is_dir() {
        collect_bundled(&bundled, &mut out);
    }

    let cpython_test = workspace_root
        .join("vendor")
        .join("cpython")
        .join("Lib")
        .join("test");
    if cpython_test.is_dir() {
        for name in CPYTHON_REGRTEST_INCLUDE {
            let p = cpython_test.join(name);
            if p.is_file() {
                out.push(RegrtestFile {
                    path: p,
                    label: format!("cpython/Lib/test/{name}"),
                });
            }
        }
    }

    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// Curated CPython regression tests we attempt. Add to this list (and
/// `expectations.toml`) as features come online.
pub const CPYTHON_REGRTEST_INCLUDE: &[&str] = &[
    "test_grammar.py",
    "test_tokenize.py",
    "test_dict.py",
    "test_list.py",
    "test_set.py",
];

fn collect_bundled(root: &Path, out: &mut Vec<RegrtestFile>) {
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n,
            None => continue,
        };
        let is_py = Path::new(name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("py"));
        if !name.starts_with("test_") || !is_py {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(path);
        out.push(RegrtestFile {
            path: path.to_path_buf(),
            label: format!("bundled/{}", rel.display()),
        });
    }
}

/// Run every discovered regrtest file and grade it against `expectations`.
pub fn run_all(
    files: &[RegrtestFile],
    expectations: &Expectations,
    timeout: Duration,
) -> Vec<TestReport> {
    files
        .iter()
        .map(|f| run_one(f, expectations, timeout))
        .collect()
}

/// Drive one regression test through a fresh [`weavepy::vm::Interpreter`].
///
/// The wall budget is honoured by the polite path only (we don't
/// SIGSTOP a runaway). The interpreter is single-threaded so a hang in
/// pure Python will eat the budget gracefully when the next opcode
/// dispatches.
pub fn run_one(file: &RegrtestFile, expectations: &Expectations, timeout: Duration) -> TestReport {
    let expected = expectations.get(&file.label);

    if expected == Some(TestStatus::Skip) {
        return TestReport {
            label: file.label.clone(),
            status: TestStatus::Skip,
            duration_ms: None,
            detail: expectations
                .tests
                .get(&file.label)
                .and_then(|e| e.reason.clone()),
            expected,
        };
    }

    let source = match fs::read_to_string(&file.path) {
        Ok(s) => s,
        Err(e) => {
            return TestReport {
                label: file.label.clone(),
                status: TestStatus::Error,
                duration_ms: Some(0),
                detail: Some(format!("read failed: {e}")),
                expected,
            };
        }
    };

    let opts = RunOptions::new(file.path.display().to_string())
        .with_argv(vec![file.path.display().to_string()])
        .with_script_dir(
            file.path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from(".")),
        )
        .with_flags(InterpreterFlags::default());

    let start = Instant::now();
    let result = weavepy::run_source_with_options(&source, &opts);
    let elapsed = start.elapsed();

    let (status, detail) = if elapsed > timeout {
        (TestStatus::Timeout, Some(format!("budget {timeout:?}")))
    } else {
        match result {
            Ok(()) => (TestStatus::Pass, None),
            Err(err) => match &err {
                weavepy::Error::Parse(_) | weavepy::Error::Compile(_) => {
                    let msg = err.format(&source, &opts.filename);
                    (TestStatus::Error, Some(truncate_detail(&msg)))
                }
                weavepy::Error::Runtime(_) => {
                    let msg = err.format(&source, &opts.filename);
                    (TestStatus::Fail, Some(truncate_detail(&msg)))
                }
            },
        }
    };

    TestReport {
        label: file.label.clone(),
        status,
        duration_ms: Some(elapsed.as_millis()),
        detail,
        expected,
    }
}

fn truncate_detail(msg: &str) -> String {
    const LIMIT: usize = 1024;
    if msg.len() <= LIMIT {
        msg.to_owned()
    } else {
        let mut s = String::with_capacity(LIMIT + 16);
        s.push_str(&msg[..LIMIT]);
        s.push_str("…[truncated]");
        s
    }
}

/// Render the report as a Markdown table for `report.md`.
pub fn report_to_markdown(reports: &[TestReport]) -> String {
    let summary = RegrtestSummary::from_reports(reports);
    let mut out = String::new();
    let _ = writeln!(out, "# WeavePy regrtest");
    let _ = writeln!(
        out,
        "{} total — pass {} / fail {} / error {} / skip {} / timeout {} — unexpected {}",
        summary.total,
        summary.pass,
        summary.fail,
        summary.error,
        summary.skip,
        summary.timeout,
        summary.unexpected,
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "| Test | Status | Expected | Time (ms) | Note |");
    let _ = writeln!(out, "|------|--------|----------|-----------|------|");
    for r in reports {
        let exp = r
            .expected
            .map(|s| s.label().to_owned())
            .unwrap_or_else(|| "—".to_owned());
        let dur = r
            .duration_ms
            .map(|m| m.to_string())
            .unwrap_or_else(|| "—".to_owned());
        let mark = if r.matches_expectation() { "" } else { " ❗" };
        let detail = r
            .detail
            .as_deref()
            .map(|s| s.lines().next().unwrap_or("").replace('|', "\\|"))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "| `{}` | {}{} | {} | {} | {} |",
            r.label,
            r.status.label(),
            mark,
            exp,
            dur,
            detail
        );
    }
    out
}

/// Default per-test wall budget, in seconds. Tests under
/// `tests/regrtest/` should run in well under one second; CPython
/// `Lib/test/` modules need more headroom but the runner is still
/// expected to make forward progress on every opcode.
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Tiny TOML subset parser. Enough for our `expectations.toml`
/// shape (top-level keys + `[tests.<id>]` sections with `status`/
/// `reason`) without pulling in the full `toml` crate (which would
/// add ~50 KB to the conformance binary). If we ever need richer
/// TOML, swap this out.
mod simple_toml {
    use std::collections::BTreeMap;

    use super::{Expectations, ExpectedEntry, TestStatus};

    pub(super) fn parse(body: &str) -> Result<Expectations, String> {
        let mut top = Expectations::default();
        let mut current_section: Option<String> = None;
        let mut current_table: BTreeMap<String, String> = BTreeMap::new();

        for (lineno, raw_line) in body.lines().enumerate() {
            let line = strip_comment(raw_line).trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                flush(&mut top, current_section.take(), &mut current_table)?;
                let header = &line[1..line.len() - 1];
                current_section = Some(header.to_owned());
                continue;
            }
            let (k, v) = parse_kv(line, lineno)?;
            if current_section.is_some() {
                current_table.insert(k, v);
            } else if k == "timeout_seconds" {
                let n: u64 = v
                    .parse()
                    .map_err(|_| format!("line {}: bad timeout", lineno + 1))?;
                top.timeout_seconds = Some(n);
            }
        }
        flush(&mut top, current_section, &mut current_table)?;
        Ok(top)
    }

    fn flush(
        top: &mut Expectations,
        section: Option<String>,
        table: &mut BTreeMap<String, String>,
    ) -> Result<(), String> {
        let Some(section) = section else {
            table.clear();
            return Ok(());
        };
        let raw_label = section
            .strip_prefix("tests.")
            .ok_or_else(|| format!("unknown section [{section}]"))?;
        // `tests."bundled/foo.py"` → `bundled/foo.py`
        let label = strip_quotes(raw_label.trim()).to_owned();
        let status = table
            .get("status")
            .ok_or_else(|| format!("[tests.{label}] missing status"))?;
        let status = match status.as_str() {
            "pass" => TestStatus::Pass,
            "fail" => TestStatus::Fail,
            "error" => TestStatus::Error,
            "skip" => TestStatus::Skip,
            "timeout" => TestStatus::Timeout,
            other => return Err(format!("[tests.{label}] bad status {other:?}")),
        };
        let reason = table.get("reason").cloned();
        top.tests.insert(label, ExpectedEntry { status, reason });
        table.clear();
        Ok(())
    }

    fn parse_kv(line: &str, lineno: usize) -> Result<(String, String), String> {
        let eq = line
            .find('=')
            .ok_or_else(|| format!("line {}: no `=` in {line:?}", lineno + 1))?;
        let key = line[..eq].trim().to_owned();
        let val = line[eq + 1..].trim();
        let val = strip_quotes(val);
        Ok((key, val.to_owned()))
    }

    fn strip_quotes(s: &str) -> &str {
        if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
            || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
        {
            &s[1..s.len() - 1]
        } else {
            s
        }
    }

    fn strip_comment(line: &str) -> &str {
        if let Some(idx) = line.find('#') {
            // Naive — assumes `#` never appears inside quoted strings,
            // which holds for our expectations file.
            &line[..idx]
        } else {
            line
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_minimal_expectations() {
            let body = "\
                timeout_seconds = 5\n\
                \n\
                [tests.\"bundled/test_basic.py\"]\n\
                status = \"pass\"\n\
                \n\
                [tests.\"cpython/Lib/test/test_grammar.py\"]\n\
                status = \"fail\"\n\
                reason = \"top-level await unsupported\"\n\
            ";
            let exp = parse(body).unwrap();
            assert_eq!(exp.timeout_seconds, Some(5));
            assert_eq!(exp.get("bundled/test_basic.py"), Some(TestStatus::Pass));
            assert_eq!(
                exp.get("cpython/Lib/test/test_grammar.py"),
                Some(TestStatus::Fail)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_counts_each_status() {
        let r = vec![
            TestReport {
                label: "a".into(),
                status: TestStatus::Pass,
                duration_ms: Some(1),
                detail: None,
                expected: Some(TestStatus::Pass),
            },
            TestReport {
                label: "b".into(),
                status: TestStatus::Fail,
                duration_ms: Some(2),
                detail: None,
                expected: Some(TestStatus::Pass),
            },
            TestReport {
                label: "c".into(),
                status: TestStatus::Skip,
                duration_ms: None,
                detail: None,
                expected: Some(TestStatus::Skip),
            },
        ];
        let s = RegrtestSummary::from_reports(&r);
        assert_eq!(s.total, 3);
        assert_eq!(s.pass, 1);
        assert_eq!(s.fail, 1);
        assert_eq!(s.skip, 1);
        assert_eq!(s.unexpected, 1);
    }

    #[test]
    fn missing_expectations_default_to_pass() {
        let r = TestReport {
            label: "new".into(),
            status: TestStatus::Pass,
            duration_ms: Some(0),
            detail: None,
            expected: None,
        };
        assert!(r.matches_expectation());
    }

    #[test]
    fn missing_expectations_flag_failures() {
        let r = TestReport {
            label: "new".into(),
            status: TestStatus::Fail,
            duration_ms: Some(0),
            detail: None,
            expected: None,
        };
        assert!(!r.matches_expectation());
    }
}
