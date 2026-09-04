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
    /// `weavepy` column measures the JIT. RFC 0077 WS1 makes this
    /// column default-on and *gated* (`interp_ratio`): the tier-1
    /// floor is what every non-compiled frame, deopt, and generic
    /// call runs on, and it had never been measured against a
    /// baseline. `--no-interp` opts out for quick local runs.
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
            include_interp: true,
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

/// One mode's thread-scaling measurement (RFC 0076 WS12): the
/// `parallel_scaling.py` fixture's serial and 8-thread walls for the
/// same total work, under one GIL mode.
#[derive(Debug, Clone)]
pub struct ScalingRow {
    /// Human-readable mode label (`gil=1 (default)` / `gil=0`).
    pub mode: &'static str,
    /// Median serial wall across samples, ns.
    pub serial_ns: f64,
    /// Median 8-thread wall across samples, ns.
    pub parallel_ns: f64,
}

impl ScalingRow {
    /// serial / parallel — ~1× means the threads serialized (the GIL
    /// build's expected shape), >1× means real parallel speedup.
    pub fn scaling(&self) -> f64 {
        self.serial_ns / self.parallel_ns
    }
}

/// Run the RFC 0076 WS12 thread-scaling fixture under the default
/// (GIL) mode and `-X gil=0`, returning one row per mode. This is a
/// *measurement*, not a gated suite member: the fixture is not in
/// [`crate::fixtures::FIXTURES`], so the committed baseline and the
/// CI gate never see it — the scaling claim is reported, per the
/// RFC's "measured, not marketing" clause.
pub fn run_scaling(opts: &RunOpts, work: u32) -> io::Result<Vec<ScalingRow>> {
    let weavepy = resolve_weavepy(opts)?;
    let fixture = crate::fixtures::fixtures_dir().join("parallel_scaling.py");
    if !fixture.is_file() {
        return Err(io::Error::other(format!(
            "scaling fixture missing: {}",
            fixture.display()
        )));
    }
    let modes: [(&'static str, &[&str]); 2] =
        [("gil=1 (default)", &[]), ("gil=0", &["-X", "gil=0"])];
    let mut rows = Vec::with_capacity(modes.len());
    for (mode, extra_args) in modes {
        let runs = if opts.warmup {
            opts.samples + 1
        } else {
            opts.samples
        };
        let mut serial = Vec::with_capacity(opts.samples as usize);
        let mut parallel = Vec::with_capacity(opts.samples as usize);
        for i in 0..runs {
            let (s, p) = time_scaling_subprocess(&weavepy, extra_args, &fixture, work)?;
            if !opts.warmup || i > 0 {
                serial.push(s);
                parallel.push(p);
            }
        }
        rows.push(ScalingRow {
            mode,
            serial_ns: median(&mut serial),
            parallel_ns: median(&mut parallel),
        });
    }
    Ok(rows)
}

fn median(samples: &mut [f64]) -> f64 {
    samples.sort_by(|a, b| a.total_cmp(b));
    match samples.len() {
        0 => f64::NAN,
        n if n % 2 == 1 => samples[n / 2],
        n => f64::midpoint(samples[n / 2 - 1], samples[n / 2]),
    }
}

/// Run `weavepy [extra_args] parallel_scaling.py` once and parse the
/// fixture's `WEAVEPY_BENCH_SERIAL_NS` / `WEAVEPY_BENCH_PARALLEL_NS`
/// marker pair.
fn time_scaling_subprocess(
    weavepy: &Path,
    extra_args: &[&str],
    fixture: &Path,
    work: u32,
) -> io::Result<(f64, f64)> {
    let mut cmd = Command::new(weavepy);
    cmd.args(extra_args)
        .arg(fixture)
        .env("WEAVEPY_BENCH_WORK", work.to_string())
        .env_remove("WEAVEPY_JIT")
        .env_remove("WEAVEPY_VM_STATS")
        // The mode under test comes from `extra_args` alone — an
        // exported PYTHON_GIL in the invoking shell would skew a leg.
        .env_remove("PYTHON_GIL");
    let out = run_with_rusage(&mut cmd)?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "{} {} exited {} on {}: {}",
            weavepy.display(),
            extra_args.join(" "),
            out.status.code().unwrap_or(-1),
            fixture.display(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let find = |marker: &str| {
        text.lines()
            .rev()
            .find_map(|line| line.trim().strip_prefix(marker))
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|v| v as f64)
    };
    match (
        find("WEAVEPY_BENCH_SERIAL_NS="),
        find("WEAVEPY_BENCH_PARALLEL_NS="),
    ) {
        (Some(s), Some(p)) => Ok((s, p)),
        _ => Err(io::Error::other(format!(
            "scaling fixture did not print both WEAVEPY_BENCH_SERIAL_NS and \
             WEAVEPY_BENCH_PARALLEL_NS; stdout was: {text}"
        ))),
    }
}

/// A flat top-of-stack profile of one fixture run (RFC 0077 WS1).
#[derive(Debug, Clone, Default)]
pub struct Census {
    /// Samples attributed to the interpreter thread (the thread with
    /// the most samples; the others are idle helpers).
    pub total_samples: u64,
    /// `(self samples, demangled symbol)`, descending.
    pub top: Vec<(u64, String)>,
}

/// Run `weavepy fixture.py` with an inflated work size and sample its
/// interpreter thread for `secs` seconds; return the flat self-time
/// census. macOS uses `sample(1)`; Linux uses `perf record` +
/// `perf report --no-children`. Other platforms are an error.
pub fn profile_fixture(
    weavepy: &Path,
    fix: &Fixture,
    work: u32,
    secs: u32,
    jit: bool,
) -> io::Result<Census> {
    // The fixture must outlive the sampling window; the runner's
    // baseline work sizes finish in well under a second, so scale the
    // work up and let the profiler stop first (`-mayDie` tolerates a
    // fixture that finishes early).
    let inflated = work.saturating_mul(secs.max(1) * 4);
    let mut cmd = Command::new(weavepy);
    cmd.arg(&fix.path)
        .env("WEAVEPY_BENCH_WORK", inflated.to_string())
        .env_remove("WEAVEPY_VM_STATS")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if jit {
        cmd.env_remove("WEAVEPY_JIT");
    } else {
        cmd.env("WEAVEPY_JIT", "0");
    }
    if cfg!(target_os = "macos") {
        let mut child = cmd.spawn()?;
        // Let startup and the fixture's warm-up finish before sampling.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let out = Command::new("sample")
            .arg(child.id().to_string())
            .arg(secs.to_string())
            .arg("1")
            .arg("-mayDie")
            .output();
        let _ = child.kill();
        let _ = child.wait();
        let out = out?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "sample(1) failed: {}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        if let Ok(path) = std::env::var("WEAVEPY_BENCH_PROFILE_RAW") {
            std::fs::write(path, text.as_bytes())?;
        }
        Ok(parse_sample_output(&text))
    } else if cfg!(target_os = "linux") {
        let data =
            std::env::temp_dir().join(format!("weavepy-bench-perf-{}.data", std::process::id()));
        let mut perf = Command::new("perf");
        perf.arg("record")
            .arg("-F")
            .arg("999")
            .arg("-g")
            .arg("-o")
            .arg(&data)
            .arg("--")
            .arg(weavepy)
            .arg(&fix.path)
            .env("WEAVEPY_BENCH_WORK", inflated.to_string())
            .env_remove("WEAVEPY_VM_STATS")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if jit {
            perf.env_remove("WEAVEPY_JIT");
        } else {
            perf.env("WEAVEPY_JIT", "0");
        }
        let status = perf.status()?;
        if !status.success() {
            return Err(io::Error::other("perf record failed"));
        }
        let out = Command::new("perf")
            .arg("report")
            .arg("--no-children")
            .arg("--stdio")
            .arg("-i")
            .arg(&data)
            .output()?;
        let _ = std::fs::remove_file(&data);
        Ok(parse_perf_report(&String::from_utf8_lossy(&out.stdout)))
    } else {
        Err(io::Error::other(
            "profile: no sampler on this platform (macOS `sample` and Linux `perf` are supported)",
        ))
    }
}

/// Parse `sample(1)`'s call-tree text into a flat self-time census.
/// Each line is `<prefix><count> <symbol>  (in <image>) ...` where
/// the prefix's width encodes depth; a node's self samples are its
/// count minus the sum of its children's counts. Only the thread with
/// the most samples is counted.
fn parse_sample_output(text: &str) -> Census {
    // Every thread is sampled the same number of times, and the CLI
    // runs the interpreter on a spawned big-stack thread (the process
    // main thread just waits on it), so the interpreter thread is the
    // one with the richest call tree: idle helpers sit in one wait
    // syscall.
    struct Block {
        total: u64,
        nodes: Vec<(usize, u64, String)>,
    }
    let better =
        |a: &Block, b: &Block| -> bool { (a.nodes.len(), a.total) > (b.nodes.len(), b.total) };
    let mut best: Option<Block> = None;
    let mut cur: Option<Block> = None;
    let flush = |cur: &mut Option<Block>, best: &mut Option<Block>| {
        if let Some(b) = cur.take() {
            if !b.nodes.is_empty() && best.as_ref().is_none_or(|bb| better(&b, bb)) {
                *best = Some(b);
            }
        }
    };
    for line in text.lines() {
        let trimmed = line.trim_start();
        let is_thread_header = trimmed.contains(" Thread_") && !trimmed.starts_with('+');
        if is_thread_header {
            if let Some((count, _)) = parse_count_symbol(trimmed) {
                flush(&mut cur, &mut best);
                cur = Some(Block {
                    total: count,
                    nodes: Vec::new(),
                });
            }
            continue;
        }
        if line.starts_with("Total number in stack")
            || line.starts_with("Sort by top of stack")
            || line.starts_with("Binary Images:")
        {
            flush(&mut cur, &mut best);
            break;
        }
        let Some(pos) = line.find(|c: char| c.is_ascii_digit()) else {
            continue;
        };
        let prefix = &line[..pos];
        if !prefix
            .chars()
            .all(|c| matches!(c, ' ' | '+' | '!' | ':' | '|'))
        {
            continue;
        }
        if let Some((count, sym)) = parse_count_symbol(&line[pos..]) {
            if let Some(b) = cur.as_mut() {
                b.nodes.push((prefix.len(), count, sym));
            }
        }
    }
    flush(&mut cur, &mut best);
    let Some(Block { total, nodes, .. }) = best else {
        return Census::default();
    };
    // Self samples: count minus the sum of the immediate children.
    let mut selfs: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for (i, (depth, count, sym)) in nodes.iter().enumerate() {
        let mut children = 0u64;
        for (d2, c2, _) in &nodes[i + 1..] {
            if d2 <= depth {
                break;
            }
            if *d2 == depth + 2 {
                children += c2;
            }
        }
        // Some `sample` versions indent by 2, others by more; fall back
        // to "next-deeper" when no child sat at depth + 2.
        if children == 0 {
            let mut next_depth = None;
            for (d2, c2, _) in &nodes[i + 1..] {
                if d2 <= depth {
                    break;
                }
                match next_depth {
                    None => {
                        next_depth = Some(*d2);
                        children += c2;
                    }
                    Some(nd) if *d2 == nd => children += c2,
                    _ => {}
                }
            }
        }
        let self_n = count.saturating_sub(children);
        if self_n > 0 {
            *selfs.entry(sym.clone()).or_insert(0) += self_n;
        }
    }
    let mut top: Vec<(u64, String)> = selfs.into_iter().map(|(s, n)| (n, s)).collect();
    top.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    Census {
        total_samples: total,
        top,
    }
}

/// `"<count> <symbol>  (in image) + off  [addr]"` -> `(count, demangled symbol)`.
fn parse_count_symbol(s: &str) -> Option<(u64, String)> {
    let mut it = s.splitn(2, ' ');
    let count: u64 = it.next()?.parse().ok()?;
    let rest = it.next()?.trim_start();
    let sym = match rest.find("  (in ") {
        Some(p) => &rest[..p],
        None => rest.split("  [").next().unwrap_or(rest),
    };
    Some((count, demangle_rust(sym.trim())))
}

/// Parse `perf report --no-children --stdio` into a census.
fn parse_perf_report(text: &str) -> Census {
    let mut top = Vec::new();
    let mut total = 0u64;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# Samples: ") {
            let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            total = num.parse().unwrap_or(0);
            if rest.contains('K') {
                total *= 1000;
            } else if rest.contains('M') {
                total *= 1_000_000;
            }
        }
        let t = line.trim_start();
        if t.is_empty() || t.starts_with('#') || !t.contains("[.] ") && !t.contains("[k] ") {
            continue;
        }
        // "    12.34%  weavepy  weavepy  [.] symbol"
        let mut cols = t.split_whitespace();
        let Some(pct) = cols.next().and_then(|p| p.strip_suffix('%')) else {
            continue;
        };
        let Ok(pct) = pct.parse::<f64>() else {
            continue;
        };
        let sym = t
            .split("[.] ")
            .nth(1)
            .or_else(|| t.split("[k] ").nth(1))
            .unwrap_or("")
            .trim();
        if sym.is_empty() {
            continue;
        }
        let n = (pct / 100.0 * total as f64).round() as u64;
        top.push((n, demangle_rust(sym)));
    }
    top.sort_by_key(|b| std::cmp::Reverse(b.0));
    Census {
        total_samples: total,
        top,
    }
}

