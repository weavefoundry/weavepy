#!/usr/bin/env python3
"""Remap a macOS linker order file onto the symbols of a release binary.

First pair v0 crate disambiguators from the instrumented and release builds.
Then match remaining symbols by their demangled names, supporting both Rust
mangling formats. Only unique matches are used; generic instantiations whose
legacy names lose type information are deliberately left unmatched.

Pass --retain-originals when refreshing a checked-in order file to preserve
symbols for other targets or toolchains alongside their current equivalents.
Requires nm and llvm-cxxfilt (or Apple's xcrun llvm-cxxfilt).
"""

import argparse
import collections
from pathlib import Path
import re
import shutil
import subprocess

CRATE = re.compile(r"Cs([0-9A-Za-z]*)_(\d+)")
LEGACY_HASH = re.compile(r"::h[0-9a-f]{16}$")
ESCAPES = {
    "$LT$": "<", "$GT$": ">", "$SP$": "@", "$BP$": "*",
    "$RF$": "&", "$LP$": "(", "$RP$": ")", "$C$": ",",
}


def symbols(binary):
    output = subprocess.run(
        ["nm", "-j", "--defined-only", str(binary)],
        capture_output=True, text=True, check=True,
    ).stdout
    return list(dict.fromkeys(output.splitlines()))


def crate_hashes(names):
    seen = collections.defaultdict(set)
    for symbol in names:
        for match in CRATE.finditer(symbol):
            size = int(match.group(2))
            name = symbol[match.end():match.end() + size]
            if len(name) == size:
                seen[name].add(match.group(1))
    return seen


def crate_mapping(instrumented, target):
    mapping = {}
    source_hashes, target_hashes = crate_hashes(instrumented), crate_hashes(target)
    for name, source in source_hashes.items():
        dest = target_hashes.get(name, set())
        only_source, only_dest = source - dest, dest - source
        if len(only_source) == len(only_dest) == 1:
            mapping[(next(iter(only_source)), name)] = next(iter(only_dest))
    return mapping


def canonical_name(name):
    """Normalize demangler spelling without erasing generic arguments."""
    name = LEGACY_HASH.sub("", name)
    for old, new in ESCAPES.items():
        name = name.replace(old, new)
    name = re.sub(r"\$u([0-9a-f]+)\$", lambda match: chr(int(match[1], 16)), name)
    name = name.replace("..", "::").replace("_<", "<")
    # v0 wraps inherent impl types in angle brackets; legacy names don't.
    if name.startswith("<"):
        depth = 0
        for index, char in enumerate(name):
            depth += (char == "<") - (char == ">")
            if depth == 0:
                if " as " not in name[:index]:
                    name = name[1:index] + name[index + 1:]
                break
    return name


def demangle(names):
    command = ["llvm-cxxfilt"] if shutil.which("llvm-cxxfilt") else ["xcrun", "llvm-cxxfilt"]
    output = subprocess.run(
        command, input="\n".join(names) + "\n", capture_output=True,
        text=True, check=True,
    ).stdout.splitlines()
    if len(output) != len(names):
        raise RuntimeError("demangler returned a different number of names")
    return [canonical_name(name) for name in output]


def remap(order, target, mapping, source_names, target_names, retain_originals=False):
    """Return ordered, deduplicated symbols and match counts."""
    by_name = collections.defaultdict(set)
    for symbol, name in zip(target, target_names):
        by_name[name].add(symbol)
    defined = set(target)
    result = []
    counts = collections.Counter()

    def substitute(match):
        name = match.string[match.end():match.end() + int(match[2])]
        replacement = mapping.get((match[1], name))
        return match[0] if replacement is None else "Cs%s_%s" % (replacement, match[2])

    for symbol, name in zip(order, source_names):
        if retain_originals:
            result.append(symbol)
        rewritten = CRATE.sub(substitute, symbol)
        if rewritten in defined:
            result.append(rewritten)
            counts["exact"] += 1
        elif len(by_name[name]) == 1:
            result.append(next(iter(by_name[name])))
            counts["semantic"] += 1
        else:
            counts["ambiguous" if by_name[name] else "missing"] += 1
    return list(dict.fromkeys(result)), counts


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("order", type=Path)
    parser.add_argument("instrumented_bin", type=Path)
    parser.add_argument("target_bin", type=Path)
    parser.add_argument("--retain-originals", action="store_true")
    args = parser.parse_args()
    order = [line.strip() for line in args.order.read_text().splitlines()
             if line.strip() and not line.lstrip().startswith("#")]
    source, target = symbols(args.instrumented_bin), symbols(args.target_bin)
    mapping = crate_mapping(source, target)
    kept, counts = remap(
        order, target, mapping, demangle(order), demangle(target), args.retain_originals
    )
    if not counts["exact"] + counts["semantic"]:
        raise SystemExit("no ordered symbols matched; leaving the order file unchanged")
    args.order.write_text("".join(symbol + "\n" for symbol in kept))
    print("remapped %d crates; %d exact, %d demangled, %d ambiguous, %d missing; wrote %d symbols" % (
        len(mapping), counts["exact"], counts["semantic"], counts["ambiguous"],
        counts["missing"], len(kept),
    ))


if __name__ == "__main__":
    main()
