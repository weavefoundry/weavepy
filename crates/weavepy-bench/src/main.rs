//! `weavepy-bench` CLI entry point.
//!
//! Subcommands:
//!
//! - `run` — runs all fixtures, prints a markdown report.
//! - `run --json` — emits the report as JSON to stdout.
//! - `run --update-baseline` — overwrites
//!   `baselines/bench.json` with the run's results.
//! - `gate` — runs the suite, compares against the baseline,
//!   and exits non-zero if any fixture regressed.
//!
//! For maximum portability we hand-roll arg parsing rather than
//! pull in `clap` — the tool has at most a handful of flags.

use std::env;
use std::fs;
use std::io;
use std::process::ExitCode;

use weavepy_bench::fixtures::baseline_path;
use weavepy_bench::report::Report;
use weavepy_bench::runner::{run_suite, RunOpts};
use weavepy_vm::specialize::{format_stats_markdown, snapshot, stats_enabled};

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
    eprintln!("weavepy-bench — RFC 0021 microbench harness");
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("    weavepy-bench [run|gate|help] [flags]");
    eprintln!();
    eprintln!("COMMANDS:");
    eprintln!("    run    Run the suite and print a markdown report.");
    eprintln!("    gate   Run the suite and compare against the baseline.");
    eprintln!("    help   Print this message.");
    eprintln!();
    eprintln!("FLAGS for `run`:");
    eprintln!("    --json                Print report as JSON.");
    eprintln!("    --update-baseline     Overwrite baselines/bench.json.");
    eprintln!("    --no-cpython          Skip the host CPython subprocess.");
    eprintln!("    --samples=N           Timing samples per fixture (default 5).");
    eprintln!();
    eprintln!("FLAGS for `gate`:");
    eprintln!("    --pct=PCT             Regression threshold (default 10).");
}

fn cmd_run(args: &[String]) -> io::Result<()> {
    let mut opts = RunOpts::default();
    let mut emit_json = false;
    let mut update_baseline = false;
    for a in args {
        match a.as_str() {
            "--json" => emit_json = true,
            "--update-baseline" => update_baseline = true,
            "--no-cpython" => opts.include_cpython = false,
            x if x.starts_with("--samples=") => {
                opts.samples = x[10..].parse().unwrap_or(opts.samples);
            }
            other => {
                return Err(io::Error::other(format!("unknown flag '{other}'")));
            }
        }
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
        if stats_enabled() {
            // RFC 0021 — when WEAVEPY_VM_STATS=1 is set, append a
            // markdown stats table to the report so users can see
            // how the specialization layer performed across the
            // suite. Off by default; cheap when off.
            println!();
            println!("{}", format_stats_markdown(&snapshot()));
            // RFC 0032 — append tier-2 JIT counters when compiled in.
            if let Some(jit) = weavepy_vm::jit_stats_markdown() {
                println!();
                println!("{jit}");
            }
        }
    }
    Ok(())
}

fn cmd_gate(args: &[String]) -> io::Result<bool> {
    let mut pct = 10.0_f64;
    let mut opts = RunOpts::default();
    for a in args {
        match a.as_str() {
            x if x.starts_with("--pct=") => {
                pct = x[6..].parse().unwrap_or(pct);
            }
            "--no-cpython" => opts.include_cpython = false,
            other => {
                return Err(io::Error::other(format!("unknown flag '{other}'")));
            }
        }
    }
    let baseline_bytes = fs::read_to_string(baseline_path())?;
    let baseline: Report = serde_json::from_str(&baseline_bytes)?;
    let rows = run_suite(&opts)?;
    let report = Report::new(rows);
    let regs = report.regressions(&baseline, pct);
    if regs.is_empty() {
        println!("OK: no regressions over {pct:.1}%");
        Ok(true)
    } else {
        println!("REGRESSIONS:");
        for r in &regs {
            println!("  {r}");
        }
        Ok(false)
    }
}
