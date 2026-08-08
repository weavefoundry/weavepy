//! JSON / markdown report formatting and ratio-based gating for the
//! bench runner (RFC 0058 WS1).

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
/// timing for each runtime, and the WeavePy/CPython median ratio
/// (slowdown; lower is better, 1.0 = parity).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    pub name: String,
    pub work: u32,
    pub weavepy: RunSet,
    #[serde(default)]
    pub cpython: Option<RunSet>,
    /// Optional `WEAVEPY_JIT=1` column (reported, never gated).
    #[serde(default)]
    pub jit: Option<RunSet>,
    /// `weavepy.median_ns / cpython.median_ns`.
    #[serde(default)]
    pub ratio: Option<f64>,
}

impl Row {
    pub fn new(
        name: String,
        work: u32,
        weavepy: RunSet,
        cpython: Option<RunSet>,
        jit: Option<RunSet>,
    ) -> Self {
        let ratio = cpython
            .as_ref()
            .filter(|c| c.median_ns > 0.0 && weavepy.median_ns > 0.0)
            .map(|c| weavepy.median_ns / c.median_ns);
        Self {
            name,
            work,
            weavepy,
            cpython,
            jit,
            ratio,
        }
    }
}

/// Top-level report shape. Persisted as `baselines/bench.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub version: u32,
    pub host: String,
    pub created_at: String,
    /// Geometric mean of the per-fixture WeavePy/CPython ratios
    /// (fixtures without a CPython column are excluded).
    #[serde(default)]
    pub geomean_ratio: Option<f64>,
    pub rows: Vec<Row>,
}

impl Report {
    pub fn new(rows: Vec<Row>) -> Self {
        let ratios: Vec<f64> = rows.iter().filter_map(|r| r.ratio).collect();
        let geomean_ratio = if ratios.is_empty() {
            None
        } else {
            Some(stats::geometric_mean(&ratios))
        };
        Self {
            version: 2,
            host: hostname_or_unknown(),
            created_at: now_rfc3339(),
            geomean_ratio,
            rows,
        }
    }

