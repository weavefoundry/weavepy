#!/usr/bin/env python3
"""Write the linker order file for a profile-guided build from its own
training run.

    tools/pgo/order.py LLVM_PROFDATA MERGED.profdata BASE.order OUT.order
        [--runs DIR] [--hot COUNT]

The checked-in order file (crates/weavepy-cli/weavepy.order) goes stale as
the code changes, and a profile-guided build inlines other functions than
the build that file was made from: about half of its names match nothing
there, and some of the hottest functions aren't in it at all. This writes
the order from what the training itself executed, in three parts:

1. The hot group, between the markers `weavepy_text_hot_start` and
   `weavepy_text_hot_end`: the functions some training process ran at
   least COUNT times (default 1,000), hottest first. That leaves out what
   only start-up, an import or a compile runs. `--runs DIR` names a
   directory with one profile per training process (`*.profdata`), which
   is what tells a function one process ran a million times from one that
   a thousand processes ran once each; without it the merged profile's
   counts stand in.
2. Everything else the training executed: BASE.order's entries in their
   order (start-up's first-use order), then the rest, those the most
   processes ran first.
3. BASE.order's other entries (another platform's start-up code).

A function the training never ran isn't listed, so the linker places it
after all of these. The runtime releases the pages after the hot group
once a process has stopped running them (crates/weavepy-cli/src/
text_trim.rs).

The linker wrapper (scripts/link-ordered.py) remaps the names onto the
link's own symbols, so the names here are the profile's, with Mach-O's
leading underscore.
"""

import collections
import os
import re
import subprocess
import sys

HOT_START = "_weavepy_text_hot_start"
HOT_END = "_weavepy_text_hot_end"


# As scripts/link-ordered.py matches names: v0 crate disambiguators
# (`Cs<base62>_`), legacy symbol hashes and thin LTO's promotion suffix
# masked out, and no leading underscores.
CRATE = re.compile(r"Cs([0-9A-Za-z]+)_")
LEGACY = re.compile(r"17h[0-9a-f]{16}E")
PROMOTED = re.compile(r"\.llvm\.[0-9]+$")


def key(symbol):
    symbol = symbol.lstrip("_")
    symbol = CRATE.sub(lambda m: "Cs" + "#" * len(m.group(1)) + "_", symbol)
    return PROMOTED.sub("", LEGACY.sub("17h#E", symbol))


def executed(profdata_tool, profile):
    """`{key: (symbol, hottest counter)}` for the functions `profile`
    executed."""
    out = subprocess.run(
        [profdata_tool, "show", "--all-functions", "--counts", profile],
        capture_output=True, text=True, errors="replace", check=True,
    ).stdout
    ran = {}
    name = None
    for line in out.splitlines():
        if line.startswith("  ") and not line.startswith("    ") and line.endswith(":"):
            # (A local function's name carries its file: `file;symbol`.)
            name = line.strip()[:-1].rsplit(";", 1)[-1]
        elif line.startswith("    Block counts:") and name:
            hottest = max((int(n) for n in re.findall(r"\d+", line.split(":", 1)[1])), default=0)
            if hottest > 0:
                k = key(name)
                ran[k] = (name, max(ran.get(k, (name, 0))[1], hottest))
            name = None
    return ran


def option(args, flag, default=None):
    if flag not in args:
        return default
    i = args.index(flag)
    value = args[i + 1]
    del args[i:i + 2]
    return value


def layout(ran, runs, lines, hot_count):
    """The order file's lines: `ran` the merged profile's executed
    functions and each of `runs` one process's (as [`executed`] returns
    them), `lines` the base order file's entries."""
    # Per function: how many processes ran it, and the most one of them did.
    popularity = collections.Counter()
    most = collections.Counter()
    for run in runs:
        for k, (_, count) in run.items():
            popularity[k] += 1
            most[k] = max(most[k], count)
    if not runs:
        most = collections.Counter({k: count for k, (_, count) in ran.items()})
    markers = {key(HOT_START), key(HOT_END)}
    listed = {}
    for line in lines:
        if key(line) not in markers:
            listed.setdefault(key(line), line)

    def count(k):
        return ran[k][1]

    def symbol(k):
        # (With Mach-O's leading underscore, as BASE.order has it.)
        return "_" + ran[k][0]

    hot = sorted((k for k in ran if most[k] >= hot_count), key=lambda k: (-count(k), k))
    hot_set = set(hot)
    warm_listed = [k for k in listed if k in ran and k not in hot_set]
    seen = hot_set.union(warm_listed)
    warm_rest = sorted((k for k in ran if k not in seen), key=lambda k: (-popularity[k], -count(k), k))
    unseen = [k for k in listed if k not in ran]
    out = [HOT_START]
    out += [symbol(k) for k in hot]
    out.append(HOT_END)
    out += [listed[k] for k in warm_listed]
    out += [symbol(k) for k in warm_rest]
    out += [listed[k] for k in unseen]
    return out, (len(hot), len(warm_listed) + len(warm_rest), len(unseen))


def main():
    args = sys.argv[1:]
    runs_dir = option(args, "--runs")
    hot_count = int(option(args, "--hot", "1000"))
    if len(args) != 4:
        raise SystemExit(__doc__)
    tool, merged, base, out = args
    ran = executed(tool, merged)
    runs = []
    if runs_dir:
        runs = [
            executed(tool, os.path.join(runs_dir, f))
            for f in sorted(os.listdir(runs_dir))
            if f.endswith(".profdata")
        ]
    with open(base, encoding="utf-8") as f:
        lines = [line.strip() for line in f if line.strip() and not line.startswith("#")]
    ordered, (hot, warm, unseen) = layout(ran, runs, lines, hot_count)
    with open(out, "w", encoding="utf-8") as f:
        f.write("".join(line + "\n" for line in ordered))
    print("%s: %d hot, %d more the training ran, %d others from %s" % (out, hot, warm, unseen, base))


if __name__ == "__main__":
    main()
