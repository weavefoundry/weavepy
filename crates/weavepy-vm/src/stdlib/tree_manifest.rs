//! Inputs and fingerprint for the materialized stdlib cache.
//! Compiled by both build.rs and the VM so their cache identities cannot drift.

use super::frozen_sources::FrozenSource;

/// Non-`.py` files materialized into the stdlib tree (RFC 0055 WS2):
/// the verbatim `venv` activation scripts, found by
/// `venv.EnvBuilder.setup_scripts` relative to `venv.__file__`.
/// Paths are relative to the stdlib dir, `/`-separated.
pub(crate) const DATA_FILES: &[(&str, &str)] = &[
    // (`os.py` is projected by its frozen-source registration now —
    // the startup import stays native, but a fresh `import os`
    // executes the source like CPython post-startup.)
    // RFC 0068 WS4 — `importlib._bootstrap[_external]` are NOT frozen
    // under their dotted names (importlib/__init__ aliases the frozen
    // `_frozen_importlib*` modules into sys.modules; a second frozen
    // execution would mint duplicate classes with the wrong
    // `__module__`). The source files still must exist on disk for
    // `importlib/__init__`'s ImportError fallback and test_importlib's
    // "Source" variant, which blocks the frozen names and re-imports
    // importlib from disk.
    (
        "importlib/_bootstrap.py",
        include_str!("python/importlib_bootstrap.py"),
    ),
    (
        "importlib/_bootstrap_external.py",
        include_str!("python/importlib_bootstrap_external.py"),
    ),
    (
        "venv/scripts/common/activate",
        include_str!("python/venv/scripts/common/activate"),
    ),
    (
        "venv/scripts/common/Activate.ps1",
        include_str!("python/venv/scripts/common/Activate.ps1"),
    ),
    (
        "venv/scripts/common/activate.fish",
        include_str!("python/venv/scripts/common/activate.fish"),
    ),
    (
        "venv/scripts/posix/activate.csh",
        include_str!("python/venv/scripts/posix/activate.csh"),
    ),
    // The NT activation pair (RFC 0063 WS6), adopted verbatim from
    // CPython 3.13's `Lib/venv/scripts/nt/` — including the chcp
    // code-page save/restore dance and `VIRTUAL_ENV_PROMPT`. CRLF line
    // endings are load-bearing for cmd.exe and preserved end to end
    // (a directory-local .gitattributes exempts them from the repo's
    // LF normalization). `Activate.ps1` already lives in `common`.
    (
        "venv/scripts/nt/activate.bat",
        include_str!("python/venv/scripts/nt/activate.bat"),
    ),
    (
        "venv/scripts/nt/deactivate.bat",
        include_str!("python/venv/scripts/nt/deactivate.bat"),
    ),
    // pydoc's stylesheet, served by `pydoc` and `xmlrpc.server`'s
    // DocXMLRPCServer (`_get_css` opens it relative to `__file__`).
    (
        "pydoc_data/_pydoc.css",
        include_str!("python/pydoc_data/_pydoc.css"),
    ),
    // RFC 0066 WS4: the bundled greenlet's dist-info. The stdlib tree
    // is a `sys.path` entry, so importlib.metadata (and pip listing)
    // sees the bundled native greenlet as an installed distribution —
    // dependents like SQLAlchemy's asyncio extension probe exactly
    // this. The version must agree with `_greenlet.GREENLET_VERSION`
    // and the directory name below.
    (
        "greenlet-3.2.0.dist-info/METADATA",
        include_str!("python/greenlet_dist_info/METADATA"),
    ),
    (
        "greenlet-3.2.0.dist-info/INSTALLER",
        include_str!("python/greenlet_dist_info/INSTALLER"),
    ),
    (
        "greenlet-3.2.0.dist-info/top_level.txt",
        include_str!("python/greenlet_dist_info/top_level.txt"),
    ),
    (
        "greenlet-3.2.0.dist-info/RECORD",
        include_str!("python/greenlet_dist_info/RECORD"),
    ),
];

/// Layout version of the materialized tree itself (directory shape,
/// aliases, markers — anything `materialize` writes that is not an
/// embedded source). Bump when the shape changes so existing caches
/// keyed on unchanged sources are not mistaken for the new layout.
pub(crate) const TREE_FORMAT: &str = "tree-format-4";

/// Preserve the cache key's byte order, including package flags and data files.
pub(crate) fn fingerprint(version: &str, sources: &[FrozenSource]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut eat = |bytes: &[u8]| {
        for &byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    eat(version.as_bytes());
    eat(TREE_FORMAT.as_bytes());
    for source in sources {
        eat(source.name.as_bytes());
        eat(&[u8::from(source.is_package)]);
        eat(source.source.as_bytes());
    }
    for (path, contents) in DATA_FILES {
        eat(path.as_bytes());
        eat(contents.as_bytes());
    }
    hash
}
