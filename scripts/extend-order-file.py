#!/usr/bin/env python3
"""Build the linker order file from callgrind profiles.

Usage: extend-order-file.py ORDER OUT STARTUP_PROFILE [PROFILE...]

The layout, in groups whose members a process tends to run together:

1. what start-up runs: the functions STARTUP_PROFILE (say, of `-c pass`)
   executed, in ORDER's first-use order where ORDER has them, and ORDER's
   entries no profile executed (another platform's start-up code);
2. the functions the other profiles executed, those the most profiles ran
   first;
3. ORDER's remaining entries.

Profile with mangled names, for example on Linux:

    valgrind --tool=callgrind --demangle=no --compress-strings=no \\
        --callgrind-out-file=cg.NAME target/quick/weavepy \\
        tools/pybench/harness.py NAME 1 1

Entries that differ only in crate hashes or thin-LTO suffixes (the same
function in another profile's or toolchain's build) collapse to the first:
the linker wrapper (scripts/link-ordered.py) matches names with those
masked out.
"""

import collections
import re
import sys

CRATE = re.compile(r"Cs([0-9A-Za-z]+)_")
LEGACY = re.compile(r"17h[0-9a-f]{16}E")
PROMOTED = re.compile(r"\.llvm\.[0-9]+$")
# Callgrind's mark for a recursion level (`fn'2`).
LEVEL = re.compile(r"'[0-9]+$")


def key(symbol):
    """`link-ordered.py`'s matching key."""
    symbol = symbol.lstrip("_")
    symbol = CRATE.sub(lambda m: "Cs" + "#" * len(m.group(1)) + "_", symbol)
    return PROMOTED.sub("", LEGACY.sub("17h#E", symbol))


def executed(path):
    """The functions of the `weavepy` object a callgrind profile ran."""
    names, obj = [], None
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            if line.startswith("ob="):
                obj = line[3:].strip()
            elif line.startswith("fn=") and obj and obj.endswith("weavepy"):
                names.append(LEVEL.sub("", line[3:].strip()))
    return list(dict.fromkeys(names))


def main():
    if len(sys.argv) < 4:
        raise SystemExit(__doc__)
    with open(sys.argv[1], encoding="utf-8") as f:
        order = [line.strip() for line in f if line.strip() and not line.startswith("#")]
    startup = executed(sys.argv[3])
    others = [executed(path) for path in sys.argv[4:]]
    ran = {key(n) for n in startup} | {key(n) for names in others for n in names}
    startup_keys = {key(n) for n in startup}
    out, seen = [], set()

    def add(name):
        if key(name) not in seen:
            seen.add(key(name))
            out.append(name)

    for name in order:
        if key(name) in startup_keys or key(name) not in ran:
            add(name)
    for name in startup:
        add(name)
    groups = [len(out)]
    runs, first = collections.Counter(), {}
    for names in others:
        for name in names:
            runs[name] += 1
            first.setdefault(name, len(first))
    for name in sorted(runs, key=lambda n: (-runs[n], first[n])):
        add(name)
    groups.append(len(out))
    for name in order:
        add(name)
    with open(sys.argv[2], "w", encoding="utf-8") as f:
        f.write("".join(name + "\n" for name in out))
    print("start-up %d, workloads %d, rest %d entries" % (
        groups[0], groups[1] - groups[0], len(out) - groups[1]))


if __name__ == "__main__":
    main()
