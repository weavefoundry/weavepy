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
    /// RFC 0063 WS7: the OSes this baseline was *measured* on
    /// (top-level `measured_os = ["macos", "linux"]`, spelled like
    /// `std::env::consts::OS` — the same names the per-OS suffix keys
    /// use). On a host OS not in the stamp, a `--check` run still
    /// prints the full report and writes results, but unexpected
    /// results are advisory (a NOTE line, exit 0) until a measured
    /// baseline for that OS lands and its name joins the stamp.
    /// `None` (no stamp in the file) means "all OSes measured" —
    /// the pre-RFC-0063 behaviour.
    #[serde(default)]
    pub measured_os: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedEntry {
    pub status: TestStatus,
    /// Human-readable reason (free-form), e.g. "blocked on UnpackEx".
    #[serde(default)]
    pub reason: Option<String>,
    /// Per-test wall-clock budget override, in seconds. When present it
    /// supersedes the global [`Expectations::timeout_seconds`] for this
    /// test only. Used for correct-but-slow tests whose *verdict* is
    /// stable but whose WeavePy wall-time sits near the global budget —
    /// e.g. an O(n^2) Python-callback hash-collision stress — so a
    /// loaded/thermally-throttled host would otherwise SIGKILL them
    /// mid-run and flip a `pass`/`fail` into a spurious `timeout`. This
    /// only ever *raises* headroom; a genuine runaway still trips the
    /// (larger) budget.
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
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

    /// RFC 0063 WS7: `true` when `host_os` has a measured baseline in
    /// this file — i.e. the `measured_os` stamp names it, or the file
    /// carries no stamp at all (missing stamp ≡ "all OSes measured",
    /// preserving pre-RFC-0063 behaviour).
    pub fn os_is_measured(&self, host_os: &str) -> bool {
        match &self.measured_os {
            Some(stamp) => stamp.iter().any(|os| os == host_os),
            None => true,
        }
    }
}

/// RFC 0063 WS7 — resolve the `--check` gate against the `measured_os`
/// stamp for the current host. Returns `true` when unexpected results
/// should fail the run (measured host); on an unmeasured host it prints
/// a clearly-labelled advisory NOTE instead and returns `false`, so the
/// caller exits 0 with the full report/artifacts already written.
pub fn strict_gate_blocks(expectations: &Expectations, summary: &RegrtestSummary) -> bool {
    strict_gate_blocks_for_os(expectations, summary, std::env::consts::OS)
}

/// Host-OS-explicit seam for [`strict_gate_blocks`], unit-testable on
/// every platform.
fn strict_gate_blocks_for_os(
    expectations: &Expectations,
    summary: &RegrtestSummary,
    host_os: &str,
) -> bool {
    if summary.unexpected == 0 {
        return false;
    }
    if expectations.os_is_measured(host_os) {
        return true;
    }
    eprintln!(
        "NOTE: {host_os} is not in measured_os; {} unexpected result(s) reported, \
         gate is advisory until a measured baseline lands (RFC 0063)",
        summary.unexpected
    );
    false
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
                if !name.starts_with("test_") {
                    continue;
                }
                if p.is_file() && name.to_ascii_lowercase().ends_with(".py") {
                    allowlist.insert(name.to_owned());
                } else if p.is_dir() && p.join("__init__.py").is_file() {
                    // Test *packages* (`test_asyncio/`, `test_json/`, …) are
                    // scheduled under the same `<name>.py` label convention
                    // the curated allowlist uses, so an expectations row keys
                    // identically whether the target is a file or a package.
                    allowlist.insert(format!("{name}.py"));
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
                continue;
            }
            // Some regression tests are *packages* (`test_dataclasses/`
            // with an `__init__.py`). When the package is a plain
            // container of `test_*.py` submodules (RFC 0054 WS4), grade
            // each submodule as its own labelled row —
            // `cpython/Lib/test/test_asyncio/test_futures.py` — so one
            // hanging/failing file no longer poisons 30 green siblings
            // and each row gets its own timeout budget. Packages whose
            // `load_tests` *composes* something beyond the directory scan
            // (test_json parametrizes shared cases across C/pure
            // variants) keep the single package label.
            let pkg_dir = dir.join(name.trim_end_matches(".py"));
            let pkg_init = pkg_dir.join("__init__.py");
            if pkg_init.is_file() {
                if let Some(children) = package_test_children(&pkg_dir) {
                    let pkg = name.trim_end_matches(".py");
                    for child in children {
                        out.push(RegrtestFile {
                            path: pkg_dir.join(&child),
                            label: format!("cpython/Lib/test/{pkg}/{child}"),
                        });
                    }
                } else {
                    out.push(RegrtestFile {
                        path: pkg_init,
                        label: format!("cpython/Lib/test/{name}"),
                    });
                }
            }
        }
    }

    out.sort_by(|a, b| a.label.cmp(&b.label));
    out.dedup_by(|a, b| a.label == b.label);
    out
}

