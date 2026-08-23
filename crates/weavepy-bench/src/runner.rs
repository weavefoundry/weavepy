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

/// One measurement: the fixture's timing plus the subprocess's peak
/// resident set size (RFC 0059 WS5b; `None` where the platform can't
/// report it).
#[derive(Debug, Clone, Copy)]
struct Sample {
    ns: f64,
    max_rss: Option<u64>,
}

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
    /// Also collect a WeavePy column with `WEAVEPY_JIT=0` (RFC 0067
    /// WS4). The default binary ships with the tier-2 JIT on, so the
    /// gated `weavepy` column measures the JIT; this extra column
    /// keeps interpreter-only progress a measured, reported number.
    pub include_interp: bool,
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
            include_interp: false,
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
    let interp = if opts.include_interp {
        Some(runset(&collect_samples(
            weavepy.as_os_str(),
            fix,
            opts,
            &[("WEAVEPY_JIT", "0")],
        )?))
    } else {
        None
    };
    let cpython = match python {
        Some(py) => Some(runset(&collect_samples(
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
        runset(&weavepy_samples),
        cpython,
        interp,
    ))
}

/// Summarize timing samples into a [`RunSet`], attaching the peak RSS
/// observed across the samples (RFC 0059 WS5b).
fn runset(samples: &[Sample]) -> RunSet {
    let ns: Vec<f64> = samples.iter().map(|s| s.ns).collect();
    let max_rss = samples.iter().filter_map(|s| s.max_rss).max();
    RunSet::from_samples_ns(&ns).with_max_rss(max_rss)
}

/// Time a single fixture under the PR binary *and* a merge-base
/// binary, interleaving the legs sample by sample (PR, base, CPython,
/// PR, base, CPython, …) so slow drift on a shared runner — thermal
/// ramp, a neighbor tenant spinning up — lands on both legs equally
/// and cancels in the PR/base ratio. This is what makes the A/B gate
/// immune to the machine skew that a committed-baseline comparison
/// spends its whole envelope absorbing.
///
/// A base-leg failure is not an error: a fixture added by the PR
/// under test may exercise syntax the merge-base binary cannot run
/// yet. The row simply carries no base leg (the A/B gate skips it,
/// with a note from the caller).
pub fn run_one_ab(
    fix: &Fixture,
    opts: &RunOpts,
    weavepy: &Path,
    base: &Path,
    python: Option<&str>,
) -> io::Result<Row> {
    let runs = if opts.warmup {
        opts.samples + 1
    } else {
        opts.samples
    };
    let mut pr_samples = Vec::with_capacity(opts.samples as usize);
    let mut base_samples = Vec::with_capacity(opts.samples as usize);
    let mut py_samples = Vec::with_capacity(opts.samples as usize);
    let mut base_ok = true;
    for i in 0..runs {
        let keep = !opts.warmup || i > 0;
        let pr = time_subprocess(weavepy.as_os_str(), fix, &[])?;
        if keep {
            pr_samples.push(pr);
        }
        if base_ok {
            match time_subprocess(base.as_os_str(), fix, &[]) {
                Ok(b) => {
                    if keep {
                        base_samples.push(b);
                    }
                }
                Err(_) => base_ok = false,
            }
        }
        if let Some(py) = python {
            let p = time_subprocess(std::ffi::OsStr::new(py), fix, &[])?;
            if keep {
                py_samples.push(p);
            }
        }
    }
    let cpython = (!py_samples.is_empty()).then(|| runset(&py_samples));
    let base_set = (base_ok && !base_samples.is_empty()).then(|| runset(&base_samples));
    Ok(Row::new(
        fix.name.clone(),
        fix.work,
        runset(&pr_samples),
        cpython,
        None,
    )
    .with_base(base_set))
}

/// Run all known fixtures A/B (see [`run_one_ab`]) and return one
/// [`Row`] per fixture.
pub fn run_suite_ab(opts: &RunOpts, base: &Path) -> io::Result<Vec<Row>> {
    let weavepy = resolve_weavepy(opts)?;
    let python = if opts.include_cpython {
        Some(resolve_python(opts)?)
    } else {
        None
    };
    let mut rows = Vec::new();
    for fix in discover_fixtures() {
        rows.push(run_one_ab(&fix, opts, &weavepy, base, python.as_deref())?);
    }
    Ok(rows)
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
) -> io::Result<Vec<Sample>> {
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

/// Run `interp fixture.py` once and return the measured nanoseconds —
/// the fixture's self-reported `WEAVEPY_BENCH_NS` for normal fixtures,
/// or the subprocess wall time for wall-clock fixtures — plus the
/// child's peak RSS (RFC 0059 WS5b).
fn time_subprocess(
    interp: &std::ffi::OsStr,
    fix: &Fixture,
    extra_env: &[(&str, &str)],
) -> io::Result<Sample> {
    let mut cmd = Command::new(interp);
    cmd.arg(&fix.path)
        .env("WEAVEPY_BENCH_WORK", fix.work.to_string())
        // The default column must measure the shipped configuration:
        // an exported `WEAVEPY_JIT=0` (or a stats run's
        // `WEAVEPY_VM_STATS=1`) in the invoking shell would silently
        // skew it. The interp column re-adds its override below.
        .env_remove("WEAVEPY_JIT")
        .env_remove("WEAVEPY_VM_STATS");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let start = Instant::now();
    let out = run_with_rusage(&mut cmd)?;
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
        return Ok(Sample {
            ns: wall,
            max_rss: out.max_rss,
        });
    }
    let ns = parse_bench_ns(&out.stdout).ok_or_else(|| {
        io::Error::other(format!(
            "{} did not print WEAVEPY_BENCH_NS=<int> for {}; stdout was: {}",
            interp.to_string_lossy(),
            fix.name,
            String::from_utf8_lossy(&out.stdout)
        ))
    })?;
    Ok(Sample {
        ns,
        max_rss: out.max_rss,
    })
}

/// `Command::output()` plus the child's `ru_maxrss` (RFC 0059 WS5b).
struct OutputWithRss {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    max_rss: Option<u64>,
}

/// Run the command to completion, capturing output and — on Unix —
/// the child's peak RSS via `wait4(2)` (which `Command::output()`'s
/// internal `waitpid` would discard). Windows would read
/// `PROCESS_MEMORY_COUNTERS` here; until someone benches there, the
/// column is simply absent (`None`).
#[cfg(unix)]
fn run_with_rusage(cmd: &mut Command) -> io::Result<OutputWithRss> {
    use std::io::Read;
    use std::os::unix::process::ExitStatusExt;
    use std::process::Stdio;

    // `struct rusage`: two `timeval`s (16 bytes each on the 64-bit
    // Unixes we support) followed by 14 `long`s, `ru_maxrss` first.
    // Oversized spare tail for safety; the kernel only writes its own
    // struct's length.
    #[repr(C)]
    struct RUsage {
        ru_utime: [u64; 2],
        ru_stime: [u64; 2],
        ru_maxrss: i64,
        rest: [i64; 16],
    }
    extern "C" {
        fn wait4(pid: i32, status: *mut i32, options: i32, rusage: *mut RUsage) -> i32;
    }

    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Drain stderr on a helper thread so a chatty child can't deadlock
    // against a full pipe while we block on stdout (same discipline as
    // `Command::output()`).
    let mut err_pipe = child.stderr.take().expect("stderr piped");
    let err_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err_pipe.read_to_end(&mut buf);
        buf
    });
    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .expect("stdout piped")
        .read_to_end(&mut stdout)?;
    let stderr = err_thread.join().unwrap_or_default();

    let pid = child.id() as i32;
    let mut status_raw: i32 = 0;
    let mut ru = RUsage {
        ru_utime: [0; 2],
        ru_stime: [0; 2],
        ru_maxrss: 0,
        rest: [0; 16],
    };
    let reaped = loop {
        let r = unsafe { wait4(pid, &raw mut status_raw, 0, &raw mut ru) };
        if r == pid {
            break true;
        }
        let e = io::Error::last_os_error();
        if r == -1 && e.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        break false;
    };
    let (status, max_rss) = if reaped {
        // macOS reports `ru_maxrss` in bytes; Linux (and the BSDs) in
        // kilobytes.
        #[cfg(target_os = "macos")]
        let bytes = u64::try_from(ru.ru_maxrss).unwrap_or(0);
        #[cfg(not(target_os = "macos"))]
        let bytes = u64::try_from(ru.ru_maxrss).unwrap_or(0) * 1024;
        (
            ExitStatusExt::from_raw(status_raw),
            (bytes > 0).then_some(bytes),
        )
    } else {
        // wait4 failed (ECHILD, etc.) — fall back to the std reaper so
        // the child is never leaked; the sample just loses its RSS.
        (child.wait()?, None)
    };
    Ok(OutputWithRss {
        status,
        stdout,
        stderr,
        max_rss,
    })
}

#[cfg(not(unix))]
fn run_with_rusage(cmd: &mut Command) -> io::Result<OutputWithRss> {
    let out = cmd.output()?;
    Ok(OutputWithRss {
        status: out.status,
        stdout: out.stdout,
        stderr: out.stderr,
        max_rss: None,
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
