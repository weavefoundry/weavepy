#!/usr/bin/env python3
"""Link through `cc`, first remapping the `weavepy` symbol order file onto
the symbols this link actually defines.

`crates/weavepy-cli/build.rs` hands the linker `weavepy.order`, the
functions start-up and common workloads run, in first-use order (lld's
`--symbol-ordering-file`, ld64's `-order_file`). Both linkers match names
exactly, and a Rust symbol embeds each crate's disambiguator, a hash of
cargo's metadata (the toolchain, the profile, the target, the dependency
versions), so a checked-in file goes stale with any of them and the layout
silently degrades to none: start-up then touches code pages spread over
the whole text segment (on Linux, whose page faults map 64 KiB around each
touched page, `-c pass` peaked at 22 MiB against 14 MiB ordered).

This wrapper (the configured linker for the host targets in
`.cargo/config.toml`, via `link-ordered.sh`) lists the symbols the link's
inputs define, matches the order file's names to them with the crate
disambiguators and legacy hashes masked out, writes the matched names
next to the output, and runs `cc` with that file in place of the original.
Any other link, or any failure here, runs `cc` with the arguments
unchanged.
"""

import os
import re
import subprocess
import sys

CC = os.environ.get("WEAVEPY_LINK_CC", "cc")
# v0 crate disambiguators (`Cs<base62>_`) and legacy symbol hashes
# (`17h<16 hex>E`). A v0 back-reference counts bytes, so a mask keeps
# each disambiguator's length: names whose crates' hashes differ in
# length don't match (rare: most are eleven characters).
CRATE = re.compile(r"Cs([0-9A-Za-z]+)_")
LEGACY = re.compile(r"17h[0-9a-f]{16}E")
# The suffix thin LTO gives a local symbol it promotes (a hash too).
PROMOTED = re.compile(r"\.llvm\.[0-9]+$")


def key(symbol):
    symbol = symbol.lstrip("_")
    symbol = CRATE.sub(lambda m: "Cs" + "#" * len(m.group(1)) + "_", symbol)
    return PROMOTED.sub("", LEGACY.sub("17h#E", symbol))




def defined(inputs):
    """The text symbols `inputs` define, in a dictionary keyed by `key`
    (dropping keys more than one symbol shares)."""
    names = {}
    shared = set()
    for chunk in range(0, len(inputs), 200):
        result = subprocess.run(
            ["nm", "-P", "--defined-only"] + inputs[chunk:chunk + 200],
            capture_output=True, text=True, errors="replace", check=False,
        )
        for line in result.stdout.splitlines():
            fields = line.split()
            if len(fields) < 2 or fields[1] not in ("T", "t", "W", "w"):
                continue
            name = fields[0]
            k = key(name)
            if names.get(k, name) != name:
                shared.add(k)
            names[k] = name
    for k in shared:
        del names[k]
    return names


def remap(args):
    # (A link long enough for rustc to pass a response file is left alone.)
    if any(a.startswith("@") for a in args):
        return None
    flat = list(args)
    for i, arg in enumerate(flat):
        for prefix in ("-Wl,--symbol-ordering-file=", "-Wl,-order_file,"):
            if arg.startswith(prefix):
                order_path = arg[len(prefix):]
                break
        else:
            continue
        break
    else:
        return None
    if prefix.startswith("-Wl,--") and not any(a.startswith("-fuse-ld=lld") for a in flat):
        # Only lld takes `--symbol-ordering-file` (GNU ld rejects it).
        return [a for a in flat if a != arg and a != "-Wl,--no-warn-symbol-ordering"]
    output = flat[flat.index("-o") + 1] if "-o" in flat else None
    if output is None or not os.path.isfile(order_path):
        return None
    inputs = [a for a in flat if a.endswith((".o", ".rlib", ".a")) and os.path.isfile(a)]
    names = defined(inputs)
    if not names:
        return None
    ordered = []
    with open(order_path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line and not line.startswith("#"):
                name = names.get(key(line))
                if name is not None:
                    ordered.append(name)
    ordered = list(dict.fromkeys(ordered))
    mapped = output + ".order"
    with open(mapped, "w", encoding="utf-8") as f:
        f.write("".join(name + "\n" for name in ordered))
    flat[i] = prefix + mapped
    return flat


def main():
    args = sys.argv[1:]
    try:
        remapped = remap(args)
    except Exception:  # noqa: BLE001 - linking must not depend on this
        remapped = None
    os.execvp(CC, [CC] + (remapped if remapped is not None else args))


if __name__ == "__main__":
    main()
