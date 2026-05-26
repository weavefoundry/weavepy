//! Regression test runner — drive individual `test_*.py` files end-to-end
//! through WeavePy and grade them against a checked-in baseline.
//!
//! RFC 0026 rewrite. Three test pools are recognised:
//!
//! 1. **Bundled regression tests** under `tests/regrtest/` in the repo
//!    root. These are small, hand-curated fixtures that exercise the
//!    Rust↔Python boundary. They should all pass on `main`; a
//!    regression breaks CI.
//! 2. **CPython `Lib/test/`** when `vendor/cpython/` is checked out as
//!    a submodule (or its slimmer cousin `vendor/cpython-tests/`).
//!    The full CPython test suite is enormous so we operate off an
//!    allowlist (see [`Expectations`]) plus optional auto-discovery.
//! 3. **Synthetic tests** generated on the fly for the
//!    `weavepy-conformance regrtest synth --kind …` helper. Used for
//!    quick smoke-tests in CI.
//!
//! Each test is graded with one of [`TestStatus`]:
//!
//! - `Pass`   — script ran to completion without an uncaught exception.
//! - `Fail`   — uncaught exception escaped the script.
//! - `Error`  — pre-execution failure (parse/compile/IO).
//! - `Skip`   — the expectations file marked the test as `skip`.
//! - `Timeout`— exceeded the per-test wall budget.
//!
//! The runner supports two execution modes:
//!
//! - **In-process** ([`ExecutionMode::InProcess`]). Each test gets a
//!   fresh [`weavepy::vm::Interpreter`]; reports drop straight back into
//!   the caller's [`Vec`]. Cheapest, fastest, but cannot recover from
//!   real interpreter aborts (stack overflow, abort()).
//! - **Subprocess** ([`ExecutionMode::Subprocess`]). Each test is
//!   spawned in a fresh `weavepy --run-test PATH` child process with a
//!   real wall-clock timer that SIGKILLs the worker on overrun. Much
//!   slower; survives any crash; the CPython `Lib/test/` pool always
//!   uses this mode.
//!
//! Parallelism is controlled by [`RunnerOptions::workers`]: a value of
//! `1` runs serially, anything larger spreads tests across a pool of
//! OS threads. Subprocess isolation pairs naturally with parallelism.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use weavepy::{InterpreterFlags, RunOptions};

/// Outcome of one regression test run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestStatus {
    Pass,
    Fail,
    Error,
    Skip,
    Timeout,
}

impl TestStatus {
    pub fn label(self) -> &'static str {
        match self {
            TestStatus::Pass => "pass",
            TestStatus::Fail => "fail",
            TestStatus::Error => "error",
            TestStatus::Skip => "skip",
            TestStatus::Timeout => "timeout",
        }
    }

    /// `true` when the run was a successful execution from the runner's
    /// point of view. Equivalent to `==Pass`, but spelled out so callers
    /// reading the source don't have to remember the convention.
    pub fn is_passing(self) -> bool {
        self == TestStatus::Pass
    }
}

/// Per-test record produced by the runner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestReport {
    /// Stable label, e.g. `"bundled/test_basic.py"`.
    pub label: String,
    pub status: TestStatus,
    /// Wall-clock execution time. `None` for `Skip` (we never ran it).
    pub duration_ms: Option<u128>,
    /// Free-form diagnostic detail (truncated stderr/traceback).
    pub detail: Option<String>,
    /// Status the expectations file demanded. `None` ≡ no expectation.
    pub expected: Option<TestStatus>,
}

impl TestReport {
    /// `true` iff the observed status matches the expected one. When no
    /// expectation is declared, anything but `Fail`/`Error`/`Timeout`
    /// counts as a pass (i.e. new tests default to "expect pass").
    pub fn matches_expectation(&self) -> bool {
        match self.expected {
            Some(exp) => exp == self.status,
            None => self.status == TestStatus::Pass || self.status == TestStatus::Skip,
        }
    }
}

/// Aggregated counts for a single regrtest run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegrtestSummary {
    pub total: usize,
    pub pass: usize,
    pub fail: usize,
    pub error: usize,
    pub skip: usize,
    pub timeout: usize,
    /// Tests whose observed status differed from the expectations file —
    /// the regressions that should block CI.
    pub unexpected: usize,
}

