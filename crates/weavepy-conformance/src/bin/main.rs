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

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

use weavepy_conformance::{corpus, oracle, oracle_python, report, runner, CPYTHON_TARGET_VERSION};

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

    /// (Placeholder.) Run individual CPython `test_*.py` files end-to-end
    /// through WeavePy. Wired up once the VM can execute Python.
    Regrtest,
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
        Cmd::Regrtest => cmd_regrtest(),
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

// Result<()> signature is preserved so this arm matches the others in
// the dispatch match; the body will gain fallible operations in Stage B.
#[allow(clippy::unnecessary_wraps)]
fn cmd_regrtest() -> Result<()> {
    eprintln!(
        "regrtest mode is not implemented yet: it requires a working interpreter.\n\
         Track the rollout in docs/CONFORMANCE.md (\"Stage B\")."
    );
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
