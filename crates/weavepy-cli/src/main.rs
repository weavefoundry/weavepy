//! The `weavepy` command-line interpreter.
//!
//! The intent is for `weavepy` to be argv-compatible with `python` so that it
//! can serve as a drop-in replacement in CI scripts and shebang lines. This
//! file currently implements only the smallest useful subset; the rest is
//! tracked in the project README and architecture docs.

use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
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
/// - `weavepy -m pkg.mod`       -- run a module as `__main__`
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

    /// Run library module `<MODULE>` as `__main__` (mirrors `python -m`).
    #[arg(short = 'm', value_name = "MODULE")]
    module: Option<String>,

    /// Path to a Python script to execute. Use `-` to read from stdin.
    script: Option<PathBuf>,

    /// Trailing arguments forwarded to the script as `sys.argv[1:]`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

/// Sentinel string used by [`run_source`] to signal that it has already
/// emitted a CPython-style traceback to stderr and `main` should exit
/// without printing the generic `weavepy: ...` envelope.
const DIAGNOSTIC_SENTINEL: &str = "exited with diagnostic";

fn main() -> ExitCode {
    if let Err(err) = real_main() {
        if err.to_string() != DIAGNOSTIC_SENTINEL {
            let mut stderr = io::stderr().lock();
            let _ = writeln!(stderr, "weavepy: {err:#}");
        }
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
        // Per CPython: `python -c '...' arg1 arg2` sets argv[0] to "-c"
        // and forwards every trailing argument as argv[1:]. Clap's
        // positional parser greedily consumed the first token after
        // `-c "..."` as `script`; recover it here.
        let mut argv = vec!["-c".to_owned()];
        if let Some(p) = cli.script.as_ref() {
            argv.push(p.to_string_lossy().into_owned());
        }
        argv.extend(cli.args.iter().cloned());
        let opts = weavepy::RunOptions::new("<string>")
            .with_argv(argv)
            .with_script_dir(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        return run_source_with_options(&source, &opts);
    }

    if let Some(module) = cli.module {
        let mut extra = Vec::new();
        if let Some(p) = cli.script.as_ref() {
            extra.push(p.to_string_lossy().into_owned());
        }
        extra.extend(cli.args.iter().cloned());
        return run_module(&module, extra);
    }

    match cli.script.as_deref() {
        Some(path) if path.as_os_str() == "-" => run_stdin(cli.args),
        Some(path) => run_path(path, cli.args),
        None => run_repl(),
    }
}

/// `weavepy -m pkg.mod` — resolve `pkg.mod` via `sys.path` and run
/// it under `__name__ = "__main__"`. The discovered source file
/// also becomes `__file__`. Errors out if the module isn't a
/// straight `.py` file (CPython falls back to `__main__.py` inside
/// a package; we don't yet).
fn run_module(name: &str, args: Vec<String>) -> Result<()> {
    let mut argv = vec![name.to_owned()];
    argv.extend(args);
    let mut search: Vec<PathBuf> =
        vec![std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))];
    let rel: PathBuf = name.split('.').collect();
    let (source_path, _is_package) = search
        .drain(..)
        .find_map(|dir| {
            let m = dir.join(&rel).with_extension("py");
            if m.is_file() {
                Some((m, false))
            } else {
                let init = dir.join(&rel).join("__init__.py");
                init.is_file().then_some((init, true))
            }
        })
        .with_context(|| format!("No module named '{name}'"))?;
    let source = fs::read_to_string(&source_path)
        .with_context(|| format!("failed to read {}", source_path.display()))?;
    let filename = source_path.display().to_string();
    let opts = weavepy::RunOptions::new(filename.clone())
        .with_argv(argv)
        .with_script_dir(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    run_source_with_options(&source, &opts)
}

fn run_path(path: &Path, extra: Vec<String>) -> Result<()> {
    let source =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let filename = path.display().to_string();
    let mut argv = vec![filename.clone()];
    argv.extend(extra);
    let script_dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let opts = weavepy::RunOptions::new(filename.clone())
        .with_argv(argv)
        .with_script_dir(script_dir);
    run_source_with_options(&source, &opts)
}

fn run_stdin(extra: Vec<String>) -> Result<()> {
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .context("failed to read stdin")?;
    let mut argv = vec!["-".to_owned()];
    argv.extend(extra);
    let opts = weavepy::RunOptions::new("<stdin>")
        .with_argv(argv)
        .with_script_dir(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    run_source_with_options(&buf, &opts)
}

/// Execute `source` and, on failure, surface a CPython-style traceback
/// on stderr and exit with status 1 (matching `python` behaviour).
fn run_source_with_options(source: &str, opts: &weavepy::RunOptions) -> Result<()> {
    match weavepy::run_source_with_options(source, opts) {
        Ok(()) => Ok(()),
        Err(err) => {
            let mut stderr = io::stderr().lock();
            let diag = err.format(source, &opts.filename);
            let _ = stderr.write_all(diag.as_bytes());
            anyhow::bail!(DIAGNOSTIC_SENTINEL);
        }
    }
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