impl RegrtestSummary {
    pub fn from_reports(reports: &[TestReport]) -> Self {
        let mut s = Self::default();
        for r in reports {
            s.total += 1;
            match r.status {
                TestStatus::Pass => s.pass += 1,
                TestStatus::Fail => s.fail += 1,
                TestStatus::Error => s.error += 1,
                TestStatus::Skip => s.skip += 1,
                TestStatus::Timeout => s.timeout += 1,
            }
            if !r.matches_expectation() {
                s.unexpected += 1;
            }
        }
        s
    }
}

/// Expectations file format. Keyed by stable test label.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Expectations {
    /// Per-test expectations.
    #[serde(default)]
    pub tests: BTreeMap<String, ExpectedEntry>,
    /// Per-test wall-clock budget. Honoured only when present.
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedEntry {
    pub status: TestStatus,
    /// Human-readable reason (free-form), e.g. "blocked on UnpackEx".
    #[serde(default)]
    pub reason: Option<String>,
}

impl Expectations {
    /// Parse a TOML expectations file. Missing/empty file ≡ "everything
    /// should pass."
    pub fn load(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let body = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let parsed: Expectations = simple_toml::parse(&body)
            .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;
        Ok(parsed)
    }

    pub fn get(&self, label: &str) -> Option<TestStatus> {
        self.tests.get(label).map(|e| e.status)
    }
}

/// A single bundled test file scheduled for execution.
#[derive(Debug, Clone)]
pub struct RegrtestFile {
    pub path: PathBuf,
    pub label: String,
}

/// Discover regrtest files under `workspace_root`.
///
/// Returns the bundled tests in `tests/regrtest/` plus, when present,
/// the CPython `Lib/test/` files. CPython tests come from one of:
/// `vendor/cpython/Lib/test/`, `vendor/cpython-tests/`, or — when the
/// caller passes [`DiscoveryOptions::cpython_dir`] — an explicit
/// directory. Only the files mentioned in `expectations.toml` (or the
/// curated [`CPYTHON_REGRTEST_INCLUDE`] list) are scheduled, unless
/// the caller opts into auto-discovery via [`DiscoveryOptions::include_all_cpython`].
pub fn discover(workspace_root: &Path) -> Vec<RegrtestFile> {
    discover_with(workspace_root, &DiscoveryOptions::default(), None)
}