/// Demangle a Rust symbol (legacy `_ZN…E` or v0 `_R…`) for the census,
/// dropping the legacy hash suffix; non-Rust symbols pass through.
/// `sample(1)` strips the leading underscore on macOS, so both spellings
/// are tried.
fn demangle_rust(sym: &str) -> String {
    let candidates = [sym.to_owned(), format!("_{sym}")];
    for c in &candidates {
        if let Ok(d) = rustc_demangle::try_demangle(c) {
            return format!("{d:#}");
        }
    }
    sym.to_owned()
}

#[cfg(test)]
mod tests {
    use super::{demangle_rust, median, parse_bench_ns, parse_sample_output, ScalingRow};

    #[test]
    fn demangles_legacy_rust_symbols() {
        assert_eq!(
            demangle_rust("_ZN10weavepy_vm11Interpreter4step17h0123456789abcdefE"),
            "weavepy_vm::Interpreter::step"
        );
        // macOS `sample` drops the leading underscore of v0 symbols.
        assert_eq!(
            demangle_rust("RNvMs2_Cse9Klhfpw3ze_10weavepy_vmNtB5_11Interpreter4step"),
            "<weavepy_vm::Interpreter>::step"
        );
        assert_eq!(
            demangle_rust(
                "_ZN4core3ptr47drop_in_place$LT$weavepy_vm..object..Object$GT$17h0123456789abcdefE"
            ),
            "core::ptr::drop_in_place<weavepy_vm::object::Object>"
        );
        assert_eq!(demangle_rust("malloc"), "malloc");
    }

