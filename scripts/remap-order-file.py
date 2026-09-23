#!/usr/bin/env python3
"""remap-order-file.py ORDER INSTRUMENTED_BIN TARGET_BIN

Rewrite the crate disambiguators in a v0-mangled order file from the
instrumented build's to the target build's. Cargo gives each crate a
different `-C metadata` hash per build configuration, so the symbol
names `llvm-profdata order` reports for the instrumented binary differ
from the release binary's in exactly those hashes. Crates are paired by
name; a name whose hashes cannot be paired one-to-one is left alone.
Symbols the target binary does not define (inlined under LTO) are
dropped."""
import collections, re, subprocess, sys

CRATE = re.compile(r"Cs([0-9A-Za-z]*)_(\d+)")


def crate_hashes(binary):
    out = subprocess.run(["nm", "-j", binary], capture_output=True, text=True, check=True).stdout
    seen = collections.defaultdict(set)
    for sym in out.split():
        for m in CRATE.finditer(sym):
            n = int(m.group(2))
            name = sym[m.end():m.end() + n]
            if len(name) == n:
                seen[name].add(m.group(1))
    return seen


def main():
    order, instr_bin, target_bin = sys.argv[1:4]
    a, b = crate_hashes(instr_bin), crate_hashes(target_bin)
    mapping = {}
    for name, ha in a.items():
        hb = b.get(name, set())
        only_a, only_b = ha - hb, hb - ha
        if len(only_a) == 1 and len(only_b) == 1:
            mapping[next(iter(only_a))] = next(iter(only_b))
    def sub(m):
        h = mapping.get(m.group(1))
        return m.group(0) if h is None else "Cs%s_%s" % (h, m.group(2))
    with open(order) as f:
        lines = [CRATE.sub(sub, line) for line in f]
    target_syms = set(subprocess.run(["nm", "-j", target_bin], capture_output=True, text=True, check=True).stdout.split())
    kept = [line for line in lines if line.strip() in target_syms]
    with open(order, "w") as f:
        f.writelines(kept)
    print("remapped %d crates; kept %d of %d ordered symbols (defined in %s)" % (len(mapping), len(kept), len(lines), target_bin))


main()
