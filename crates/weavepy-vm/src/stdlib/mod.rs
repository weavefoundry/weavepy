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

use crate::import::{FrozenSource, ModuleCache};

pub mod ast_mod;
pub mod binascii_mod;
pub mod bisect_accel;
pub mod bz2_mod;
pub mod codecs_mod;
pub mod csv_mod;
pub mod datetime_mod;
pub mod errno_mod;
pub mod faulthandler_mod;
pub mod fcntl_mod;
pub mod functools_mod;
pub mod gc_mod;
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
pub mod operator_accel;
pub mod os;
pub mod os_process;
pub mod posixsubprocess_mod;
pub mod pyexpat_mod;
pub mod resource_mod;
pub mod secrets_mod;
pub mod select_mod;
pub mod shutil_mod;
pub mod signal_mod;
pub mod socket_mod;
pub mod sqlite3_mod;
pub mod sre_mod;
pub mod statistics_accel;
pub mod struct_mod;
pub mod subprocess_mod;
pub mod symtable_mod;
pub mod sys;
pub mod sys_monitoring;
pub mod tempfile_mod;
pub mod testinternalcapi_mod;
pub mod thread;
pub mod time;
pub mod tracemalloc_real;
mod unicode_decomp_data;
pub mod unicodedata_mod;
pub mod uuid_mod;
pub mod weakref_mod;
pub mod zlib_mod;
// RFC 0023 — drop-in stdlib parity.
pub mod abc_mod;
pub mod atexit_mod;
pub mod contextvars_mod;
pub mod https_mod;
pub mod io_full;
pub mod locale_mod;
pub mod mmap_mod;
pub mod pickle_accel;
pub mod random_core;
pub mod ssl_real;
pub mod string_mod;
pub mod warnings_mod;

pub mod gc_real;
pub mod multiprocessing_mod;
pub mod thread_real;
pub mod weakref_real;

/// Register the built-in modules into `cache`. Called once at
/// interpreter startup.
pub fn register_all(cache: &ModuleCache) {
    // Rust-defined factories.
    cache.register_builtin("sys", sys::build);
    cache.register_builtin("math", math::build);
    cache.register_builtin("os", os::build);
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
    // RFC 0040 WS4 — the native core is `_signal`; the frozen `signal.py`
    // (CPython's) layers the `Signals`/`Handlers`/`Sigmasks` IntEnums and
    // the enum-coercing `signal`/`getsignal`/`pthread_sigmask` wrappers.
    cache.register_builtin("_signal", signal_mod::build);
    cache.register_builtin("select", select_mod::build);
    cache.register_builtin("_socket", socket_mod::build);
    cache.register_builtin("_subprocess", subprocess_mod::build);
    // RFC 0040 WS2 — the CPython-faithful fork+exec primitive behind the
    // verbatim `subprocess.Popen` driver.
    cache.register_builtin("_posixsubprocess", posixsubprocess_mod::build);
    cache.register_builtin("hashlib", hashlib_mod::build);
    cache.register_builtin("_operator", operator_accel::build);
    cache.register_builtin("_heapq", heapq_accel::build);
    cache.register_builtin("_bisect", bisect_accel::build);
    // RFC 0041 WS-statistics — native `_normal_dist_inv_cdf` (AS241) behind
    // the verbatim `statistics` module's `try: from _statistics import …`.
    cache.register_builtin("_statistics", statistics_accel::build);
    cache.register_builtin("binascii", binascii_mod::build);
    cache.register_builtin("secrets", secrets_mod::build);
    cache.register_builtin("uuid", uuid_mod::build);
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
    cache.register_builtin("_struct", struct_mod::build);
    cache.register_builtin("_codecs", codecs_mod::build);
    cache.register_builtin("marshal", marshal_mod::build);
    // RFC 0035 — native SRE regex core behind the frozen `re` package.
    cache.register_builtin("_sre", sre_mod::build);
    // RFC 0033 — native AST parsing core behind the frozen `ast` module.
    cache.register_builtin("_ast", ast_mod::build);
    // RFC 0033 — native symbol-table core behind the frozen `symtable` module.
    cache.register_builtin("_symtable", symtable_mod::build);
    cache.register_builtin("_gzip", gzip_mod::build);
    cache.register_builtin("_bz2", bz2_mod::build);
    cache.register_builtin("_lzma", lzma_mod::build);
    cache.register_builtin("_sqlite3", sqlite3_mod::build);
    cache.register_builtin("_csv", csv_mod::build);
    cache.register_builtin("_weakref", weakref_real::build);
    cache.register_builtin("gc", gc_real::build);
    cache.register_builtin("_multiprocessing", multiprocessing_mod::build);
    // RFC 0040 WS5 — native XML parser behind `xml.parsers.expat`; drives the
    // `xmlrpc` serializer the `multiprocessing.managers` server process uses.
    cache.register_builtin("pyexpat", pyexpat_mod::build);
    // RFC 0040 (WS5): shm_open/shm_unlink core for `multiprocessing`'s
    // resource_tracker + shared_memory.
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
    cache.register_builtin("_pickle", pickle_accel::build);
    cache.register_builtin("mmap", mmap_mod::build);
    cache.register_builtin("_locale", locale_mod::build);
    cache.register_builtin("_abc", abc_mod::build);
    cache.register_builtin("_contextvars", contextvars_mod::build);
    cache.register_builtin("atexit", atexit_mod::build);
    cache.register_builtin("_https", https_mod::build);
    // RFC 0026 — POSIX-flavoured stdlib that user code (and the
    // multiprocessing rewrite) imports unconditionally.
    cache.register_builtin("fcntl", fcntl_mod::build);
    cache.register_builtin("resource", resource_mod::build);
    // RFC 0031 — debugger / profiler observability is now fully
    // wired in the VM dispatch loop; the modules below expose the
    // user-visible registration / snapshot API.
    cache.register_builtin("tracemalloc", tracemalloc_real::build);
    cache.register_builtin("_tracemalloc", tracemalloc_real::build_ext);
    // RFC 0031 — PEP 684 sub-interpreters. Frontend lives in the
    // pure-Python `interpreters.py` shim; this is the C-extension
    // façade.
    cache.register_builtin("_xxsubinterpreters", interpreters_mod::build);

    // Frozen Python sources (pure-Python stdlib).
    for src in frozen_sources() {
        cache.register_frozen(*src);
    }
}

