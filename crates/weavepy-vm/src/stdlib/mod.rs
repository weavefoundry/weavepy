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

pub mod io;
pub mod json;
pub mod math;
pub mod os;
pub mod random;
pub mod re;
pub mod sys;
pub mod time;

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
    ]
}
