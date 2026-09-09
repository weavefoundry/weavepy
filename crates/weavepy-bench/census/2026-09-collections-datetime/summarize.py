"""Regenerate report tables from complete paired measurement files."""

import json
from pathlib import Path
import statistics

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]


def main():
    suite = json.loads((HERE / "suite.json").read_text())
    probes = json.loads((HERE / "probes.json").read_text())
    rows = suite["rows"]
    assert len(rows) == 24, "The standard suite must finish before summarizing"
    out = ["## Standard suite\n\n",
           "Values are candidate/reference ratios; smaller values mean less time "
           "or memory. Geometric means give each fixture equal weight. The "
           "workload aggregate includes all 23 timed workloads; the process "
           "aggregates include all 24 fixtures, including startup.\n\n",
           "| Metric | New/base JIT | New/base interpreter | New/CPython JIT | New/CPython interpreter |\n",
           "| --- | ---: | ---: | ---: | ---: |\n"]
    for metric, label in [("ns", "Timed workload"), ("wall_ns", "Process elapsed time"),
                          ("cpu_ns", "Process CPU time"), ("rss_bytes", "Peak RSS")]:
        selected = [r for name, r in rows.items() if metric != "ns" or name != "startup"]
        values = [statistics.geometric_mean(r[group][mode][metric] for r in selected)
                  for group in ("comparisons", "cpython_comparisons")
                  for mode in ("jit", "interp")]
        out.append("| " + label + " | " + " | ".join(f"{v:.3f}" for v in values) + " |\n")
    out.extend(["\n| Fixture | New/base JIT time | New/base interpreter time | New/CPython JIT time | New/CPython JIT RSS |\n",
                "| --- | ---: | ---: | ---: | ---: |\n"])
    for name, row in rows.items():
        values = [row["comparisons"]["jit"]["ns"], row["comparisons"]["interp"]["ns"],
                  row["cpython_comparisons"]["jit"]["ns"], row["cpython_comparisons"]["jit"]["rss_bytes"]]
        out.append(f"| `{name}` | " + " | ".join(f"{v:.3f}" for v in values) + " |\n")
    out.extend(["\n## Supplemental probes\n\n",
                "Elapsed times are workload medians in milliseconds. CPU and RSS "
                "ratios are medians of paired candidate/reference samples. Workload "
                "CPU excludes setup; peak RSS covers the entire process.\n\n",
                "| Probe | Baseline ms | New ms | CPython ms | New/base workload CPU | New/base RSS | New/CPython RSS |\n",
                "| --- | ---: | ---: | ---: | ---: | ---: | ---: |\n"])
    for name, row in probes["rows"].items():
        elapsed = [statistics.median(v["ns"] for v in row["samples"][label]) / 1e6
                   for label in ("base", "new", "cpython")]
        ratios = [row["comparisons"]["base"]["work_cpu_ns"],
                  row["comparisons"]["base"]["rss_bytes"],
                  row["comparisons"]["cpython"]["rss_bytes"]]
        out.append(f"| `{name}` | " + " | ".join(f"{v:.4f}" for v in elapsed) + " | " +
                   " | ".join(f"{v:.6f}" if v < 0.01 else f"{v:.3f}" for v in ratios) + " |\n")
    out.append("\nThe large rotation probes use 100,000 elements and 200 alternating "
               "one-position rotations. `deque_rotate_large` starts after 99 left pops; "
               "`deque_rotate_large_no_prefix` includes the initial prefix allocation. "
               "These probe-specific speedups should not be generalized to all rotations.\n\n")
    doc = ROOT / "docs/PERFORMANCE-COLLECTIONS-DATETIME.md"
    start, end = "<!-- measurements:start -->", "<!-- measurements:end -->"
    text = doc.read_text()
    block = start + "\n" + "".join(out) + end + "\n\n"
    if start in text:
        a, b = text.index(start), text.index(end) + len(end)
        text = text[:a] + block.rstrip("\n") + text[b:]
    else:
        text = text.replace("## Changes", block + "## Changes", 1)
    doc.write_text(text)


if __name__ == "__main__":
    main()