/// Discover regrtest files honouring the expectations file (so the
/// CPython allowlist comes from the live config rather than only the
/// hard-coded constant).
pub fn discover_with(
    workspace_root: &Path,
    opts: &DiscoveryOptions,
    expectations: Option<&Expectations>,
) -> Vec<RegrtestFile> {
    let mut out = Vec::new();

    let bundled = workspace_root.join("tests").join("regrtest");
    if bundled.is_dir() {
        collect_bundled(&bundled, &mut out);
    }

    let cpython_test = opts
        .cpython_dir
        .clone()
        .or_else(|| {
            let candidate = workspace_root
                .join("vendor")
                .join("cpython")
                .join("Lib")
                .join("test");
            candidate.is_dir().then_some(candidate)
        })
        .or_else(|| {
            let candidate = workspace_root.join("vendor").join("cpython-tests");
            candidate.is_dir().then_some(candidate)
        });

    if let Some(dir) = cpython_test {
        let mut allowlist: BTreeSet<String> = CPYTHON_REGRTEST_INCLUDE
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        if let Some(exp) = expectations {
            for label in exp.tests.keys() {
                if let Some(stripped) = label.strip_prefix("cpython/Lib/test/") {
                    allowlist.insert(stripped.to_owned());
                }
            }
        }
        if opts.include_all_cpython {
            for entry in walkdir::WalkDir::new(&dir)
                .max_depth(1)
                .into_iter()
                .filter_map(Result::ok)
            {
                let p = entry.path();
                let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if name.starts_with("test_") && name.to_ascii_lowercase().ends_with(".py") {
                    allowlist.insert(name.to_owned());
                }
            }
        }
        for name in &allowlist {
            let p = dir.join(name);
            if p.is_file() {
                out.push(RegrtestFile {
                    path: p,
                    label: format!("cpython/Lib/test/{name}"),
                });
            }
        }
    }

    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// Options that control how [`discover_with`] picks up CPython tests.
#[derive(Debug, Clone, Default)]
pub struct DiscoveryOptions {
    /// Explicit CPython `Lib/test/` directory. If unset, the runner
    /// tries `vendor/cpython/Lib/test/` then `vendor/cpython-tests/`.
    pub cpython_dir: Option<PathBuf>,
    /// When `true`, every `test_*.py` file under the chosen CPython
    /// directory is scheduled (subject to expectations). Defaults to
    /// `false` so the harness stays predictable.
    pub include_all_cpython: bool,
}

/// Curated CPython regression tests we attempt. Add to this list (and
/// `expectations.toml`) as features come online. The expectations file
/// is now the source of truth; this constant is the floor.
pub const CPYTHON_REGRTEST_INCLUDE: &[&str] = &[
    "test_grammar.py",
    "test_tokenize.py",
    "test_dict.py",
    "test_list.py",
    "test_set.py",
    "test_tuple.py",
    "test_bytes.py",
    "test_string.py",
    "test_unicode.py",
    "test_math.py",
    "test_int.py",
    "test_float.py",
    "test_complex.py",
    "test_decimal.py",
    "test_fractions.py",
    "test_collections.py",
    "test_array.py",
    "test_heapq.py",
    "test_bisect.py",
    "test_itertools.py",
    "test_functools.py",
    "test_operator.py",
    "test_copy.py",
    "test_pickle.py",
    "test_copyreg.py",
    "test_marshal.py",
    "test_re.py",
    "test_json.py",
    "test_base64.py",
    "test_binascii.py",
    "test_hashlib.py",
    "test_hmac.py",
    "test_zlib.py",
    "test_gzip.py",
    "test_bz2.py",
    "test_lzma.py",
    "test_zipfile.py",
    "test_tarfile.py",
    "test_io.py",
    "test_os.py",
    "test_posixpath.py",
    "test_pathlib.py",
    "test_tempfile.py",
    "test_glob.py",
    "test_fnmatch.py",
    "test_shutil.py",
    "test_stat.py",
    "test_textwrap.py",
    "test_string_literals.py",
    "test_format.py",
    "test_fstring.py",
    "test_class.py",
    "test_dataclass.py",
    "test_dataclasses.py",
    "test_enum.py",
    "test_inspect.py",
    "test_typing.py",
    "test_abc.py",
    "test_descr.py",
    "test_iter.py",
    "test_generators.py",
    "test_coroutines.py",
    "test_asyncgen.py",
    "test_with.py",
    "test_exceptions.py",
    "test_traceback.py",
    "test_warnings.py",
    "test_contextlib.py",
    "test_contextlib_async.py",
    "test_contextvars.py",
    "test_keywordonlyarg.py",
    "test_unpack.py",
    "test_unpack_ex.py",
    "test_args.py",
    "test_compile.py",
    "test_decorators.py",
    "test_assert.py",
    "test_audit.py",
    "test_call.py",
    "test_isinstance.py",
    "test_subclassinit.py",
    "test_typing_extensions.py",
    "test_threading.py",
    "test_thread.py",
    "test_threadedtempfile.py",
    "test_threadsignals.py",
    "test_gc.py",
    "test_weakref.py",
    "test_weakset.py",
    "test_socket.py",
    "test_subprocess.py",
    "test_select.py",
    "test_signal.py",
    "test_ssl.py",
    "test_urllib.py",
    "test_urllib2.py",
    "test_urlparse.py",
    "test_http_cookiejar.py",
    "test_http_cookies.py",
    "test_httplib.py",
    "test_logging.py",
    "test_csv.py",
    "test_sqlite3.py",
    "test_xml_etree.py",
    "test_xml_etree_c.py",
    "test_html.py",
    "test_email.py",
    "test_mimetypes.py",
    "test_locale.py",
    "test_calendar.py",
    "test_time.py",
    "test_datetime.py",
    "test_zoneinfo.py",
    "test_struct.py",
    "test_codecs.py",
    "test_bigaddrspace.py",
    "test_bytecodes.py",
    "test_dis.py",
    "test_audit_class.py",
    "test_descrtut.py",
    "test_grammar.py",
    "test_optparse.py",
    "test_getopt.py",
    "test_argparse.py",
    "test_tomllib.py",
    "test_pprint.py",
    "test_pdb.py",
    "test_bdb.py",
    "test_pkgutil.py",
    "test_importlib.py",
    "test_importlib_metadata.py",
    "test_importlib_resources.py",
    "test_runpy.py",
    "test_atexit.py",
    "test_resource.py",
    "test_fcntl.py",
    "test_posix.py",
    "test_uuid.py",
    "test_secrets.py",
    "test_hmac.py",
    "test_random.py",
    "test_statistics.py",
    "test_numeric_tower.py",
    "test_unicodedata.py",
    "test_unicode_identifiers.py",
    "test_string.py",
    "test_complex.py",
    "test_multiprocessing_main_handling.py",
    "test_multiprocessing_fork.py",
    "test_multiprocessing_spawn.py",
    "test_multiprocessing_forkserver.py",
    "test_concurrent_futures.py",
    "test_asyncio.py",
    "test_queue.py",
    "test_concurrent_collections.py",
    "test_sched.py",
    "test_selectors.py",
    "test_socketserver.py",
    "test_smtplib.py",
    "test_poplib.py",
    "test_imaplib.py",
    "test_nntplib.py",
    "test_ftplib.py",
    "test_telnetlib.py",
    "test_socket_ipv6.py",
];

fn collect_bundled(root: &Path, out: &mut Vec<RegrtestFile>) {
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n,
            None => continue,
        };
        let is_py = Path::new(name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("py"));
        if !name.starts_with("test_") || !is_py {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(path);
        out.push(RegrtestFile {
            path: path.to_path_buf(),
            label: format!("bundled/{}", rel.display()),
        });
    }
}

