"""Compare the existing thread-scaling fixture with the installed CPython."""

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
    parser.add_argument("--weavepy", required=True)
    parser.add_argument("--python", default="python3.14")
    parser.add_argument("--samples", type=int, default=5)
    parser.add_argument("--work", type=int, default=3000000)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if args.samples < 1 or args.work < 1:
        parser.error("--samples and --work must be positive")
    variants = [("weavepy_gil", args.weavepy, 1),
                ("weavepy_free", args.weavepy, 0),
                ("cpython_gil", args.python, 1)]
    samples = {label: [] for label, _, _ in variants}
    for cycle in range(args.samples + 1):
        for label, binary, gil in variants if cycle % 2 == 0 else reversed(variants):
            value = measure(binary, gil, args.work)
            if cycle:
                samples[label].append(value)
        print("Completed cycle", cycle, flush=True)
    comparisons = {
        label: {
            metric: statistics.median(new[metric] / old[metric] for new, old in
                                     zip(values, samples["cpython_gil"], strict=True))
            for metric in values[0]
        }
        for label, values in samples.items() if label != "cpython_gil"
    }
    report = {
        "work": args.work,
        "weavepy_sha256": hashlib.sha256(Path(args.weavepy).read_bytes()).hexdigest(),
        "cpython": args.python,
        "notes": "Both WeavePy modes request WEAVEPY_JIT=1; runtime JIT gating still applies. CPython uses the installed GIL build; no free-threaded CPython build is available. Scaling is serial/parallel time, so higher scaling is better. Other metrics are better when lower.",
        "samples": samples, "comparisons": comparisons,
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2) + "\n")
    print(comparisons, flush=True)


if __name__ == "__main__":
    main()
