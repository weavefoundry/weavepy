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

1. The hot group: the functions a program keeps running once it has
   started, imported and compiled what it needs. `--runs DIR` names a
   directory with two profiles per training program, `NAME.short-*.profdata`
   from a short run and `NAME.long-*.profdata` from one of the same program
   that repeats its work more often: a function is in the group when the
   long run of some program executed it at least COUNT times more (default
   1,000). What start-up, an import or a compile runs counts the same in
   both, however often that is. The functions the most programs keep
   running come first, then each program's own, together. Without `--runs`
   the group is what the merged profile executed COUNT times, which takes
   in a good deal of start-up.
2. Everything else the training executed: BASE.order's entries in their
   order (start-up's first-use order), then the rest, hottest first.
3. BASE.order's other entries (another platform's start-up code).

A function the training never ran isn't listed, so the linker places it
after all of these. The runtime releases its code's pages now and then,
and what a program keeps running comes back (crates/weavepy-cli/src/
text_trim.rs): the closer together that code is, the fewer pages do.

The linker wrapper (scripts/link-ordered.py) remaps the names onto the
link's own symbols, so the names here are the profile's, with Mach-O's
leading underscore.
"""

import collections
import os
import re
import subprocess
import sys



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


def steady(short, long):
    """`{key: count}`: how much more often the long run of a program
    executed each function than the short one (see the module docs)."""
    more = {}
    for k, (_, count) in long.items():
        extra = count - short.get(k, (None, 0))[1]
        if extra > 0:
            more[k] = extra
    return more


def layout(ran, programs, lines, hot_count):
    """The order file's lines: `ran` the merged profile's executed
    functions (as [`executed`] returns them), `programs` each training
    program's steady counts (as [`steady`] returns them) in training
    order, `lines` the base order file's entries."""
    listed = {}
    for line in lines:
        listed.setdefault(key(line), line)

    def count(k):
        return ran[k][1]

    def symbol(k):
        # (With Mach-O's leading underscore, as BASE.order has it.)
        return "_" + ran[k][0]

    if programs:
        # Per function: how many programs keep running it, the first that
        # does, and the most one of them does.
        keep = collections.Counter()
        first = {}
        most = collections.Counter()
        for i, program in enumerate(programs):
            for k, extra in program.items():
                if extra >= hot_count and k in ran:
                    keep[k] += 1
                    first.setdefault(k, i)
                    most[k] = max(most[k], extra)
        # What most programs keep running is one block; the rest goes by
        # program, so that a program's own code shares its pages with
        # little of another's.
        shared = max(2, len(programs) // 4)
        hot = sorted(
            keep,
            key=lambda k: (0, -keep[k], -most[k], k) if keep[k] >= shared else (1, first[k], -most[k], k),
        )
    else:
        hot = sorted((k for k in ran if count(k) >= hot_count), key=lambda k: (-count(k), k))
    hot_set = set(hot)
    warm_listed = [k for k in listed if k in ran and k not in hot_set]
    seen = hot_set.union(warm_listed)
    warm_rest = sorted((k for k in ran if k not in seen), key=lambda k: (-count(k), k))
    unseen = [k for k in listed if k not in ran]
    out = [symbol(k) for k in hot]
    out += [listed[k] for k in warm_listed]
    out += [symbol(k) for k in warm_rest]
    out += [listed[k] for k in unseen]
    return out, (len(hot), len(warm_listed) + len(warm_rest), len(unseen))


def programs_in(tool, runs_dir):
    """Each training program's steady counts, from the `NAME.short-*` and
    `NAME.long-*` profiles in `runs_dir`, in name order."""
    names = {}
    for f in sorted(os.listdir(runs_dir)):
        m = re.match(r"(.+)\.(short|long)-[^.]*\.profdata$", f)
        if m:
            names.setdefault(m.group(1), {})[m.group(2)] = os.path.join(runs_dir, f)
    programs = []
    for name in sorted(names):
        pair = names[name]
        if "short" in pair and "long" in pair:
            programs.append(steady(executed(tool, pair["short"]), executed(tool, pair["long"])))
    return programs


def main():
    args = sys.argv[1:]
    runs_dir = option(args, "--runs")
    hot_count = int(option(args, "--hot", "1000"))
    if len(args) != 4:
        raise SystemExit(__doc__)
    tool, merged, base, out = args
    ran = executed(tool, merged)
    programs = programs_in(tool, runs_dir) if runs_dir else []
    with open(base, encoding="utf-8") as f:
        lines = [line.strip() for line in f if line.strip() and not line.startswith("#")]
    ordered, (hot, warm, unseen) = layout(ran, programs, lines, hot_count)
    with open(out, "w", encoding="utf-8") as f:
        f.write("".join(line + "\n" for line in ordered))
    print("%s: %d hot (from %d programs), %d more the training ran, %d others from %s" % (
        out, hot, len(programs), warm, unseen, base))


if __name__ == "__main__":
    main()
