//! Built-in modules that ship with the WeavePy interpreter.
//!
//! Two kinds of modules live here:
//!
//! - Rust-defined factories that build a `PyModule` directly (used
//!   for engine-heavy or low-level modules: `sys`, `math`, `os`,
//!   `io`, `re`, `json`, `random`, `time`).
//! - "Frozen" Python sources baked into the binary (used for
//!   pure-Python modules where it's easier to write Python: e.g.
//!   `collections`, `itertools`, `functools`, `pathlib`,
//!   `argparse`, `contextlib`). These compile and execute on first
//!   import exactly like a real `.py` file.
//!
//! [`register_all`] wires both kinds into the import cache.

use crate::import::ModuleCache;

pub(crate) mod frozen_sources;
#[allow(dead_code)] // Fingerprinting runs in build.rs; the VM recomputes it in tests.
pub(crate) mod tree_manifest;
pub(crate) use frozen_sources::frozen_sources;

mod frozen_index {
    include!(concat!(env!("OUT_DIR"), "/frozen_index.rs"));
}

/// The row of [`frozen_sources`] registered under `name`, found through
/// the generated sorted name index (see build.rs) so a lookup never
/// reads the name literals laid out beside each module's source text.
pub(crate) fn frozen_lookup(name: &str) -> Option<frozen_sources::FrozenSource> {
    use frozen_index::{FROZEN_INDEX, FROZEN_NAMES};
    let at = FROZEN_INDEX
        .binary_search_by(|&(off, len, _)| {
            FROZEN_NAMES[off as usize..off as usize + len as usize].cmp(name)
        })
        .ok()?;
    frozen_sources().get(FROZEN_INDEX[at].2 as usize).copied()
}

pub mod ast_convert;
pub mod ast_mod;
pub mod asyncio_mod;
pub mod binascii_mod;
pub mod bisect_accel;
pub mod bz2_mod;
pub mod cmath_mod;
pub mod codecs_engine;
pub mod codecs_mod;
pub mod csv_mod;
pub mod datetime_accel;
pub mod datetime_mod;
pub(crate) mod datetime_native;
pub mod errno_mod;
pub mod faulthandler_mod;
#[cfg(unix)]
pub mod fcntl_mod;
pub mod functools_mod;
pub mod gc_mod;
#[cfg(unix)]
pub mod grp_mod;
pub mod gzip_mod;
pub mod hashlib_mod;
pub mod heapq_accel;
pub mod hmac_mod;
pub mod imp_mod;
pub mod interpreters_mod;
pub mod io;
pub mod itertools_mod;
pub mod json_accel;
pub mod lzma_mod;
pub mod marshal_mod;
pub mod math;
pub mod pickle_accel;
// RFC 0063 — the Windows wave: shared NT plumbing (CRT fd layer,
// winerror bridge) plus the native module quartet the frozen Windows
// stdlib consumes.
#[cfg(windows)]
pub mod msvcrt_mod;
// `pub` (not `pub(crate)`): `weavepy-capi`'s extension loader reuses
// the `format_message` strerror source for CPython's "DLL load failed
// while importing …" ImportError shape (RFC 0064 WS2).
#[cfg(windows)]
pub mod nt_support;
// `_WindowsConsoleIO` + the ReadConsoleW/WriteConsoleW byte bridge
// the PyFile stdio monolith reroutes through (RFC 0064 WS4).
pub mod operator_accel;
pub mod os;
pub mod os_process;
#[cfg(windows)]
pub mod overlapped_mod;
#[cfg(unix)]
pub mod posixsubprocess_mod;
#[cfg(unix)]
pub mod pwd_mod;
pub mod pyexpat_mod;
#[cfg(unix)]
pub mod resource_mod;
pub mod select_mod;
pub mod shutil_mod;
pub mod signal_mod;
pub mod socket_mod;
pub mod sqlite3_native;
pub mod sre_mod;
pub mod statistics_accel;
pub mod struct_mod;
pub mod subprocess_mod;
pub mod symtable_mod;
pub mod sys;
pub mod sys_monitoring;
pub mod sysconfig_native;
pub mod tempfile_mod;
#[cfg(unix)]
pub mod termios_mod;
pub mod testcapi_call;
pub mod testcapi_monitoring;
pub mod testinternalcapi_mod;
pub mod thread;
pub mod time;
pub mod tokenize_mod;
pub mod tracemalloc_real;
pub mod ucd;
pub mod unicodedata_mod;
pub mod weakref_mod;
pub mod weave_frame_mod;
#[cfg(windows)]
pub(crate) mod win_console;
#[cfg(windows)]
pub mod winapi_mod;
#[cfg(windows)]
pub mod winreg_mod;
pub mod zlib_mod;
pub mod zstd_mod;
// RFC 0023 — drop-in stdlib parity.
pub mod abc_mod;
pub mod atexit_mod;
pub mod ctypes_native;
pub mod greenlet_native;
pub mod https_mod;
pub mod io_full;
pub mod locale_mod;
pub mod mmap_mod;
pub mod random_core;
pub mod ssl_real;
pub mod string_mod;
pub mod warnings_mod;