/// How tests should be executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionMode {
    /// Run each test inside a fresh [`weavepy::vm::Interpreter`] in
    /// the current process. Cheapest mode; the wall budget is honoured
    /// politely (the next opcode dispatch tips us out of a runaway
    /// loop) but a real crash kills the runner.
    #[default]
    InProcess,
    /// Spawn each test in a `weavepy` subprocess. The wall budget is
    /// enforced by SIGKILL; a crash (panic, abort) is captured as
    /// `Error`. Slower but bulletproof.
    Subprocess,
}

/// Runner knobs.
#[derive(Debug, Clone)]
pub struct RunnerOptions {
    pub timeout: Duration,
    pub mode: ExecutionMode,
    /// Number of worker threads to use. `1` runs serially.
    pub workers: usize,
    /// Path to the `weavepy` binary used for [`ExecutionMode::Subprocess`].
    /// When `None`, the runner falls back to `std::env::current_exe()`.
    pub weavepy_binary: Option<PathBuf>,
    /// When `true`, the per-test result is printed to stderr as it
    /// completes (useful while a long CPython run is in flight).
    pub stream_results: bool,
}

impl Default for RunnerOptions {
    fn default() -> Self {
        RunnerOptions {
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            mode: ExecutionMode::InProcess,
            workers: 1,
            weavepy_binary: None,
            stream_results: false,
        }
    }
}

/// Drive every discovered regrtest file and grade against `expectations`.
///
/// Honours [`RunnerOptions::workers`] for parallelism. Tests are
/// scheduled in input order; results come back in label order so the
/// rendered report is stable.
pub fn run_all(
    files: &[RegrtestFile],
    expectations: &Expectations,
    timeout: Duration,
) -> Vec<TestReport> {
    let opts = RunnerOptions {
        timeout,
        ..RunnerOptions::default()
    };
    run_all_with(files, expectations, &opts)
}

/// Like [`run_all`] but with explicit runner options.
pub fn run_all_with(
    files: &[RegrtestFile],
    expectations: &Expectations,
    opts: &RunnerOptions,
) -> Vec<TestReport> {
    if files.is_empty() {
        return Vec::new();
    }
    if opts.workers <= 1 {
        return files
            .iter()
            .map(|f| run_one_with(f, expectations, opts))
            .collect();
    }
    // Parallel dispatch. Each worker pulls the next index off a
    // shared counter; the report buffer is filled in label order so
    // the consumer sees a deterministic sequence.
    let total = files.len();
    let cursor = Arc::new(Mutex::new(0usize));
    let reports: Arc<Mutex<Vec<Option<TestReport>>>> =
        Arc::new(Mutex::new((0..total).map(|_| None).collect()));
    std::thread::scope(|scope| {
        let n = opts.workers.min(total);
        for _ in 0..n {
            let cursor = cursor.clone();
            let reports = reports.clone();
            scope.spawn(move || loop {
                let idx = {
                    let mut c = cursor.lock().unwrap();
                    if *c >= total {
                        return;
                    }
                    let i = *c;
                    *c += 1;
                    i
                };
                let report = run_one_with(&files[idx], expectations, opts);
                if opts.stream_results {
                    eprintln!(
                        "[{}/{}] {} -> {}",
                        idx + 1,
                        total,
                        report.label,
                        report.status.label()
                    );
                }
                reports.lock().unwrap()[idx] = Some(report);
            });
        }
    });
    let mut buffer = reports.lock().unwrap();
    buffer.iter_mut().filter_map(|r| r.take()).collect()
}

