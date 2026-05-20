//! CPython conformance harness for WeavePy.
//!
//! This crate grades each WeavePy pipeline phase (lexer, parser, compiler,
//! virtual machine) against CPython as the oracle. It runs a corpus of
//! Python source files through both implementations and reports, per phase,
//! whether the outputs agree.
//!
//! Today only the lexer is actively diffed; the parser and compiler diffs
//! are wired up to the oracle but mark the WeavePy side as
//! [`PhaseOutcome::Skipped`] until WeavePy can emit comparable output. The
//! `regrtest` mode (running individual `Lib/test/test_*.py` files end-to-end)
//! is a placeholder until the interpreter can execute anything.
//!
//! See `docs/CONFORMANCE.md` for the project-level overview, including how
//! to optionally check out CPython as a submodule to widen the corpus.

pub mod corpus;
pub mod normalize;
pub mod oracle;
pub mod report;
pub mod runner;

pub use runner::{run_file, FileReport, PhaseOutcome};

/// The CPython release WeavePy is graded against.
///
/// Bumping this should be a deliberate change with the delta reviewed; see
/// `docs/ARCHITECTURE.md` ("Compatibility strategy").
pub const CPYTHON_TARGET_VERSION: &str = "3.13";

/// Environment variable that overrides the python interpreter used as the
/// oracle (defaults to `python3`).
pub const PYTHON_ENV_VAR: &str = "WEAVEPY_PYTHON";

/// Default oracle interpreter name. Resolved on `PATH` at runtime.
pub const DEFAULT_PYTHON: &str = "python3";

/// Pick the oracle interpreter, honoring `$WEAVEPY_PYTHON`.
pub fn oracle_python() -> String {
    std::env::var(PYTHON_ENV_VAR).unwrap_or_else(|_| DEFAULT_PYTHON.to_owned())
}
