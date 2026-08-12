//! `weavepy-bench` CLI entry point (RFC 0058 WS1).
//!
//! Subcommands:
//!
//! - `run` — runs all fixtures, prints a markdown report.
//! - `run --json` — emits the report as JSON to stdout.
//! - `run --update-baseline` — overwrites the host platform's
//!   `baselines/bench-{os}-{arch}.json` with the run's results
//!   (requires the CPython column so the baseline carries ratios).
//! - `gate` — runs the suite, compares WeavePy/CPython ratios (and
//!   the suite geomean) against the host platform's baseline, and
//!   exits non-zero on regressions beyond the threshold. Regressed
//!   fixtures are re-measured once first — a regression must survive
//!   the retry to fail the gate, which rejects one-off noise
//!   excursions on shared CI runners without loosening the threshold.
//!   Missing per-platform baselines are an error unless
//!   `--allow-missing-baseline` makes the gate advisory (RFC 0062
//!   WS3).
//!
//! For maximum portability we hand-roll arg parsing rather than
//! pull in `clap` — the tool has at most a handful of flags.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use weavepy_bench::fixtures::{baseline_path, discover_fixtures, platform_key};
use weavepy_bench::report::Report;
use weavepy_bench::runner::{resolve_python, resolve_weavepy, run_one, run_suite, RunOpts};

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
    eprintln!("    --update-baseline     Overwrite the host platform's baseline");
    eprintln!("                          (baselines/bench-{{os}}-{{arch}}.json).");
    eprintln!();
    eprintln!("FLAGS for `gate`:");
    eprintln!("    --pct=PCT             Regression threshold (default 10).");
    eprintln!("    --allow-missing-baseline");
    eprintln!("                          If the host platform has no baseline file, print");
    eprintln!("                          an advisory note and exit 0 instead of failing.");
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
    let mut allow_missing = false;
    let mut opts = RunOpts::default();
    for a in args {
        if parse_common(&mut opts, a) {
            continue;
        }
        match a.as_str() {
            "--allow-missing-baseline" => allow_missing = true,
            x if x.starts_with("--pct=") => {
                pct = x[6..].parse().unwrap_or(pct);
            }
            other => {
                return Err(io::Error::other(format!("unknown flag '{other}'")));
            }
        }
    }
    let host_platform = platform_key();
    let baseline = load_baseline(&baseline_path(), &host_platform, allow_missing)?;
    let rows = run_suite(&opts)?;
    let mut report = Report::new(rows);
    println!("{}", report.to_markdown());
    let Some(baseline) = baseline else {
        println!("==============================================================");
        println!("NOTE: no bench baseline for this platform ({host_platform}) —");
        println!("the gate is advisory: nothing was compared, exiting 0.");
        println!("Record one with `weavepy-bench run --update-baseline` and");
        println!("commit baselines/bench-{host_platform}.json to make it strict.");
        println!("==============================================================");
        return Ok(true);
    };
    let mut regs = report.regressions(&baseline, pct);

    // Shared CI runners take multi-second noise excursions that land
    // squarely on whichever fixture is running (observed: deltablue
    // jumping +51% on one macos-latest run while the suite geomean sat
    // *below* baseline). Re-measure just the regressed fixtures once
    // and keep the better measurement of the two: noise only inflates
    // ratios, so the minimum is the truer estimate, while a genuine
    // regression reproduces on the retry and still fails the gate.
    let retry_names = report.regressed_fixture_names(&baseline, pct);
    if !retry_names.is_empty() {
        println!(
            "RETRY: re-measuring {} regressed fixture(s) to reject one-off runner noise: {}",
            retry_names.len(),
            retry_names.join(", ")
        );
        let weavepy = resolve_weavepy(&opts)?;
        let python = if opts.include_cpython {
            Some(resolve_python(&opts)?)
        } else {
            None
        };
        let mut rows = report.rows;
        for fix in discover_fixtures() {
            if !retry_names.contains(&fix.name) {
                continue;
            }
            let retry = run_one(&fix, &opts, &weavepy, python.as_deref())?;
            let slot = rows
                .iter_mut()
                .find(|r| r.name == fix.name)
                .expect("regressed fixture has a report row");
            let better = match (retry.ratio, slot.ratio) {
                (Some(nr), Some(or)) => nr < or,
                _ => retry.weavepy.median_ns < slot.weavepy.median_ns,
            };
            match (slot.ratio, retry.ratio) {
                (Some(or), Some(nr)) => println!(
                    "  {}: {:.2}× -> {:.2}× on retry ({})",
                    fix.name,
                    or,
                    nr,
                    if better {
                        "kept retry"
                    } else {
                        "kept original"
                    }
                ),
                _ => println!(
                    "  {}: re-measured ({})",
                    fix.name,
                    if better {
                        "kept retry"
                    } else {
                        "kept original"
                    }
                ),
            }
            if better {
                *slot = retry;
            }
        }
        report = Report::new(rows);
        regs = report.regressions(&baseline, pct);
    }

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

