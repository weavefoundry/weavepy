"""Compare interpreters on the realistic workload suite.

    python3 tools/pybench/run.py --weavepy target/release/weavepy [--base OTHER]
        [--cpython python3.14] [--procs 3] [--reps 5] [--filter SUBSTR]
        [--json OUT.json] [--instructions]

Each benchmark runs in fresh processes, interleaving the interpreters
(CPython, WeavePy, base, CPython, ...). Within a process the first call is
the cold sample and the following calls are warm samples. The report gives
the median warm time, the median cold time, and the peak RSS of each
interpreter, with WeavePy/CPython ratios (below 1.00 means WeavePy is
faster or smaller) and their geometric means. Results are compared with
CPython's: a mismatch is reported as a failure.

``--instructions`` instead counts instructions retired (macOS
``/usr/bin/time -l``) for a process doing 1 and 1 + REPS calls, and reports
the per-call difference, which cancels startup and import work and is
stable on a loaded machine.

Each interpreter gets a frozen code cache of its own under
``target/pybench-frozen/``, filled by one untimed warm-up run per
benchmark. WeavePy's per-user cache is shared by every build, and with
``PYTHONDONTWRITEBYTECODE`` set (as agent shells do) a build whose embedded
stdlib differs from the one that last wrote a module compiles that module
from source on every run, which inflates its time and memory. The warm-up
also leaves no benchmark ``.pyc`` behind: one written by another build
could carry different code under the same cache tag.
"""

import argparse
import hashlib
import json
import math
import os
import re
import shutil
import statistics
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
BENCH_DIR = os.path.join(HERE, "benchmarks")
HARNESS = os.path.join(HERE, "harness.py")
FROZEN_ROOT = os.path.join(os.path.dirname(os.path.dirname(HERE)), "target", "pybench-frozen")


def interp_env(interp, write=False):
    """The environment for a run of `interp`: its own frozen code cache
    (see the module docs), written only when `write` is set."""
    env = dict(os.environ)
    env.pop("PYTHONPATH", None)
    path = os.path.realpath(shutil.which(interp) or interp)
    key = hashlib.sha1(path.encode()).hexdigest()[:16]
    env["WEAVEPY_FROZEN_CACHE"] = os.path.join(FROZEN_ROOT, key)
    if write:
        env.pop("PYTHONDONTWRITEBYTECODE", None)
    else:
        env["PYTHONDONTWRITEBYTECODE"] = "1"
    return env


def warm(interp, name, timeout):
    """Fill `interp`'s frozen code cache with what benchmark `name`
    imports, then drop the benchmark pycs the run wrote."""
    try:
        subprocess.run([interp, HARNESS, name, "1", "0"], capture_output=True,
                       timeout=timeout, env=interp_env(interp, write=True))
    except subprocess.TimeoutExpired:
        pass
    shutil.rmtree(os.path.join(BENCH_DIR, "__pycache__"), ignore_errors=True)


def discover():
    names = []
    for f in sorted(os.listdir(BENCH_DIR)):
        if f.endswith(".py") and not f.startswith("_"):
            names.append(f[:-3])
    return names


def work_for(name):
    with open(os.path.join(BENCH_DIR, name + ".py")) as fh:
        m = re.search(r"^WORK\s*=\s*(\d+)", fh.read(), re.M)
    return int(m.group(1)) if m else 1


def run_once(interp, name, work, reps, timeout):
    proc = subprocess.run(
        [interp, HARNESS, name, str(work), str(reps)],
        capture_output=True, text=True, timeout=timeout, env=interp_env(interp),
    )
    if proc.returncode != 0:
        return {"error": (proc.stderr or proc.stdout).strip().splitlines()[-1:] or ["exit %d" % proc.returncode]}
    line = proc.stdout.strip().splitlines()[-1]
    return json.loads(line)


def instructions(interp, name, work, reps, timeout):
    def count(r):
        proc = subprocess.run(
            ["/usr/bin/time", "-l", interp, HARNESS, name, str(work), str(r)],
            capture_output=True, text=True, timeout=timeout, env=interp_env(interp),
        )
        m = re.search(r"(\d+)\s+instructions retired", proc.stderr)
        if proc.returncode != 0 or not m:
            return None
        return int(m.group(1))

    a = min(filter(None, [count(0), count(0)]), default=None)
    b = min(filter(None, [count(reps), count(reps)]), default=None)
    if a is None or b is None:
        return None
    return (b - a) / reps