    /// Render as a markdown table — what the CLI prints when run
    /// without `--json`.
    pub fn to_markdown(&self) -> String {
        use std::fmt::Write;
        let has_jit = self.rows.iter().any(|r| r.jit.is_some());
        let mut out = String::new();
        let _ = writeln!(
            out,
            "# WeavePy bench (host: `{}`, created: `{}`)",
            self.host, self.created_at
        );
        let _ = writeln!(out);
        if has_jit {
            let _ = writeln!(
                out,
                "| fixture | work | WeavePy | WeavePy+JIT | CPython | ×CPython (lower is better) |"
            );
            let _ = writeln!(out, "|---|---|---|---|---|---|");
        } else {
            let _ = writeln!(
                out,
                "| fixture | work | WeavePy | CPython | ×CPython (lower is better) |"
            );
            let _ = writeln!(out, "|---|---|---|---|---|");
        }
        for r in &self.rows {
            let wp = format_ns(r.weavepy.median_ns);
            let cp = match &r.cpython {
                Some(c) => format_ns(c.median_ns),
                None => "-".to_owned(),
            };
            let ratio = match r.ratio {
                Some(x) => format!("{x:.2}×"),
                None => "-".to_owned(),
            };
            if has_jit {
                let jit = match &r.jit {
                    Some(j) => format_ns(j.median_ns),
                    None => "-".to_owned(),
                };
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} | {} | {} |",
                    r.name, r.work, wp, jit, cp, ratio
                );
            } else {
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} | {} |",
                    r.name, r.work, wp, cp, ratio
                );
            }
        }
        if let Some(g) = self.geomean_ratio {
            let _ = writeln!(out);
            let _ = writeln!(out, "Geometric mean: **{g:.2}× CPython**");
        }
        out
    }

    /// Compare against a baseline [`Report`] and return one
    /// regression string per problem. Empty vec = gate passes.
    ///
    /// Per fixture the WeavePy/CPython **ratio** is compared when
    /// both reports carry one (host-independent); otherwise the
    /// absolute WeavePy median is the fallback (only meaningful on
    /// the host that produced the baseline, e.g. `--no-cpython`
    /// local runs). Fixtures with no baseline row fail the gate —
    /// new fixtures must be baselined in the same change. The suite
    /// geometric mean is gated with the same threshold.
    pub fn regressions(&self, baseline: &Report, pct_threshold: f64) -> Vec<String> {
        let mut out = Vec::new();
        let factor = 1.0 + pct_threshold / 100.0;
        for new in &self.rows {
            let Some(old) = baseline.rows.iter().find(|r| r.name == new.name) else {
                out.push(format!(
                    "{}: no baseline row — run `weavepy-bench run --update-baseline` and commit it",
                    new.name
                ));
                continue;
            };
            match (new.ratio, old.ratio) {
                (Some(nr), Some(or)) if or > 0.0 => {
                    if nr > or * factor {
                        out.push(format!(
                            "{}: ratio {:.2}× -> {:.2}× vs CPython ({:+.1}%)",
                            new.name,
                            or,
                            nr,
                            100.0 * (nr - or) / or,
                        ));
                    }
                }
                _ => {
                    if old.weavepy.median_ns > 0.0
                        && new.weavepy.median_ns > old.weavepy.median_ns * factor
                    {
                        out.push(format!(
                            "{}: median {} -> {} ({:+.1}%; absolute fallback — no ratio in baseline)",
                            new.name,
                            format_ns(old.weavepy.median_ns),
                            format_ns(new.weavepy.median_ns),
                            100.0 * (new.weavepy.median_ns - old.weavepy.median_ns)
                                / old.weavepy.median_ns,
                        ));
                    }
                }
            }
        }
        if let (Some(ng), Some(og)) = (self.geomean_ratio, baseline.geomean_ratio) {
            if og > 0.0 && ng > og * factor {
                out.push(format!(
                    "geomean: {:.2}× -> {:.2}× vs CPython ({:+.1}%)",
                    og,
                    ng,
                    100.0 * (ng - og) / og,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn runset(median: f64) -> RunSet {
        RunSet::from_samples_ns(&[median])
    }

    fn row(name: &str, weavepy_ns: f64, cpython_ns: Option<f64>) -> Row {
        Row::new(
            name.to_owned(),
            1,
            runset(weavepy_ns),
            cpython_ns.map(runset),
            None,
        )
    }

    #[test]
    fn ratio_and_geomean_computed() {
        let report = Report::new(vec![
            row("a", 200.0, Some(100.0)),
            row("b", 800.0, Some(100.0)),
        ]);
        assert_eq!(report.rows[0].ratio, Some(2.0));
        assert_eq!(report.rows[1].ratio, Some(8.0));
        let g = report.geomean_ratio.unwrap();
        assert!((g - 4.0).abs() < 1e-9, "geomean of 2 and 8 is 4, got {g}");
    }

    #[test]
    fn gate_flags_ratio_regression_not_host_speed() {
        // Same ratio on a 2x slower host: no regression.
        let baseline = Report::new(vec![row("a", 200.0, Some(100.0))]);
        let slower_host = Report::new(vec![row("a", 400.0, Some(200.0))]);
        assert!(slower_host.regressions(&baseline, 10.0).is_empty());

        // Ratio got 50% worse: regression.
        let worse = Report::new(vec![row("a", 300.0, Some(100.0))]);
        let regs = worse.regressions(&baseline, 10.0);
        assert_eq!(regs.len(), 2, "row + geomean should both fire: {regs:?}");
    }

    #[test]
    fn gate_fails_unbaselined_fixture() {
        let baseline = Report::new(vec![row("a", 200.0, Some(100.0))]);
        let with_new = Report::new(vec![
            row("a", 200.0, Some(100.0)),
            row("brand_new", 100.0, Some(100.0)),
        ]);
        let regs = with_new.regressions(&baseline, 10.0);
        assert_eq!(regs.len(), 1);
        assert!(regs[0].contains("brand_new"));
    }

    #[test]
    fn gate_absolute_fallback_without_ratios() {
        let baseline = Report::new(vec![row("a", 200.0, None)]);
        let ok = Report::new(vec![row("a", 210.0, None)]);
        assert!(ok.regressions(&baseline, 10.0).is_empty());
        let bad = Report::new(vec![row("a", 300.0, None)]);
        assert_eq!(bad.regressions(&baseline, 10.0).len(), 1);
    }
}