/// Test packages graded one row per `test_*.py` submodule instead of as a
/// single unit. RFC 0054 WS4 introduced this for `test_asyncio`: its ~40
/// submodules are independent files where one hanging/failing module
/// poisoned 30 green siblings and a single timeout budget. The list is
/// deliberately explicit rather than shape-driven — the rest of the
/// conformance baseline (`expectations.toml`) is expressed at package
/// granularity, with statuses and timeout budgets measured for the package
/// as a unit (`test_multiprocessing_spawn` passes as one 1200s row);
/// silently expanding every `load_package_tests` delegator would orphan
/// those rows and re-grade the submodules against default budgets. Add a
/// package here together with its per-submodule expectations rows.
const EXPANDED_PACKAGES: &[&str] = &["test_asyncio"];

/// RFC 0054 WS4: one row per `test_*.py` submodule for the packages in
/// [`EXPANDED_PACKAGES`]. The package must also be a plain delegation to
/// `test.support.load_package_tests` that passes the caller's `tests`
/// through unchanged — meaning it is nothing more than a directory of
/// independently loadable test files.
fn package_test_children(pkg_dir: &Path) -> Option<Vec<String>> {
    let pkg_name = pkg_dir.file_name()?.to_str()?;
    if !EXPANDED_PACKAGES.contains(&pkg_name) {
        return None;
    }
    let init = fs::read_to_string(pkg_dir.join("__init__.py")).ok()?;
    if !init.contains("load_package_tests") {
        return None;
    }
    let delegates = init.contains("load_package_tests(os.path.dirname(__file__), *args)")
        || init.contains("load_package_tests(pkg_dir, loader, tests, pattern)");
    if !delegates {
        return None;
    }
    let mut children: Vec<String> = fs::read_dir(pkg_dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .filter(|n| {
            n.starts_with("test_")
                && Path::new(n)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("py"))
        })
        .collect();
    if children.is_empty() {
        return None;
    }
    children.sort();
    Some(children)
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
    "test_poll.py",
    "test_kqueue.py",
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

    // A per-test `timeout_seconds` override raises the wall budget for
    // this label only (correct-but-slow tests near the global budget).
    let eff_timeout = expectations
        .tests
        .get(&file.label)
        .and_then(|e| e.timeout_seconds)
        .map(Duration::from_secs)
        .unwrap_or(opts.timeout);

    match opts.mode {
        ExecutionMode::InProcess => run_inprocess(file, expected, eff_timeout),
        ExecutionMode::Subprocess => {
            if eff_timeout == opts.timeout {
                run_subprocess(file, expected, opts)
            } else {
                let scoped = RunnerOptions {
                    timeout: eff_timeout,
                    ..opts.clone()
                };
                run_subprocess(file, expected, &scoped)
            }
        }
    }
}

/// For vendored CPython tests, build a libregrtest-style bootstrap:
/// import the file as `test.<name>` (so `__name__`/`__module__` match
/// what CPython's test runner produces — `global_enum` reprs, pickling
/// of test-defined classes, …) and run its unittest suite explicitly,
/// since the `if __name__ == '__main__'` guard never fires on import.
///
/// Returns `None` for bundled tests, which keep script semantics.
/// The external CPython `Lib` directory for a vendored test file: the
/// ancestor whose child is the `test` package the file lives in. This is
/// the directory that must be on `sys.path` (and exported via
/// `WEAVEPY_CPYTHON_LIB` so spawned child interpreters inherit it) for
/// `import test.<name>` to resolve.
fn cpython_lib_dir(file: &RegrtestFile) -> Option<String> {
    file.label.strip_prefix("cpython/Lib/test/")?;
    let mut lib_dir = file.path.parent()?;
    while lib_dir.file_name().and_then(|n| n.to_str()) != Some("test") {
        lib_dir = lib_dir.parent()?;
    }
    Some(sanitized_lib_dir(lib_dir.parent()?))
}