def geomean(xs):
    xs = [x for x in xs if x and x > 0]
    return math.exp(sum(math.log(x) for x in xs) / len(xs)) if xs else float("nan")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--weavepy", required=True)
    ap.add_argument("--base")
    ap.add_argument("--cpython", default="python3.14")
    ap.add_argument("--procs", type=int, default=3)
    ap.add_argument("--reps", type=int, default=5)
    ap.add_argument("--filter", action="append")
    ap.add_argument("--exclude", action="append")
    ap.add_argument("--json")
    ap.add_argument("--instructions", action="store_true")
    ap.add_argument("--timeout", type=int, default=600)
    args = ap.parse_args()

    interps = [("cpython", args.cpython), ("weavepy", args.weavepy)]
    if args.base:
        interps.append(("base", args.base))
    names = discover()
    if args.filter:
        names = [n for n in names if any(f in n for f in args.filter)]
    if args.exclude:
        names = [n for n in names if not any(f in n for f in args.exclude)]

    rows = []
    for name in names:
        work = work_for(name)
        for _, interp in interps:
            warm(interp, name, args.timeout)
        if args.instructions:
            row = {"name": name}
            for label, interp in interps:
                row[label] = instructions(interp, name, work, args.reps, args.timeout)
            rows.append(row)
            c, w = row["cpython"], row["weavepy"]
            ratio = w / c if c and w else float("nan")
            row["ratio"] = ratio
            extra = ""
            if args.base and row.get("base") and w:
                row["vs_base"] = w / row["base"]
                extra = "  vs base %5.2f" % row["vs_base"]
            print("%-22s cpython %14s  weavepy %14s  ratio %6.2f%s" % (
                name, "%.0f" % c if c else "-", "%.0f" % w if w else "-", ratio, extra), flush=True)
            continue
        samples = {label: [] for label, _ in interps}
        for p in range(args.procs):
            order = interps if p % 2 == 0 else list(reversed(interps))
            for label, interp in order:
                try:
                    samples[label].append(run_once(interp, name, work, args.reps, args.timeout))
                except subprocess.TimeoutExpired:
                    samples[label].append({"error": ["timeout"]})
        row = {"name": name, "work": work}
        for label, _ in interps:
            ok = [s for s in samples[label] if "error" not in s]
            if not ok:
                row[label] = {"error": samples[label][0]["error"]}
                continue
            row[label] = {
                "warm": statistics.median(x for s in ok for x in s["warm_ns"]) / 1e6,
                "cold": statistics.median(s["cold_ns"] for s in ok) / 1e6,
                "import": statistics.median(s["import_ns"] for s in ok) / 1e6,
                "rss": statistics.median(s["rss"] for s in ok) / (1 << 20),
                "result": ok[0]["result"],
            }
        cp, wp = row["cpython"], row["weavepy"]
        if "error" in wp or "error" in cp:
            row["status"] = "ERROR " + " ".join(wp.get("error", cp.get("error", [])))
        elif wp["result"] != cp["result"]:
            row["status"] = "MISMATCH"
        else:
            row["status"] = "ok"
            row["warm_ratio"] = wp["warm"] / cp["warm"]
            row["cold_ratio"] = wp["cold"] / cp["cold"]
            row["rss_ratio"] = wp["rss"] / cp["rss"]
            if args.base and "error" not in row["base"]:
                row["vs_base"] = wp["warm"] / row["base"]["warm"]
        rows.append(row)
        if row["status"] == "ok":
            extra = "  vs base %5.2f" % row["vs_base"] if "vs_base" in row else ""
            print("%-22s cpy %9.2f ms  wp %9.2f ms  warm %5.2f  cold %5.2f  rss %5.2f%s" % (
                name, cp["warm"], wp["warm"], row["warm_ratio"], row["cold_ratio"], row["rss_ratio"], extra), flush=True)
        else:
            print("%-22s %s %s" % (name, row["status"], wp.get("result", "") if row["status"] == "MISMATCH" else ""), flush=True)

    if args.instructions:
        ok = [r for r in rows if r.get("ratio") == r.get("ratio") and r.get("ratio")]
        print()
        print("geomean over %d benchmarks: instructions %.3f%s" % (
            len(ok), geomean(r["ratio"] for r in ok),
            "  vs base %.3f" % geomean(r["vs_base"] for r in ok if "vs_base" in r) if args.base else ""))
    else:
        ok = [r for r in rows if r.get("status") == "ok"]
        print()
        print("geomean over %d benchmarks: warm %.3f  cold %.3f  rss %.3f%s" % (
            len(ok),
            geomean(r["warm_ratio"] for r in ok),
            geomean(r["cold_ratio"] for r in ok),
            geomean(r["rss_ratio"] for r in ok),
            "  vs base %.3f" % geomean(r["vs_base"] for r in ok if "vs_base" in r) if args.base else "",
        ))
        slower = sorted((r for r in ok if r["warm_ratio"] > 0.8), key=lambda r: -r["warm_ratio"])
        if slower:
            print("not yet 1.25x faster than CPython (warm): " + ", ".join(
                "%s %.2f" % (r["name"], r["warm_ratio"]) for r in slower))
        bad = [r["name"] + " " + r["status"] for r in rows if r.get("status") != "ok"]
        if bad:
            print("failures: " + ", ".join(bad))
    if args.json:
        with open(args.json, "w") as fh:
            json.dump(rows, fh, indent=1)


if __name__ == "__main__":
    sys.exit(main())
