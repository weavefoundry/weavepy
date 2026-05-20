//! Render conformance results as Markdown (for humans) and JSON (for CI).
//!
//! The Markdown view is what shows up in PR comments and locally on stdout.
//! The JSON view is machine-readable and is the artifact future tooling
//! diffs against to detect regressions.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::runner::{FileReport, PhaseOutcome, Summary};

/// Full report: every per-file outcome plus a per-phase summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    /// CPython version this report grades against.
    pub cpython_target: String,
    /// Banner from `python -V` for the oracle that produced this report.
    pub oracle_banner: String,
    /// One entry per corpus file.
    pub files: Vec<FileReport>,
    /// Aggregate scoreboard.
    pub summary: ReportSummary,
}

/// Aggregate scoreboard across all files, one row per phase.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReportSummary {
    pub tokens: Summary,
    pub ast: Summary,
    pub dis: Summary,
}

impl Report {
    /// Build the summary from the per-file outcomes.
    pub fn new(cpython_target: String, oracle_banner: String, files: Vec<FileReport>) -> Self {
        let summary = ReportSummary {
            tokens: Summary::from_phase(files.iter().map(|f| &f.tokens)),
            ast: Summary::from_phase(files.iter().map(|f| &f.ast)),
            dis: Summary::from_phase(files.iter().map(|f| &f.dis)),
        };
        Self {
            cpython_target,
            oracle_banner,
            files,
            summary,
        }
    }

    /// Write the report to `out_dir` as both `report.md` and `report.json`.
    pub fn write_to(&self, out_dir: &Path) -> io::Result<()> {
        fs::create_dir_all(out_dir)?;
        let md_path = out_dir.join("report.md");
        let json_path = out_dir.join("report.json");
        fs::write(&md_path, self.to_markdown())?;
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        fs::write(&json_path, json)?;
        Ok(())
    }

    /// Render the report as Markdown for humans.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "# WeavePy ↔ CPython {} conformance",
            self.cpython_target
        );
        let _ = writeln!(out, "\nOracle: `{}`\n", self.oracle_banner);

        let _ = writeln!(out, "## Summary\n");
        let _ = writeln!(
            out,
            "| phase  | match | mismatch | weavepy-error | oracle-error | skipped | rate |"
        );
        let _ = writeln!(
            out,
            "|--------|------:|---------:|--------------:|-------------:|--------:|-----:|"
        );
        write_summary_row(&mut out, "tokens", &self.summary.tokens);
        write_summary_row(&mut out, "ast", &self.summary.ast);
        write_summary_row(&mut out, "dis", &self.summary.dis);

        if !self.files.is_empty() {
            let _ = writeln!(out, "\n## Per-file outcomes\n");
            let _ = writeln!(out, "| file | tokens | ast | dis |");
            let _ = writeln!(out, "|------|--------|-----|-----|");
            for f in &self.files {
                let _ = writeln!(
                    out,
                    "| `{}` | {} | {} | {} |",
                    f.label,
                    cell(&f.tokens),
                    cell(&f.ast),
                    cell(&f.dis),
                );
            }
        }

        out
    }
}

fn write_summary_row(out: &mut String, name: &str, s: &Summary) {
    let _ = writeln!(
        out,
        "| {name:<6} | {match_:>5} | {mismatch:>8} | {we:>13} | {oe:>12} | {sk:>7} | {rate:>5.1}% |",
        match_ = s.match_,
        mismatch = s.mismatch,
        we = s.weavepy_error,
        oe = s.oracle_error,
        sk = s.skipped,
        rate = s.match_rate() * 100.0,
    );
}

fn cell(o: &PhaseOutcome) -> String {
    match o {
        PhaseOutcome::Match => "✓ match".to_owned(),
        PhaseOutcome::Mismatch { detail } => format!("✗ mismatch — {detail}"),
        PhaseOutcome::WeavepyError { message } => format!("⚠ weavepy: {}", truncate(message, 60)),
        PhaseOutcome::OracleError { message } => format!("⚠ oracle: {}", truncate(message, 60)),
        PhaseOutcome::Skipped { reason: _ } => "· skipped".to_owned(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    let one_line = s.lines().next().unwrap_or("");
    if one_line.chars().count() <= max {
        one_line.to_owned()
    } else {
        let mut t: String = one_line.chars().take(max).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_report_renders_without_panic() {
        let r = Report::new("3.13".to_owned(), "Python 3.13.1".to_owned(), Vec::new());
        let md = r.to_markdown();
        assert!(md.contains("WeavePy"));
        assert!(md.contains("Summary"));
        // No "Per-file outcomes" section when there are no files.
        assert!(!md.contains("Per-file outcomes"));
    }

    #[test]
    fn truncate_keeps_short_strings() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_shortens_long_strings() {
        let s = "a".repeat(200);
        let t = truncate(&s, 10);
        assert!(t.chars().count() <= 11); // 10 + ellipsis
        assert!(t.ends_with('…'));
    }
}