fn frozen_sources() -> &'static [FrozenSource] {
    &[
        FrozenSource {
            name: "builtins",
            source: include_str!("python/builtins.py"),
            is_package: false,
        },
        // RFC 0040 WS1 — upgrades the native `os` module's `environ`/
        // `environb` to CPython's write-through `_Environ` mappings. Imported
        // for its side effect immediately after the native `os` module is
        // built (see `Interpreter::load_one`).
        FrozenSource {
            name: "_weave_envinit",
            source: include_str!("python/_weave_envinit.py"),
            is_package: false,
        },
        // RFC 0040 WS7 — CPython's pure-Python `io` reference implementation.
        // `test_io`/`test_fileio` import `_pyio` and exercise *both* the native
        // `io` and `_pyio` side-by-side; without it the whole suite fails to
        // import. Vendored verbatim from CPython (`Lib/_pyio.py`).
        FrozenSource {
            name: "_pyio",
            source: include_str!("python/_pyio.py"),
            is_package: false,
        },
        // RFC 0040 WS7 — the public `io` module: a thin re-export of the native
        // `_io` accelerator, mirroring CPython's real `Lib/io.py`. Preserves
        // type identity (`io.BufferedReader is _io.BufferedReader`) and the
        // shared IOBase ABC family; `_pyio` stays the separate pure-Python twin.
        FrozenSource {
            name: "io",
            source: include_str!("python/io.py"),
            is_package: false,
        },
        // RFC 0040 WS4 — CPython's `signal.py`: layers the `Signals`/
        // `Handlers`/`Sigmasks` IntEnums over the native `_signal` core and
        // wraps `signal`/`getsignal`/`pthread_sigmask`/`sigwait`/
        // `valid_signals` to coerce ints to/from those enums.
        FrozenSource {
            name: "signal",
            source: include_str!("python/signal.py"),
            is_package: false,
        },
        // `keyword` — verbatim CPython keyword/soft-keyword lists +
        // membership predicates. Imported by `dataclasses` (field-name
        // validation) and `pydoc`/`inspect`-adjacent code.
        FrozenSource {
            name: "keyword",
            source: include_str!("python/keyword.py"),
            is_package: false,
        },
        // `random` — verbatim CPython distribution layer over the
        // Rust `_random` MT19937 core (RFC 0037: `random.Random(42)`
        // is stream-identical to CPython).
        FrozenSource {
            name: "random",
            source: include_str!("python/random_mod.py"),
            is_package: false,
        },
        // Internal: `_SeqIter`, the lazy legacy-`__getitem__` iterator
        // `iter(obj)` returns when *obj* has no `__iter__` (CPython's
        // built-in `iterator`/seqiterobject). Kept out of `builtins` to
        // avoid leaking a name into every module's global namespace.
        FrozenSource {
            name: "_seqtools",
            source: include_str!("python/_seqtools.py"),
            is_package: false,
        },
        // `collections` is the verbatim CPython package init; the
        // `_collections` accelerator below supplies `deque`/`defaultdict`
        // (which have no pure-Python fallback in the real module), while
        // `OrderedDict`/`namedtuple` run the reference pure-Python paths.
        // The verbatim CPython `_collections_abc` carries the ABC
        // definitions and `collections.abc` re-exports them (RFC 0037 WS8).
        FrozenSource {
            name: "collections",
            source: include_str!("python/collections.py"),
            is_package: true,
        },
        FrozenSource {
            name: "_collections",
            source: include_str!("python/_collections.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_collections_abc",
            source: include_str!("python/_collections_abc.py"),
            is_package: false,
        },
        // `_weakrefset` (verbatim CPython): the `WeakSet` source module
        // that `abc`/`_py_abc` import directly to back the ABC virtual-
        // subclass registry/caches (RFC 0037 WS8).
        FrozenSource {
            name: "_weakrefset",
            source: include_str!("python/_weakrefset.py"),
            is_package: false,
        },
        // `_py_abc` (verbatim CPython): the pure-Python `ABCMeta`
        // reference implementation. `test_abc` imports it directly to
        // exercise the Python ABC machinery alongside the C `_abc` path.
        FrozenSource {
            name: "_py_abc",
            source: include_str!("python/_py_abc.py"),
            is_package: false,
        },
        // `_colorize`: CPython 3.13's ANSI-colour helper (verbatim). Imported
        // by `traceback`/`test_traceback` (and the 3.13 REPL); honours
        // NO_COLOR/FORCE_COLOR and TTY detection.
        FrozenSource {
            name: "_colorize",
            source: include_str!("python/_colorize.py"),
            is_package: false,
        },
        // `__future__`: the feature-flag table (verbatim CPython 3.13).
        // `from __future__ import annotations` is a compiler directive, but
        // the module must still be importable because real modules read its
        // `_Feature` objects (e.g. `__future__.annotations`).
        FrozenSource {
            name: "__future__",
            source: include_str!("python/future_module.py"),
            is_package: false,
        },
        FrozenSource {
            name: "collections.abc",
            source: include_str!("python/collections_abc.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_collections_user",
            source: include_str!("python/_collections_user.py"),
            is_package: false,
        },
        // RFC 0036 — `string` (constants + `Template` + `Formatter` over
        // the native `_string`) and `platform`, carried verbatim from
        // CPython 3.13.
        FrozenSource {
            name: "string",
            source: include_str!("python/string.py"),
            is_package: false,
        },
        // `base64` is CPython's `Lib/base64.py` ported verbatim (pure Python
        // over `binascii` + `struct` + `re`). It supersedes the old Rust
        // `base64` module, which covered only RFC 3548 and ignored
        // `altchars`/`validate`; the frozen copy adds a85/b85/z85 and the
        // exact decode semantics `test_base64` checks.
        FrozenSource {
            name: "base64",
            source: include_str!("python/base64_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "platform",
            source: include_str!("python/platform.py"),
            is_package: false,
        },
        // Verbatim CPython 3.13 `hmac`. The Rust shim it replaces could not
        // satisfy `test_hmac`'s identity check (`hmac.compare_digest is
        // _operator._compare_digest`) nor the full `HMAC` class surface;
        // ported over `hashlib` + `_operator._compare_digest` instead.
        FrozenSource {
            name: "hmac",
            source: include_str!("python/hmac.py"),
            is_package: false,
        },
        FrozenSource {
            name: "itertools",
            source: include_str!("python/itertools.py"),
            is_package: false,
        },
        FrozenSource {
            name: "functools",
            source: include_str!("python/functools.py"),
            is_package: false,
        },
        // RFC 0041 WS-json — the verbatim CPython `json` package. Each
        // submodule prefers the native `_json` accelerator and falls back to
        // its pure-Python twin, so blocking `_json` (the way `test_json`
        // probes for the C build) transparently selects the Python path.
        FrozenSource {
            name: "json",
            source: include_str!("python/json/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "json.decoder",
            source: include_str!("python/json/decoder.py"),
            is_package: false,
        },
        FrozenSource {
            name: "json.encoder",
            source: include_str!("python/json/encoder.py"),
            is_package: false,
        },
        FrozenSource {
            name: "json.scanner",
            source: include_str!("python/json/scanner.py"),
            is_package: false,
        },
        FrozenSource {
            name: "json.tool",
            source: include_str!("python/json/tool.py"),
            is_package: false,
        },
        // RFC 0037 WS8 verbatim/faithful module ports that gate import-time
        // clusters: `cmath` (pure-Python over the `math` core) unblocks
        // `test_fractions`; the C-locale `locale` unblocks `test_format`
        // and backs `calendar`'s `LocaleTextCalendar`; `calendar` is the
        // verbatim CPython 3.13 module.
        FrozenSource {
            name: "cmath",
            source: include_str!("python/cmath.py"),
            is_package: false,
        },
        FrozenSource {
            name: "locale",
            source: include_str!("python/locale.py"),
            is_package: false,
        },
        FrozenSource {
            name: "calendar",
            source: include_str!("python/calendar.py"),
            is_package: false,
        },
        // RFC 0040 WS8 — `time.strptime` delegates here, exactly as
        // CPython's `timemodule.c` does (`_strptime._strptime_time`).
        FrozenSource {
            name: "_strptime",
            source: include_str!("python/_strptime.py"),
            is_package: false,
        },
        FrozenSource {
            name: "contextlib",
            source: include_str!("python/contextlib.py"),
            is_package: false,
        },
        // `pathlib` is CPython 3.13's verbatim package: the thin `__init__`
        // re-exports `_abc` (the `PurePathBase`/`PathBase` ABCs the
        // `test_pathlib_abc` suite drives) and `_local` (the concrete
        // `PurePath`/`Path`/`PurePosixPath`/`PosixPath`/… classes). Ported
        // wholesale rather than re-approximated (RFC 0038 WS-B).
        FrozenSource {
            name: "pathlib",
            source: include_str!("python/pathlib.py"),
            is_package: true,
        },
        FrozenSource {
            name: "pathlib._abc",
            source: include_str!("python/pathlib_abc.py"),
            is_package: false,
        },
        FrozenSource {
            name: "pathlib._local",
            source: include_str!("python/pathlib_local.py"),
            is_package: false,
        },
        FrozenSource {
            name: "argparse",
            source: include_str!("python/argparse.py"),
            is_package: false,
        },
        FrozenSource {
            name: "abc",
            source: include_str!("python/abc.py"),
            is_package: false,
        },
        FrozenSource {
            name: "enum",
            source: include_str!("python/enum.py"),
            is_package: false,
        },
        // `termios` — constants-only shim so pure-Python `tty` (pulled in by
        // `test_asyncio.test_events`) imports. Real terminal control is not
        // implemented; the tty syscalls fail cleanly on non-terminal fds and
        // the pty-backed tests skip (no `os.openpty`).
        FrozenSource {
            name: "termios",
            source: include_str!("python/termios_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "dataclasses",
            source: include_str!("python/dataclasses.py"),
            is_package: false,
        },
        FrozenSource {
            name: "typing",
            source: include_str!("python/typing.py"),
            is_package: false,
        },
        FrozenSource {
            name: "heapq",
            source: include_str!("python/heapq.py"),
            is_package: false,
        },
        FrozenSource {
            name: "threading",
            source: include_str!("python/threading.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_threading_local",
            source: include_str!("python/_threading_local.py"),
            is_package: false,
        },
        FrozenSource {
            name: "queue",
            source: include_str!("python/queue.py"),
            is_package: false,
        },
        // RFC 0040 (WS5): the *real* CPython `multiprocessing` package,
        // frozen verbatim from `vendor/cpython/Lib/multiprocessing/`,
        // running over the native `_multiprocessing` SemLock core, the
        // `_posixshmem` shared-memory core, `_posixsubprocess.fork_exec`
        // (spawn rides the standard `weavepy -c ...` + `os.posix_spawn`
        // path), and `os.fork` (the fork start method). Replaces the
        // single-file RFC 0026 shim. The Windows-only submodules
        // (`popen_spawn_win32`) are frozen for completeness but never
        // imported on POSIX.
        FrozenSource {
            name: "multiprocessing",
            source: include_str!("python/multiprocessing/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "multiprocessing.connection",
            source: include_str!("python/multiprocessing/connection.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.context",
            source: include_str!("python/multiprocessing/context.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.forkserver",
            source: include_str!("python/multiprocessing/forkserver.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.heap",
            source: include_str!("python/multiprocessing/heap.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.managers",
            source: include_str!("python/multiprocessing/managers.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.pool",
            source: include_str!("python/multiprocessing/pool.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.popen_fork",
            source: include_str!("python/multiprocessing/popen_fork.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.popen_forkserver",
            source: include_str!("python/multiprocessing/popen_forkserver.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.popen_spawn_posix",
            source: include_str!("python/multiprocessing/popen_spawn_posix.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.popen_spawn_win32",
            source: include_str!("python/multiprocessing/popen_spawn_win32.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.process",
            source: include_str!("python/multiprocessing/process.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.queues",
            source: include_str!("python/multiprocessing/queues.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.reduction",
            source: include_str!("python/multiprocessing/reduction.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.resource_sharer",
            source: include_str!("python/multiprocessing/resource_sharer.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.resource_tracker",
            source: include_str!("python/multiprocessing/resource_tracker.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.shared_memory",
            source: include_str!("python/multiprocessing/shared_memory.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.sharedctypes",
            source: include_str!("python/multiprocessing/sharedctypes.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.spawn",
            source: include_str!("python/multiprocessing/spawn.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.synchronize",
            source: include_str!("python/multiprocessing/synchronize.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.util",
            source: include_str!("python/multiprocessing/util.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing.dummy",
            source: include_str!("python/multiprocessing/dummy/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "multiprocessing.dummy.connection",
            source: include_str!("python/multiprocessing/dummy/connection.py"),
            is_package: false,
        },
        // RFC 0039 (WS7): the *real* CPython `concurrent.futures`
        // package, frozen verbatim from `vendor/cpython/Lib/concurrent/
        // futures/`. `ThreadPoolExecutor` now spins up real OS worker
        // threads (the old single-file shim ran `submit`ted work
        // synchronously on the caller, which broke `run_in_executor`
        // thread-affinity and the `test_asyncio` executor tests). The
        // dotted names resolve via the registered module name, not the
        // source filename. `process` is a stub (no multiprocessing
        // runtime); it stays importable so the lazy `__getattr__` in
        // `__init__` and `from concurrent.futures import *` still work.
        FrozenSource {
            name: "concurrent",
            source: "",
            is_package: true,
        },
        FrozenSource {
            name: "concurrent.futures",
            source: include_str!("python/concurrent_futures_init.py"),
            is_package: true,
        },
        FrozenSource {
            name: "concurrent.futures._base",
            source: include_str!("python/concurrent_futures_base.py"),
            is_package: false,
        },
        FrozenSource {
            name: "concurrent.futures.thread",
            source: include_str!("python/concurrent_futures_thread.py"),
            is_package: false,
        },
        FrozenSource {
            name: "concurrent.futures.process",
            source: include_str!("python/concurrent_futures_process.py"),
            is_package: false,
        },
        // RFC 0039 (WS7): the *real* CPython `asyncio` package, frozen
        // verbatim from `vendor/cpython/Lib/asyncio/`, running over the WS6
        // native selector backends. Replaces the old cooperative single-file
        // shim. The Windows-only submodules (`windows_events`/`windows_utils`/
        // `proactor_events`) are frozen for completeness but never imported on
        // a non-win32 build, so their `_winapi`/`_overlapped` deps don't load.
        FrozenSource {
            name: "asyncio",
            source: include_str!("python/asyncio/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "asyncio.base_events",
            source: include_str!("python/asyncio/base_events.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.base_futures",
            source: include_str!("python/asyncio/base_futures.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.base_subprocess",
            source: include_str!("python/asyncio/base_subprocess.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.base_tasks",
            source: include_str!("python/asyncio/base_tasks.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.constants",
            source: include_str!("python/asyncio/constants.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.coroutines",
            source: include_str!("python/asyncio/coroutines.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.events",
            source: include_str!("python/asyncio/events.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.exceptions",
            source: include_str!("python/asyncio/exceptions.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.format_helpers",
            source: include_str!("python/asyncio/format_helpers.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.futures",
            source: include_str!("python/asyncio/futures.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.locks",
            source: include_str!("python/asyncio/locks.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.log",
            source: include_str!("python/asyncio/log.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.mixins",
            source: include_str!("python/asyncio/mixins.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.proactor_events",
            source: include_str!("python/asyncio/proactor_events.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.protocols",
            source: include_str!("python/asyncio/protocols.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.queues",
            source: include_str!("python/asyncio/queues.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.runners",
            source: include_str!("python/asyncio/runners.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.selector_events",
            source: include_str!("python/asyncio/selector_events.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.sslproto",
            source: include_str!("python/asyncio/sslproto.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.staggered",
            source: include_str!("python/asyncio/staggered.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.streams",
            source: include_str!("python/asyncio/streams.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.subprocess",
            source: include_str!("python/asyncio/subprocess.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.taskgroups",
            source: include_str!("python/asyncio/taskgroups.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.tasks",
            source: include_str!("python/asyncio/tasks.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.threads",
            source: include_str!("python/asyncio/threads.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.timeouts",
            source: include_str!("python/asyncio/timeouts.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.transports",
            source: include_str!("python/asyncio/transports.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.trsock",
            source: include_str!("python/asyncio/trsock.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.unix_events",
            source: include_str!("python/asyncio/unix_events.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.windows_events",
            source: include_str!("python/asyncio/windows_events.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.windows_utils",
            source: include_str!("python/asyncio/windows_utils.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio.__main__",
            source: include_str!("python/asyncio/__main__.py"),
            is_package: false,
        },
        // High-level wrappers over Rust cores from RFC 0017.
        FrozenSource {
            name: "subprocess",
            source: include_str!("python/subprocess.py"),
            is_package: false,
        },
        FrozenSource {
            name: "socket",
            source: include_str!("python/socket.py"),
            is_package: false,
        },
        // RFC 0042 WS2 — CPython-shaped `ssl` over the native rustls `_ssl`
        // core (mirrors CPython's `Lib/ssl.py` over its `_ssl` C extension).
        FrozenSource {
            name: "ssl",
            source: include_str!("python/ssl.py"),
            is_package: false,
        },
        FrozenSource {
            name: "selectors",
            source: include_str!("python/selectors.py"),
            is_package: false,
        },
        FrozenSource {
            name: "tempfile",
            source: include_str!("python/tempfile.py"),
            is_package: false,
        },
        FrozenSource {
            name: "shutil",
            source: include_str!("python/shutil.py"),
            is_package: false,
        },
        // `fnmatch` / `glob` — verbatim CPython 3.13 ports (replacing the
        // earlier Rust shims). `glob` exposes the `_Globber`/`_StringGlobber`
        // helpers that the 3.13 `pathlib` rewrite imports.
        FrozenSource {
            name: "fnmatch",
            source: include_str!("python/fnmatch.py"),
            is_package: false,
        },
        FrozenSource {
            name: "glob",
            source: include_str!("python/glob.py"),
            is_package: false,
        },
        FrozenSource {
            name: "csv",
            source: include_str!("python/csv.py"),
            is_package: false,
        },
        FrozenSource {
            name: "mimetypes",
            source: include_str!("python/mimetypes.py"),
            is_package: false,
        },
        FrozenSource {
            name: "ipaddress",
            source: include_str!("python/ipaddress.py"),
            is_package: false,
        },
        FrozenSource {
            name: "socketserver",
            source: include_str!("python/socketserver.py"),
            is_package: false,
        },
        FrozenSource {
            name: "html",
            source: include_str!("python/html.py"),
            is_package: false,
        },
        FrozenSource {
            name: "html.parser",
            source: include_str!("python/html_parser.py"),
            is_package: false,
        },
        FrozenSource {
            name: "html.entities",
            source: include_str!("python/html_entities.py"),
            is_package: false,
        },
        // RFC 0042 WS4 — `urllib`, vendored verbatim from
        // `vendor/cpython/Lib/urllib/` (the `__init__` is empty upstream).
        // `request`/`response`/`error` now ride the WS1 `socket.makefile()`
        // and WS2 `ssl` stacks; `parse` was already verbatim.
        FrozenSource {
            name: "urllib",
            source: "",
            is_package: true,
        },
        FrozenSource {
            name: "urllib.parse",
            source: include_str!("python/urllib/parse.py"),
            is_package: false,
        },
        FrozenSource {
            name: "urllib.error",
            source: include_str!("python/urllib/error.py"),
            is_package: false,
        },
        FrozenSource {
            name: "urllib.response",
            source: include_str!("python/urllib/response.py"),
            is_package: false,
        },
        FrozenSource {
            name: "urllib.request",
            source: include_str!("python/urllib/request.py"),
            is_package: false,
        },
        // RFC 0042 WS3 — `http`, vendored verbatim from
        // `vendor/cpython/Lib/http/`. The real `__init__` exports the
        // `HTTPStatus`/`HTTPMethod` enums; `client`/`server` run over the WS1
        // `socket.makefile()` + WS2 `ssl` stacks. `cookiejar` (WS4) lets
        // `urllib.request.HTTPCookieProcessor` work unchanged.
        FrozenSource {
            name: "http",
            source: include_str!("python/http/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "http.client",
            source: include_str!("python/http/client.py"),
            is_package: false,
        },
        FrozenSource {
            name: "http.server",
            source: include_str!("python/http/server.py"),
            is_package: false,
        },
        FrozenSource {
            name: "http.cookies",
            source: include_str!("python/http/cookies.py"),
            is_package: false,
        },
        FrozenSource {
            name: "http.cookiejar",
            source: include_str!("python/http/cookiejar.py"),
            is_package: false,
        },
        // RFC 0042 WS3/WS5 — the real CPython `email` package, vendored
        // verbatim from `vendor/cpython/Lib/email/`. `http.client` parses
        // response headers with `email.parser`/`email.message`, and the
        // WS5 mail clients (`smtplib` etc.) build messages with `email.mime`.
        FrozenSource {
            name: "email",
            source: include_str!("python/email/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "email._encoded_words",
            source: include_str!("python/email/_encoded_words.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email._header_value_parser",
            source: include_str!("python/email/_header_value_parser.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email._parseaddr",
            source: include_str!("python/email/_parseaddr.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email._policybase",
            source: include_str!("python/email/_policybase.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.base64mime",
            source: include_str!("python/email/base64mime.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.charset",
            source: include_str!("python/email/charset.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.contentmanager",
            source: include_str!("python/email/contentmanager.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.encoders",
            source: include_str!("python/email/encoders.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.errors",
            source: include_str!("python/email/errors.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.feedparser",
            source: include_str!("python/email/feedparser.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.generator",
            source: include_str!("python/email/generator.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.header",
            source: include_str!("python/email/header.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.headerregistry",
            source: include_str!("python/email/headerregistry.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.iterators",
            source: include_str!("python/email/iterators.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.message",
            source: include_str!("python/email/message.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.parser",
            source: include_str!("python/email/parser.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.policy",
            source: include_str!("python/email/policy.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.quoprimime",
            source: include_str!("python/email/quoprimime.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.utils",
            source: include_str!("python/email/utils.py"),
            is_package: false,
        },
        // `email.mime.*` — message construction helpers (WS5 mail clients).
        FrozenSource {
            name: "email.mime",
            source: include_str!("python/email/mime/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "email.mime.application",
            source: include_str!("python/email/mime/application.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.mime.audio",
            source: include_str!("python/email/mime/audio.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.mime.base",
            source: include_str!("python/email/mime/base.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.mime.image",
            source: include_str!("python/email/mime/image.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.mime.message",
            source: include_str!("python/email/mime/message.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.mime.multipart",
            source: include_str!("python/email/mime/multipart.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.mime.nonmultipart",
            source: include_str!("python/email/mime/nonmultipart.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.mime.text",
            source: include_str!("python/email/mime/text.py"),
            is_package: false,
        },
        // `quopri` — quoted-printable codec used by `email`'s encoders/parsers
        // (verbatim CPython, over the native `binascii` a2b_qp/b2a_qp).
        FrozenSource {
            name: "quopri",
            source: include_str!("python/quopri.py"),
            is_package: false,
        },
        // `_scproxy` — macOS system-proxy shim (reports "no system proxy"); the
        // verbatim `urllib.request` imports it unconditionally on darwin.
        FrozenSource {
            name: "_scproxy",
            source: include_str!("python/_scproxy.py"),
            is_package: false,
        },
        // `stringprep` (RFC 3454 tables) + the `encodings.idna`/`encodings.punycode`
        // codecs. WeavePy serves most codecs natively, but `idna`/`punycode` are
        // pure-Python in CPython and are resolved on demand by `codecs.lookup`
        // (see `python/codecs.py`). `http.client`/`urllib` need `idna` to encode
        // non-ASCII hostnames. The `encodings` package is intentionally minimal
        // (just these two modules); it is NOT the codec search bootstrap.
        FrozenSource {
            name: "stringprep",
            source: include_str!("python/stringprep.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings",
            source: include_str!("python/encodings/__init__.py"),
            is_package: true,
        },
        FrozenSource {
            name: "encodings.idna",
            source: include_str!("python/encodings/idna.py"),
            is_package: false,
        },
        FrozenSource {
            name: "encodings.punycode",
            source: include_str!("python/encodings/punycode.py"),
            is_package: false,
        },
        // RFC 0042 WS5 — application-protocol clients, vendored verbatim from
        // CPython 3.13. They ride the WS1 `socket`/`makefile()` and WS2 `ssl`
        // stacks (`*_SSL` variants, `starttls`/`stls`). `nntplib`/`telnetlib`
        // were removed upstream in 3.13, so they are intentionally absent.
        FrozenSource {
            name: "ftplib",
            source: include_str!("python/ftplib.py"),
            is_package: false,
        },
        FrozenSource {
            name: "poplib",
            source: include_str!("python/poplib.py"),
            is_package: false,
        },
        FrozenSource {
            name: "imaplib",
            source: include_str!("python/imaplib.py"),
            is_package: false,
        },
        FrozenSource {
            name: "smtplib",
            source: include_str!("python/smtplib.py"),
            is_package: false,
        },
        // `xml` package and submodules — only `etree.ElementTree`.
        FrozenSource {
            name: "xml",
            source: "",
            is_package: true,
        },
        FrozenSource {
            name: "xml.etree",
            source: "",
            is_package: true,
        },
        FrozenSource {
            name: "xml.etree.ElementTree",
            source: include_str!("python/xml_etree.py"),
            is_package: false,
        },
        // RFC 0018 — introspection, test infrastructure, exception groups.
        FrozenSource {
            name: "struct",
            source: include_str!("python/struct.py"),
            is_package: false,
        },
        FrozenSource {
            name: "codecs",
            source: include_str!("python/codecs.py"),
            is_package: false,
        },
        // RFC 0040 WS7 — the JIS X 0213:2004 `euc_jis_2004` CJK codec, ported
        // faithfully (incl. its 25 stateful combining sequences) so the codec's
        // incremental *encoder* is stateful — exercised by
        // `test_io.test_seek_with_encoder_state`. Loaded lazily by
        // `codecs._lookup_uncached` (its 70 KB of packed tables stay cold until
        // the encoding is first used).
        FrozenSource {
            name: "_codec_euc_jis_2004",
            source: include_str!("python/_codec_euc_jis_2004.py"),
            is_package: false,
        },
        FrozenSource {
            name: "weakref",
            source: include_str!("python/weakref.py"),
            is_package: false,
        },
        FrozenSource {
            name: "datetime",
            source: include_str!("python/datetime.py"),
            is_package: false,
        },
        // RFC 0041 WS-datetime: CPython's verbatim pure-Python datetime
        // implementation, imported by the `datetime` shim above and exercised
        // directly by `test_datetime`'s _Pure pass.
        FrozenSource {
            name: "_pydatetime",
            source: include_str!("python/_pydatetime.py"),
            is_package: false,
        },
        FrozenSource {
            name: "linecache",
            source: include_str!("python/linecache.py"),
            is_package: false,
        },
        FrozenSource {
            name: "reprlib",
            source: include_str!("python/reprlib.py"),
            is_package: false,
        },
        FrozenSource {
            name: "warnings",
            source: include_str!("python/warnings.py"),
            is_package: false,
        },
        FrozenSource {
            name: "traceback",
            source: include_str!("python/traceback.py"),
            is_package: false,
        },
        FrozenSource {
            name: "inspect",
            source: include_str!("python/inspect.py"),
            is_package: false,
        },
        FrozenSource {
            name: "contextvars",
            source: include_str!("python/contextvars.py"),
            is_package: false,
        },
        FrozenSource {
            name: "logging",
            source: include_str!("python/logging.py"),
            is_package: false,
        },
        FrozenSource {
            name: "unittest",
            source: include_str!("python/unittest.py"),
            is_package: true,
        },
        FrozenSource {
            name: "unittest.mock",
            source: include_str!("python/unittest_mock.py"),
            is_package: false,
        },
        FrozenSource {
            name: "unittest.__main__",
            source: include_str!("python/unittest_main.py"),
            is_package: false,
        },
        // `doctest` (RFC 0034) — interactive-example testing, used by
        // `test.support.run_doctest` and stdlib self-tests.
        FrozenSource {
            name: "doctest",
            source: include_str!("python/doctest.py"),
            is_package: false,
        },
        // RFC 0034 — the `test` package: CPython's regression-test
        // harness glue. `test.support` (+ 3.13 helper submodules) is the
        // import-time prerequisite for every `Lib/test/test_*.py`;
        // `test.libregrtest` + `test.__main__` drive `weavepy -m test`.
        FrozenSource {
            name: "test",
            source: include_str!("python/test_init.py"),
            is_package: true,
        },
        FrozenSource {
            name: "test.support",
            source: include_str!("python/test_support_init.py"),
            is_package: true,
        },
        FrozenSource {
            name: "test.support.os_helper",
            source: include_str!("python/test_support_os_helper.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.support.import_helper",
            source: include_str!("python/test_support_import_helper.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.support.warnings_helper",
            source: include_str!("python/test_support_warnings_helper.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.support.threading_helper",
            source: include_str!("python/test_support_threading_helper.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.support.script_helper",
            source: include_str!("python/test_support_script_helper.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.support.socket_helper",
            source: include_str!("python/test_support_socket_helper.py"),
            is_package: false,
        },
        // `test.support.hashlib_helper` (verbatim) — `requires_hashdigest`
        // gate used by test_hmac and friends.
        FrozenSource {
            name: "test.support.hashlib_helper",
            source: include_str!("python/test_support_hashlib_helper.py"),
            is_package: false,
        },
        // `test.support.i18n_helper` — minimal shim (snapshot tests skip) so
        // test_getopt/test_optparse import; their own tests still run.
        FrozenSource {
            name: "test.support.i18n_helper",
            source: include_str!("python/test_support_i18n_helper.py"),
            is_package: false,
        },
        // RFC 0036 — two more 3.13 helper submodules carried verbatim:
        // `testcase` (ExceptionIsLikeMixin + float/complex assertions used
        // by test_float/test_complex) and `numbers` (the numeric-tower
        // sample values test_int/test_complex iterate over).
        FrozenSource {
            name: "test.support.testcase",
            source: include_str!("python/test_support_testcase.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.support.numbers",
            source: include_str!("python/test_support_numbers.py"),
            is_package: false,
        },
        // `test.tokenizedata`: vendored lexer/tokenizer fixtures.
        // `test_unicode_identifiers` imports `badsyntax_3131` to assert the
        // exact `SyntaxError` for an invalid PEP 3131 identifier (`€`).
        FrozenSource {
            name: "test.tokenizedata",
            source: include_str!("python/test_tokenizedata_init.py"),
            is_package: true,
        },
        FrozenSource {
            name: "test.tokenizedata.badsyntax_3131",
            source: include_str!("python/test_tokenizedata_badsyntax_3131.py"),
            is_package: false,
        },
        // `test.string_tests`: the shared CommonTest/MixinStrUnicodeUserStringTest
        // base classes that `test_bytes`/`test_bytearray`/`test_str` derive
        // from. Carried verbatim from CPython 3.13.
        FrozenSource {
            name: "test.string_tests",
            source: include_str!("python/test_string_tests.py"),
            is_package: false,
        },
        // `test.seq_tests` / `test.list_tests`: shared sequence/list test
        // mixins (verbatim CPython 3.13) that `test_bytes`/`test_list`/
        // `test_tuple`/`test_deque` and friends import.
        FrozenSource {
            name: "test.seq_tests",
            source: include_str!("python/test_seq_tests.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.list_tests",
            source: include_str!("python/test_list_tests.py"),
            is_package: false,
        },
        // `test.pickletester`: only `ExtensionSaver` is carried (test_copyreg
        // imports it); the full CPython file is ~4900 lines of pickle matrix.
        FrozenSource {
            name: "test.pickletester",
            source: include_str!("python/test_pickletester.py"),
            is_package: false,
        },
        // `test.__main__` / `test.regrtest`: drive `weavepy -m test` and
        // `weavepy -m test.regrtest`. The runner itself lives in the
        // `test.libregrtest` package below.
        FrozenSource {
            name: "test.__main__",
            source: include_str!("python/test_main.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.regrtest",
            source: include_str!("python/test_regrtest.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest",
            source: include_str!("python/test_libregrtest_init.py"),
            is_package: true,
        },
        FrozenSource {
            name: "test.libregrtest.result",
            source: include_str!("python/test_libregrtest_result.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.cmdline",
            source: include_str!("python/test_libregrtest_cmdline.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.findtests",
            source: include_str!("python/test_libregrtest_findtests.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.save_env",
            source: include_str!("python/test_libregrtest_save_env.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.single",
            source: include_str!("python/test_libregrtest_single.py"),
            is_package: false,
        },
        FrozenSource {
            name: "test.libregrtest.main",
            source: include_str!("python/test_libregrtest_main.py"),
            is_package: false,
        },
        FrozenSource {
            name: "runpy",
            source: include_str!("python/runpy.py"),
            is_package: false,
        },
        // RFC 0040 WS5 — import modules from ZIP archives on `sys.path`
        // (PEP 273). Self-contained reimplementation over the frozen
        // `zipfile`; plugs into `sys.path_hooks` for the Python `find_spec`
        // path and is reached by the Rust loader's meta-path fallback below.
        FrozenSource {
            name: "zipimport",
            source: include_str!("python/zipimport.py"),
            is_package: false,
        },
        // RFC 0040 WS5 — bridge the Rust import loader to `sys.meta_path`
        // for module kinds it doesn't resolve natively (zip archives,
        // sourceless `.pyc` via a custom finder). Called from `load_one`.
        FrozenSource {
            name: "_weave_import_fallback",
            source: include_str!("python/_weave_import_fallback.py"),
            is_package: false,
        },
        FrozenSource {
            name: "codeop",
            source: include_str!("python/codeop.py"),
            is_package: false,
        },
        FrozenSource {
            name: "code",
            source: include_str!("python/code.py"),
            is_package: false,
        },
        // Compression wrappers (RFC 0019).
        // Shared buffered/decompress reader used by gzip/bz2/lzma (CPython
        // `Lib/_compression.py`, ported verbatim).
        FrozenSource {
            name: "_compression",
            source: include_str!("python/_compression.py"),
            is_package: false,
        },
        FrozenSource {
            name: "gzip",
            source: include_str!("python/gzip.py"),
            is_package: false,
        },
        FrozenSource {
            name: "bz2",
            source: include_str!("python/bz2.py"),
            is_package: false,
        },
        FrozenSource {
            name: "lzma",
            source: include_str!("python/lzma.py"),
            is_package: false,
        },
        // RFC 0040 WS8 — `zipfile` is CPython 3.13's faithful package
        // (`zipfile/__init__.py` + the `zipfile._path` Path accessor), not
        // the old custom single-module shim. Bundled verbatim and frozen as
        // a package so `zipfile.Path`, `PyZipFile`, ZIP64, per-file
        // compression, `mkdir`, `testzip`, etc. all work.
        FrozenSource {
            name: "zipfile",
            source: include_str!("python/zipfile.py"),
            is_package: true,
        },
        FrozenSource {
            name: "zipfile._path",
            source: include_str!("python/zipfile__path.py"),
            is_package: true,
        },
        FrozenSource {
            name: "zipfile._path.glob",
            source: include_str!("python/zipfile__path_glob.py"),
            is_package: false,
        },
        // `python -m zipfile` runs the package's `__main__` (runpy redirects
        // `<pkg>` -> `<pkg>.__main__`); ship it so the CLI works.
        FrozenSource {
            name: "zipfile.__main__",
            source: include_str!("python/zipfile__main__.py"),
            is_package: false,
        },
        FrozenSource {
            name: "tarfile",
            source: include_str!("python/tarfile.py"),
            is_package: false,
        },
        FrozenSource {
            name: "sqlite3",
            source: include_str!("python/sqlite3.py"),
            is_package: false,
        },
        FrozenSource {
            name: "copyreg",
            source: include_str!("python/copyreg.py"),
            is_package: false,
        },
        FrozenSource {
            name: "pickle",
            source: include_str!("python/pickle.py"),
            is_package: false,
        },
        FrozenSource {
            name: "shelve",
            source: include_str!("python/shelve.py"),
            is_package: false,
        },
        FrozenSource {
            name: "fractions",
            source: include_str!("python/fractions.py"),
            is_package: false,
        },
        FrozenSource {
            name: "decimal",
            source: include_str!("python/decimal.py"),
            is_package: false,
        },
        // Full CPython pure-Python decimal (IEEE 754-2008: NaN/Infinity,
        // contexts, traps, exact float/Decimal comparison + hashing). The
        // `decimal` shim above re-exports this via `sys.modules` like CPython.
        FrozenSource {
            name: "_pydecimal",
            source: include_str!("python/_pydecimal.py"),
            is_package: false,
        },
        FrozenSource {
            name: "py_compile",
            source: include_str!("python/py_compile.py"),
            is_package: false,
        },
        FrozenSource {
            name: "compileall",
            source: include_str!("python/compileall.py"),
            is_package: false,
        },
        // RFC 0020 — bootstrap modules for the "real `python(1)`" arc.
        FrozenSource {
            name: "site",
            source: include_str!("python/site.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib",
            source: include_str!("python/importlib_init.py"),
            is_package: true,
        },
        FrozenSource {
            name: "importlib.machinery",
            source: include_str!("python/importlib_machinery.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib.util",
            source: include_str!("python/importlib_util.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib.abc",
            source: include_str!("python/importlib_abc.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib.metadata",
            source: include_str!("python/importlib_metadata.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib.resources",
            source: include_str!("python/importlib_resources.py"),
            is_package: false,
        },
        // CPython's frozen import-core modules; stdlib code (pydoc,
        // pkgutil-adjacent paths) imports these by name.
        FrozenSource {
            name: "importlib._bootstrap",
            source: include_str!("python/importlib_bootstrap.py"),
            is_package: false,
        },
        FrozenSource {
            name: "importlib._bootstrap_external",
            source: include_str!("python/importlib_bootstrap_external.py"),
            is_package: false,
        },
        FrozenSource {
            name: "pkgutil",
            source: include_str!("python/pkgutil.py"),
            is_package: false,
        },
        // RFC 0037 WS8 — pydoc and its dependency closure.
        FrozenSource {
            name: "pydoc",
            source: include_str!("python/pydoc.py"),
            is_package: false,
        },
        FrozenSource {
            name: "token",
            source: include_str!("python/token.py"),
            is_package: false,
        },
        FrozenSource {
            name: "tokenize",
            source: include_str!("python/tokenize.py"),
            is_package: false,
        },
        FrozenSource {
            name: "sysconfig",
            source: include_str!("python/sysconfig.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pyrepl",
            source: include_str!("python/_pyrepl_init.py"),
            is_package: true,
        },
        FrozenSource {
            name: "_pyrepl.pager",
            source: include_str!("python/_pyrepl_pager.py"),
            is_package: false,
        },
        FrozenSource {
            name: "venv",
            source: include_str!("python/venv_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "ensurepip",
            source: include_str!("python/ensurepip.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_minipip",
            source: include_str!("python/_minipip.py"),
            is_package: false,
        },
        // Debugger.
        FrozenSource {
            name: "cmd",
            source: include_str!("python/cmd_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "bdb",
            source: include_str!("python/bdb_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "pdb",
            source: include_str!("python/pdb_mod.py"),
            is_package: false,
        },
        // RFC 0031 — PEP 684 sub-interpreters friendly frontend.
        FrozenSource {
            name: "interpreters",
            source: include_str!("python/interpreters.py"),
            is_package: false,
        },
        // Small stdlib modules.
        FrozenSource {
            name: "pprint",
            source: include_str!("python/pprint_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "tomllib",
            source: include_str!("python/tomllib_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "configparser",
            source: include_str!("python/configparser_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "getopt",
            source: include_str!("python/getopt_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "gettext",
            source: include_str!("python/gettext_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "optparse",
            source: include_str!("python/optparse_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "timeit",
            source: include_str!("python/timeit_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "profile",
            source: include_str!("python/profile_mod.py"),
            is_package: false,
        },
        // `cProfile` is an alias for `profile` in WeavePy — we don't
        // (yet) ship a C-accelerated profiler.
        FrozenSource {
            name: "cProfile",
            source: include_str!("python/profile_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "pstats",
            source: include_str!("python/pstats_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "webbrowser",
            source: include_str!("python/webbrowser_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "array",
            source: include_str!("python/array_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "plistlib",
            source: include_str!("python/plistlib_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "zoneinfo",
            source: include_str!("python/zoneinfo_mod.py"),
            is_package: false,
        },
        FrozenSource {
            // RFC 0039 WS7: faithful CPython async_case (persistent Runner).
            name: "unittest.async_case",
            source: include_str!("python/unittest_async.py"),
            is_package: false,
        },
        // RFC 0023 — fill in the small but commonly-imported stdlib
        // gaps.
        FrozenSource {
            name: "bisect",
            source: include_str!("python/bisect_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "operator",
            source: include_str!("python/operator_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "copy",
            source: include_str!("python/copy_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "stat",
            source: include_str!("python/stat_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "genericpath",
            source: include_str!("python/genericpath_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "posixpath",
            source: include_str!("python/posixpath_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "ntpath",
            source: include_str!("python/ntpath_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "textwrap",
            source: include_str!("python/textwrap_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "numbers",
            source: include_str!("python/numbers_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "statistics",
            source: include_str!("python/statistics_mod.py"),
            is_package: false,
        },
        // RFC 0026 — fill in the last commonly-imported gaps.
        FrozenSource {
            name: "types",
            source: include_str!("python/types_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "posix",
            source: include_str!("python/posix_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "nt",
            source: include_str!("python/nt_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_oswalk",
            source: include_str!("python/_oswalk.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_multiprocessing_helpers",
            source: include_str!("python/_multiprocessing_helpers.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_concurrent_process",
            source: include_str!("python/_concurrent_process.py"),
            is_package: false,
        },
        // RFC 0030 — real PyPI client (packaging utils, PEP 517 builds),
        // numpy facade, pytest+pluggy.
        FrozenSource {
            name: "_packaging",
            source: include_str!("python/_packaging.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pip_resolver",
            source: include_str!("python/_pip_resolver.py"),
            is_package: false,
        },
        FrozenSource {
            name: "_pep517",
            source: include_str!("python/_pep517.py"),
            is_package: false,
        },
        // Expose the WeavePy pip under the canonical `pip` name as well.
        FrozenSource {
            name: "pip",
            source: include_str!("python/_minipip.py"),
            is_package: false,
        },
        // `packaging` is a third-party project on PyPI but extremely
        // commonly imported. Map it to our in-tree `_packaging`.
        FrozenSource {
            name: "packaging",
            source: include_str!("python/packaging_init.py"),
            is_package: true,
        },
        FrozenSource {
            name: "packaging.version",
            source: include_str!("python/packaging_version.py"),
            is_package: false,
        },
        FrozenSource {
            name: "packaging.specifiers",
            source: include_str!("python/packaging_specifiers.py"),
            is_package: false,
        },
        FrozenSource {
            name: "packaging.requirements",
            source: include_str!("python/packaging_requirements.py"),
            is_package: false,
        },
        FrozenSource {
            name: "packaging.markers",
            source: include_str!("python/packaging_markers.py"),
            is_package: false,
        },
        FrozenSource {
            name: "packaging.utils",
            source: include_str!("python/packaging_utils.py"),
            is_package: false,
        },
        FrozenSource {
            name: "packaging.tags",
            source: include_str!("python/packaging_tags.py"),
            is_package: false,
        },
        // numpy-compatible facade over the bundled `_numpylike` C
        // extension. Real numpy code that doesn't reach into the
        // C-level internals "just works".
        FrozenSource {
            name: "_numpy_pure",
            source: include_str!("python/_numpy_pure.py"),
            is_package: false,
        },
        FrozenSource {
            name: "numpy",
            source: include_str!("python/numpy_init.py"),
            is_package: false,
        },
        // pytest + pluggy shims.
        FrozenSource {
            name: "pluggy",
            source: include_str!("python/_pluggy.py"),
            is_package: false,
        },
        FrozenSource {
            name: "pytest",
            source: include_str!("python/_pytest.py"),
            is_package: false,
        },
        FrozenSource {
            name: "iniconfig",
            source: include_str!("python/iniconfig_mod.py"),
            is_package: false,
        },
        FrozenSource {
            name: "exceptiongroup",
            source: include_str!("python/exceptiongroup_mod.py"),
            is_package: false,
        },
        // RFC 0033 — bytecode & introspection compatibility layer.
        FrozenSource {
            name: "opcode",
            source: include_str!("python/opcode.py"),
            is_package: false,
        },
        FrozenSource {
            name: "dis",
            source: include_str!("python/dis.py"),
            is_package: false,
        },
        FrozenSource {
            name: "ast",
            source: include_str!("python/ast.py"),
            is_package: false,
        },
        FrozenSource {
            name: "symtable",
            source: include_str!("python/symtable.py"),
            is_package: false,
        },
        // RFC 0035 — the `re` package: a faithful port of CPython's
        // secret-labs engine. `_constants` / `_parser` / `_compiler` /
        // `_casefix` are verbatim from CPython 3.13; `_engine` builds the
        // Pattern / Match objects on top of the native `_sre` core.
        FrozenSource {
            name: "re",
            source: include_str!("python/re_init.py"),
            is_package: true,
        },
        FrozenSource {
            name: "re._constants",
            source: include_str!("python/re_constants.py"),
            is_package: false,
        },
        FrozenSource {
            name: "re._casefix",
            source: include_str!("python/re_casefix.py"),
            is_package: false,
        },
        FrozenSource {
            name: "re._parser",
            source: include_str!("python/re_parser.py"),
            is_package: false,
        },
        FrozenSource {
            name: "re._compiler",
            source: include_str!("python/re_compiler.py"),
            is_package: false,
        },
        FrozenSource {
            name: "re._engine",
            source: include_str!("python/re_engine.py"),
            is_package: false,
        },
        // Deprecated 3.x aliases kept for compatibility with code that
        // still imports the pre-3.11 module names.
        FrozenSource {
            name: "sre_constants",
            source: include_str!("python/sre_constants.py"),
            is_package: false,
        },
        FrozenSource {
            name: "sre_parse",
            source: include_str!("python/sre_parse.py"),
            is_package: false,
        },
        FrozenSource {
            name: "sre_compile",
            source: include_str!("python/sre_compile.py"),
            is_package: false,
        },
        // Pure-Python stand-in for CPython's `_testlimitedcapi` C test
        // helper. The conformance suite (e.g. `test_bytes`) imports it at
        // class-body scope; without it the whole module aborts. We supply
        // faithful Python equivalents of the abstract `PySequence_*`
        // wrappers it exercises.
        FrozenSource {
            name: "_testlimitedcapi",
            source: include_str!("python/_testlimitedcapi.py"),
            is_package: false,
        },
        // Pure-Python stand-in for `_testcapi`, covering the traceback
        // hooks (`exception_print` -> PyErr_Display via the traceback
        // module, `traceback_print` -> PyTraceBack_Print).
        FrozenSource {
            name: "_testcapi",
            source: include_str!("python/_testcapi.py"),
            is_package: false,
        },
    ]
}
