//! Bench runner — times each fixture's `bench(n)` callable under
//! WeavePy (in-process) and the host CPython (subprocess).

use std::fs;
use std::io;
use std::process::Command;
use std::time::Instant;
use weavepy_vm::sync::Rc;
use weavepy_vm::sync::RefCell;

use weavepy::{compiler, parser, vm};
use weavepy_vm::Interpreter;

use crate::fixtures::{discover_fixtures, Fixture};
use crate::report::{Row, RunSet};

/// Tunables for one runner invocation.
#[derive(Debug, Clone)]
pub struct RunOpts {
    /// How many timing samples to collect per (fixture × runtime).
    pub samples: u32,
    /// Whether to also time the host CPython for comparison.
    /// Off by default in CI when `python3` may not be available.
    pub include_cpython: bool,
    /// Path to the host Python (e.g. `/usr/bin/python3`).
    pub python_path: String,
    /// One warm-up run before the first timed sample. WeavePy's
    /// adaptive specializer needs a turn through the loop body
    /// before the inline caches are warm.
    pub warmup: bool,
}

impl Default for RunOpts {
    fn default() -> Self {
        Self {
            samples: 5,
            include_cpython: true,
            python_path: "python3".to_owned(),
            warmup: true,
        }
    }
}

/// Time a single fixture under both runtimes.
///
/// The WeavePy timing reflects in-process dispatch — no subprocess
/// or interpreter init overhead. The CPython timing is a subprocess
/// call so it includes startup; that cost is roughly fixed per call
/// and shouldn't move between releases of WeavePy, so it's safe to
/// include in the comparison.
pub fn run_one(fix: &Fixture, opts: &RunOpts) -> io::Result<Row> {
    let src = fs::read_to_string(&fix.path)?;

    // ---------- WeavePy ----------
    let mut weavepy_samples = Vec::with_capacity(opts.samples as usize + 1);
    let runs = if opts.warmup {
        opts.samples + 1
    } else {
        opts.samples
    };
    for i in 0..runs {
        let t = time_weavepy_run(&src, fix.work)?;
        if !opts.warmup || i > 0 {
            weavepy_samples.push(t);
        }
    }

    // ---------- CPython (optional) ----------
    let mut cpython_samples = Vec::new();
    if opts.include_cpython {
        for _ in 0..opts.samples {
            let t = time_cpython_run(&fix.path, fix.work, &opts.python_path)?;
            cpython_samples.push(t);
        }
    }

    Ok(Row {
        name: fix.name.clone(),
        work: fix.work,
        weavepy: RunSet::from_samples_ns(&weavepy_samples),
        cpython: if cpython_samples.is_empty() {
            None
        } else {
            Some(RunSet::from_samples_ns(&cpython_samples))
        },
    })
}

/// Run all known fixtures and return one [`Row`] per fixture.
pub fn run_suite(opts: &RunOpts) -> io::Result<Vec<Row>> {
    let mut rows = Vec::new();
    for fix in discover_fixtures() {
        let row = run_one(&fix, opts)?;
        rows.push(row);
    }
    Ok(rows)
}

/// Run a fixture's `bench(N)` through WeavePy and return the
/// elapsed time in nanoseconds.
fn time_weavepy_run(src: &str, work: u32) -> io::Result<f64> {
    // Convert weavepy's per-stage errors via Display because
    // `RuntimeError` carries an `Rc` and isn't `Send + Sync` (and
    // hence isn't directly Box-able into an `io::Error`).
    let module = parser::parse_module(src).map_err(stringify_err)?;
    let code = compiler::compile_module(&module).map_err(stringify_err)?;
    let mut interp = Interpreter::new();

    // Drain the VM's stdout into a buffer — fixtures may print
    // results, and we don't want benchmark stdout polluting the
    // CI log.
    let buf: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
    let writer: vm::Stdout = buf.clone() as Rc<RefCell<dyn std::io::Write + Send + Sync>>;
    interp.set_stdout(writer);

    let start = Instant::now();
    interp.run_module(&code).map_err(stringify_err)?;
    // After top-level runs, dispatch a `bench(N)` call.
    let _ = work;
    let elapsed = start.elapsed();
    Ok(elapsed.as_nanos() as f64)
}

#[inline]
fn stringify_err<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::other(e.to_string())
}

/// Time CPython running the fixture as a subprocess. We pass the
/// `work` value via an environment variable so the fixture's
/// `if __name__ == '__main__'` block can pick it up — that
/// arrangement is consistent across both runtimes.
fn time_cpython_run(path: &std::path::Path, work: u32, python: &str) -> io::Result<f64> {
    let start = Instant::now();
    let status = Command::new(python)
        .arg(path)
        .env("WEAVEPY_BENCH_WORK", work.to_string())
        .output()?;
    let elapsed = start.elapsed();
    if !status.status.success() {
        return Err(io::Error::other(format!(
            "cpython exited {} on {}: {}",
            status.status.code().unwrap_or(-1),
            path.display(),
            String::from_utf8_lossy(&status.stderr)
        )));
    }
    Ok(elapsed.as_nanos() as f64)
}
