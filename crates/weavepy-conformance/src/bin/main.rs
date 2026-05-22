//! `weavepy-conformance`: grade WeavePy against CPython.
//!
//! The binary has three modes:
//!
//! - `run`     — grade every corpus file across every phase, write a report.
//! - `diff`    — same, restricted to a single phase. Handy in tight dev loops.
//! - `regrtest`— placeholder for the eventual end-to-end CPython test runner.
//!
//! In all cases the host's `python3` (overridable with `$WEAVEPY_PYTHON`) is
//! used as the oracle. The harness exits 0 even when matches are 0% — a
//! report is only an "error" when the oracle infrastructure itself failed.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

use weavepy_conformance::{
    corpus, oracle, oracle_python, regrtest, report, runner, CPYTHON_TARGET_VERSION,
};

#[derive(Debug, Parser)]
#[command(
    name = "weavepy-conformance",
    bin_name = "weavepy-conformance",
    about = "Compare WeavePy against CPython as an oracle, per pipeline phase.",
    version
)]
struct Cli {
    /// Path to the workspace root. Defaults to the repo containing this
    /// binary, walked up from the current directory.
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,

    /// Where to write `report.md` / `report.json`. Defaults to
    /// `<workspace>/target/conformance`.
    #[arg(long, value_name = "DIR")]
    report_dir: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Grade every corpus file across every phase.
    Run,

    /// Grade only one phase. Useful when iterating on a single pipeline stage.
    Diff {
        /// Which phase to diff.
        phase: Phase,
    },

