//! `weavepy regrtest` — drive the bundled regression harness.
//!
//! This is the CLI-side wrapper around [`weavepy_conformance::regrtest`].
//! It exists in the CLI (rather than only in `weavepy-conformance`) so
//! `python(1)` users have a single binary to reach for:
//!
//! ```text
//! weavepy regrtest [--workspace DIR] [--filter TEXT] [--no-check]
//!                  [--mode in-process|subprocess] [--workers N]
//!                  [--cpython-dir DIR] [--include-all-cpython]
//!                  [--stream]
//! ```
//!
//! When the embedder needs the lower-level reports as JSON for CI, the
//! `weavepy-conformance regrtest` subcommand is still the right tool —
//! it shares 100% of the underlying code path.
//!
//! Exit code:
//! - `0` — every test matched its expectation
//! - `1` — at least one unexpected status (regression) or a hard error
//!
//! Discovery rules mirror the conformance crate: bundled tests live in
//! `<workspace>/tests/regrtest/`; CPython tests come from
//! `<workspace>/vendor/cpython/Lib/test/` (or
//! `<workspace>/vendor/cpython-tests/`) when present.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{ArgAction, Parser, ValueEnum};

use weavepy_conformance::{
    discover_regrtest_with, regrtest_to_markdown, run_all_with, DiscoveryOptions, ExecutionMode,
    Expectations, RegrtestSummary, RunnerOptions, DEFAULT_TIMEOUT_SECS,
};

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ModeArg {
    InProcess,
    Subprocess,
}

impl From<ModeArg> for ExecutionMode {
    fn from(m: ModeArg) -> Self {
        match m {
            ModeArg::InProcess => ExecutionMode::InProcess,
            ModeArg::Subprocess => ExecutionMode::Subprocess,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "weavepy regrtest",
    bin_name = "weavepy regrtest",
    about = "Run the WeavePy regression test harness and grade against expectations.toml.",
    disable_help_subcommand = true,
    arg_required_else_help = false
)]
struct Cli {
    /// Workspace root (defaults to the nearest `[workspace]` ancestor
    /// of the current directory).
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,

    /// Path to expectations baseline. Defaults to
    /// `<workspace>/tests/regrtest/expectations.toml`.
    #[arg(long, value_name = "FILE")]
    expectations: Option<PathBuf>,

    /// Where to write `regrtest.md` / `regrtest.json`. Defaults to
    /// `<workspace>/target/regrtest`.
    #[arg(long, value_name = "DIR")]
    report_dir: Option<PathBuf>,

    /// Per-test wall budget, in seconds.
    #[arg(long, value_name = "SECS")]
    timeout: Option<u64>,

    /// Only run tests whose label contains the substring.
    #[arg(long, value_name = "FILTER")]
    filter: Option<String>,

    /// Don't gate on the expectations file. Useful for refreshing the
    /// baseline (`weavepy regrtest --no-check > /tmp/out`).
    #[arg(long = "no-check", action = ArgAction::SetTrue)]
    no_check: bool,

    /// Suppress per-test rows from stdout; print only the summary line.
    #[arg(short = 'q', long = "quiet", action = ArgAction::SetTrue)]
    quiet: bool,

    /// How to execute each test. `subprocess` is mandatory if you want
    /// hangs and crashes to be containable.
    #[arg(long, value_enum, default_value_t = ModeArg::InProcess)]
    mode: ModeArg,

    /// Number of parallel test workers. `0` picks a sensible default
    /// based on `available_parallelism`.
    #[arg(long, short = 'j', value_name = "N", default_value_t = 1)]
    workers: usize,

    /// Path to the WeavePy binary used for subprocess mode. Defaults to
    /// the currently running executable.
    #[arg(long, value_name = "PATH")]
    weavepy_binary: Option<PathBuf>,

    /// Explicit CPython `Lib/test/` directory. Overrides auto-discovery.
    #[arg(long, value_name = "DIR")]
    cpython_dir: Option<PathBuf>,

    /// Include every `test_*.py` in the CPython test directory (not
    /// just the curated allowlist + the expectations.toml entries).
    #[arg(long = "include-all-cpython", action = ArgAction::SetTrue)]
    include_all_cpython: bool,

    /// Stream per-test results to stderr as they finish. Pairs well
    /// with `--workers > 1` so you can see live progress.
    #[arg(long, action = ArgAction::SetTrue)]
    stream: bool,
}

pub(crate) fn run(argv: Vec<String>) -> Result<ExitCode> {
    let cli = Cli::parse_from(argv);
    let workspace = resolve_workspace(cli.workspace.as_deref())?;
    let report_dir = cli
        .report_dir
        .unwrap_or_else(|| workspace.join("target/regrtest"));
    let default_expectations = workspace.join("tests/regrtest/expectations.toml");
    let exp_path = cli.expectations.as_deref().unwrap_or(&default_expectations);
    let expectations = Expectations::load(exp_path)?;
    let timeout_secs = cli
        .timeout
        .or(expectations.timeout_seconds)
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    let timeout = Duration::from_secs(timeout_secs);

    let discovery = DiscoveryOptions {
        cpython_dir: cli.cpython_dir.clone(),
        include_all_cpython: cli.include_all_cpython,
    };
    let mut files = discover_regrtest_with(&workspace, &discovery, Some(&expectations));
    if let Some(needle) = cli.filter.as_deref() {
        files.retain(|f| f.label.contains(needle));
    }
    if files.is_empty() {
        eprintln!(
            "no regrtest files found under {} (filter={:?})",
            workspace.join("tests/regrtest").display(),
            cli.filter,
        );
    }

    let workers = if cli.workers == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    } else {
        cli.workers
    };
    let runner_opts = RunnerOptions {
        timeout,
        mode: cli.mode.into(),
        workers,
        weavepy_binary: cli.weavepy_binary.clone(),
        stream_results: cli.stream,
    };
    let reports = run_all_with(&files, &expectations, &runner_opts);
    let summary = RegrtestSummary::from_reports(&reports);

    if cli.quiet {
        println!(
            "{} total — pass {} / fail {} / error {} / skip {} / timeout {} — unexpected {}",
            summary.total,
            summary.pass,
            summary.fail,
            summary.error,
            summary.skip,
            summary.timeout,
            summary.unexpected
        );
    } else {
        print!("{}", regrtest_to_markdown(&reports));
    }

    std::fs::create_dir_all(&report_dir)
        .with_context(|| format!("failed to create {}", report_dir.display()))?;
    let md_path = report_dir.join("regrtest.md");
    std::fs::write(&md_path, regrtest_to_markdown(&reports))
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

    if !cli.no_check && summary.unexpected > 0 {
        return Ok(ExitCode::from(1));
    }
    Ok(ExitCode::SUCCESS)
}

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
