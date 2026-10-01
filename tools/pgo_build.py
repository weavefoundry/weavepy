#!/usr/bin/env python3
"""Build a profile-guided (PGO) release of the `weavepy` CLI.

CPython's release builds are profile guided (`--enable-optimizations`
trains on the regression suite), and so is this build: it compiles an
instrumented interpreter, runs a training workload, merges the profile,
and rebuilds the release binary with it.

The training workload is the benchmark fixtures at the benchmark harness's
work sizes (with the JIT on and off), the bundled regression suite, and a
stdlib import sweep. Work sizes matter: code the training never reaches is
laid out as cold, so a fixture run at a toy size (large-integer
multiplication in `pidigits`, for one) gets slower, not faster.

Requirements: the `llvm-tools` rustup component (`rustup component add
llvm-tools`), which provides the `llvm-profdata` matching the compiler.

Usage (from the repository root):

    python3 tools/pgo_build.py
    python3 tools/pgo_build.py --skip-regrtest   # fixtures and imports only

The optimized binary is written to `target/pgo/optimized/release/weavepy`
(with the runtime library beside it); `--install` also copies it over
`target/release/weavepy`. Intermediate files stay under `target/pgo/`.
"""

import argparse
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PGO = ROOT / "target" / "pgo"
FIXTURES = ROOT / "crates" / "weavepy-bench" / "fixtures"
FIXTURES_RS = ROOT / "crates" / "weavepy-bench" / "src" / "fixtures.rs"
PACKAGES = ["-p", "weavepy-cli", "-p", "weavepy-pylib"]
IMPORTS = (
    "import argparse, ast, asyncio, collections, csv, dataclasses, datetime, "
    "decimal, email, enum, fractions, functools, heapq, inspect, itertools, "
    "json, logging, pathlib, pickle, random, re, statistics, string, "
    "textwrap, typing, unittest, urllib.parse"
)


def run(cmd, env=None, cwd=ROOT, check=True):
    print("+", " ".join(str(c) for c in cmd), flush=True)
    return subprocess.run(cmd, env=env, cwd=cwd, check=check)


def llvm_profdata():
    sysroot = subprocess.run(
        ["rustc", "--print", "sysroot"], capture_output=True, text=True, check=True
    ).stdout.strip()
    host = re.search(
        r"^host: (\S+)$",
        subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True).stdout,
        re.M,
    ).group(1)
    tool = Path(sysroot) / "lib" / "rustlib" / host / "bin" / "llvm-profdata"
    if os.name == "nt":
        tool = tool.with_suffix(".exe")
    if not tool.exists():
        sys.exit(f"{tool} not found: run `rustup component add llvm-tools` first")
    return tool


def build(target_dir, rustflags):
    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = str(target_dir)
    env["RUSTFLAGS"] = (env.get("RUSTFLAGS", "") + " " + rustflags).strip()
    run(["cargo", "build", "--release", *PACKAGES], env=env)
    exe = target_dir / "release" / ("weavepy.exe" if os.name == "nt" else "weavepy")
    if not exe.exists():
        sys.exit(f"build produced no {exe}")
    return exe


def fixture_work():
    """The benchmark harness's `(fixture, work)` pairs, read from its source."""
    text = FIXTURES_RS.read_text()
    return re.findall(r'"(\w+)" => ([\d_]+),', text)


def train(exe, skip_regrtest):
    for name, work in fixture_work():
        path = FIXTURES / f"{name}.py"
        if not path.exists():
            continue
        for jit in ("1", "0"):
            env = dict(os.environ, WEAVEPY_BENCH_WORK=work.replace("_", ""), WEAVEPY_JIT=jit)
            run([exe, path], env=env, check=False)
    for jit in ("1", "0"):
        run([exe, "-c", IMPORTS], env=dict(os.environ, WEAVEPY_JIT=jit), check=False)
    if not skip_regrtest:
        report = PGO / "regrtest"
        run(
            [exe, "regrtest", "--mode", "subprocess", "--workers", "0",
             "--timeout", "600", "-q", "--no-check", "--report-dir", report],
            check=False,
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--skip-regrtest", action="store_true",
                        help="train on the fixtures and imports only")
    parser.add_argument("--install", action="store_true",
                        help="also copy the optimized binary to target/release/weavepy")
    args = parser.parse_args()

    profdata_tool = llvm_profdata()
    raw = PGO / "raw"
    shutil.rmtree(raw, ignore_errors=True)
    raw.mkdir(parents=True)

    instrumented = build(PGO / "instrumented", f"-Cprofile-generate={raw}")
    train(instrumented, args.skip_regrtest)

    merged = PGO / "weavepy.profdata"
    profiles = sorted(raw.glob("*.profraw"))
    if not profiles:
        sys.exit("training produced no profiles")
    run([profdata_tool, "merge", "-o", merged, *profiles])

    optimized = build(PGO / "optimized", f"-Cprofile-use={merged}")
    print(f"optimized binary: {optimized}")
    if args.install:
        dest = ROOT / "target" / "release" / optimized.name
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(optimized, dest)
        print(f"installed: {dest}")


if __name__ == "__main__":
    main()
