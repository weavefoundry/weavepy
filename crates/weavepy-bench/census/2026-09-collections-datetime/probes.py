"""Measure datetime and deque throughput and peak memory with paired, interleaved processes."""

import argparse
import hashlib
import json
import platform
import runpy
import statistics
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
helpers = runpy.run_path(str(ROOT / "crates/weavepy-bench/census/2026-09-execution/probes.py"))
measure = helpers["measure"]
TIMER = helpers["TIMER"]
verify_runtime = runpy.run_path(str(ROOT / "tools/bench_compare.py"))["verify_runtime"]

KERNELS = {
    "timedelta_integer": """
from datetime import timedelta
def bench():
    for i in range(20000):
        result = timedelta(days=3, seconds=i, microseconds=-17, minutes=2)
    assert result == timedelta(days=3, seconds=20118, microseconds=999983)
""",
    "timedelta_float": """
from datetime import timedelta
def bench():
    for i in range(10000):
        result = timedelta(days=0.5, seconds=i, microseconds=-1.5)
    assert result == timedelta(seconds=53198, microseconds=999998)
""",
    "timedelta_bigint": """
from datetime import timedelta
huge = 10**100
def bench():
    for i in range(2000):
        result = timedelta(days=huge, seconds=-huge*86400, microseconds=i)
    assert result == timedelta(microseconds=1999)
""",
    "calendar_roundtrip": """
from datetime import date
def bench():
    for i in range(1, 20001):
        result = date.fromordinal(i * 100)
        assert result.toordinal() == i * 100
""",
    "deque_iteration": """
from collections import deque
data = deque(range(10000))
def bench():
    for i in range(10):
        assert sum(data) == 49995000
""",
    "deque_indexing": """
from collections import deque
data = deque(range(10000))
for _ in range(50): data.popleft()
def bench():
    for i in range(20000):
        assert data[0] == 50 and data[-1] == 9999
        assert len(data) == 9950 and data
""",
    "deque_rotate_small": """
from collections import deque
data = deque(range(64), maxlen=64)
def bench():
    for i in range(20000):
        data.rotate(3)
        data.rotate(-3)
    assert data[0] == 0 and data[-1] == 63
""",
    "deque_rotate_large": """
from collections import deque
data = deque(range(100000))
for _ in range(99): data.popleft()
def bench():
    for i in range(100):
        data.rotate(1)
        data.rotate(-1)
    assert data[0] == 99 and data[-1] == 99999
""",
    "deque_rotate_large_no_prefix": """
from collections import deque
data = deque(range(100000))
def bench():
    for i in range(100):
        data.rotate(1)
        data.rotate(-1)
    assert data[0] == 0 and data[-1] == 99999
""",
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True)
    parser.add_argument("--new", required=True)
    parser.add_argument("--python", default="python3.14")
    parser.add_argument("--samples", type=int, default=7)
    parser.add_argument("--only", nargs="+", choices=KERNELS)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    verify_runtime(args.base)
    verify_runtime(args.new)
    if args.samples < 1:
        parser.error("--samples must be positive")
    variants = [("base", args.base), ("new", args.new), ("cpython", args.python)]
    report = {
        "platform": platform.platform(), "jit": False, "samples": args.samples,
        "binaries": {
            name: {"path": binary, "bytes": Path(binary).stat().st_size,
                   "sha256": hashlib.sha256(Path(binary).read_bytes()).hexdigest()}
            for name, binary in variants[:2]
        },
        "rows": {},
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    for name, source in KERNELS.items():
        if args.only and name not in args.only:
            continue
        samples = {label: [] for label, _ in variants}
        errors = {}
        for cycle in range(args.samples + 1):
            for label, binary in variants if cycle % 2 == 0 else reversed(variants):
                if label in errors:
                    continue
                try:
                    value = measure(binary, [], source + TIMER, True)
                except RuntimeError as error:
                    # A broken baseline isn't a valid speed reference. Keep
                    # its failure visible; candidate and CPython failures
                    # still stop the run. Never weaken workload assertions.
                    if label != "base":
                        raise
                    errors[label] = str(error)
                    samples[label].clear()
                    continue
                if cycle:
                    samples[label].append(value)
        comparisons = {
            label: {
                metric: statistics.median(new[metric] / base[metric] for new, base in
                                         zip(samples["new"], samples[label], strict=True))
                for metric in samples["new"][0]
            }
            for label in ("base", "cpython") if label not in errors
        }
        report["rows"][name] = {"samples": samples, "comparisons": comparisons,
                                "errors": errors}
        args.out.write_text(json.dumps(report, indent=2) + "\n")
        if errors:
            print(name, "baseline failed; see errors in the JSON report", flush=True)
        print(name, {label: {key: round(value, 3) for key, value in values.items()}
                     for label, values in comparisons.items()}, flush=True)


if __name__ == "__main__":
    main()
