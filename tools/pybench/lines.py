"""Attribute a samply profile's samples to source lines (macOS).

    samply record --save-only -o prof.json.gz --rate 4000 -- BIN script.py
    python3 tools/pybench/lines.py prof.json.gz BIN [--func SUBSTR] [--top 40]
        [--inclusive]

Counts the self samples (or, with --inclusive, every sample whose stack
passes through the function) at each address of the main interpreter
thread inside `BIN`, keeps the functions whose demangled name contains
SUBSTR, and maps the addresses to `file:line` with `atos`. This shows
where in a large function (the core dispatch loop) the time goes, which
sampling call graphs can't.
"""

import argparse
import collections
import gzip
import json
import os
import subprocess
import sys

TEXT_BASE = 0x100000000  # the default __TEXT vmaddr of a Mach-O executable


def load(path):
    opener = gzip.open if path.endswith(".gz") else open
    with opener(path) as fh:
        return json.load(fh)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("profile")
    ap.add_argument("binary")
    ap.add_argument("--func", default="")
    ap.add_argument("--top", type=int, default=40)
    ap.add_argument("--inclusive", action="store_true")
    ap.add_argument("--thread", default="weavepy-main")
    ap.add_argument("--chain", action="store_true", help="show the enclosing inlined frames too")
    args = ap.parse_args()

    prof = load(args.profile)
    syms_path = args.profile.replace(".json.gz", ".json.syms.json")
    names = {}
    if os.path.exists(syms_path):
        syms = json.load(open(syms_path))
        table = syms["string_table"]
        for lib in syms["data"]:
            if os.path.basename(lib.get("debug_name", "")) != os.path.basename(args.binary):
                continue
            for entry in lib["symbol_table"]:
                names[(entry["rva"], entry["size"])] = table[entry["symbol"]]
    ranges = sorted(names.items())

    def symbol_at(addr):
        lo, hi = 0, len(ranges)
        while lo < hi:
            mid = (lo + hi) // 2
            (rva, size), _ = ranges[mid]
            if addr < rva:
                hi = mid
            elif addr >= rva + size:
                lo = mid + 1
            else:
                return ranges[mid][1]
        return "?"

    thread = next(t for t in prof["threads"] if t["name"] == args.thread)
    frames = thread["frameTable"]
    stacks = thread["stackTable"]
    lib_index = next(i for i, l in enumerate(prof["libs"]) if l["name"] == os.path.basename(args.binary))
    func_res = thread["funcTable"]["resource"]
    res_lib = thread["resourceTable"]["lib"]

    def frame_addr(f):
        func = frames["func"][f]
        res = func_res[func]
        if res is None or res < 0 or res_lib[res] != lib_index:
            return None
        return frames["address"][f]

    counts = collections.Counter()
    total = 0
    for stack in thread["samples"]["stack"]:
        if stack is None:
            continue
        total += 1
        seen = set()
        s = stack
        first = True
        while s is not None:
            addr = frame_addr(stacks["frame"][s])
            if addr is not None and addr >= 0:
                name = symbol_at(addr)
                if args.func in name and (first or args.inclusive) and addr not in seen:
                    counts[addr] += 1
                    seen.add(addr)
            if not args.inclusive:
                break
            first = False
            s = stacks["prefix"][s]

    top = counts.most_common(args.top * 4)
    addrs = [a for a, _ in top]
    lines = {}
    for a in addrs:
        # `-i` lists the inlined frames innermost first; the innermost one
        # is where the sample's instruction came from.
        out = subprocess.run(
            ["atos", "-i", "-o", args.binary, "-l", hex(TEXT_BASE), hex(TEXT_BASE + a)],
            capture_output=True, text=True,
        ).stdout.splitlines()
        chain = [l for l in out if l.strip()]
        if not chain:
            continue
        def loc(line):
            return line.rsplit("(", 1)[-1].rstrip(")") if "(" in line else line
        lines[a] = loc(chain[0]) + ("  <- " + " <- ".join(loc(c) for c in chain[1:3]) if args.chain else "")
    by_line = collections.Counter()
    for a, n in top:
        by_line[lines.get(a, hex(a))] += n
    matched = sum(counts.values())
    print("samples: %d, in matching functions: %d" % (total, matched))
    for loc, n in by_line.most_common(args.top):
        print("%6d %5.1f%%  %s" % (n, 100.0 * n / max(total, 1), loc))


if __name__ == "__main__":
    sys.exit(main())