/// Load the host's per-platform baseline for `gate` (RFC 0062 WS3).
///
/// - Missing file + `allow_missing` → `Ok(None)` (advisory gate).
/// - Missing file otherwise → a clear error naming the per-platform
///   file and how to record one.
/// - Present file whose recorded platform mismatches `host_platform`
///   → error (a copied baseline must not silently gate).
fn load_baseline(
    path: &Path,
    host_platform: &str,
    allow_missing: bool,
) -> io::Result<Option<Report>> {
    if !path.is_file() {
        if allow_missing {
            return Ok(None);
        }
        return Err(io::Error::other(format!(
            "no bench baseline for this platform: {} does not exist. Record one with \
             `cargo run --release -p weavepy-bench -- run --update-baseline` on a \
             {host_platform} host and commit it, or pass --allow-missing-baseline to \
             make the gate advisory",
            path.display()
        )));
    }
    let bytes = fs::read_to_string(path)?;
    let baseline: Report = serde_json::from_str(&bytes)?;
    baseline
        .check_platform(host_platform)
        .map_err(io::Error::other)?;
    Ok(Some(baseline))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp path that does not exist yet.
    fn temp_json(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "weavepy-bench-test-{tag}-{}.json",
            std::process::id()
        ))
    }

    fn write_baseline(path: &Path, platform: &str) {
        let mut report = Report::new(vec![]);
        report.platform = Some(platform.to_owned());
        fs::write(path, serde_json::to_string(&report).unwrap()).unwrap();
    }

    #[test]
    fn missing_baseline_is_an_error_by_default() {
        let path = temp_json("missing-strict");
        let err = load_baseline(&path, "linux-x86_64", false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no bench baseline for this platform"), "{msg}");
        assert!(
            msg.contains(path.to_str().unwrap()),
            "error should name the missing file: {msg}"
        );
        assert!(msg.contains("--allow-missing-baseline"), "{msg}");
    }

    #[test]
    fn missing_baseline_allowed_yields_advisory_none() {
        let path = temp_json("missing-advisory");
        let loaded = load_baseline(&path, "linux-x86_64", true).unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn matching_platform_baseline_loads() {
        let path = temp_json("match");
        write_baseline(&path, "linux-x86_64");
        let loaded = load_baseline(&path, "linux-x86_64", false).unwrap();
        assert_eq!(loaded.unwrap().platform.as_deref(), Some("linux-x86_64"));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn copied_baseline_from_other_platform_refused() {
        let path = temp_json("mismatch");
        write_baseline(&path, "macos-aarch64");
        let err = load_baseline(&path, "linux-x86_64", false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("platform mismatch"), "{msg}");
        // --allow-missing-baseline must not paper over a *present*
        // but foreign baseline.
        assert!(load_baseline(&path, "linux-x86_64", true).is_err());
        let _ = fs::remove_file(&path);
    }
}
