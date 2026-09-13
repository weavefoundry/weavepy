"""Check completed-call semantics and integer-result pin pressure."""

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
            [binary, *flags, str(ROOT / "tests/regrtest/test_jit_unboxed_calls.py")],
            env={**env, "WEAVEPY_JIT": jit}, text=True,
            capture_output=True, timeout=180)
        compiled = sorted(set(re.findall(r'jit compile "([^"]+)"', result.stderr)))
        runs[name] = {"returncode": result.returncode, "stdout": result.stdout,
                      "stderr": result.stderr, "compiled": compiled}
        print(name, result.returncode, "compiled:", compiled, flush=True)
    steady = {}
    kernels = runpy.run_path(str(HERE / "long_scalar_probes.py"))["KERNELS"]
    for name, source in kernels.items():
        argument = "SOURCE" if name == "callable_instance_sum" else "source"
        delta = {"integer_callback_sum": 0, "integer_callback_left": 1,
                 "keyword_callback_sum": 2, "callable_instance_sum": 3}[name]
        # Invoke collect from module code so each activation reaches the
        # framed exit trace. A compiled bench wrapper would route collect's
        # fallback through native-to-native call accounting instead.
        driver = f'''
for _ in range(60):
    assert collect(512, {argument}) == 130816 + {delta} * 512
assert collect(102400, {argument}) == 5242828800 + {delta} * 102400
'''
        result = subprocess.run(
            [args.new, "-c", source + driver],
            env={**env, "WEAVEPY_JIT": "1"}, text=True,
            capture_output=True, timeout=180)
        steady[name] = {"returncode": result.returncode,
                        "compiled": 'jit compile "collect"' in result.stderr,
                        "deopts": len(re.findall(r'jit deopt "collect"', result.stderr)),
                        "immediate_consumer": name != "integer_callback_left",
                        "stderr": result.stderr}
        print(name, "compiled:", steady[name]["compiled"],
              "deopts:", steady[name]["deopts"], flush=True)
    report = {"runs": runs, "long_activations": steady, "binaries": {
        name: {"path": binary, "sha256": hashlib.sha256(Path(binary).read_bytes()).hexdigest()}
        for name, binary in (("base", args.base), ("new", args.new))}}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2) + "\n")
    assert all(run["returncode"] == 0 and run["stdout"] == "ok\n" for run in runs.values())
    assert {"sum_results", "guarded_calls"} <= set(runs["new"]["compiled"])
    assert all(row["returncode"] == 0 and row["compiled"]
               and (not row["immediate_consumer"] or row["deopts"] == 0)
               for row in steady.values())

if __name__ == "__main__":
    main()
