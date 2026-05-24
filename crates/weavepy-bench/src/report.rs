//! JSON / markdown report formatting for the bench runner.

use serde::{Deserialize, Serialize};

use crate::stats;

/// One sample summary — captures the timing distribution for a
/// single (fixture × runtime) pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSet {
    pub samples: Vec<f64>,
    pub mean_ns: f64,
    pub median_ns: f64,
    pub p95_ns: f64,
    pub stddev_ns: f64,
}

impl RunSet {
    /// Build a [`RunSet`] from raw timing samples (in nanoseconds).
    pub fn from_samples_ns(samples: &[f64]) -> Self {
        Self {
            samples: samples.to_vec(),
            mean_ns: stats::mean(samples),
            median_ns: stats::median(samples),
            p95_ns: stats::percentile(samples, 95.0),
            stddev_ns: stats::stddev(samples),
        }
    }
}

/// One row of the bench report — fixture name, work parameter,
/// and timing for each runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    pub name: String,
    pub work: u32,
    pub weavepy: RunSet,
    pub cpython: Option<RunSet>,
}

/// Top-level report shape. Persisted as `baselines/bench.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub version: u32,
    pub host: String,
    pub created_at: String,
    pub rows: Vec<Row>,
}

impl Report {
    pub fn new(rows: Vec<Row>) -> Self {
        Self {
            version: 1,
            host: hostname_or_unknown(),
            created_at: now_rfc3339(),
            rows,
        }
    }

    /// Render as a markdown table — what the CLI prints when run
    /// without `--json`.
    pub fn to_markdown(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "# WeavePy bench (host: `{}`, created: `{}`)",
            self.host, self.created_at
        );
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "| fixture | work | WeavePy median | CPython median | speedup vs CPython |"
        );
        let _ = writeln!(
            out,
            "|---------|------|----------------|----------------|--------------------|"
        );
        for r in &self.rows {
            let wp = format_ns(r.weavepy.median_ns);
            let cp = match &r.cpython {
                Some(c) => format_ns(c.median_ns),
                None => "-".to_owned(),
            };
            let speedup = match &r.cpython {
                Some(c) if c.median_ns > 0.0 => format!("{:.2}×", c.median_ns / r.weavepy.median_ns),
                _ => "-".to_owned(),
            };
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} |",
                r.name, r.work, wp, cp, speedup
            );
        }
        out
    }

    /// Compare against an older [`Report`] and return one regression
    /// string per fixture whose WeavePy median got worse by more
    /// than `pct_threshold`%. Empty vec = clean.
    pub fn regressions(&self, baseline: &Report, pct_threshold: f64) -> Vec<String> {
        let mut out = Vec::new();
        for new in &self.rows {
            let Some(old) = baseline.rows.iter().find(|r| r.name == new.name) else {
                continue;
            };
            if old.weavepy.median_ns <= 0.0 {
                continue;
            }
            let delta_pct =
                100.0 * (new.weavepy.median_ns - old.weavepy.median_ns) / old.weavepy.median_ns;
            if delta_pct > pct_threshold {
                out.push(format!(
                    "{}: median {} -> {} ({:+.2}%)",
                    new.name,
                    format_ns(old.weavepy.median_ns),
                    format_ns(new.weavepy.median_ns),
                    delta_pct,
                ));
            }
        }
        out
    }
}

fn format_ns(ns: f64) -> String {
    if ns < 1_000.0 {
        format!("{ns:.0}ns")
    } else if ns < 1_000_000.0 {
        format!("{:.1}µs", ns / 1_000.0)
    } else if ns < 1_000_000_000.0 {
        format!("{:.1}ms", ns / 1_000_000.0)
    } else {
        format!("{:.2}s", ns / 1_000_000_000.0)
    }
}

fn hostname_or_unknown() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_owned())
}

fn now_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| format!("ts={}", d.as_secs()))
        .unwrap_or_else(|_| "ts=0".to_owned())
}