pub mod collections_native;
pub mod gc_real;
pub mod multiprocessing_mod;
pub mod queue_native;
pub mod thread_real;
pub mod weakref_real;

/// Register the built-in modules into `cache`. Called once at
/// interpreter startup.
pub fn register_all(cache: &ModuleCache) {
    // Rust-defined factories.
    cache.register_builtin("sys", sys::build);
    cache.register_builtin("math", math::build);
    // Native port of CPython 3.13's `Modules/cmathmodule.c` — builtin
    // functions must not bind as instance methods (test_cmath's
    // `isclose = cmath.isclose` class attribute), and the C special-value
    // tables demand exact signed-zero fidelity a Python port can't give.
    cache.register_builtin("cmath", cmath_mod::build);
    cache.register_builtin("os", os::build);
    // The same native surface under an internal alias: the `posix`/`nt`
    // shims re-export through it. They used to `import os`, but a
    // *fresh* source import of `os` (see the frozen `os` registration)
    // executes `os.py` → `from posix import *` → the shim — importing
    // `os` there would see the half-initialized source module.
    cache.register_builtin("_weave_posix", os::build);
    cache.register_builtin("os.path", os::build_path);
    // RFC 0040 WS7 — the public `io` module is a thin frozen wrapper
    // (`python/io.py`) that re-exports the native `_io` accelerator, exactly
    // like CPython's real `Lib/io.py` (`io.BufferedReader is _io.BufferedReader`,
    // `type(open(f,'rb')) is io.BufferedReader`, shared IOBase ABC family). The
    // native classes live in `_io` (see `io_full::build`, which calls
    // `io::build` internally); `_pyio` is the separate pure-Python twin that
    // `test_io` imports directly as its "Py" variant.
    // RFC 0041 WS-json — `json` is the verbatim CPython package
    // (`stdlib/python/json/`) running over the native `_json` accelerator.
    // The package's `scanner`/`decoder`/`encoder` `from _json import …` with
    // a pure-Python fallback, exactly like CPython, so `test_json` can build
    // its C-vs-Python test pairs (`import_fresh_module('json', blocked=['_json'])`).
    cache.register_builtin("_json", json_accel::build);
    // RFC 0054 WS1 — the asyncio C accelerator: native `Future`/`Task`,
    // the per-thread running-loop slot, and the task registries. The frozen
    // `asyncio/{futures,tasks,events}.py` adoption hooks bind these exactly
    // as CPython's do.
    cache.register_builtin("_asyncio", asyncio_mod::build);
    cache.register_builtin("time", time::build);
    cache.register_builtin("_thread", thread_real::build);
    cache.register_builtin("errno", errno_mod::build);
    // RFC 0040 WS6 — CPython's C `faulthandler`. Its private crash
    // primitives (`_sigsegv`, `_sigabrt`, …) are what
    // `test_concurrent_futures.test_deadlock` fires inside pool workers to
    // verify `BrokenProcessPool` recovery; without the module those cases
    // hung until `LONG_TIMEOUT`.
    cache.register_builtin("faulthandler", faulthandler_mod::build);
    cache.register_builtin("_testinternalcapi", testinternalcapi_mod::build);
    // RFC 0060 — native primitives behind the frozen
    // `_weave_frame_locals` module's PEP 667 `FrameLocalsProxy`.
    cache.register_builtin("_weave_frame", weave_frame_mod::build);
    // RFC 0040 WS4 — the native core is `_signal`; the frozen `signal.py`
    // (CPython's) layers the `Signals`/`Handlers`/`Sigmasks` IntEnums and
    // the enum-coercing `signal`/`getsignal`/`pthread_sigmask` wrappers.
    cache.register_builtin("_signal", signal_mod::build);
    cache.register_builtin("select", select_mod::build);
    cache.register_builtin("_socket", socket_mod::build);
    cache.register_builtin("_subprocess", subprocess_mod::build);
    // RFC 0040 WS2 — the CPython-faithful fork+exec primitive behind the
    // verbatim `subprocess.Popen` driver. POSIX-only, like CPython: on
    // Windows `import _posixsubprocess` must fail so portable code
    // (and the frozen `subprocess.py`) takes the `_winapi` arm
    // (RFC 0063 truthful-inventory rule).
    #[cfg(unix)]
    cache.register_builtin("_posixsubprocess", posixsubprocess_mod::build);
    // RFC 0063 — the Windows-native quartet the frozen Windows stdlib
    // (subprocess, multiprocessing, shutil, asyncio.windows_events,
    // platform, mimetypes) imports. Windows-only, like CPython.
    #[cfg(windows)]
    {
        cache.register_builtin("_winapi", winapi_mod::build);
        cache.register_builtin("msvcrt", msvcrt_mod::build);
        cache.register_builtin("winreg", winreg_mod::build);
        cache.register_builtin("_overlapped", overlapped_mod::build);
    }
    cache.register_builtin("hashlib", hashlib_mod::build);
    // RFC 0060 WS3 — CPython-shaped hash accelerator modules, importable
    // individually and consulted by `hashlib.__get_builtin_constructor`.
    cache.register_builtin("_md5", hashlib_mod::build_md5);
    cache.register_builtin("_sha1", hashlib_mod::build_sha1);
    cache.register_builtin("_sha2", hashlib_mod::build_sha2);
    cache.register_builtin("_sha3", hashlib_mod::build_sha3);
    cache.register_builtin("_blake2", hashlib_mod::build_blake2);
    cache.register_builtin("_operator", operator_accel::build);
    cache.register_builtin("_heapq", heapq_accel::build);
    cache.register_builtin("_bisect", bisect_accel::build);
    // RFC 0041 WS-statistics — native `_normal_dist_inv_cdf` (AS241) behind
    // the verbatim `statistics` module's `try: from _statistics import …`.
    cache.register_builtin("_statistics", statistics_accel::build);
    cache.register_builtin("binascii", binascii_mod::build);
    // `uuid` is CPython's verbatim pure-Python `Lib/uuid.py` (registered as a
    // frozen source below), NOT a native dict shim — the shim's fake UUID
    // (a `dict`) could not carry a real `__str__`, so `str(uuid.uuid4())`
    // returned a dict repr. See `frozen_sources()`.
    cache.register_builtin("_tempfile", tempfile_mod::build);
    cache.register_builtin("_shutil", shutil_mod::build);
    cache.register_builtin("_functools", functools_mod::build);
    cache.register_builtin("_itertools", itertools_mod::build);
    // RFC 0042 WS2 — TLS unification. The native rustls core is `_ssl`; the
    // public `ssl` module is the CPython-shaped frozen `ssl.py`
    // (`SSLContext`/`SSLSocket`/`SSLObject`) that sits on top of it, exactly
    // like CPython's `Lib/ssl.py` over its `_ssl` C extension.
    cache.register_builtin("_ssl", ssl_real::build);
    cache.register_builtin("zlib", zlib_mod::build);
    // RFC 0076 WS15 — PEP 784: the native core under the verbatim
    // `compression.zstd` package (see `frozen_sources`).
    cache.register_builtin("_zstd", zstd_mod::build);
    cache.register_builtin("_struct", struct_mod::build);
    cache.register_builtin("_codecs", codecs_mod::build);
    cache.register_builtin("marshal", marshal_mod::build);
    // RFC 0035 — native SRE regex core behind the frozen `re` package.
    cache.register_builtin("_sre", sre_mod::build);
    // RFC 0033 — native AST parsing core behind the frozen `ast` module.
    cache.register_builtin("_ast", ast_mod::build);
    // RFC 0033 — native symbol-table core behind the frozen `symtable` module.
    cache.register_builtin("_symtable", symtable_mod::build);
    // RFC 0055 WS1 — CPython 3.13's native build-info module (gh-103480).
    // `sysconfig._init_non_posix` merges `config_vars()` on Windows and
    // `test_sysconfig` imports it unconditionally.
    cache.register_builtin("_sysconfig", sysconfig_native::build);
    // RFC 0052 — native lexer core behind the frozen `_tokenize` module
    // (the CPython 3.13 `Parser/lexer` port `tokenize.py` drives).
    cache.register_builtin("_tokenize_core", tokenize_mod::build);
    cache.register_builtin("_gzip", gzip_mod::build);
    cache.register_builtin("_bz2", bz2_mod::build);
    cache.register_builtin("_lzma", lzma_mod::build);
    cache.register_builtin("_sqlite3", sqlite3_native::build);
    cache.register_builtin("_csv", csv_mod::build);
    cache.register_builtin("_weakref", weakref_real::build);
    // Native `SimpleQueue.put` behind the `_queue` Python shim (the
    // bound form must be a `builtin_function_or_method` —
    // test_types.test_method_descriptor_crash).
    cache.register_builtin("_weave_queue", queue_native::build);
    // Atomic `deque` end operations behind the `_collections` Python
    // stand-in (CPython documents append/pop from either side as
    // thread-safe; `SimpleQueue` and asyncio's ready queue rely on it).
    cache.register_builtin("_weave_collections", collections_native::build);
    cache.register_builtin("_weave_datetime", datetime_accel::build);
    cache.register_builtin("_weave_pickle", pickle_accel::build);
    cache.register_builtin("gc", gc_real::build);
    cache.register_builtin("_multiprocessing", multiprocessing_mod::build);
    // RFC 0040 WS5 — native XML parser behind `xml.parsers.expat`; drives the
    // `xmlrpc` serializer the `multiprocessing.managers` server process uses.
    cache.register_builtin("pyexpat", pyexpat_mod::build);
    // RFC 0040 (WS5): shm_open/shm_unlink core for `multiprocessing`'s
    // resource_tracker + shared_memory. POSIX-only, like CPython: the
    // frozen `shared_memory.py` selects its NT arm off the
    // ImportError (RFC 0063).
    #[cfg(unix)]
    cache.register_builtin("_posixshmem", multiprocessing_mod::build_posixshmem);
    // RFC 0041 WS-datetime: `datetime` is now CPython's verbatim shim over the
    // bundled pure-Python `_pydatetime`. The old constants-only native
    // `_datetime` is intentionally NOT registered so `from _datetime import *`
    // raises `ImportError` and the shim falls through to `_pydatetime` (and so
    // `test_datetime`'s `import_fresh_module(..., blocked=['_pydatetime'])`
    // _Fast pass is cleanly skipped rather than importing a half-built module).
    // RFC 0029 — `_imp` bridges the C-extension loader into the
    // frozen `importlib.machinery.ExtensionFileLoader`.
    cache.register_builtin("_imp", imp_mod::build);
    // RFC 0023 — drop-in stdlib parity.
    cache.register_builtin("unicodedata", unicodedata_mod::build);
    cache.register_builtin("_io", io_full::build);
    cache.register_builtin("_string", string_mod::build);
    cache.register_builtin("_random", random_core::build);
    cache.register_builtin("_warnings", warnings_mod::build);
    cache.register_builtin("mmap", mmap_mod::build);
    cache.register_builtin("_locale", locale_mod::build);
    cache.register_builtin("_abc", abc_mod::build);
    // `_contextvars` is the frozen alias of the pure-Python `contextvars`
    // (see `python/_contextvars.py`): 3.14's `threading` and
    // `_py_warnings` import the accelerator name directly and must see
    // the same `Context`/`ContextVar` types `contextvars` hands out.
    // RFC 0066 WS4: native greenlets over real stack switching.
    cache.register_builtin("_greenlet", greenlet_native::build);
    // RFC 0046 (wave 5): native primitive layer behind the frozen `_ctypes`
    // reimplementation (memory peek/poke, dlopen/dlsym, platform C type
    // sizes, libffi call bridge) that backs the verbatim CPython `ctypes`
    // package. The host `_ctypes.*.so` is core-built (links `_PyRuntime`),
    // so it cannot be dlopen'd like a stable-ABI wheel — we reimplement it.
    cache.register_builtin("_ctypes_native", ctypes_native::build);
    cache.register_builtin("atexit", atexit_mod::build);
    cache.register_builtin("_https", https_mod::build);
    // RFC 0026 — POSIX-flavoured stdlib that user code (and the
    // multiprocessing rewrite) imports unconditionally. POSIX-only
    // since RFC 0063: CPython has no `fcntl` on Windows and portable
    // code keys off the ImportError; the old always-registered stub
    // module sent it down the wrong branch.
    #[cfg(unix)]
    cache.register_builtin("fcntl", fcntl_mod::build);
    // CPython has no `resource` module on Windows — every stdlib caller
    // guards `import resource` with ImportError — and the non-unix stubs
    // in `resource_mod` fail at call time anyway (e.g. regrtest's
    // `adjust_rlimit_nofile` dying on `getrlimit`), so don't register it.
    #[cfg(unix)]
    cache.register_builtin("resource", resource_mod::build);
    // RFC 0075 WS9 — user/group database access (CPython's pwdmodule.c /
    // grpmodule.c). gunicorn imports both at module level for its
    // user-switching surface; POSIX-only like CPython.
    #[cfg(unix)]
    cache.register_builtin("pwd", pwd_mod::build);
    #[cfg(unix)]
    cache.register_builtin("grp", grp_mod::build);
    // RFC 0055 WS6 — real POSIX terminal control (CPython's termios is a
    // core C extension; `tty`/`pty` above are pure-Python over it).
    #[cfg(unix)]
    cache.register_builtin("termios", termios_mod::build);
    // RFC 0031 — debugger / profiler observability is now fully
    // wired in the VM dispatch loop; the modules below expose the
    // user-visible registration / snapshot API.
    // RFC 0057 WS6: `tracemalloc` is now CPython's verbatim
    // `Lib/tracemalloc.py` (frozen below) over this raw `_tracemalloc`
    // core, mirroring the upstream split.
    cache.register_builtin("_tracemalloc", tracemalloc_real::build);
    // RFC 0031 — PEP 684 sub-interpreters. Frontend lives in the
    // pure-Python `interpreters.py` shim; this is the C-extension
    // façade.
    cache.register_builtin("_xxsubinterpreters", interpreters_mod::build);

    // Frozen Python sources (pure-Python stdlib).
    //
    // RFC 0046 (wave 4): `numpy`/`_numpy_pure` are a pure-Python compatibility
    // shim that, being frozen, would otherwise shadow a real numpy installed on
    // `sys.path`. Setting `WEAVEPY_NO_NUMPY_SHIM` suppresses the shim so the
    // binary-ABI loader imports the genuine `numpy._core._multiarray_umath`
    // extension instead.
    let suppress_numpy_shim = std::env::var_os("WEAVEPY_NO_NUMPY_SHIM").is_some();
    // Mirror of `WEAVEPY_NO_NUMPY_SHIM` for the frozen `pytest`/`pluggy`/
    // `iniconfig` shims: suppressing them lets a real pytest installed on
    // `sys.path` load instead (or an editable copy of our shim during
    // development), rather than being shadowed by the frozen source.
    let suppress_pytest_shim = std::env::var_os("WEAVEPY_NO_PYTEST_SHIM").is_some();
    // General-purpose escape hatch (comma-separated module names) so a frozen
    // module can be shadowed by an editable copy on `sys.path` during
    // development — the same idea as the two shims above, but for arbitrary
    // modules while iterating on their pure-Python source without a rebuild.
    let suppress_list = std::env::var("WEAVEPY_SUPPRESS_FROZEN").unwrap_or_default();
    let mut suppressed: Vec<Box<str>> = suppress_list
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(Box::from)
        .collect();
    if suppress_numpy_shim {
        suppressed.extend(["numpy", "_numpy_pure"].map(Box::from));
    }
    if suppress_pytest_shim {
        suppressed.extend(["pytest", "pluggy", "iniconfig"].map(Box::from));
    }
    // The table itself is static (see `frozen_lookup`); installing it
    // only records the suppressed names.
    cache.install_frozen_table(suppressed);
}
