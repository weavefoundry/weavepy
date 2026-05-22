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

pub mod base64_mod;
pub mod binascii_mod;
pub mod csv_mod;
pub mod datetime_mod;
pub mod errno_mod;
pub mod fnmatch_mod;
pub mod gc_mod;
pub mod glob_mod;
pub mod hashlib_mod;
pub mod hmac_mod;
pub mod io;
pub mod json;
pub mod math;
pub mod os;
pub mod random;
pub mod re;
pub mod secrets_mod;
pub mod select_mod;
pub mod shutil_mod;
pub mod signal_mod;
pub mod socket_mod;
pub mod ssl_mod;
pub mod subprocess_mod;
pub mod sys;
pub mod tempfile_mod;
pub mod thread;
pub mod time;
pub mod uuid_mod;
pub mod weakref_mod;
pub mod zlib_mod;

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
    cache.register_builtin("_thread", thread::build);
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
    cache.register_builtin("_csv", csv_mod::build);
    cache.register_builtin("_weakref", weakref_mod::build);
    cache.register_builtin("gc", gc_mod::build);
    cache.register_builtin("_datetime", datetime_mod::build);

    // Frozen Python sources (pure-Python stdlib).
    for src in frozen_sources() {
        cache.register_frozen(*src);
    }
}

fn frozen_sources() -> &'static [FrozenSource] {
    &[
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
    ]
}
