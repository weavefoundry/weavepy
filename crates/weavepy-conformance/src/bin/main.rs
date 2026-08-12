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

        /// Explicit CPython `Lib/test/` directory. Overrides the
        /// `vendor/cpython/Lib/test/` (then `vendor/cpython-tests/`)
        /// auto-discovery — this is how CI points the harness at a real
        /// CPython 3.13 checkout.
        #[arg(long, value_name = "DIR")]
        cpython_dir: Option<PathBuf>,

        /// Schedule *every* `test_*.py` under the CPython directory
        /// (still graded against expectations). Off by default so a run
        /// stays restricted to the curated allowlist + expectations keys.
        #[arg(long = "all-cpython", action = clap::ArgAction::SetTrue)]
        all_cpython: bool,

        /// How to execute each test. `in-process` is fastest but a hard
        /// crash (stack overflow, abort) takes the runner down with it;
        /// `subprocess` isolates every test behind a SIGKILL wall timer.
        #[arg(long, value_enum, default_value_t = Mode::InProcess)]
        mode: Mode,

        /// Worker threads. `1` runs serially; higher values fan tests out
        /// across a thread pool (pairs naturally with `--mode subprocess`).
        #[arg(long, value_name = "N", default_value_t = 1)]
        jobs: usize,

        /// Path to the `weavepy` binary used by `--mode subprocess`. When
        /// omitted, the runner looks for a `weavepy` sibling of this
        /// executable, then `<workspace>/target/release/weavepy`.
        #[arg(long, value_name = "BIN")]
        weavepy: Option<PathBuf>,

        /// Print each test's verdict to stderr as it finishes — handy for
        /// watching a long subprocess sweep make progress.
        #[arg(long, action = clap::ArgAction::SetTrue)]
        stream: bool,
    },

    /// Validate WeavePy against real PyPI packages: per manifest row,
    /// create a scratch venv, `pip install` the requirement set, run the
    /// smoke probe, and grade against the ecosystem baseline (RFC 0055).
    Ecosystem {
        /// Path to the package matrix. Defaults to
        /// `<workspace>/tests/ecosystem/manifest.toml`.
        #[arg(long, value_name = "FILE")]
        manifest: Option<PathBuf>,

        /// Path to the baseline. Defaults to
        /// `<workspace>/tests/ecosystem/expectations.toml`.
        #[arg(long, value_name = "FILE")]
        expectations: Option<PathBuf>,

        /// Only run rows whose name contains `<FILTER>` as a substring.
        #[arg(long, value_name = "FILTER")]
        filter: Option<String>,

        /// Per-row wall budget (venv + install + probe), in seconds.
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,

        /// Offline mode: install from this wheel-cache directory
        /// (`pip install --no-index --find-links <DIR>`); populate it with
        /// `tools/ecosystem_fetch.py`. Without it installs hit PyPI.
        #[arg(long, value_name = "DIR")]
        wheels: Option<PathBuf>,

        /// Path to the `weavepy` binary the scratch venvs are built from.
        /// Defaults to a `weavepy` sibling of this executable, then
        /// `<workspace>/target/release/weavepy`.
        #[arg(long, value_name = "BIN")]
        weavepy: Option<PathBuf>,

        /// Also run each row's upstream test suite (the RFC 0062 WS4
        /// self-test tier) after its probe passes, graded against the
        /// baseline's `selftest_status` rows. Off by default so the
        /// quick lane stays quick; CI passes it.
        #[arg(long, action = clap::ArgAction::SetTrue)]
        selftests: bool,

        /// Keep the scratch venvs around for post-mortem.
        #[arg(long, action = clap::ArgAction::SetTrue)]
        keep_venvs: bool,

        /// Exit non-zero if any row diverged from its expected status.
        /// Defaults to true; pass `--no-check` to grade without gating.
        #[arg(long = "check", action = clap::ArgAction::SetTrue, default_value_t = true)]
        check: bool,

        /// Disable the strict-grading exit code.
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

/// CLI spelling of [`regrtest::ExecutionMode`].
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Mode {
    InProcess,
    Subprocess,
}

impl From<Mode> for regrtest::ExecutionMode {
    fn from(m: Mode) -> Self {
        match m {
            Mode::InProcess => regrtest::ExecutionMode::InProcess,
            Mode::Subprocess => regrtest::ExecutionMode::Subprocess,
        }
    }
}

fn main() -> ExitCode {
    run_on_large_stack(run_real_main)
}

fn run_real_main() -> ExitCode {
    match real_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("weavepy-conformance: {err:#}");
            ExitCode::from(1)
        }
    }
}

