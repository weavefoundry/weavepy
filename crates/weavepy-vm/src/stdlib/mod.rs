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
pub mod base64_mod;
pub mod binascii_mod;
pub mod bz2_mod;
pub mod codecs_mod;
pub mod csv_mod;
pub mod datetime_mod;
pub mod errno_mod;
pub mod fcntl_mod;
pub mod fnmatch_mod;
pub mod gc_mod;
pub mod glob_mod;
pub mod gzip_mod;
pub mod hashlib_mod;
pub mod hmac_mod;
pub mod imp_mod;
pub mod interpreters_mod;
pub mod io;
pub mod json;
pub mod lzma_mod;
pub mod marshal_mod;
pub mod math;
pub mod os;
pub mod random;
pub mod re;
pub mod resource_mod;
pub mod secrets_mod;
pub mod select_mod;
pub mod shutil_mod;
pub mod signal_mod;
pub mod socket_mod;
pub mod sqlite3_mod;
pub mod ssl_mod;
pub mod struct_mod;
pub mod subprocess_mod;
pub mod symtable_mod;
pub mod sys;
pub mod sys_monitoring;
pub mod tempfile_mod;
pub mod thread;
pub mod time;
pub mod tracemalloc_real;
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
    cache.register_builtin("io", io::build);
    cache.register_builtin("re", re::build);
    cache.register_builtin("json", json::build);
    cache.register_builtin("random", random::build);
    cache.register_builtin("time", time::build);
    cache.register_builtin("_thread", thread_real::build);
    cache.register_builtin("errno", errno_mod::build);
    cache.register_builtin("signal", signal_mod::build);
    cache.register_builtin("select", select_mod::build);
    cache.register_builtin("_socket", socket_mod::build);
    cache.register_builtin("_subprocess", subprocess_mod::build);
    cache.register_builtin("hashlib", hashlib_mod::build);
    cache.register_builtin("hmac", hmac_mod::build);
    cache.register_builtin("base64", base64_mod::build);
    cache.register_builtin("binascii", binascii_mod::build);
    cache.register_builtin("secrets", secrets_mod::build);
    cache.register_builtin("uuid", uuid_mod::build);
    cache.register_builtin("_tempfile", tempfile_mod::build);
    cache.register_builtin("fnmatch", fnmatch_mod::build);
    cache.register_builtin("glob", glob_mod::build);
    cache.register_builtin("_shutil", shutil_mod::build);
    cache.register_builtin("ssl", ssl_mod::build);
    cache.register_builtin("zlib", zlib_mod::build);
    cache.register_builtin("_struct", struct_mod::build);
    cache.register_builtin("_codecs", codecs_mod::build);
    cache.register_builtin("marshal", marshal_mod::build);
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
    cache.register_builtin("_datetime", datetime_mod::build);
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
        FrozenSource {
            name: "collections",
            source: include_str!("python/collections.py"),
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
        FrozenSource {
            name: "contextlib",
            source: include_str!("python/contextlib.py"),
            is_package: false,
        },
        FrozenSource {
            name: "pathlib",
            source: include_str!("python/pathlib.py"),
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
            name: "queue",
            source: include_str!("python/queue.py"),
            is_package: false,
        },
        FrozenSource {
            name: "multiprocessing",
            source: include_str!("python/multiprocessing.py"),
            is_package: false,
        },
        // The `concurrent` package is a tiny shim that re-exports
        // `futures`. We model it as a frozen package with an
        // (effectively empty) `__init__` and a flat `futures`
        // submodule. Note we use `concurrent_futures.py` on disk —
        // the dotted name still resolves correctly because the
        // import machinery keys off the registered module name, not
        // the source filename.
        FrozenSource {
            name: "concurrent",
            source: "",
            is_package: true,
        },
        FrozenSource {
            name: "concurrent.futures",
            source: include_str!("python/concurrent_futures.py"),
            is_package: false,
        },
        FrozenSource {
            name: "asyncio",
            source: include_str!("python/asyncio.py"),
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
        // `urllib` is a package containing three submodules.
        FrozenSource {
            name: "urllib",
            source: "",
            is_package: true,
        },
        FrozenSource {
            name: "urllib.parse",
            source: include_str!("python/urllib_parse.py"),
            is_package: false,
        },
        FrozenSource {
            name: "urllib.error",
            source: include_str!("python/urllib_error.py"),
            is_package: false,
        },
        FrozenSource {
            name: "urllib.response",
            source: include_str!("python/urllib_response.py"),
            is_package: false,
        },
        FrozenSource {
            name: "urllib.request",
            source: include_str!("python/urllib_request.py"),
            is_package: false,
        },
        // `http` package and submodules.
        FrozenSource {
            name: "http",
            source: "",
            is_package: true,
        },
        FrozenSource {
            name: "http.client",
            source: include_str!("python/http_client.py"),
            is_package: false,
        },
        FrozenSource {
            name: "http.server",
            source: include_str!("python/http_server.py"),
            is_package: false,
        },
        FrozenSource {
            name: "http.cookies",
            source: include_str!("python/http_cookies.py"),
            is_package: false,
        },
        // `email` package and submodules.
        FrozenSource {
            name: "email",
            source: include_str!("python/email_init.py"),
            is_package: true,
        },
        FrozenSource {
            name: "email.message",
            source: include_str!("python/email_message.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.parser",
            source: include_str!("python/email_parser.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.utils",
            source: include_str!("python/email_utils.py"),
            is_package: false,
        },
        FrozenSource {
            name: "email.generator",
            source: include_str!("python/email_generator.py"),
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
        FrozenSource {
            name: "linecache",
            source: include_str!("python/linecache.py"),
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
            name: "runpy",
            source: include_str!("python/runpy.py"),
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
        FrozenSource {
            name: "zipfile",
            source: include_str!("python/zipfile.py"),
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
        FrozenSource {
            name: "pkgutil",
            source: include_str!("python/pkgutil.py"),
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
    ]
}
