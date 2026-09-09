"""Repeat paired scaling measurements without alternating the GIL modes."""

import argparse
import hashlib
import json
from pathlib import Path
import runpy
import statistics

ROOT = Path(__file__).resolve().parents[4]
measure = runpy.run_path(str(
    ROOT / "crates/weavepy-bench/census/2026-09-execution/scaling.py"
))["measure"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True)
    parser.add_argument("--new", required=True)
    parser.add_argument("--samples", type=int, default=7)
    parser.add_argument("--work", type=int, default=3000000)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if args.samples < 1 or args.work < 1:
        parser.error("--samples and --work must be positive")
    variants = [("base", args.base), ("new", args.new)]
    report = {
        "work": args.work,
        "notes": "Modes run separately; each mode discards one process pair and reverses binary order on alternate cycles. This checks the order effect seen after heavy free-threaded processes in scaling.json. Higher scaling is better; lower time and memory are better.",
        "binaries": {label: {"path": binary, "sha256": hashlib.sha256(
            Path(binary).read_bytes()).hexdigest()} for label, binary in variants},
        "samples": {}, "comparisons": {},
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    for gil in (1, 0):
        samples = {label: [] for label, _ in variants}
        for cycle in range(args.samples + 1):
            for label, binary in variants if cycle % 2 == 0 else reversed(variants):
                value = measure(binary, gil, args.work)
                if cycle:
                    samples[label].append(value)
            print("GIL", gil, "completed cycle", cycle, flush=True)
        comparisons = {
            metric: statistics.median(new[metric] / old[metric] for new, old in
                                     zip(samples["new"], samples["base"], strict=True))
            for metric in samples["new"][0]
        }
        report["samples"][str(gil)] = samples
        report["comparisons"][str(gil)] = comparisons
        args.out.write_text(json.dumps(report, indent=2) + "\n")
        print("GIL", gil, comparisons, flush=True)


if __name__ == "__main__":
    main()
