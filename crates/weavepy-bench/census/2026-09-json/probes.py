"""Measure JSON throughput and peak memory with paired, interleaved processes."""

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

KERNELS = {
    "prefix_checks": '''
text = "word é " * 500000
def bench():
    for _ in range(20):
        assert text.startswith("word")
        assert text.endswith("é ")
''',
    "rsplit_bounded": '''
text = "word \\u2003" * 200000
def bench():
    for _ in range(10):
        parts = text.rsplit(None, 1)
    assert len(parts) == 2 and parts[-1] == "word" and parts[0].startswith("word")
''',
    "bounded_prefix_checks": '''
text = "word é " * 500000
def bench():
    for _ in range(20):
        assert text.startswith("word", 0, 4)
        assert text.endswith("é", 0, 6)
        assert text.startswith("word", 7, 11)
''',
    "negative_suffix_checks": '''
text = "word é " * 500000
def bench():
    for _ in range(20):
        assert text.startswith("é", -2)
        assert text.endswith("é", -3, -1)
''',
    "encode_records": '''
import json
data = [{"id": i, "name": "record-%d" % i, "values": [i, i + 1, i * .5]} for i in range(2000)]
def bench():
    for _ in range(30):
        blob = json.dumps(data)
    assert blob.startswith('[{"id": 0,')
''',
    "decode_records": '''
import json
blob = json.dumps([{"id": i, "name": "record-%d" % i, "values": [i, i + 1, i * .5]} for i in range(2000)])
def bench():
    for _ in range(30):
        data = json.loads(blob)
    assert data[-1]["values"] == [1999, 2000, 999.5]
''',
    "encode_large_ascii": '''
import json
data = "abcdefgh " * 1000000
def bench():
    for _ in range(5):
        blob = json.dumps(data)
    assert len(blob) == len(data) + 2
''',
    "decode_large_ascii": '''
import json
blob = '"' + "abcdefgh " * 1000000 + '"'
def bench():
    for _ in range(5):
        data = json.loads(blob)
    assert len(data) == 9000000
''',
    "encode_unicode": '''
import json
data = "café Σ 😀; " * 100000
def bench():
    for _ in range(10):
        escaped = json.dumps(data)
        raw = json.dumps([data], ensure_ascii=False)
    assert "\\\\u00e9" in escaped and "café" in raw
''',
    "decode_unicode": '''
import json
blob = json.dumps(["café Σ 😀; " * 100000], ensure_ascii=False)
def bench():
    for _ in range(10):
        data = json.loads(blob)
    assert data[0].startswith("café Σ 😀")
''',
    "decode_numbers": '''
import json
blob = "[" + ",".join(str(i * .5) for i in range(20000)) + "]"
def bench():
    for _ in range(20):
        data = json.loads(blob)
    assert data[12345] == 6172.5
''',
    "decode_hooks": '''
import json
blob = '[{"v": 1.25}, {"v": 17}]'
def number(value):
    return "number:" + value
def hook(pairs):
    return pairs
def bench():
    for _ in range(1000):
        data = json.loads(blob, parse_int=number, parse_float=number, object_pairs_hook=hook)
    assert data == [[("v", "number:1.25")], [("v", "number:17")]]
''',
    "encode_surrogates": '''
import json
data = "ab\\ud800" * 250000
def bench():
    for _ in range(10):
        blob = json.dumps([data], ensure_ascii=False)
    assert len(blob) == len(data) + 4 and "\\ud800" in blob
''',
    "decode_surrogates": '''
import json
blob = '["' + "ab\\ud800" * 250000 + '"]'
def bench():
    for _ in range(10):
        data = json.loads(blob)
    assert len(data[0]) == 750000 and "\\ud800" in data[0]
''',
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
