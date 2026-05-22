//! Per-file runner: grade one corpus entry across every pipeline phase.
//!
//! Each phase has a tiny state machine: try to run WeavePy, try to run the
//! oracle, compare. The outcome is one of [`PhaseOutcome`]. We deliberately
//! never panic on bad inputs — an exception from the oracle or an error
//! from WeavePy is itself a meaningful classification.
//!
//! Phases that compare normalised forms:
//!
//! * **Tokens** — `(kind, lexeme)` pairs; `INDENT`/`DEDENT` columns and
//!   line/col attributes stripped.
//! * **AST** — `ast.dump` flattened to whitespace-free shape; line/col
//!   and `type_ignores`/`type_comment` fields stripped.
//! * **`dis`** — opcode names + arg numbers, one per line.
//!
//! None of these comparisons are byte-exact against CPython today; the
//! point of the harness is to track *normalised divergence* over time
//! and produce diffable JSON reports the CI can plot.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::corpus::CorpusFile;
use crate::{normalize, oracle};

/// Outcome of comparing one phase on one corpus file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PhaseOutcome {
    /// WeavePy and CPython produced equivalent canonical outputs.
    Match,
    /// Both sides succeeded but the canonical outputs differ.
    Mismatch {
        /// Short, human-readable summary of the disagreement.
        detail: String,
    },
    /// WeavePy returned an error (lex/parse/compile failure).
    WeavepyError { message: String },
    /// The CPython oracle returned an error (e.g. `SyntaxError` on input).
    OracleError { message: String },
    /// This phase is not yet wired up for diffing. Records that the
    /// oracle ran (so we know infrastructure works) but the WeavePy side
    /// has no comparable output yet.
    Skipped { reason: String },
}

impl PhaseOutcome {
    /// Convenience: collapse to the short label used in summary tables.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::Mismatch { .. } => "mismatch",
            Self::WeavepyError { .. } => "weavepy-error",
            Self::OracleError { .. } => "oracle-error",
            Self::Skipped { .. } => "skipped",
        }
    }
}

/// Combined per-file outcome across all phases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileReport {
    /// Stable corpus label, e.g. `"in-tree/lex_arith.py"`.
    pub label: String,
    /// Lexer phase outcome.
    pub tokens: PhaseOutcome,
    /// Parser phase outcome.
    pub ast: PhaseOutcome,
    /// Compiler phase outcome.
    pub dis: PhaseOutcome,
}

/// Top-level summary across many files.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Summary {
    pub total: usize,
    pub match_: usize,
    pub mismatch: usize,
    pub weavepy_error: usize,
    pub oracle_error: usize,
    pub skipped: usize,
}

impl Summary {
    /// Aggregate the outcomes for one phase across many files.
    pub fn from_phase<'a, I>(outcomes: I) -> Self
    where
        I: IntoIterator<Item = &'a PhaseOutcome>,
    {
        let mut s = Self::default();
        for o in outcomes {
            s.total += 1;
            match o {
                PhaseOutcome::Match => s.match_ += 1,
                PhaseOutcome::Mismatch { .. } => s.mismatch += 1,
                PhaseOutcome::WeavepyError { .. } => s.weavepy_error += 1,
                PhaseOutcome::OracleError { .. } => s.oracle_error += 1,
                PhaseOutcome::Skipped { .. } => s.skipped += 1,
            }
        }
        s
    }

    /// Match rate as a `0.0..=1.0` fraction. Files that the oracle itself
    /// couldn't process are excluded from the denominator so a broken
    /// fixture can't artificially deflate the score.
    ///
    /// The internal `usize` counters are converted to `f64` for the
    /// division; corpus sizes are small enough that the f64 mantissa
    /// limit (2^53) is not a real concern here.
    #[allow(clippy::cast_precision_loss)]
    pub fn match_rate(&self) -> f64 {
        let denom = self.total.saturating_sub(self.oracle_error);
        if denom == 0 {
            0.0
        } else {
            self.match_ as f64 / denom as f64
        }
    }
}

/// Run every phase on one file.
pub fn run_file(python: &str, file: &CorpusFile) -> FileReport {
    let source = match fs::read_to_string(&file.path) {
        Ok(s) => s,
        Err(e) => {
            let msg = format!("failed to read {}: {e}", file.path.display());
            return FileReport {
                label: file.label.clone(),
                tokens: PhaseOutcome::WeavepyError {
                    message: msg.clone(),
                },
                ast: PhaseOutcome::WeavepyError {
                    message: msg.clone(),
                },
                dis: PhaseOutcome::WeavepyError { message: msg },
            };
        }
    };

    FileReport {
        label: file.label.clone(),
        tokens: diff_tokens(python, &file.path, &source),
        ast: diff_ast(python, &file.path, &source),
        dis: diff_dis(python, &file.path, &source),
    }
}