/// Backward-compat wrapper: drive one regrtest file through the
/// in-process VM with the default options.
pub fn run_one(file: &RegrtestFile, expectations: &Expectations, timeout: Duration) -> TestReport {
    let opts = RunnerOptions {
        timeout,
        ..RunnerOptions::default()
    };
    run_one_with(file, expectations, &opts)
}

/// Drive one regression test, honouring `opts.mode`.
pub fn run_one_with(
    file: &RegrtestFile,
    expectations: &Expectations,
    opts: &RunnerOptions,
) -> TestReport {
    let expected = expectations.get(&file.label);

    if expected == Some(TestStatus::Skip) {
        return TestReport {
            label: file.label.clone(),
            status: TestStatus::Skip,
            duration_ms: None,
            detail: expectations
                .tests
                .get(&file.label)
                .and_then(|e| e.reason.clone()),
            expected,
        };
    }

    match opts.mode {
        ExecutionMode::InProcess => run_inprocess(file, expected, opts.timeout),
        ExecutionMode::Subprocess => run_subprocess(file, expected, opts),
    }
}

fn run_inprocess(
    file: &RegrtestFile,
    expected: Option<TestStatus>,
    timeout: Duration,
) -> TestReport {
    let source = match fs::read_to_string(&file.path) {
        Ok(s) => s,
        Err(e) => {
            return TestReport {
                label: file.label.clone(),
                status: TestStatus::Error,
                duration_ms: Some(0),
                detail: Some(format!("read failed: {e}")),
                expected,
            };
        }
    };

    let opts = RunOptions::new(file.path.display().to_string())
        .with_argv(vec![file.path.display().to_string()])
        .with_script_dir(
            file.path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from(".")),
        )
        .with_flags(InterpreterFlags::default());

    let start = Instant::now();
    let result = weavepy::run_source_with_options(&source, &opts);
    let elapsed = start.elapsed();

    let (status, detail) = if elapsed > timeout {
        (TestStatus::Timeout, Some(format!("budget {timeout:?}")))
    } else {
        match result {
            Ok(()) => (TestStatus::Pass, None),
            Err(err) => match &err {
                weavepy::Error::Parse(_) | weavepy::Error::Compile(_) => {
                    let msg = err.format(&source, &opts.filename);
                    (TestStatus::Error, Some(truncate_detail(&msg)))
                }
                weavepy::Error::Runtime(_) => {
                    let msg = err.format(&source, &opts.filename);
                    (TestStatus::Fail, Some(truncate_detail(&msg)))
                }
            },
        }
    };

    TestReport {
        label: file.label.clone(),
        status,
        duration_ms: Some(elapsed.as_millis()),
        detail,
        expected,
    }
}

fn run_subprocess(
    file: &RegrtestFile,
    expected: Option<TestStatus>,
    runner: &RunnerOptions,
) -> TestReport {
    let weavepy_bin = runner
        .weavepy_binary
        .clone()
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from("weavepy"));
    let start = Instant::now();
    let mut cmd = std::process::Command::new(&weavepy_bin);
    cmd.arg(&file.path);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.env("WEAVEPY_REGRTEST_CHILD", "1");
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return TestReport {
                label: file.label.clone(),
                status: TestStatus::Error,
                duration_ms: Some(0),
                detail: Some(format!("spawn failed: {e}")),
                expected,
            };
        }
    };

    let outcome = wait_with_timeout(child, runner.timeout);
    let elapsed = start.elapsed();
    let label = file.label.clone();
    match outcome {
        ChildOutcome::Exited(status, stdout, stderr) => {
            let detail = if !stderr.trim().is_empty() {
                Some(truncate_detail(&stderr))
            } else if !stdout.trim().is_empty() {
                Some(truncate_detail(&stdout))
            } else {
                None
            };
            let test_status = if status.success() {
                TestStatus::Pass
            } else if matches!(status.code(), Some(0)) {
                TestStatus::Pass
            } else if let Some(code) = status.code() {
                if code == 2 {
                    TestStatus::Error
                } else {
                    TestStatus::Fail
                }
            } else {
                TestStatus::Fail
            };
            TestReport {
                label,
                status: test_status,
                duration_ms: Some(elapsed.as_millis()),
                detail,
                expected,
            }
        }
        ChildOutcome::TimedOut => TestReport {
            label,
            status: TestStatus::Timeout,
            duration_ms: Some(elapsed.as_millis()),
            detail: Some(format!("killed after {:?}", runner.timeout)),
            expected,
        },
        ChildOutcome::IoError(msg) => TestReport {
            label,
            status: TestStatus::Error,
            duration_ms: Some(elapsed.as_millis()),
            detail: Some(msg),
            expected,
        },
    }
}

