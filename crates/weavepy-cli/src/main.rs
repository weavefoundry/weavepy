//! The `weavepy` command-line interpreter.
//!
//! The intent is for `weavepy` to be argv-compatible with `python` so that it
//! can serve as a drop-in replacement in CI scripts and shebang lines. This
//! file currently implements only the smallest useful subset; the rest is
//! tracked in the project README and architecture docs.

use std::{
    fs,
    io::{self, Read, Write},
    path::PathBuf,
    process::ExitCode,
};

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Argv-compatible entry to the WeavePy interpreter.
///
/// Supported today:
/// - `weavepy script.py`        -- execute a script
/// - `weavepy -c "<source>"`    -- execute an inline source string
/// - `weavepy -V` / `--version` -- print the WeavePy version
/// - `weavepy` (no args)        -- enter the REPL (placeholder)
#[derive(Debug, Parser)]
#[command(
    name = "weavepy",
    bin_name = "weavepy",
    version = VERSION,
    about = "WeavePy: a high-performance, CPython-compatible Python interpreter written in Rust.",
    disable_version_flag = true,
)]
struct Cli {
    /// Print the WeavePy version and exit (mirrors `python -V`).
    #[arg(short = 'V', long = "version", action = clap::ArgAction::SetTrue)]
    version: bool,

    /// Execute the given source string and exit (mirrors `python -c`).
    #[arg(short = 'c', value_name = "SOURCE")]
    command: Option<String>,

    /// Path to a Python script to execute. Use `-` to read from stdin.
    script: Option<PathBuf>,

    /// Trailing arguments forwarded to the script as `sys.argv[1:]`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

fn main() -> ExitCode {
    if let Err(err) = real_main() {
        let mut stderr = io::stderr().lock();
        let _ = writeln!(stderr, "weavepy: {err:#}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn real_main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();

    if cli.version {
        println!("WeavePy {VERSION}");
        return Ok(());
    }

    if let Some(source) = cli.command {
        return weavepy::run_source(&source).map_err(Into::into);
    }

    match cli.script.as_deref() {
        Some(path) if path.as_os_str() == "-" => run_stdin(),
        Some(path) => run_path(path),
        None => run_repl(),
    }
}

fn run_path(path: &std::path::Path) -> Result<()> {
    let source =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    weavepy::run_source(&source).map_err(Into::into)
}

fn run_stdin() -> Result<()> {
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .context("failed to read stdin")?;
    weavepy::run_source(&buf).map_err(Into::into)
}

fn run_repl() -> Result<()> {
    let mut stdout = io::stdout().lock();
    writeln!(
        stdout,
        "WeavePy {VERSION} (REPL not yet implemented; pass a script or use -c)"
    )?;
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_env("WEAVEPY_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}