/// Run the harness on a generously-sized stack, mirroring `weavepy-cli`'s
/// `run_on_large_stack`. `--mode in-process` executes each `test_*.py`
/// inside *this* process, so without a large reserve a deep-but-bounded
/// Python recursion (e.g. a `RecursionError` guard test, or the recursive
/// drop of its traceback chain) overflows the fixed 8 MiB OS main-thread
/// stack before the interpreter's own recursion guard can fire. The 1 GiB
/// reserve is committed lazily by the OS, so it costs address space, not
/// resident memory.
fn run_on_large_stack(entry: fn() -> ExitCode) -> ExitCode {
    const STACK_BYTES: usize = 1024 * 1024 * 1024; // 1 GiB reserve
    match std::thread::Builder::new()
        .name("weavepy-conformance-main".to_owned())
        .stack_size(STACK_BYTES)
        .spawn(entry)
    {
        Ok(handle) => handle.join().unwrap_or(ExitCode::FAILURE),
        Err(_) => entry(),
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
            cpython_dir,
            all_cpython,
            mode,
            jobs,
            weavepy,
            stream,
        } => {
            let strict = check && !no_check;
            cmd_regrtest(
                &workspace,
                &report_dir,
                RegrtestArgs {
                    expectations: expectations.as_deref(),
                    timeout_override: timeout,
                    filter: filter.as_deref(),
                    strict,
                    cpython_dir,
                    all_cpython,
                    mode: mode.into(),
                    jobs,
                    weavepy,
                    stream,
                },
            )
        }
        Cmd::Ecosystem {
            manifest,
            expectations,
            filter,
            timeout,
            wheels,
            weavepy,
            selftests,
            keep_venvs,
            check,
            no_check,
        } => cmd_ecosystem(
            &workspace,
            &report_dir,
            EcosystemArgs {
                manifest,
                expectations,
                filter,
                timeout,
                wheels,
                weavepy,
                selftests,
                keep_venvs,
                strict: check && !no_check,
            },
        ),
    }
}

/// Bundle of [`cmd_ecosystem`] inputs.
struct EcosystemArgs {
    manifest: Option<PathBuf>,
    expectations: Option<PathBuf>,
    filter: Option<String>,
    timeout: Option<u64>,
    wheels: Option<PathBuf>,
    weavepy: Option<PathBuf>,
    selftests: bool,
    keep_venvs: bool,
    strict: bool,
}

fn cmd_ecosystem(workspace: &Path, report_dir: &Path, args: EcosystemArgs) -> Result<()> {
    use weavepy_conformance::ecosystem;

    let manifest_path = args
        .manifest
        .unwrap_or_else(|| workspace.join("tests/ecosystem/manifest.toml"));
    let mut manifest = ecosystem::Manifest::load(&manifest_path)?;
    if let Some(needle) = &args.filter {
        manifest.rows.retain(|r| r.name.contains(needle.as_str()));
    }
    if manifest.rows.is_empty() {
        anyhow::bail!(
            "no ecosystem rows scheduled from {} (filter={:?})",
            manifest_path.display(),
            args.filter,
        );
    }

    let exp_path = args
        .expectations
        .unwrap_or_else(|| workspace.join("tests/ecosystem/expectations.toml"));
    let expectations = ecosystem::EcosystemExpectations::load(&exp_path)?;

    let weavepy =
        resolve_weavepy_binary(args.weavepy, regrtest::ExecutionMode::Subprocess, workspace)
            .ok_or_else(|| {
                anyhow::anyhow!(
            "could not locate a weavepy binary — build target/release/weavepy or pass --weavepy"
        )
            })?;

    let scratch_dir = report_dir.join("ecosystem-scratch");
    std::fs::create_dir_all(&scratch_dir)
        .with_context(|| format!("failed to create {}", scratch_dir.display()))?;

    // Local-TLS probes (requests over HTTPS) read the bundled cert pair.
    let mut probe_env = Vec::new();
    let certs = workspace.join("tests/regrtest/certs");
    if certs.is_dir() {
        probe_env.push((
            "WEAVEPY_ECOSYSTEM_CERTS".to_owned(),
            certs.display().to_string(),
        ));
    }

    let opts = ecosystem::EcosystemOptions {
        weavepy,
        wheels: args.wheels,
        timeout: Duration::from_secs(args.timeout.unwrap_or(ecosystem::DEFAULT_ROW_TIMEOUT_SECS)),
        selftests: args.selftests,
        keep_venvs: args.keep_venvs,
        scratch_dir,
        probe_env,
    };

    let reports = ecosystem::run_all(&manifest, &expectations, &opts);
    let summary = ecosystem::EcosystemSummary::from_reports(&reports);
    print!("{}", ecosystem::report_to_markdown(&reports));

    std::fs::create_dir_all(report_dir)
        .with_context(|| format!("failed to create {}", report_dir.display()))?;
    let md_path = report_dir.join("ecosystem.md");
    std::fs::write(&md_path, ecosystem::report_to_markdown(&reports))
        .with_context(|| format!("failed to write {}", md_path.display()))?;
    let json_path = report_dir.join("ecosystem.json");
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
        "wrote ecosystem.md and ecosystem.json to {}",
        report_dir.display()
    );

    // RFC 0063 WS7: on a host OS outside the baseline's `measured_os`
    // stamp the gate is advisory — the helper prints the NOTE line and
    // returns false, so the run exits 0 with the reports still written.
    if args.strict && ecosystem::strict_gate_blocks(&expectations, &summary) {
        anyhow::bail!(
            "{} ecosystem regression(s) — see {}",
            summary.unexpected,
            md_path.display()
        );
    }
    Ok(())
}