enum ChildOutcome {
    Exited(std::process::ExitStatus, String, String),
    TimedOut,
    IoError(String),
}

/// Wait up to `timeout` for `child` to exit. If it doesn't, SIGKILL the
/// child and return [`ChildOutcome::TimedOut`].
fn wait_with_timeout(mut child: std::process::Child, timeout: Duration) -> ChildOutcome {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut s) = child.stdout.take() {
                    let _ = s.read_to_string(&mut stdout);
                }
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_string(&mut stderr);
                }
                return ChildOutcome::Exited(status, stdout, stderr);
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return ChildOutcome::TimedOut;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return ChildOutcome::IoError(format!("waitpid: {e}")),
        }
    }
}

fn truncate_detail(msg: &str) -> String {
    const LIMIT: usize = 1024;
    if msg.len() <= LIMIT {
        msg.to_owned()
    } else {
        let mut s = String::with_capacity(LIMIT + 16);
        s.push_str(&msg[..LIMIT]);
        s.push_str("…[truncated]");
        s
    }
}

/// Render the report as a Markdown table for `report.md`.
pub fn report_to_markdown(reports: &[TestReport]) -> String {
    let summary = RegrtestSummary::from_reports(reports);
    let mut out = String::new();
    let _ = writeln!(out, "# WeavePy regrtest");
    let _ = writeln!(
        out,
        "{} total — pass {} / fail {} / error {} / skip {} / timeout {} — unexpected {}",
        summary.total,
        summary.pass,
        summary.fail,
        summary.error,
        summary.skip,
        summary.timeout,
        summary.unexpected,
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "| Test | Status | Expected | Time (ms) | Note |");
    let _ = writeln!(out, "|------|--------|----------|-----------|------|");
    for r in reports {
        let exp = r
            .expected
            .map(|s| s.label().to_owned())
            .unwrap_or_else(|| "—".to_owned());
        let dur = r
            .duration_ms
            .map(|m| m.to_string())
            .unwrap_or_else(|| "—".to_owned());
        let mark = if r.matches_expectation() { "" } else { " ❗" };
        let detail = r
            .detail
            .as_deref()
            .map(|s| s.lines().next().unwrap_or("").replace('|', "\\|"))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "| `{}` | {}{} | {} | {} | {} |",
            r.label,
            r.status.label(),
            mark,
            exp,
            dur,
            detail
        );
    }
    out
}

/// Default per-test wall budget, in seconds. Tests under
/// `tests/regrtest/` should run in well under one second; CPython
/// `Lib/test/` modules need more headroom but the runner is still
/// expected to make forward progress on every opcode.
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Tiny TOML subset parser. Enough for our `expectations.toml`
/// shape (top-level keys + `[tests.<id>]` sections with `status`/
/// `reason`) without pulling in the full `toml` crate (which would
/// add ~50 KB to the conformance binary). If we ever need richer
/// TOML, swap this out.
mod simple_toml {
    use std::collections::BTreeMap;

    use super::{Expectations, ExpectedEntry, TestStatus};

