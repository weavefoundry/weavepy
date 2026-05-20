//! Per-file runner: grade one corpus entry across every pipeline phase.
//!
//! Each phase has a tiny state machine: try to run WeavePy, try to run the
//! oracle, compare. The outcome is one of [`PhaseOutcome`]. We deliberately
//! never panic on bad inputs — an exception from the oracle or an error
//! from WeavePy is itself a meaningful classification.
//!
//! Today only the lexer phase produces a real diff. The parser and
//! compiler phases call the oracle (so we know the oracle works end-to-end)
//! but report the WeavePy side as [`PhaseOutcome::Skipped`] until WeavePy
//! can emit comparable output. This keeps the harness honest: we never
//! claim to be measuring something we're not.

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
        ast: diff_ast(python, &file.path),
        dis: diff_dis(python, &file.path),
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
    let oracle_norm: Vec<_> = oracle_toks.iter().map(normalize::from_oracle).collect();
    let weavepy_norm: Vec<_> = weavepy_toks
        .iter()
        .map(|t| normalize::from_weavepy(source, t))
        .collect();
    if oracle_norm == weavepy_norm {
        PhaseOutcome::Match
    } else {
        PhaseOutcome::Mismatch {
            detail: format!(
                "{} oracle tokens vs {} weavepy tokens",
                oracle_norm.len(),
                weavepy_norm.len()
            ),
        }
    }
}

fn diff_ast(python: &str, path: &Path) -> PhaseOutcome {
    // We still run the oracle so we exercise the subprocess path and
    // surface oracle errors (e.g. SyntaxError in a corpus file) early.
    // The WeavePy side is intentionally skipped until the parser can emit
    // an `ast.dump`-shaped representation.
    if let Err(e) = oracle::ast_dump(python, path) {
        return PhaseOutcome::OracleError {
            message: short_err(&e),
        };
    }
    PhaseOutcome::Skipped {
        reason: "weavepy parser does not yet emit ast.dump-compatible output".to_owned(),
    }
}

fn diff_dis(python: &str, path: &Path) -> PhaseOutcome {
    if let Err(e) = oracle::dis(python, path) {
        return PhaseOutcome::OracleError {
            message: short_err(&e),
        };
    }
    PhaseOutcome::Skipped {
        reason: "weavepy compiler does not yet emit dis-compatible output".to_owned(),
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
