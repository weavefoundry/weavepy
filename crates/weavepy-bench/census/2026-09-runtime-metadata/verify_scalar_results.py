"""Check arithmetic fallback correctness and generic-call compilation."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import runpy
import subprocess

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True)
    parser.add_argument("--new", required=True)
    parser.add_argument("--python", default="python3.14")
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    env = {**os.environ,
           "WEAVEPY_STDLIB_CACHE": str(ROOT / "target/performance-stdlib-cache"),
           "WEAVEPY_JIT_THRESHOLD": "3", "WEAVEPY_JIT_TRACE": "1"}
    runs = {}
    for name, binary, flags, jit in (
        ("cpython", args.python, [], "0"), ("base", args.base, [], "1"),
        ("new", args.new, [], "1"), ("new_interp", args.new, [], "0"),
        ("new_gil0", args.new, ["-X", "gil=0"], "1"),
    ):
        result = subprocess.run(
            [binary, *flags, str(ROOT / "tests/regrtest/test_jit_scalar_results.py")],
            env={**env, "WEAVEPY_JIT": jit}, text=True,
            capture_output=True, timeout=180)
        compiled = sorted(set(re.findall(r'jit compile "([^"]+)"', result.stderr)))
        runs[name] = {"returncode": result.returncode, "stdout": result.stdout,
                      "stderr": result.stderr, "compiled": compiled}
        print(name, result.returncode, "compiled:", compiled, flush=True)

    steady = {}
    kernels = runpy.run_path(str(HERE / "scalar_result_probes.py"))["KERNELS"]
    for name, source in kernels.items():
        result = subprocess.run([args.new, "-c", source + "\nbench(60)\n"],
                                env={**env, "WEAVEPY_JIT": "1"}, text=True,
                                capture_output=True, timeout=180)
        steady[name] = {"returncode": result.returncode,
                        "compiled": 'jit compile "collect"' in result.stderr,
                        "deopts": len(re.findall(r'jit deopt "collect"', result.stderr)),
                        "stderr": result.stderr}
        print(name, "compiled:", steady[name]["compiled"],
              "deopts:", steady[name]["deopts"], flush=True)
    report = {"runs": runs, "steady_paths": steady, "binaries": {
        name: {"path": binary, "sha256": hashlib.sha256(Path(binary).read_bytes()).hexdigest()}
        for name, binary in (("base", args.base), ("new", args.new))}}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2) + "\n")
    assert all(run["returncode"] == 0 and run["stdout"] == "ok\n" for run in runs.values())
    targets = {"add_results", "left_results", "divide_results", "xor_results",
               "exact_division", "default_gap_calls"}
    assert targets <= set(runs["new"]["compiled"])
    assert all(row["returncode"] == 0 and row["compiled"] and row["deopts"] == 0
               for row in steady.values())


if __name__ == "__main__":
    main()