    #[test]
    fn sample_tree_self_samples() {
        let text = "\
Call graph:
    100 Thread_1   DispatchQueue_1: com.apple.main-thread  (serial)
    + 100 start  (in dyld) + 6076  [0x1]
    +   100 main  (in weavepy) + 40  [0x2]
    +     70 _ZN10weavepy_vm11Interpreter4step17h0123456789abcdefE  (in weavepy) + 4  [0x3]
    +     ! 30 malloc  (in libsystem_malloc.dylib) + 4  [0x4]
    +     30 free  (in libsystem_malloc.dylib) + 4  [0x5]
    5 Thread_2
    + 5 __psynch_cvwait  (in libsystem_kernel.dylib) + 8  [0x6]

Total number in stack (recursive counted multiple, when >=5):
";
        let c = parse_sample_output(text);
        assert_eq!(c.total_samples, 100);
        let get = |name: &str| c.top.iter().find(|(_, s)| s == name).map(|(n, _)| *n);
        assert_eq!(get("weavepy_vm::Interpreter::step"), Some(40));
        assert_eq!(get("malloc"), Some(30));
        assert_eq!(get("free"), Some(30));
        assert_eq!(get("main"), None);
        assert_eq!(get("__psynch_cvwait"), None);
    }

    #[test]
    fn parses_last_ns_line() {
        let out = b"warming\nWEAVEPY_BENCH_NS=100\nWEAVEPY_BENCH_NS=42\n";
        assert_eq!(parse_bench_ns(out), Some(42.0));
    }

    #[test]
    fn rejects_missing_marker() {
        assert_eq!(parse_bench_ns(b"hello\n"), None);
    }

    #[test]
    fn median_of_odd_and_even_sample_sets() {
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&mut [4.0, 1.0, 2.0, 3.0]), 2.5);
    }

    #[test]
    fn scaling_is_serial_over_parallel() {
        let row = ScalingRow {
            mode: "gil=0",
            serial_ns: 8.0e9,
            parallel_ns: 2.0e9,
        };
        assert!((row.scaling() - 4.0).abs() < 1e-9);
    }
}