fn diff_tokens(python: &str, path: &Path, source: &str) -> PhaseOutcome {
    let oracle_toks = match oracle::tokens(python, path) {
        Ok(t) => t,
        Err(e) => {
            return PhaseOutcome::OracleError {
                message: short_err(&e),
            };
        }
    };
    let weavepy_toks = match weavepy::lexer::tokenize(source) {
        Ok(t) => t,
        Err(e) => {
            return PhaseOutcome::WeavepyError {
                message: e.to_string(),
            };
        }
    };
    let oracle_norm = normalize::normalize_oracle_tokens(&oracle_toks);
    let weavepy_norm = normalize::normalize_weavepy_tokens(source, &weavepy_toks);
    if oracle_norm == weavepy_norm {
        PhaseOutcome::Match
    } else {
        // Find the first diverging index for a more useful detail message.
        let first_diff = oracle_norm
            .iter()
            .zip(weavepy_norm.iter())
            .position(|(a, b)| a != b);
        let detail = match first_diff {
            Some(idx) => format!(
                "first diff at token {}: oracle {:?} vs weavepy {:?} (len {} vs {})",
                idx,
                oracle_norm.get(idx),
                weavepy_norm.get(idx),
                oracle_norm.len(),
                weavepy_norm.len(),
            ),
            None => format!(
                "{} oracle tokens vs {} weavepy tokens",
                oracle_norm.len(),
                weavepy_norm.len()
            ),
        };
        PhaseOutcome::Mismatch { detail }
    }
}

fn diff_ast(python: &str, path: &Path, source: &str) -> PhaseOutcome {
    let oracle_dump = match oracle::ast_dump(python, path) {
        Ok(t) => t,
        Err(e) => {
            return PhaseOutcome::OracleError {
                message: short_err(&e),
            };
        }
    };
    let module = match weavepy::parser::parse_module(source) {
        Ok(m) => m,
        Err(e) => {
            return PhaseOutcome::WeavepyError {
                message: e.to_string(),
            };
        }
    };
    let weavepy_dump = weavepy::parser::ast::dump_module(&module);
    let oracle_canon = normalize::canonical_ast(&oracle_dump);
    let weavepy_canon = normalize::canonical_ast(&weavepy_dump);
    if oracle_canon == weavepy_canon {
        PhaseOutcome::Match
    } else {
        PhaseOutcome::Mismatch {
            detail: format!(
                "{} oracle chars vs {} weavepy chars (canonical)",
                oracle_canon.len(),
                weavepy_canon.len()
            ),
        }
    }
}

fn diff_dis(python: &str, path: &Path, source: &str) -> PhaseOutcome {
    let oracle_dis = match oracle::dis(python, path) {
        Ok(t) => t,
        Err(e) => {
            return PhaseOutcome::OracleError {
                message: short_err(&e),
            };
        }
    };
    let module = match weavepy::parser::parse_module(source) {
        Ok(m) => m,
        Err(e) => {
            return PhaseOutcome::WeavepyError {
                message: e.to_string(),
            };
        }
    };
    let code = match weavepy::compiler::compile_module_with_filename(&module, "<conformance>") {
        Ok(c) => c,
        Err(e) => {
            return PhaseOutcome::WeavepyError {
                message: e.to_string(),
            };
        }
    };
    let weavepy_dis = code.format_dis();
    let oracle_canon = normalize::canonical_dis(&oracle_dis);
    let weavepy_canon = normalize::canonical_dis(&weavepy_dis);
    if oracle_canon == weavepy_canon {
        PhaseOutcome::Match
    } else {
        PhaseOutcome::Mismatch {
            detail: format!(
                "{} oracle lines vs {} weavepy lines (canonical)",
                oracle_canon.lines().count(),
                weavepy_canon.lines().count(),
            ),
        }
    }
}

/// Truncate multi-line oracle errors down to the first line for the report.
fn short_err(err: &anyhow::Error) -> String {
    err.to_string().lines().next().unwrap_or("").to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_match_rate_excludes_oracle_errors() {
        let s = Summary {
            total: 10,
            match_: 3,
            mismatch: 5,
            weavepy_error: 0,
            oracle_error: 2,
            skipped: 0,
        };
        // 3 matches out of (10 - 2) = 8 gradable files.
        assert!((s.match_rate() - 0.375).abs() < 1e-9);
    }

    #[test]
    fn summary_match_rate_handles_zero_denominator() {
        let s = Summary {
            total: 0,
            ..Summary::default()
        };
        assert!(s.match_rate().abs() < f64::EPSILON);
    }
}