/// Bundle of [`cmd_regrtest`] inputs — keeps the call site readable now
/// that the subcommand wires through discovery, execution-mode, and
/// parallelism knobs.
struct RegrtestArgs<'a> {
    expectations: Option<&'a Path>,
    timeout_override: Option<u64>,
    filter: Option<&'a str>,
    strict: bool,
    cpython_dir: Option<PathBuf>,
    all_cpython: bool,
    mode: regrtest::ExecutionMode,
    jobs: usize,
    weavepy: Option<PathBuf>,
    stream: bool,
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

fn cmd_regrtest(workspace: &Path, report_dir: &Path, args: RegrtestArgs<'_>) -> Result<()> {
    let default_expectations = workspace.join("tests/regrtest/expectations.toml");
    let exp_path = args.expectations.unwrap_or(&default_expectations);
    let expectations = regrtest::Expectations::load(exp_path)?;
    let timeout_secs = args
        .timeout_override
        .or(expectations.timeout_seconds)
        .unwrap_or(regrtest::DEFAULT_TIMEOUT_SECS);
    let timeout = Duration::from_secs(timeout_secs);

    // Honour an explicit `--cpython-dir` (canonicalised so the runner's
    // `is_dir()` probe and the spawned child agree on the path) and the
    // `--all-cpython` opt-in for full-directory scheduling.
    let cpython_dir = match args.cpython_dir {
        Some(dir) => Some(
            dir.canonicalize()
                .with_context(|| format!("--cpython-dir does not exist: {}", dir.display()))?,
        ),
        None => None,
    };
    let discovery = regrtest::DiscoveryOptions {
        cpython_dir,
        include_all_cpython: args.all_cpython,
    };

    let mut files = regrtest::discover_with(workspace, &discovery, Some(&expectations));
    if let Some(needle) = args.filter {
        files.retain(|f| f.label.contains(needle));
    }
    if files.is_empty() {
        eprintln!(
            "no regrtest files found under {} (filter={:?})",
            workspace.join("tests/regrtest").display(),
            args.filter,
        );
    }

    let runner = regrtest::RunnerOptions {
        timeout,
        mode: args.mode,
        workers: args.jobs.max(1),
        weavepy_binary: resolve_weavepy_binary(args.weavepy, args.mode, workspace),
        stream_results: args.stream,
    };
    let reports = regrtest::run_all_with(&files, &expectations, &runner);
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

    // RFC 0063 WS7: on a host OS outside the baseline's `measured_os`
    // stamp the gate is advisory — the helper prints the NOTE line and
    // returns false, so the run exits 0 with the reports still written.
    if args.strict && regrtest::strict_gate_blocks(&expectations, &summary) {
        anyhow::bail!(
            "{} regrtest regression(s) — see {}",
            summary.unexpected,
            md_path.display()
        );
    }
    Ok(())
}

/// Decide which `weavepy` binary the subprocess runner should spawn.
///
/// In-process runs never shell out, so we return `None` and skip the
/// filesystem probing. For subprocess runs we honour an explicit
/// `--weavepy`, then a `weavepy` sibling of *this* executable (the usual
/// `target/<profile>/` layout puts both binaries together), then the
/// canonical `<workspace>/target/release/weavepy`. Returning `None`
/// lets the runner fall back to `current_exe()`, which at least fails
/// loudly rather than silently mis-running.
fn resolve_weavepy_binary(
    explicit: Option<PathBuf>,
    mode: regrtest::ExecutionMode,
    workspace: &Path,
) -> Option<PathBuf> {
    if mode != regrtest::ExecutionMode::Subprocess {
        return None;
    }
    if let Some(p) = explicit {
        return Some(p);
    }
    let exe_name = if cfg!(windows) {
        "weavepy.exe"
    } else {
        "weavepy"
    };
    if let Ok(cur) = std::env::current_exe() {
        if let Some(sibling) = cur.parent().map(|d| d.join(exe_name)) {
            if sibling.is_file() {
                return Some(sibling);
            }
        }
    }
    let release = workspace.join("target").join("release").join(exe_name);
    release.is_file().then_some(release)
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