    /// Run individual `test_*.py` files end-to-end through WeavePy and
    /// grade against the checked-in expectations baseline.
    Regrtest {
        /// Path to the `expectations.toml` baseline. Defaults to
        /// `<workspace>/tests/regrtest/expectations.toml`.
        #[arg(long, value_name = "FILE")]
        expectations: Option<PathBuf>,

        /// Per-test wall budget, in seconds.
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,

        /// Only run tests whose label contains `<FILTER>` as a substring.
        #[arg(long, value_name = "FILTER")]
        filter: Option<String>,

        /// Exit non-zero if any test diverged from its expected status.
        /// Defaults to true; pass `--no-check` to grade without gating.
        #[arg(long = "check", action = clap::ArgAction::SetTrue, default_value_t = true)]
        check: bool,

        /// Disable the strict-grading exit code (useful when sweeping
        /// expectations to refresh the baseline).
        #[arg(long = "no-check", action = clap::ArgAction::SetTrue, conflicts_with = "check")]
        no_check: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Phase {
    Tokens,
    Ast,
    Dis,
}

fn main() -> ExitCode {
    match real_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("weavepy-conformance: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let workspace = resolve_workspace(cli.workspace.as_deref())?;
    let report_dir = cli
        .report_dir
        .unwrap_or_else(|| workspace.join("target").join("conformance"));

    match cli.cmd {
        Cmd::Run => cmd_run(&workspace, &report_dir),
        Cmd::Diff { phase } => cmd_diff(&workspace, &report_dir, phase),
        Cmd::Regrtest {
            expectations,
            timeout,
            filter,
            check,
            no_check,
        } => {
            let strict = check && !no_check;
            cmd_regrtest(
                &workspace,
                &report_dir,
                expectations.as_deref(),
                timeout,
                filter.as_deref(),
                strict,
            )
        }
    }
}

fn cmd_run(workspace: &Path, report_dir: &Path) -> Result<()> {
    let python = oracle_python();
    let banner = oracle::ensure_available(&python)?;
    let files = corpus::discover(workspace);

    if files.is_empty() {
        eprintln!(
            "no corpus files found under {} or vendor/cpython/Lib/test",
            workspace.join("conformance/corpus").display(),
        );
    }

    let reports: Vec<_> = files.iter().map(|f| runner::run_file(&python, f)).collect();
    let report = report::Report::new(CPYTHON_TARGET_VERSION.to_owned(), banner, reports);

    print!("{}", report.to_markdown());
    report
        .write_to(report_dir)
        .with_context(|| format!("failed to write report to {}", report_dir.display()))?;
    eprintln!(
        "wrote report.md and report.json to {}",
        report_dir.display()
    );
    Ok(())
}

fn cmd_diff(workspace: &Path, report_dir: &Path, phase: Phase) -> Result<()> {
    // We reuse the full runner for simplicity — it's cheap — but render
    // only the requested phase in the table.
    let python = oracle_python();
    let banner = oracle::ensure_available(&python)?;
    let files = corpus::discover(workspace);

    let all: Vec<_> = files.iter().map(|f| runner::run_file(&python, f)).collect();

    let summary = match phase {
        Phase::Tokens => runner::Summary::from_phase(all.iter().map(|r| &r.tokens)),
        Phase::Ast => runner::Summary::from_phase(all.iter().map(|r| &r.ast)),
        Phase::Dis => runner::Summary::from_phase(all.iter().map(|r| &r.dis)),
    };

    let phase_name = match phase {
        Phase::Tokens => "tokens",
        Phase::Ast => "ast",
        Phase::Dis => "dis",
    };

    println!("# WeavePy ↔ CPython {CPYTHON_TARGET_VERSION} — {phase_name}");
    println!("Oracle: {banner}");
    println!(
        "match {} / mismatch {} / weavepy-error {} / oracle-error {} / skipped {} ({:.1}%)",
        summary.match_,
        summary.mismatch,
        summary.weavepy_error,
        summary.oracle_error,
        summary.skipped,
        summary.match_rate() * 100.0,
    );

    // Even when --phase is requested, persist the full report. It's how
    // CI artifacts and PR comments stay consistent across local and CI
    // runs.
    let full = report::Report::new(CPYTHON_TARGET_VERSION.to_owned(), banner, all);
    full.write_to(report_dir)
        .with_context(|| format!("failed to write report to {}", report_dir.display()))?;
    Ok(())
}

fn cmd_regrtest(
    workspace: &Path,
    report_dir: &Path,
    expectations_path: Option<&Path>,
    timeout_override: Option<u64>,
    filter: Option<&str>,
    strict: bool,
) -> Result<()> {
    let default_expectations = workspace.join("tests/regrtest/expectations.toml");
    let exp_path = expectations_path.unwrap_or(&default_expectations);
    let expectations = regrtest::Expectations::load(exp_path)?;
    let timeout_secs = timeout_override
        .or(expectations.timeout_seconds)
        .unwrap_or(regrtest::DEFAULT_TIMEOUT_SECS);
    let timeout = Duration::from_secs(timeout_secs);

    let mut files = regrtest::discover(workspace);
    if let Some(needle) = filter {
        files.retain(|f| f.label.contains(needle));
    }
    if files.is_empty() {
        eprintln!(
            "no regrtest files found under {} (filter={:?})",
            workspace.join("tests/regrtest").display(),
            filter,
        );
    }

    let reports = regrtest::run_all(&files, &expectations, timeout);
    let summary = regrtest::RegrtestSummary::from_reports(&reports);
    print!("{}", regrtest::report_to_markdown(&reports));

    if let Err(e) = std::fs::create_dir_all(report_dir) {
        anyhow::bail!(
            "failed to create regrtest report dir {}: {e}",
            report_dir.display()
        );
    }
    let md_path = report_dir.join("regrtest.md");
    std::fs::write(&md_path, regrtest::report_to_markdown(&reports))
        .with_context(|| format!("failed to write {}", md_path.display()))?;
    let json_path = report_dir.join("regrtest.json");
    let json = serde_json::json!({
        "summary": summary,
        "reports": reports,
    });
    std::fs::write(
        &json_path,
        serde_json::to_string_pretty(&json).unwrap_or_default(),
    )
    .with_context(|| format!("failed to write {}", json_path.display()))?;
    eprintln!(
        "wrote regrtest.md and regrtest.json to {}",
        report_dir.display()
    );

    if strict && summary.unexpected > 0 {
        anyhow::bail!(
            "{} regrtest regression(s) — see {}",
            summary.unexpected,
            md_path.display()
        );
    }
    Ok(())
}

/// Locate the workspace root.
///
/// If `--workspace` was passed, validate and return it. Otherwise walk up
/// from the current directory looking for a `Cargo.toml` whose `[workspace]`
/// table is present. We bail rather than guess if neither succeeds.
fn resolve_workspace(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        let p = p
            .canonicalize()
            .with_context(|| format!("--workspace path does not exist: {}", p.display()))?;
        return Ok(p);
    }
    let mut cur = std::env::current_dir().context("failed to read current dir")?;
    loop {
        let manifest = cur.join("Cargo.toml");
        if manifest.is_file() {
            let text = std::fs::read_to_string(&manifest).unwrap_or_default();
            if text.contains("[workspace]") {
                return Ok(cur);
            }
        }
        if !cur.pop() {
            anyhow::bail!(
                "could not find a [workspace] Cargo.toml above the current directory. \
                 pass --workspace explicitly."
            );
        }
    }
}