/// Guard against site hooks living next to the vendored `test` package.
///
/// A dev machine often points `vendor/cpython/Lib` at a *live* install
/// (a Homebrew stdlib symlink). Homebrew ships `sitecustomize.py`
/// there, and since the lib dir lands on every child interpreter's
/// `sys.path` (via `WEAVEPY_CPYTHON_LIB`), `site` would import it at
/// each startup — dragging `re` + Homebrew site-packages into pristine
/// `-I` children and failing test_site's `test_startup_imports`. When
/// hooks are present, mirror the lib dir through a shim directory of
/// per-entry symlinks that omits only the hook files, so module
/// resolution (`sched`, `tabnanny`, … resolve from the vendored Lib)
/// matches a clean checkout. Clean checkouts (CI) are returned
/// unchanged.
fn sanitized_lib_dir(lib_dir: &Path) -> String {
    let raw = lib_dir.display().to_string();
    if !lib_dir.join("sitecustomize.py").is_file() && !lib_dir.join("usercustomize.py").is_file() {
        return raw;
    }
    #[cfg(unix)]
    {
        use std::hash::{Hash, Hasher};
        let mut h = std::hash::DefaultHasher::new();
        raw.hash(&mut h);
        // "v2": the mirror shim (a stale v1 shim held only `test`).
        let shim = std::env::temp_dir().join(format!("weavepy-cpython-lib-v2-{:016x}", h.finish()));
        let done = shim.join(".weavepy-shim-complete");
        if !done.is_file() {
            // Populate a staging dir, then rename into place so parallel
            // workers never observe a half-built shim; the loser of the
            // rename race just discards its staging copy.
            let stage = shim.with_extension(format!("stage-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&stage);
            if std::fs::create_dir_all(&stage).is_ok() {
                if let Ok(entries) = std::fs::read_dir(lib_dir) {
                    for entry in entries.flatten() {
                        let name = entry.file_name();
                        if name == "sitecustomize.py" || name == "usercustomize.py" {
                            continue;
                        }
                        let _ = std::os::unix::fs::symlink(entry.path(), stage.join(&name));
                    }
                }
                let _ = std::fs::File::create(stage.join(".weavepy-shim-complete"));
                if std::fs::rename(&stage, &shim).is_err() {
                    let _ = std::fs::remove_dir_all(&stage);
                }
            }
        }
        if done.is_file() {
            return shim.display().to_string();
        }
    }
    raw
}

fn libregrtest_bootstrap(file: &RegrtestFile) -> Option<String> {
    // Package submodule labels (`test_asyncio/test_futures.py`, RFC 0054
    // WS4) import as dotted module paths: `test.test_asyncio.test_futures`.
    let name = file
        .label
        .strip_prefix("cpython/Lib/test/")?
        .trim_end_matches(".py")
        .replace('/', ".");
    let lib_dir = cpython_lib_dir(file)?;
    let path = file.path.display().to_string();
    Some(format!(
        r#"
import sys, os
# The runner also exports WEAVEPY_CPYTHON_LIB, which lands the same
# directory on the default path — keep exactly one copy, at the front
# (a duplicate breaks test_venv's stdlib-copying walk of sys.path).
_lib = {lib_dir:?}
try:
    sys.path.remove(_lib)
except ValueError:
    pass
sys.path.insert(0, _lib)
del _lib
sys.argv = [{path:?}]
import unittest
# Run inside a fresh scratch working directory, like libregrtest's per-worker
# `temp_cwd`. CPython's suite assumes a disposable cwd: `test.support.os_helper`
# drops `@test_<pid>_tmpæ` files in it, and test_zipfile's extraction tests
# `rmtree('target')` — which, run from the workspace root, deleted the *build
# tree* (target/release/weavepy) mid-sweep and broke every later test that
# re-execs sys.executable.
import tempfile, shutil, atexit
_scratch = tempfile.mkdtemp(prefix="weavepy_regrtest_")
os.chdir(_scratch)
atexit.register(shutil.rmtree, _scratch, True)
# Enable the test-resource model the way libregrtest's `-u` flag would, so
# `support.requires('network')` / `@requires_resource('network')` exercise the
# loopback subset instead of raising ResourceDenied. Driven by
# WEAVEPY_REGRTEST_RESOURCES (comma-separated).
#
# The default is deliberately minimal: only `network` (the loopback-safe
# subset — never reaches an external host, no `urlfetch`) and `subprocess`,
# which are what the RFC 0042 networking suites need to grade instead of
# skip. We intentionally do NOT enable `cpu`/`walltime`/`decimal`/`tzdata`
# here: those gate slow, host-sensitive stress cases (e.g. `math`'s
# `sumprod` ULP stress, `io`'s PEP 475 EINTR signal retries, the
# `datetime`/`json`/`statistics` walltime sweeps) that the checked-in
# baseline was calibrated to skip. Turning them on surfaces pre-existing,
# non-networking failures/timeouts unrelated to this harness's job. A caller
# can still opt in via WEAVEPY_REGRTEST_RESOURCES.
_res = os.environ.get("WEAVEPY_REGRTEST_RESOURCES")
_reslist = [r for r in _res.split(",") if r] if _res else [
    "network", "subprocess",
]
try:
    import test.support as _support
    _support.use_resources = _reslist
except Exception:
    pass
try:
    mod = __import__("test.{name}", fromlist=["__spec__"])
except unittest.SkipTest as e:
    print("skipped:", e)
    sys.exit(0)
suite = unittest.TestLoader().loadTestsFromModule(mod)
result = unittest.TextTestRunner(verbosity=1).run(suite)
sys.exit(0 if result.wasSuccessful() else 1)
"#
    ))
}

fn run_inprocess(
    file: &RegrtestFile,
    expected: Option<TestStatus>,
    timeout: Duration,
) -> TestReport {
    let source = match libregrtest_bootstrap(file) {
        Some(bootstrap) => bootstrap,
        None => match fs::read_to_string(&file.path) {
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
        },
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
            Err(err) => {
                // A `SystemExit` reaching the top level is how
                // `unittest.main()` / `libregrtest` report their verdict
                // (code 0 / None ≡ success, anything else ≡ failure).
                // Grade it the same way the CLI and the subprocess path
                // do, rather than calling every `sys.exit()` a `Fail`.
                if let Some(code) = err.system_exit_code() {
                    if system_exit_is_success(&code) {
                        (TestStatus::Pass, None)
                    } else {
                        let msg = err.format(&source, &opts.filename);
                        (TestStatus::Fail, Some(truncate_detail(&msg)))
                    }
                } else {
                    match &err {
                        weavepy::Error::Parse(_) | weavepy::Error::Compile(_) => {
                            let msg = err.format(&source, &opts.filename);
                            (TestStatus::Error, Some(truncate_detail(&msg)))
                        }
                        weavepy::Error::Runtime(_) | weavepy::Error::RuntimePrinted(_) => {
                            let msg = err.format(&source, &opts.filename);
                            (TestStatus::Fail, Some(truncate_detail(&msg)))
                        }
                    }
                }
            }
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
    match libregrtest_bootstrap(file) {
        Some(bootstrap) => {
            cmd.arg("-c").arg(bootstrap);
        }
        None => {
            cmd.arg(&file.path);
        }
    }
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.env("WEAVEPY_REGRTEST_CHILD", "1");
    // Put the child in its own process group so a timeout can SIGKILL the whole
    // tree, not just the direct child. Tests that re-exec `weavepy` (subprocess,
    // multiprocessing spawn/forkserver — which leave a `resource_tracker` and
    // pool workers running) otherwise leak grandchildren that *inherit the
    // stdout/stderr pipe*; killing only the parent leaves the pipe's write end
    // open, so the draining `read_to_end` never sees EOF and the runner hangs.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    // Resource model for the child (and any further weavepy children it spawns):
    // enable the loopback-safe `-u` set so `requires('network')` grades instead
    // of skipping. Honors a caller-provided override.
    cmd.env(
        "WEAVEPY_REGRTEST_RESOURCES",
        std::env::var("WEAVEPY_REGRTEST_RESOURCES")
            .unwrap_or_else(|_| "network,subprocess".to_owned()),
    );
    // Export the external CPython `Lib` dir so child interpreters spawned
    // by the test (`assert_python_ok`, `multiprocessing` spawn,
    // `subprocess` re-execs) inherit it on their default `sys.path` even
    // under `-I`/`-E` (which strip `PYTHON*` but not this).
    if let Some(lib_dir) = cpython_lib_dir(file) {
        cmd.env("WEAVEPY_CPYTHON_LIB", lib_dir);
    }
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
///
/// stdout/stderr are drained on dedicated threads from the moment the
/// child starts. Reading only *after* the child exits (the obvious
/// approach) deadlocks against any child that writes more than one pipe
/// buffer's worth of output (~64 KiB): the child blocks in `write()`
/// waiting for us to read, while we block in `wait()` waiting for it to
/// exit. A `unittest` file with hundreds of failing assertions trips this
/// instantly, so the reader threads are load-bearing for subprocess mode.
fn wait_with_timeout(mut child: std::process::Child, timeout: Duration) -> ChildOutcome {
    fn drain(
        pipe: Option<impl std::io::Read + Send + 'static>,
    ) -> Option<std::thread::JoinHandle<Vec<u8>>> {
        pipe.map(|mut s| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = s.read_to_end(&mut buf);
                buf
            })
        })
    }
    fn collect(handle: Option<std::thread::JoinHandle<Vec<u8>>>) -> String {
        handle
            .and_then(|h| h.join().ok())
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
    }

    let out_handle = drain(child.stdout.take());
    let err_handle = drain(child.stderr.take());
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return ChildOutcome::Exited(status, collect(out_handle), collect(err_handle));
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    // SIGKILL the *whole process group* (the child is a group
                    // leader, see `spawn_subprocess`) so any grandchildren that
                    // re-exec'd `weavepy` (multiprocessing resource_tracker /
                    // pool workers, subprocess re-execs) die too and release the
                    // inherited stdout/stderr pipe — otherwise `read_to_end`
                    // below never reaches EOF and the runner hangs.
                    #[cfg(unix)]
                    unsafe {
                        libc::kill(-(child.id() as i32), libc::SIGKILL);
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    // Join the readers so the threads don't outlive us;
                    // the pipes close once the whole group is gone, so they
                    // return promptly.
                    let _ = collect(out_handle);
                    let _ = collect(err_handle);
                    return ChildOutcome::TimedOut;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return ChildOutcome::IoError(format!("waitpid: {e}")),
        }
    }
}

