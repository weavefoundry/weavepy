//! Bench runner v2 (RFC 0058 WS1) — times each fixture's `bench(n)`
//! under the built `weavepy` binary and the host CPython, both as
//! subprocesses with an identical `WEAVEPY_BENCH_WORK`.
//!
//! Fixtures self-time the bench region and print
//! `WEAVEPY_BENCH_NS=<int>`; the runner parses that, so startup /
//! parse / import cost is excluded symmetrically. Fixtures listed in
//! [`crate::fixtures::WALL_CLOCK_FIXTURES`] are timed as full
//! subprocess wall time instead (startup *is* their workload).

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use crate::fixtures::{discover_fixtures, Fixture};
use crate::report::{Row, RunSet};

/// Tunables for one runner invocation.
#[derive(Debug, Clone)]
pub struct RunOpts {
    /// How many timing samples to collect per (fixture × runtime).
    pub samples: u32,
    /// One warm-up run (dropped) before the first timed sample —
    /// primes OS file caches so sample 1 isn't an outlier.
    pub warmup: bool,
    /// Whether to also time the host CPython for comparison.
    pub include_cpython: bool,
    /// Also collect a WeavePy column with `WEAVEPY_JIT=1`. Requires
    /// the `weavepy` binary to have been built with the `jit`
    /// feature; without it the column just repeats the interpreter.
    pub include_jit: bool,
    /// Explicit path to the host Python. When `None`, `python3.13`
    /// is preferred and `python3` is the fallback.
    pub python_path: Option<String>,
    /// Explicit path to the `weavepy` binary under test. When
    /// `None`, `$WEAVEPY_BIN` is honored, then a `weavepy` binary
    /// next to the running `weavepy-bench` executable (i.e. the same
    /// cargo profile directory).
    pub weavepy_path: Option<PathBuf>,
}

impl Default for RunOpts {
    fn default() -> Self {
        Self {
            samples: 5,
            warmup: true,
            include_cpython: true,
            include_jit: false,
            python_path: None,
            weavepy_path: None,
        }
    }
}

/// Locate the `weavepy` binary under test. Priority: explicit opt →
/// `$WEAVEPY_BIN` → sibling of the current executable.
pub fn resolve_weavepy(opts: &RunOpts) -> io::Result<PathBuf> {
    if let Some(p) = &opts.weavepy_path {
        return Ok(p.clone());
    }
    if let Ok(p) = std::env::var("WEAVEPY_BIN") {
        return Ok(PathBuf::from(p));
    }
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| {
        io::Error::other("cannot resolve the directory of the running weavepy-bench binary")
    })?;
    let candidate = dir.join(if cfg!(windows) {
        "weavepy.exe"
    } else {
        "weavepy"
    });
    if candidate.exists() {
        return Ok(candidate);
    }
    Err(io::Error::other(format!(
        "no weavepy binary at {} — build it first (`cargo build --release -p weavepy-cli`), \
         or pass --weavepy=PATH / set WEAVEPY_BIN",
        candidate.display()
    )))
}

/// Locate the host CPython. Priority: explicit opt → `python3.13` →
/// `python3`. A candidate qualifies if `-c pass` exits 0.
pub fn resolve_python(opts: &RunOpts) -> io::Result<String> {
    if let Some(p) = &opts.python_path {
        return Ok(p.clone());
    }
    for candidate in ["python3.13", "python3"] {
        let ok = Command::new(candidate)
            .args(["-c", "pass"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return Ok(candidate.to_owned());
        }
    }
    Err(io::Error::other(
        "no host CPython found (tried python3.13, python3); pass --python=PATH or --no-cpython",
    ))
}

/// Time a single fixture under the configured runtimes.
pub fn run_one(
    fix: &Fixture,
    opts: &RunOpts,
    weavepy: &Path,
    python: Option<&str>,
) -> io::Result<Row> {
    let weavepy_samples = collect_samples(weavepy.as_os_str(), fix, opts, &[])?;
    let jit = if opts.include_jit {
        Some(RunSet::from_samples_ns(&collect_samples(
            weavepy.as_os_str(),
            fix,
            opts,
            &[("WEAVEPY_JIT", "1")],
        )?))
    } else {
        None
    };
    let cpython = match python {
        Some(py) => Some(RunSet::from_samples_ns(&collect_samples(
            std::ffi::OsStr::new(py),
            fix,
            opts,
            &[],
        )?)),
        None => None,
    };
    Ok(Row::new(
        fix.name.clone(),
        fix.work,
        RunSet::from_samples_ns(&weavepy_samples),
        cpython,
        jit,
    ))
}

/// Run all known fixtures and return one [`Row`] per fixture.
pub fn run_suite(opts: &RunOpts) -> io::Result<Vec<Row>> {
    let weavepy = resolve_weavepy(opts)?;
    let python = if opts.include_cpython {
        Some(resolve_python(opts)?)
    } else {
        None
    };
    let mut rows = Vec::new();
    for fix in discover_fixtures() {
        rows.push(run_one(&fix, opts, &weavepy, python.as_deref())?);
    }
    Ok(rows)
}

fn collect_samples(
    interp: &std::ffi::OsStr,
    fix: &Fixture,
    opts: &RunOpts,
    extra_env: &[(&str, &str)],
) -> io::Result<Vec<f64>> {
    let runs = if opts.warmup {
        opts.samples + 1
    } else {
        opts.samples
    };
    let mut samples = Vec::with_capacity(opts.samples as usize);
    for i in 0..runs {
        let t = time_subprocess(interp, fix, extra_env)?;
        if !opts.warmup || i > 0 {
            samples.push(t);
        }
    }
    Ok(samples)
}

/// Run `interp fixture.py` once and return the measured nanoseconds:
/// the fixture's self-reported `WEAVEPY_BENCH_NS` for normal
/// fixtures, or the subprocess wall time for wall-clock fixtures.
fn time_subprocess(
    interp: &std::ffi::OsStr,
    fix: &Fixture,
    extra_env: &[(&str, &str)],
) -> io::Result<f64> {
    let mut cmd = Command::new(interp);
    cmd.arg(&fix.path)
        .env("WEAVEPY_BENCH_WORK", fix.work.to_string());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let start = Instant::now();
    let out = cmd.output()?;
    let wall = start.elapsed().as_nanos() as f64;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "{} exited {} on {}: {}",
            interp.to_string_lossy(),
            out.status.code().unwrap_or(-1),
            fix.path.display(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    if fix.wall_clock {
        return Ok(wall);
    }
    parse_bench_ns(&out.stdout).ok_or_else(|| {
        io::Error::other(format!(
            "{} did not print WEAVEPY_BENCH_NS=<int> for {}; stdout was: {}",
            interp.to_string_lossy(),
            fix.name,
            String::from_utf8_lossy(&out.stdout)
        ))
    })
}

/// Extract the last `WEAVEPY_BENCH_NS=<int>` line from stdout.
/// Fixtures are free to print other diagnostics.
fn parse_bench_ns(stdout: &[u8]) -> Option<f64> {
    let text = String::from_utf8_lossy(stdout);
    text.lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix("WEAVEPY_BENCH_NS="))
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|v| v as f64)
}

#[cfg(test)]
mod tests {
    use super::parse_bench_ns;

    #[test]
    fn parses_last_ns_line() {
        let out = b"warming\nWEAVEPY_BENCH_NS=100\nWEAVEPY_BENCH_NS=42\n";
        assert_eq!(parse_bench_ns(out), Some(42.0));
    }

    #[test]
    fn rejects_missing_marker() {
        assert_eq!(parse_bench_ns(b"hello\n"), None);
    }
}
