"""Check list-builder parity and inspect compilation and steady JIT exits."""

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
    env = {**os.environ, "WEAVEPY_STDLIB_CACHE": str(ROOT / "target/performance-stdlib-cache"),
           "WEAVEPY_JIT_THRESHOLD": "3", "WEAVEPY_JIT_TRACE": "1"}
    runs = {}
    targets = {"build_int", "build_float", "build_bool", "build_mixed", "byte_scramble"}
    for name, binary, flags, jit in (
        ("cpython", args.python, [], "0"), ("base", args.base, [], "1"),
        ("new", args.new, [], "1"), ("new_interp", args.new, [], "0"),
        ("new_gil0", args.new, ["-X", "gil=0"], "1"),
    ):
        result = subprocess.run([binary, *flags, str(ROOT / "tests/regrtest/test_jit_list_builders.py")],
                                env={**env, "WEAVEPY_JIT": jit}, text=True,
                                capture_output=True, timeout=180)
        compiled = sorted(set(re.findall(r'jit compile "([^"]+)"', result.stderr)))
        runs[name] = {"returncode": result.returncode, "stdout": result.stdout,
                      "stderr": result.stderr[-20000:], "compiled": compiled}
        print(name, result.returncode, "compiled targets:", sorted(set(compiled) & targets), flush=True)
    steady = {}
    kernels = runpy.run_path(str(HERE / "builder_probes.py"))["KERNELS"]
    for name, source in kernels.items():
        result = subprocess.run([args.new, "-c", source + "\nbench(60)\n"],
                                env={**env, "WEAVEPY_JIT": "1"}, text=True,
                                capture_output=True, timeout=180)
        steady[name] = {"returncode": result.returncode,
                        "compiled": 'jit compile "build"' in result.stderr,
                        "deopts": len(re.findall(r'jit deopt "build"', result.stderr)),
                        "stderr": result.stderr[-20000:]}
        print(name, "compiled:", steady[name]["compiled"], "deopts:", steady[name]["deopts"], flush=True)
    lifetime = {}
    for name, binary, flags, jit in (
        ("cpython", args.python, [], "0"), ("new", args.new, [], "1"),
        ("new_interp", args.new, [], "0"), ("new_gil0", args.new, ["-X", "gil=0"], "1"),
    ):
        result = subprocess.run([binary, *flags, str(ROOT / "tests/regrtest/test_jit_list_lifetime.py")],
                                env={**env, "WEAVEPY_JIT": jit}, text=True,
                                capture_output=True, timeout=180)
        compiled = sorted(set(re.findall(r'jit compile "([^"]+)"', result.stderr)))
        lifetime[name] = {"returncode": result.returncode, "stdout": result.stdout,
                          "stderr": result.stderr[-20000:], "compiled": compiled}
        print("lifetime", name, result.returncode, "compiled:", compiled, flush=True)
    report = {"runs": runs, "binaries": {
        name: {"path": binary, "sha256": hashlib.sha256(Path(binary).read_bytes()).hexdigest()}
        for name, binary in (("base", args.base), ("new", args.new))},
        "steady_paths": steady, "lifetime_runs": lifetime}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2) + "\n")
    assert all(run["returncode"] == 0 and run["stdout"] == "ok\n" for run in runs.values())
    assert targets <= set(runs["new"]["compiled"])
    assert all(run["returncode"] == 0 and run["stdout"] == "ok\n" for run in lifetime.values())
    assert {"convert_bytes", "returned_list", "discard_objects"} <= set(lifetime["new"]["compiled"])
    assert all(row["returncode"] == 0 and row["compiled"] and row["deopts"] == 0
               for row in steady.values())


if __name__ == "__main__":
    main()