/// `true` when a `SystemExit` payload means "success" — mirroring the
/// CLI's `exit_with_system_exit` mapping: `None` / `False` / `0` / an
/// empty string are a clean exit; everything else is a failure.
fn system_exit_is_success(code: &weavepy::vm::object::Object) -> bool {
    use weavepy::vm::object::Object;
    match code {
        Object::None => true,
        Object::Bool(b) => !b,
        // Mirror the CLI's `exit_with_system_exit`: an int code becomes the
        // OS exit status `n & 0xFF`, so the subprocess path (where the OS
        // truncates to 8 bits) and this in-process path agree that e.g.
        // `sys.exit(256)` is a clean exit. The explicit mask reads clearer
        // here than clippy's `trailing_zeros` rewrite.
        #[allow(clippy::verbose_bit_mask)]
        Object::Int(n) => (n & 0xFF) == 0,
        Object::Str(s) => s.is_empty(),
        _ => false,
    }
}

fn truncate_detail(msg: &str) -> String {
    const LIMIT: usize = 1024;
    if msg.len() <= LIMIT {
        msg.to_owned()
    } else {
        // Back off to a char boundary — byte 1024 can land mid-UTF-8
        // sequence (test output routinely contains non-ASCII).
        let mut cut = LIMIT;
        while !msg.is_char_boundary(cut) {
            cut -= 1;
        }
        let mut s = String::with_capacity(cut + 16);
        s.push_str(&msg[..cut]);
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

    /// OS suffixes recognised on per-test override keys (RFC 0062
    /// WS3): `status_<os>` / `reason_<os>` / `timeout_seconds_<os>`.
    /// These are exactly the `std::env::consts::OS` values for the
    /// platforms CI runs on; anything else on one of those key stems
    /// is a hard load error (typo protection).
    const OS_SUFFIXES: &[&str] = &["macos", "linux", "windows"];

    /// Key stems that accept a per-OS suffix.
    const OVERRIDABLE_KEYS: &[&str] = &["status", "reason", "timeout_seconds"];

    pub(super) fn parse(body: &str) -> Result<Expectations, String> {
        parse_for_os(body, std::env::consts::OS)
    }

    /// Parse with an explicit host-OS suffix — the seam the unit
    /// tests use to exercise override resolution on every platform.
    fn parse_for_os(body: &str, host_os: &str) -> Result<Expectations, String> {
        let mut top = Expectations::default();
        let mut current_section: Option<String> = None;
        let mut current_table: BTreeMap<String, String> = BTreeMap::new();

        for (lineno, raw_line) in body.lines().enumerate() {
            let line = strip_comment(raw_line).trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                flush(
                    &mut top,
                    current_section.take(),
                    &mut current_table,
                    host_os,
                )?;
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
            } else if k == "measured_os" {
                top.measured_os = Some(parse_measured_os(&v, lineno)?);
            }
        }
        flush(&mut top, current_section, &mut current_table, host_os)?;
        Ok(top)
    }

    fn flush(
        top: &mut Expectations,
        section: Option<String>,
        table: &mut BTreeMap<String, String>,
        host_os: &str,
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

        // RFC 0062 WS3: validate per-OS suffixes up front so a typo
        // like `status_ubuntu` fails the load instead of silently
        // never applying. Keys that don't start with an overridable
        // stem keep today's behaviour (ignored).
        for key in table.keys() {
            for stem in OVERRIDABLE_KEYS {
                if let Some(suffix) = key.strip_prefix(&format!("{stem}_")) {
                    if !OS_SUFFIXES.contains(&suffix) {
                        return Err(format!(
                            "[tests.{label}] unknown OS suffix on key {key:?} \
                             (expected one of: {})",
                            OS_SUFFIXES.join(", ")
                        ));
                    }
                }
            }
        }

        // Resolution: `<key>_<host_os>` wins over `<key>`.
        let resolve = |stem: &str| -> Option<&String> {
            table
                .get(&format!("{stem}_{host_os}"))
                .or_else(|| table.get(stem))
        };

        let base_status = table
            .get("status")
            .ok_or_else(|| format!("[tests.{label}] missing status"))?;
        let status = table
            .get(&format!("status_{host_os}"))
            .unwrap_or(base_status);
        let status = match status.as_str() {
            "pass" => TestStatus::Pass,
            "fail" => TestStatus::Fail,
            "error" => TestStatus::Error,
            "skip" => TestStatus::Skip,
            "timeout" => TestStatus::Timeout,
            other => return Err(format!("[tests.{label}] bad status {other:?}")),
        };
        let reason = resolve("reason").cloned();
        let timeout_seconds = match resolve("timeout_seconds") {
            Some(v) => Some(
                v.parse::<u64>()
                    .map_err(|_| format!("[tests.{label}] bad timeout_seconds {v:?}"))?,
            ),
            None => None,
        };
        top.tests.insert(
            label,
            ExpectedEntry {
                status,
                reason,
                timeout_seconds,
            },
        );
        table.clear();
        Ok(())
    }

    /// Parse the top-level `measured_os = ["macos", "linux"]` stamp
    /// (RFC 0063 WS7). Single-line string arrays only — the stamp is a
    /// short list of OS names. Names are validated against the same
    /// [`OS_SUFFIXES`] set as the per-test override keys, so a typo
    /// (`measured_os = ["darwin"]`) is a hard load error rather than a
    /// silently-always-advisory gate.
    fn parse_measured_os(v: &str, lineno: usize) -> Result<Vec<String>, String> {
        let inner = v
            .trim()
            .strip_prefix('[')
            .and_then(|t| t.strip_suffix(']'))
            .ok_or_else(|| {
                format!(
                    "line {}: measured_os must be a single-line array of strings",
                    lineno + 1
                )
            })?;
        let mut out = Vec::new();
        for item in inner.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let name = strip_quotes(item);
            if name.len() == item.len() {
                return Err(format!(
                    "line {}: measured_os entry {item:?} must be a quoted string",
                    lineno + 1
                ));
            }
            if !OS_SUFFIXES.contains(&name) {
                return Err(format!(
                    "line {}: unknown OS {name:?} in measured_os \
                     (expected one of: {})",
                    lineno + 1,
                    OS_SUFFIXES.join(", ")
                ));
            }
            out.push(name.to_owned());
        }
        Ok(out)
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
                \n\
                [tests.\"cpython/Lib/test/test_set.py\"]\n\
                status = \"pass\"\n\
                timeout_seconds = 150\n\
                reason = \"slow hash-collision stress\"\n\
            ";
            let exp = parse(body).unwrap();
            assert_eq!(exp.timeout_seconds, Some(5));
            assert_eq!(exp.get("bundled/test_basic.py"), Some(TestStatus::Pass));
            assert_eq!(
                exp.get("cpython/Lib/test/test_grammar.py"),
                Some(TestStatus::Fail)
            );
            // Per-test override is parsed; tests without one stay `None`.
            assert_eq!(
                exp.tests["cpython/Lib/test/test_set.py"].timeout_seconds,
                Some(150)
            );
            assert_eq!(
                exp.tests["cpython/Lib/test/test_grammar.py"].timeout_seconds,
                None
            );
        }

        // Shared fixture for the per-OS override tests (RFC 0062 WS3).
        const OVERRIDE_BODY: &str = "\
            [tests.\"cpython/Lib/test/test_example.py\"]\n\
            status = \"pass\"\n\
            reason = \"base reason\"\n\
            timeout_seconds = 60\n\
            status_linux = \"fail\"\n\
            reason_linux = \"linux-only breakage\"\n\
            timeout_seconds_linux = 120\n\
        ";

        #[test]
        fn os_override_applies_on_matching_host() {
            let exp = parse_for_os(OVERRIDE_BODY, "linux").unwrap();
            let entry = &exp.tests["cpython/Lib/test/test_example.py"];
            assert_eq!(entry.status, TestStatus::Fail);
            assert_eq!(entry.reason.as_deref(), Some("linux-only breakage"));
            assert_eq!(entry.timeout_seconds, Some(120));
        }

        #[test]
        fn base_applies_on_other_hosts() {
            for host in ["macos", "windows"] {
                let exp = parse_for_os(OVERRIDE_BODY, host).unwrap();
                let entry = &exp.tests["cpython/Lib/test/test_example.py"];
                assert_eq!(entry.status, TestStatus::Pass, "host {host}");
                assert_eq!(entry.reason.as_deref(), Some("base reason"));
                assert_eq!(entry.timeout_seconds, Some(60));
            }
        }

        #[test]
        fn os_override_without_base_optional_keys() {
            // Overrides resolve independently per key: a lone
            // `reason_macos` / `timeout_seconds_macos` applies on
            // macOS and leaves other hosts at `None`.
            let body = "\
                [tests.\"bundled/t.py\"]\n\
                status = \"pass\"\n\
                reason_macos = \"darwin quirk\"\n\
                timeout_seconds_macos = 90\n\
            ";
            let mac = parse_for_os(body, "macos").unwrap();
            assert_eq!(
                mac.tests["bundled/t.py"].reason.as_deref(),
                Some("darwin quirk")
            );
            assert_eq!(mac.tests["bundled/t.py"].timeout_seconds, Some(90));

            let linux = parse_for_os(body, "linux").unwrap();
            assert_eq!(linux.tests["bundled/t.py"].reason, None);
            assert_eq!(linux.tests["bundled/t.py"].timeout_seconds, None);
        }

        #[test]
        fn unknown_os_suffix_is_a_hard_error() {
            for bad_key in ["status_ubuntu", "reason_darwin", "timeout_seconds_win"] {
                let body =
                    format!("[tests.\"bundled/t.py\"]\nstatus = \"pass\"\n{bad_key} = \"x\"\n");
                let err = parse_for_os(&body, "linux").unwrap_err();
                assert!(
                    err.contains(bad_key),
                    "error should name the bad key {bad_key}: {err}"
                );
                assert!(err.contains("unknown OS suffix"), "{err}");
            }
        }

        #[test]
        fn unrelated_unknown_keys_still_ignored() {
            // Non-overridable keys keep the pre-RFC-0062 behaviour:
            // silently ignored rather than rejected.
            let body = "\
                [tests.\"bundled/t.py\"]\n\
                status = \"pass\"\n\
                some_future_key = \"whatever\"\n\
            ";
            let exp = parse_for_os(body, "linux").unwrap();
            assert_eq!(exp.get("bundled/t.py"), Some(TestStatus::Pass));
        }

        #[test]
        fn status_override_requires_base_status() {
            // A row with only `status_linux` is malformed on every
            // host — the base `status` stays mandatory so the file
            // loads identically everywhere.
            let body = "[tests.\"bundled/t.py\"]\nstatus_linux = \"fail\"\n";
            for host in ["linux", "macos"] {
                let err = parse_for_os(body, host).unwrap_err();
                assert!(err.contains("missing status"), "host {host}: {err}");
            }
        }

        // -- measured_os stamp (RFC 0063 WS7) ---------------------------

        #[test]
        fn measured_os_stamp_parses() {
            let body = "\
                measured_os = [\"macos\", \"linux\"]\n\
                timeout_seconds = 5\n\
                \n\
                [tests.\"bundled/t.py\"]\n\
                status = \"pass\"\n\
            ";
            let exp = parse_for_os(body, "macos").unwrap();
            assert_eq!(
                exp.measured_os,
                Some(vec!["macos".to_owned(), "linux".to_owned()])
            );
            // The rest of the file still parses as before.
            assert_eq!(exp.timeout_seconds, Some(5));
            assert_eq!(exp.get("bundled/t.py"), Some(TestStatus::Pass));
        }

        #[test]
        fn missing_measured_os_stamp_means_all_measured() {
            let body = "[tests.\"bundled/t.py\"]\nstatus = \"pass\"\n";
            let exp = parse_for_os(body, "windows").unwrap();
            assert_eq!(exp.measured_os, None);
            for host in ["macos", "linux", "windows"] {
                assert!(exp.os_is_measured(host), "host {host}");
            }
        }

        #[test]
        fn measured_os_stamp_resolves_per_host() {
            let body = "measured_os = [\"macos\", \"linux\"]\n";
            let exp = parse_for_os(body, "windows").unwrap();
            assert!(exp.os_is_measured("macos"));
            assert!(exp.os_is_measured("linux"));
            assert!(!exp.os_is_measured("windows"));
        }

        #[test]
        fn measured_os_rejects_unknown_os_names() {
            for bad in ["measured_os = [\"darwin\"]", "measured_os = [\"ubuntu\"]"] {
                let err = parse_for_os(bad, "linux").unwrap_err();
                assert!(err.contains("unknown OS"), "{bad}: {err}");
            }
        }

        #[test]
        fn measured_os_rejects_non_array_values() {
            let err = parse_for_os("measured_os = \"macos\"\n", "linux").unwrap_err();
            assert!(err.contains("array"), "{err}");
        }

        #[test]
        fn bad_status_value_in_override_rejected() {
            let body = "\
                [tests.\"bundled/t.py\"]\n\
                status = \"pass\"\n\
                status_linux = \"flaky\"\n\
            ";
            let err = parse_for_os(body, "linux").unwrap_err();
            assert!(err.contains("bad status"), "{err}");
            // On a non-matching host the bad value is never resolved,
            // but the suffix itself is still validated (it's a known
            // OS, so the row loads with the base status).
            let exp = parse_for_os(body, "macos").unwrap();
            assert_eq!(exp.get("bundled/t.py"), Some(TestStatus::Pass));
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

    // -- measured_os advisory gate (RFC 0063 WS7) -----------------------

    fn summary_with_unexpected(n: usize) -> RegrtestSummary {
        RegrtestSummary {
            total: n,
            unexpected: n,
            ..RegrtestSummary::default()
        }
    }

    #[test]
    fn gate_blocks_on_measured_host() {
        let exp = Expectations {
            measured_os: Some(vec!["macos".to_owned(), "linux".to_owned()]),
            ..Expectations::default()
        };
        for host in ["macos", "linux"] {
            assert!(
                strict_gate_blocks_for_os(&exp, &summary_with_unexpected(2), host),
                "host {host}"
            );
        }
    }

    #[test]
    fn gate_is_advisory_on_unmeasured_host() {
        let exp = Expectations {
            measured_os: Some(vec!["macos".to_owned(), "linux".to_owned()]),
            ..Expectations::default()
        };
        assert!(!strict_gate_blocks_for_os(
            &exp,
            &summary_with_unexpected(2),
            "windows"
        ));
    }

    #[test]
    fn gate_blocks_everywhere_without_stamp() {
        // Missing stamp ≡ "all OSes measured" — pre-RFC-0063 behaviour.
        let exp = Expectations::default();
        for host in ["macos", "linux", "windows"] {
            assert!(
                strict_gate_blocks_for_os(&exp, &summary_with_unexpected(1), host),
                "host {host}"
            );
        }
    }

    #[test]
    fn gate_never_blocks_without_unexpected() {
        let exp = Expectations {
            measured_os: Some(vec!["macos".to_owned()]),
            ..Expectations::default()
        };
        for host in ["macos", "windows"] {
            assert!(
                !strict_gate_blocks_for_os(&exp, &summary_with_unexpected(0), host),
                "host {host}"
            );
        }
    }
}
