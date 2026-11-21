#!/usr/bin/env python3
"""After a failed bench gate: where a regressed fixture's time went.

    tools/bench_gate_counts.py --base BASE --new NEW --report bench-report.md
    tools/bench_gate_counts.py --base BASE --new NEW --fixtures attr_access:200000

For each fixture the gate's report lists under REGRESSIONS (or each
`name:work` of --fixtures), runs both binaries under several tier
settings, interleaved, and prints the least of N runs: the fixture's own
kernel time and, where `/usr/bin/time -l` reports them (macOS), the
process's instructions retired and cycles elapsed.

Reading it: equal instruction counts with unequal times point at code
layout or the machine, not at a code path; unequal counts under one
setting name the tier whose work changed (synchronous compiles take the
background compiler's timing out, `WEAVEPY_TIER2=0` and
`WEAVEPY_FRAME_JIT=0` take a tier out).
"""

import argparse
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FIXTURES = os.path.join(ROOT, "crates", "weavepy-bench", "fixtures")

SETTINGS = [
    ("default", {}),
    ("synchronous compiles", {"WEAVEPY_FRAME_JIT_SYNC": "1"}),
    ("tier 2 off", {"WEAVEPY_TIER2": "0"}),
    ("frame JIT off", {"WEAVEPY_FRAME_JIT": "0"}),
    ("both off", {"WEAVEPY_TIER2": "0", "WEAVEPY_FRAME_JIT": "0"}),
]


def regressed(report):
    """`(name, work)` of each fixture under the report's REGRESSIONS."""
    text = open(report, encoding="utf-8", errors="replace").read()
    works = dict(re.findall(r"^\| (\w+) \| (\d+) \|", text, re.M))
    tail = text.split("REGRESSIONS:", 1)
    if len(tail) < 2:
        return []
    names = re.findall(r"^\s+(\w+): [\d.]+× vs merge-base", tail[1], re.M)
    return [(n, works[n]) for n in names if n in works]


def run(binary, fixture, work, extra, counters):
    env = dict(os.environ, WEAVEPY_BENCH_WORK=work, PYTHONHASHSEED="0", **extra)
    env.pop("PYTHONDONTWRITEBYTECODE", None)
    cmd = [binary, os.path.join(FIXTURES, fixture + ".py")]
    if counters:
        cmd = ["/usr/bin/time", "-l"] + cmd
    r = subprocess.run(cmd, env=env, capture_output=True, text=True, errors="replace")
    ns = re.search(r"WEAVEPY_BENCH_NS=(\d+)", r.stdout)
    ins = re.search(r"(\d+)\s+instructions retired", r.stderr)
    cyc = re.search(r"(\d+)\s+cycles elapsed", r.stderr)
    return tuple(int(m.group(1)) if m else None for m in (ns, ins, cyc))


def least(a, b):
    return tuple(y if x is None else x if y is None else min(x, y) for x, y in zip(a, b))


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--base", required=True, help="the merge-base binary")
    ap.add_argument("--new", required=True, help="the PR's binary")
    ap.add_argument("--report", help="the gate's report (bench-report.md)")
    ap.add_argument("--fixtures", default="", help="name:work,... instead of a report")
    ap.add_argument("--samples", type=int, default=5)
    args = ap.parse_args()

    todo = [tuple(f.split(":", 1)) for f in args.fixtures.split(",") if f]
    if args.report:
        todo += regressed(args.report)
    if not todo:
        print("no regressed fixtures to measure")
        return 0
    counters = sys.platform == "darwin" and os.access("/usr/bin/time", os.X_OK)

    def cell(new, base, scale, unit):
        if new is None or base is None or base == 0:
            return "n/a"
        return "%.1f%s vs %.1f%s (%.3f×)" % (new / scale, unit, base / scale, unit, new / base)

    print("| fixture | setting | kernel time (PR vs base) | instructions | cycles |")
    print("|---|---|---|---|---|")
    for name, work in todo:
        for label, extra in SETTINGS:
            best = {args.base: (None, None, None), args.new: (None, None, None)}
            for _ in range(args.samples):
                for binary in (args.new, args.base, args.base, args.new):
                    best[binary] = least(best[binary], run(binary, name, work, extra, counters))
            new, base = best[args.new], best[args.base]
            print(
                "| %s | %s | %s | %s | %s |"
                % (
                    name,
                    label,
                    cell(new[0], base[0], 1e6, "ms"),
                    cell(new[1], base[1], 1e6, "M"),
                    cell(new[2], base[2], 1e6, "M"),
                ),
                flush=True,
            )
    return 0


if __name__ == "__main__":
    sys.exit(main())
