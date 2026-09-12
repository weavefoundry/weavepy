"""Measure enumeration with the standard paired timing and RSS harness."""

import argparse
import hashlib
import json
from pathlib import Path
import platform
import runpy
import tempfile

ROOT = Path(__file__).resolve().parents[4]
helpers = runpy.run_path(str(ROOT / "tools/bench_compare.py"))
measure = helpers["measure"]
relative_metrics = helpers["relative_metrics"]
verify_runtime = helpers["verify_runtime"]
KERNELS = {
    "enumerate_bytes": '''
DATA = bytes(range(256)) * 8
def checksum(data):
    total = 0
    for index, value in enumerate(data):
        total = total + (index ^ value)
    return total
def bench(n):
    total = 0
    for _ in range(n):
        total += checksum(DATA)
    assert total == n * 1835008
    return total
''',
    "enumerate_indices": '''
DATA = tuple(range(2048))
def checksum(data):
    total = 0
    for index, value in enumerate(data):
        total = total + index
    return total
def bench(n):
    total = 0
    for _ in range(n):
        total += checksum(DATA)
    assert total == n * 2096128
    return total
''',
    "enumerate_local_indices": '''
DATA = tuple(range(2048))
def checksum():
    data = DATA
    total = 0
    for index, value in enumerate(data):
        total = total + index
    return total
def bench(n):
    total = 0
    for _ in range(n):
        total += checksum()
    assert total == n * 2096128
    return total
''',
}


def main(kernels=KERNELS):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True)
    parser.add_argument("--new", required=True)
    parser.add_argument("--python", default="python3.14")
    parser.add_argument("--samples", type=int, default=9)
    parser.add_argument("--work", type=int, default=200)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if args.samples < 1 or args.work < 1:
        parser.error("samples and work must be positive")
    verify_runtime(args.base)
    verify_runtime(args.new)
    variants = [("base", args.base, "1"), ("new", args.new, "1"),
                ("base_interp", args.base, "0"), ("new_interp", args.new, "0"),
                ("cpython", args.python, None)]
    report = {"platform": platform.platform(), "samples": args.samples,
              "work": args.work, "warm": True, "binaries": {
                  name: {"path": path, "bytes": Path(path).stat().st_size,
                         "sha256": hashlib.sha256(Path(path).read_bytes()).hexdigest()}
                  for name, path in (("base", args.base), ("new", args.new))}, "rows": {}}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as temporary:
        fixtures = Path(temporary)
        for name, source in kernels.items():
            (fixtures / (name + ".py")).write_text(source)
            samples = {label: [] for label, _, _ in variants}
            for cycle in range(args.samples + 1):
                order = variants if cycle % 2 == 0 else reversed(variants)
                for label, binary, jit in order:
                    value = measure(binary, jit, name, args.work, True, fixture_root=fixtures)
                    if cycle:
                        samples[label].append(value)
            comparisons = {mode: relative_metrics(samples[new], samples[base])
                           for mode, new, base in (("jit", "new", "base"),
                                                   ("interp", "new_interp", "base_interp"),
                                                   ("cpython", "new", "cpython"))}
            report["rows"][name] = {"samples": samples, "comparisons": comparisons}
            args.out.write_text(json.dumps(report, indent=2) + "\n")
            print(name, {label: {k: round(v, 3) for k, v in row.items()}
                         for label, row in comparisons.items()}, flush=True)


if __name__ == "__main__":
    main()
