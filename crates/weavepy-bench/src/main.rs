//! `weavepy-bench` CLI entry point (RFC 0058 WS1).
//!
//! Subcommands:
//!
//! - `run` — runs all fixtures, prints a markdown report.
//! - `run --json` — emits the report as JSON to stdout.
//! - `run --update-baseline` — overwrites `baselines/bench.json`
//!   with the run's results (requires the CPython column so the
//!   baseline carries ratios).
//! - `gate` — runs the suite, compares WeavePy/CPython ratios (and
//!   the suite geomean) against the baseline, and exits non-zero on
//!   regressions beyond the threshold.
//!
//! For maximum portability we hand-roll arg parsing rather than
//! pull in `clap` — the tool has at most a handful of flags.

use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use weavepy_bench::fixtures::baseline_path;
use weavepy_bench::report::Report;
use weavepy_bench::runner::{run_suite, RunOpts};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("run");
    match cmd {
        "run" => match cmd_run(&args[2..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("weavepy-bench: {e}");
                ExitCode::FAILURE
            }
        },
        "gate" => match cmd_gate(&args[2..]) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(e) => {
                eprintln!("weavepy-bench: {e}");
                ExitCode::FAILURE
            }
        },
        "help" | "-h" | "--help" => {
            print_help();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("weavepy-bench: unknown command '{other}'");
            print_help();
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    eprintln!("weavepy-bench — RFC 0058 benchmark lane");
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("    weavepy-bench [run|gate|help] [flags]");
    eprintln!();
    eprintln!("COMMANDS:");
    eprintln!("    run    Run the suite and print a markdown report.");
    eprintln!("    gate   Run the suite and compare ratios against the baseline.");
    eprintln!("    help   Print this message.");
    eprintln!();
    eprintln!("COMMON FLAGS:");
    eprintln!("    --weavepy=PATH        weavepy binary under test (default: $WEAVEPY_BIN,");
    eprintln!("                          then a `weavepy` next to this executable).");
    eprintln!("    --python=PATH         Host CPython (default: python3.13, then python3).");
    eprintln!("    --no-cpython          Skip the host CPython column (absolute-only mode).");
    eprintln!("    --samples=N           Timing samples per fixture (default 5).");
    eprintln!("    --jit                 Add a WEAVEPY_JIT=1 column (reported, not gated;");
    eprintln!("                          the binary must be built with --features jit).");
    eprintln!();
    eprintln!("FLAGS for `run`:");
    eprintln!("    --json                Print report as JSON.");
    eprintln!("    --update-baseline     Overwrite baselines/bench.json.");
    eprintln!();
    eprintln!("FLAGS for `gate`:");
    eprintln!("    --pct=PCT             Regression threshold (default 10).");
}

fn parse_common(opts: &mut RunOpts, arg: &str) -> bool {
    match arg {
        "--no-cpython" => opts.include_cpython = false,
        "--jit" => opts.include_jit = true,
        x if x.starts_with("--samples=") => {
            opts.samples = x[10..].parse().unwrap_or(opts.samples);
        }
        x if x.starts_with("--python=") => {
            opts.python_path = Some(x[9..].to_owned());
        }
        x if x.starts_with("--weavepy=") => {
            opts.weavepy_path = Some(PathBuf::from(&x[10..]));
        }
        _ => return false,
    }
    true
}

fn cmd_run(args: &[String]) -> io::Result<()> {
    let mut opts = RunOpts::default();
    let mut emit_json = false;
    let mut update_baseline = false;
    for a in args {
        if parse_common(&mut opts, a) {
            continue;
        }
        match a.as_str() {
            "--json" => emit_json = true,
            "--update-baseline" => update_baseline = true,
            other => {
                return Err(io::Error::other(format!("unknown flag '{other}'")));
            }
        }
    }
    if update_baseline && !opts.include_cpython {
        return Err(io::Error::other(
            "--update-baseline needs the CPython column (drop --no-cpython): \
             the tracked baseline stores WeavePy/CPython ratios",
        ));
    }
    let rows = run_suite(&opts)?;
    let report = Report::new(rows);

    if update_baseline {
        let dst = baseline_path();
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&dst, serde_json::to_string_pretty(&report)?)?;
        eprintln!("baseline updated: {}", dst.display());
    }

    if emit_json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{}", report.to_markdown());
    }
    Ok(())
}

fn cmd_gate(args: &[String]) -> io::Result<bool> {
    let mut pct = 10.0_f64;
    let mut opts = RunOpts::default();
    for a in args {
        if parse_common(&mut opts, a) {
            continue;
        }
        match a.as_str() {
            x if x.starts_with("--pct=") => {
                pct = x[6..].parse().unwrap_or(pct);
            }
            other => {
                return Err(io::Error::other(format!("unknown flag '{other}'")));
            }
        }
    }
    let baseline_bytes = fs::read_to_string(baseline_path())?;
    let baseline: Report = serde_json::from_str(&baseline_bytes)?;
    let rows = run_suite(&opts)?;
    let report = Report::new(rows);
    println!("{}", report.to_markdown());
    let regs = report.regressions(&baseline, pct);
    if regs.is_empty() {
        println!("OK: no ratio regressions over {pct:.1}%");
        Ok(true)
    } else {
        println!("REGRESSIONS:");
        for r in &regs {
            println!("  {r}");
        }
        Ok(false)
    }
}