    pub(super) fn parse(body: &str) -> Result<Expectations, String> {
        let mut top = Expectations::default();
        let mut current_section: Option<String> = None;
        let mut current_table: BTreeMap<String, String> = BTreeMap::new();

        for (lineno, raw_line) in body.lines().enumerate() {
            let line = strip_comment(raw_line).trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                flush(&mut top, current_section.take(), &mut current_table)?;
                let header = &line[1..line.len() - 1];
                current_section = Some(header.to_owned());
                continue;
            }
            let (k, v) = parse_kv(line, lineno)?;
            if current_section.is_some() {
                current_table.insert(k, v);
            } else if k == "timeout_seconds" {
                let n: u64 = v
                    .parse()
                    .map_err(|_| format!("line {}: bad timeout", lineno + 1))?;
                top.timeout_seconds = Some(n);
            }
        }
        flush(&mut top, current_section, &mut current_table)?;
        Ok(top)
    }

    fn flush(
        top: &mut Expectations,
        section: Option<String>,
        table: &mut BTreeMap<String, String>,
    ) -> Result<(), String> {
        let Some(section) = section else {
            table.clear();
            return Ok(());
        };
        let raw_label = section
            .strip_prefix("tests.")
            .ok_or_else(|| format!("unknown section [{section}]"))?;
        // `tests."bundled/foo.py"` → `bundled/foo.py`
        let label = strip_quotes(raw_label.trim()).to_owned();
        let status = table
            .get("status")
            .ok_or_else(|| format!("[tests.{label}] missing status"))?;
        let status = match status.as_str() {
            "pass" => TestStatus::Pass,
            "fail" => TestStatus::Fail,
            "error" => TestStatus::Error,
            "skip" => TestStatus::Skip,
            "timeout" => TestStatus::Timeout,
            other => return Err(format!("[tests.{label}] bad status {other:?}")),
        };
        let reason = table.get("reason").cloned();
        top.tests.insert(label, ExpectedEntry { status, reason });
        table.clear();
        Ok(())
    }

    fn parse_kv(line: &str, lineno: usize) -> Result<(String, String), String> {
        let eq = line
            .find('=')
            .ok_or_else(|| format!("line {}: no `=` in {line:?}", lineno + 1))?;
        let key = line[..eq].trim().to_owned();
        let val = line[eq + 1..].trim();
        let val = strip_quotes(val);
        Ok((key, val.to_owned()))
    }

    fn strip_quotes(s: &str) -> &str {
        if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
            || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
        {
            &s[1..s.len() - 1]
        } else {
            s
        }
    }

    fn strip_comment(line: &str) -> &str {
        if let Some(idx) = line.find('#') {
            // Naive — assumes `#` never appears inside quoted strings,
            // which holds for our expectations file.
            &line[..idx]
        } else {
            line
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_minimal_expectations() {
            let body = "\
                timeout_seconds = 5\n\
                \n\
                [tests.\"bundled/test_basic.py\"]\n\
                status = \"pass\"\n\
                \n\
                [tests.\"cpython/Lib/test/test_grammar.py\"]\n\
                status = \"fail\"\n\
                reason = \"top-level await unsupported\"\n\
            ";
            let exp = parse(body).unwrap();
            assert_eq!(exp.timeout_seconds, Some(5));
            assert_eq!(exp.get("bundled/test_basic.py"), Some(TestStatus::Pass));
            assert_eq!(
                exp.get("cpython/Lib/test/test_grammar.py"),
                Some(TestStatus::Fail)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_counts_each_status() {
        let r = vec![
            TestReport {
                label: "a".into(),
                status: TestStatus::Pass,
                duration_ms: Some(1),
                detail: None,
                expected: Some(TestStatus::Pass),
            },
            TestReport {
                label: "b".into(),
                status: TestStatus::Fail,
                duration_ms: Some(2),
                detail: None,
                expected: Some(TestStatus::Pass),
            },
            TestReport {
                label: "c".into(),
                status: TestStatus::Skip,
                duration_ms: None,
                detail: None,
                expected: Some(TestStatus::Skip),
            },
        ];
        let s = RegrtestSummary::from_reports(&r);
        assert_eq!(s.total, 3);
        assert_eq!(s.pass, 1);
        assert_eq!(s.fail, 1);
        assert_eq!(s.skip, 1);
        assert_eq!(s.unexpected, 1);
    }

    #[test]
    fn missing_expectations_default_to_pass() {
        let r = TestReport {
            label: "new".into(),
            status: TestStatus::Pass,
            duration_ms: Some(0),
            detail: None,
            expected: None,
        };
        assert!(r.matches_expectation());
    }

    #[test]
    fn missing_expectations_flag_failures() {
        let r = TestReport {
            label: "new".into(),
            status: TestStatus::Fail,
            duration_ms: Some(0),
            detail: None,
            expected: None,
        };
        assert!(!r.matches_expectation());
    }
}
