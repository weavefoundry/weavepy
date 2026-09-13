"""Measure object metadata workloads with paired, interleaved processes."""

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
    "plain_instances": """
class Item:
    def __init__(self, value):
        self.value = value

def bench():
    items = [Item(i) for i in range(100000)]
    assert sum(item.value for item in items) == 4999950000
""",
    "slotted_instances": """
class Item:
    __slots__ = ("value",)
    def __init__(self, value):
        self.value = value

def bench():
    items = [Item(i) for i in range(100000)]
    assert sum(item.value for item in items) == 4999950000
""",
    "memoryviews": """
data = b"abcdefgh"
def bench():
    views = [memoryview(data) for _ in range(100000)]
    assert sum(view.nbytes for view in views) == 800000
    for view in views:
        view.release()
""",
    "memoryview_access": """
view = memoryview(b"abcdefgh")
def bench():
    total = 0
    for _ in range(100000):
        total += view.nbytes + view.itemsize + view[3]
    assert total == 10900000
""",
    "materialized_frames": """
import sys
def capture():
    return sys._getframe()
def bench():
    frames = [capture() for _ in range(20000)]
    assert all(frame.f_code.co_name == "capture" for frame in frames)
    for frame in frames:
        frame.clear()
""",
    "bytesio_streams": """
from io import BytesIO
def bench():
    streams = [BytesIO(b"abc") for _ in range(20000)]
    assert sum(len(stream.read()) for stream in streams) == 60000
    for stream in streams:
        stream.close()
""",
    "type_creation": """
def bench():
    classes = [type("Item", (), {"value": i}) for i in range(10000)]
    assert sum(cls.value for cls in classes) == 49995000
""",
    "float_repr": """
values = [(-1.0 if i % 2 else 1.0) * (i + 0.125) * 10.0 ** (i % 41 - 20)
          for i in range(10000)]
expected = [repr(value) for value in values]
def bench():
    for _ in range(10):
        result = [repr(value) for value in values]
    assert result == expected
""",
    "float_str": """
values = [(-1.0 if i % 2 else 1.0) * (i + 0.125) * 10.0 ** (i % 41 - 20)
          for i in range(10000)]
expected = [str(value) for value in values]
def bench():
    for _ in range(10):
        result = [str(value) for value in values]
    assert result == expected
""",
    "int_repr": """
values = [(i - 5000) * 1234567890123 for i in range(10000)]
expected = [repr(value) for value in values]
def bench():
    for _ in range(10):
        result = [repr(value) for value in values]
    assert result == expected
""",
    "complex_repr": """
values = [complex(i * 0.5, (-1.0 if i % 2 else 1.0) * (i + 0.125) * 10.0 ** (i % 41 - 20))
          for i in range(10000)]
expected = [repr(value) for value in values]
def bench():
    for _ in range(10):
        result = [repr(value) for value in values]
    assert result == expected
""",
    "json_float_array": """
import json
values = [(-1.0 if i % 2 else 1.0) * (i + 0.125) * 10.0 ** (i % 41 - 20)
          for i in range(10000)]
encoder = json.JSONEncoder()
expected = "".join(encoder.iterencode(values, _one_shot=False))
def bench():
    for _ in range(30):
        result = json.dumps(values)
    assert result == expected
""",
    "json_int_array": """
import json
values = [(i - 5000) * 1234567890123 for i in range(10000)]
encoder = json.JSONEncoder()
expected = "".join(encoder.iterencode(values, _one_shot=False))
def bench():
    for _ in range(50):
        result = json.dumps(values)
    assert result == expected
""",
    "json_repeated_keys": """
import json
document = json.dumps([{"identifier": i, "active": True, "label": "row"}
                       for i in range(10000)])
def bench():
    for _ in range(15):
        result = json.loads(document)
    assert len(result) == 10000
    assert result[123] == {"identifier": 123, "active": True, "label": "row"}
    assert next(iter(result[0])) is next(iter(result[-1]))
""",
    "json_unique_keys": """
import json
document = json.dumps({"identifier_%d" % i: i for i in range(10000)})
def bench():
    for _ in range(20):
        result = json.loads(document)
    assert len(result) == 10000 and result["identifier_9999"] == 9999
""",
}


def main(kernels=KERNELS):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True)
    parser.add_argument("--new", required=True)
    parser.add_argument("--python", default="python3.14")
    parser.add_argument("--samples", type=int, default=7)
    parser.add_argument("--only", nargs="+", choices=kernels)
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
    for name, source in kernels.items():
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
