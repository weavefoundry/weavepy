//! The `_sysconfig` built-in module (RFC 0055 WS1).
//!
//! CPython 3.13 split the immutable build-time variables out of the
//! generated `_sysconfigdata_*` module into a native `_sysconfig`
//! extension (gh-103480). Its single entry point, `config_vars()`,
//! returns a fresh dict of compile-time facts; `Lib/sysconfig` merges
//! it on Windows (`_init_non_posix`) and `test_sysconfig` imports the
//! module unconditionally at collection time.
//!
//! WeavePy's values mirror the frozen `_weave_sysconfigdata` module
//! (the `_sysconfigdata_*` analog) so the two sources can never
//! disagree about `EXT_SUFFIX`/`SOABI`.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

/// CPython 3.13's multiarch tag for this platform
/// (`sys.implementation._multiarch`). WeavePy reports CPython's values
/// because the RFC 0043–0047 binary ABI genuinely loads stock
/// `cp313` extensions — the tag describes the ABI accepted, not the
/// implementation name (`sysconfig` derives `_sysconfigdata_*` module
/// names and the `config-3.13-{multiarch}` directory from it).
pub const MULTIARCH: &str = if cfg!(target_os = "macos") {
    "darwin"
} else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    "x86_64-linux-gnu"
} else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
    "aarch64-linux-gnu"
} else {
    ""
};

/// The primary extension-module suffix — must equal
/// `_imp.extension_suffixes()[0]` (`test_sysconfig` asserts the
/// identity, and setuptools names built extensions with it).
pub const EXT_SUFFIX: &str = if cfg!(target_os = "macos") {
    ".cpython-313-darwin.so"
} else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    ".cpython-313-x86_64-linux-gnu.so"
} else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
    ".cpython-313-aarch64-linux-gnu.so"
} else if cfg!(windows) {
    ".cp313-win_amd64.pyd"
} else {
    ".so"
};

/// The shared-object ABI tag (`EXT_SUFFIX` minus the dot and the
/// trailing `.so`/`.pyd`).
pub const SOABI: &str = if cfg!(target_os = "macos") {
    "cpython-313-darwin"
} else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    "cpython-313-x86_64-linux-gnu"
} else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
    "cpython-313-aarch64-linux-gnu"
} else if cfg!(windows) {
    "cp313-win_amd64"
} else {
    ""
};

/// RFC 0062 WS2 — compiler variables for building C-extension sdists
/// against the installed header tree. These are what setuptools'
/// `customize_compiler()` consumes; the frozen `_weave_sysconfigdata`
/// carries the same values per-platform (keep the two in sync), and
/// `stdlib_tree::materialize` mirrors them into the on-disk
/// `config-3.13*/Makefile`.
///
/// Like a static-libpython CPython, extensions do *not* link a
/// libpython: symbols resolve from the process at load time (macOS
/// `-undefined dynamic_lookup`; Linux `-Wl,--export-dynamic` on the
/// binary — both landed with RFC 0043).
pub const CC: &str = "cc";
pub const CXX: &str = "c++";
pub const CFLAGS: &str = "-fno-strict-overflow -Wsign-compare -DNDEBUG -g -O3 -Wall";
pub const OPT: &str = "-DNDEBUG -g -O3 -Wall";
pub const CCSHARED: &str = if cfg!(target_os = "macos") {
    // Mach-O objects are position-independent by construction.
    ""
} else {
    "-fPIC"
};
pub const LDSHARED: &str = if cfg!(target_os = "macos") {
    "cc -bundle -undefined dynamic_lookup"
} else {
    "cc -shared"
};
pub const LDCXXSHARED: &str = if cfg!(target_os = "macos") {
    "c++ -bundle -undefined dynamic_lookup"
} else {
    "c++ -shared"
};

fn config_vars_dict() -> Object {
    let mut d = DictData::default();
    d.insert(
        DictKey(Object::from_static("EXT_SUFFIX")),
        Object::from_static(EXT_SUFFIX),
    );
    d.insert(
        DictKey(Object::from_static("SOABI")),
        Object::from_static(SOABI),
    );
    // 3.13's free-threading ABI marker: `"t"` on `--disable-gil`
    // builds, `""` otherwise. WeavePy has a GIL.
    d.insert(
        DictKey(Object::from_static("ABI_THREAD")),
        Object::from_static(""),
    );
    d.insert(DictKey(Object::from_static("Py_DEBUG")), Object::Int(0));
    d.insert(
        DictKey(Object::from_static("WITH_PYMALLOC")),
        Object::Int(0),
    );
    d.insert(
        DictKey(Object::from_static("Py_GIL_DISABLED")),
        Object::Int(0),
    );
    Object::Dict(Rc::new(RefCell::new(d)))
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_sysconfig"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("A helper for the sysconfig module."),
        );
        d.insert(
            DictKey(Object::from_static("config_vars")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "config_vars",
                binds_instance: false,
                call: Box::new(|_args| Ok(config_vars_dict())),
                call_kw: None,
            })),
        );
    }
    Rc::new(PyModule {
        name: "_sysconfig".to_owned(),
        filename: None,
        dict,
    })
}
