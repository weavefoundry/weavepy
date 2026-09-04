#!/usr/bin/env python3
"""Classify and re-vendor the bundled stdlib against CPython `Lib/` trees.

RFC 0077 (WS8). The bundled stdlib is the `FrozenSource` table in
`crates/weavepy-vm/src/stdlib/mod.rs`: 580-odd `include_str!` entries
over `crates/weavepy-vm/src/stdlib/python/`, some under renamed files
(`random_mod.py` is `random`, `os_source.py` is `os`, ...). This tool
reads that table so the module-name -> bundled-file mapping is never
maintained twice, then grades every bundled file against a CPython
`Lib/` tree:

  verbatim  byte-identical to the upstream file
  patched   upstream file exists, bundled copy differs (WeavePy patch)
  authored  no upstream counterpart (WeavePy shim or third-party facade)

Modes:

  --from TREE                 classify against TREE and print the census
  --from OLD --to NEW         re-vendor: verbatim files are replaced with
                              NEW's copy; patched files are 3-way merged
                              (base OLD, ours bundled, theirs NEW) with
                              `git merge-file`; conflicts are written to
                              `<file>.rej` (the bundled file is left
                              untouched); authored files are listed
  --check --from TREE         CI gate: every file that is verbatim against
                              TREE must stay byte-identical (drift is a
                              failure, not archaeology)
  --report-new --to NEW       list NEW modules absent from the table and
                              emit `FrozenSource` stanzas for them
  --report-gone --from OLD --to NEW
                              list table modules present in OLD but absent
                              from NEW (candidates for retirement)

`--only PREFIX` restricts any mode to module names starting with PREFIX
(`--only test.support`, `--only unittest`, `--only asyncio`).

The tool never edits `mod.rs`; it prints the stanzas to paste so the
table stays reviewed by hand.
"""

from __future__ import annotations

import argparse
import filecmp
import os
import re
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
MOD_RS = REPO / "crates" / "weavepy-vm" / "src" / "stdlib" / "mod.rs"
BUNDLED = REPO / "crates" / "weavepy-vm" / "src" / "stdlib" / "python"

STANZA = re.compile(
    r'FrozenSource\s*\{\s*'
    r'name:\s*"(?P<name>[^"]+)"\s*,\s*'
    r'source:\s*(?P<source>include_str!\("(?P<path>[^"]+)"\)|"(?P<inline>(?:[^"\\]|\\.)*)")\s*,\s*'
    r'is_package:\s*(?P<pkg>true|false)\s*,?\s*\}',
    re.S,
)

# Directories under Lib/ that are not importable stdlib modules.
SKIP_TOP = {
    "__pycache__",
    "site-packages",
    "idlelib",
    "turtledemo",
    "lib2to3",
    "test",  # the regrtest tree: vendored separately by weavepy-conformance
    "tkinter",
    "ensurepip",
    "venv",  # WeavePy ships its own venv package (kept in the table)
}
# Files under Lib/ that are not modules.
SKIP_FILES = {"EXTERNALLY-MANAGED", "LICENSE.txt"}


@dataclass(frozen=True)
class Entry:
    name: str
    bundled: Path | None  # None for inline-source entries
    is_package: bool

    @property
    def upstream_rel(self) -> Path:
        parts = self.name.split(".")
        if self.is_package:
            return Path(*parts, "__init__.py")
        return Path(*parts[:-1], parts[-1] + ".py")


def read_table() -> list[Entry]:
    text = MOD_RS.read_text(encoding="utf-8")
    entries: list[Entry] = []
    for m in STANZA.finditer(text):
        path = m.group("path")
        bundled = (MOD_RS.parent / path).resolve() if path else None
        entries.append(Entry(m.group("name"), bundled, m.group("pkg") == "true"))
    if not entries:
        sys.exit(f"stdlib_sync: no FrozenSource stanzas parsed from {MOD_RS}")
    return entries


def classify(entry: Entry, tree: Path) -> tuple[str, Path | None]:
    if entry.bundled is None or not entry.bundled.exists():
        return "inline", None
    upstream = tree / entry.upstream_rel
    if not upstream.exists():
        # `test.support` and `unittest` live in the tree at their real
        # paths, so this is only ever an authored file.
        return "authored", None
    same = filecmp.cmp(entry.bundled, upstream, shallow=False)
    return ("verbatim" if same else "patched"), upstream


def iter_tree_modules(tree: Path) -> dict[str, tuple[Path, bool]]:
    """Importable module name -> (path, is_package) for a `Lib/` tree."""
    out: dict[str, tuple[Path, bool]] = {}
    for root, dirs, files in os.walk(tree):
        rel = Path(root).relative_to(tree)
        top = rel.parts[0] if rel.parts else None
        if top in SKIP_TOP:
            dirs[:] = []
            continue
        dirs[:] = [d for d in dirs if d != "__pycache__" and not (rel == Path() and d in SKIP_TOP)]
        if rel.parts:
            init = Path(root) / "__init__.py"
            if not init.exists():
                dirs[:] = []
                continue
            out[".".join(rel.parts)] = (init, True)
        for f in files:
            if not f.endswith(".py") or f == "__init__.py" or f in SKIP_FILES:
                continue
            name = ".".join((*rel.parts, f[:-3]))
            out[name] = (Path(root) / f, False)
    return out


def merge3(ours: Path, base: Path, theirs: Path) -> tuple[bool, bytes]:
    """`git merge-file` with ours/base/theirs; returns (clean, merged bytes)."""
    with tempfile.TemporaryDirectory() as td:
        work = Path(td) / "ours.py"
        shutil.copyfile(ours, work)
        proc = subprocess.run(
            ["git", "merge-file", "-L", "weavepy", "-L", "base", "-L", "upstream",
             str(work), str(base), str(theirs)],
            capture_output=True,
        )
        merged = work.read_bytes()
    # `git merge-file` exits with the number of conflicts (>0), or <0 on error.
    if proc.returncode < 0:
        sys.exit(f"stdlib_sync: git merge-file failed for {ours}: {proc.stderr.decode()}")
    return proc.returncode == 0, merged


def stanza(name: str, rel: Path, is_package: bool) -> str:
    return (
        "        FrozenSource {\n"
        f'            name: "{name}",\n'
        f'            source: include_str!("python/{rel.as_posix()}"),\n'
        f"            is_package: {'true' if is_package else 'false'},\n"
        "        },"
    )


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--from", dest="from_tree", type=Path, help="CPython Lib/ the bundle currently tracks")
    ap.add_argument("--to", dest="to_tree", type=Path, help="CPython Lib/ to re-vendor onto")
    ap.add_argument("--check", action="store_true", help="fail if any verbatim file drifted from --from")
    ap.add_argument("--report-new", action="store_true", help="list --to modules missing from the table")
    ap.add_argument("--report-gone", action="store_true", help="list table modules absent from --to")
    ap.add_argument("--only", default="", help="restrict to module names with this prefix")
    ap.add_argument("--write", action="store_true", help="with --to: actually write files (default is a dry run)")
    ap.add_argument("-v", "--verbose", action="store_true")
    args = ap.parse_args()

    entries = [e for e in read_table() if e.name.startswith(args.only)]
    if not args.from_tree and not (args.report_new and args.to_tree):
        ap.error("--from TREE is required (except for --report-new --to TREE)")

    if args.report_new:
        if not args.to_tree:
            ap.error("--report-new needs --to")
        table_names = {e.name for e in entries}
        new = iter_tree_modules(args.to_tree)
        missing = sorted(n for n in new if n not in table_names and n.startswith(args.only))
        print(f"# {len(missing)} modules in {args.to_tree} absent from the frozen table")
        for n in missing:
            path, pkg = new[n]
            rel = path.relative_to(args.to_tree)
            print(stanza(n, rel, pkg))
        return 0

    census = {"verbatim": [], "patched": [], "authored": [], "inline": []}
    upstream_of: dict[str, Path] = {}
    for e in entries:
        kind, up = classify(e, args.from_tree)
        census[kind].append(e)
        if up is not None:
            upstream_of[e.name] = up

    if args.report_gone:
        if not args.to_tree:
            ap.error("--report-gone needs --to")
        gone = [e for e in entries if e.name in upstream_of and not (args.to_tree / e.upstream_rel).exists()]
        print(f"# {len(gone)} table modules present in {args.from_tree} but absent from {args.to_tree}")
        for e in gone:
            print(f"{e.name}\t{e.bundled.relative_to(REPO) if e.bundled else '<inline>'}")
        return 0

    if args.check:
        # Drift is impossible by construction (verbatim is *defined* by
        # byte-equality), so the gate is the stronger claim: every file
        # the previous census recorded as verbatim must still be. The
        # recorded set lives next to this tool.
        recorded = REPO / "tools" / "data" / "stdlib_verbatim.txt"
        if not recorded.exists():
            print(f"stdlib_sync: no recorded census at {recorded}; run with --from and --record first")
            return 2
        want = {ln.strip() for ln in recorded.read_text().splitlines() if ln.strip() and not ln.startswith("#")}
        have = {e.name for e in census["verbatim"]}
        drifted = sorted(want - have)
        for n in drifted:
            print(f"DRIFT {n}: recorded verbatim, now differs from {args.from_tree}")
        print(f"stdlib_sync --check: {len(want)} recorded verbatim, {len(drifted)} drifted")
        return 1 if drifted else 0

    if not args.to_tree:
        print(f"# bundled stdlib vs {args.from_tree}")
        for kind in ("verbatim", "patched", "authored", "inline"):
            print(f"{kind:9} {len(census[kind])}")
        if args.verbose:
            for kind in ("patched", "authored"):
                print(f"\n## {kind}")
                for e in sorted(census[kind], key=lambda e: e.name):
                    print(f"{e.name}\t{e.bundled.relative_to(REPO) if e.bundled else '<inline>'}")
        record = REPO / "tools" / "data" / "stdlib_verbatim.txt"
        if args.write:
            record.parent.mkdir(parents=True, exist_ok=True)
            names = sorted(e.name for e in census["verbatim"])
            record.write_text(
                f"# Bundled stdlib modules byte-identical to {args.from_tree.name} (tools/stdlib_sync.py --write).\n"
                + "\n".join(names) + "\n"
            )
            print(f"\nrecorded {len(names)} verbatim names to {record.relative_to(REPO)}")
        return 0

    # Re-vendor.
    to = args.to_tree
    flipped = merged = conflicted = removed = 0
    for e in census["verbatim"]:
        target = to / e.upstream_rel
        if not target.exists():
            removed += 1
            print(f"GONE     {e.name}  (verbatim in --from, absent in --to; retire or keep the old copy)")
            continue
        if filecmp.cmp(e.bundled, target, shallow=False):
            continue
        flipped += 1
        if args.verbose:
            print(f"FLIP     {e.name}")
        if args.write:
            shutil.copyfile(target, e.bundled)
    for e in census["patched"]:
        target = to / e.upstream_rel
        if not target.exists():
            removed += 1
            print(f"GONE     {e.name}  (patched; absent in --to)")
            continue
        base = upstream_of[e.name]
        if filecmp.cmp(base, target, shallow=False):
            continue  # upstream unchanged; the WeavePy patch stands as is
        clean, out = merge3(e.bundled, base, target)
        if clean:
            merged += 1
            print(f"MERGED   {e.name}")
            if args.write:
                e.bundled.write_bytes(out)
        else:
            conflicted += 1
            rej = e.bundled.with_suffix(e.bundled.suffix + ".rej")
            print(f"CONFLICT {e.name}  -> {rej.relative_to(REPO)}")
            if args.write:
                rej.write_bytes(out)
    print(
        f"\n{'wrote' if args.write else 'would write'}: {flipped} verbatim flipped, "
        f"{merged} patched merged cleanly, {conflicted} conflicts, {removed} gone; "
        f"{len(census['authored'])} authored files untouched"
    )
    if args.verbose:
        print("\n## authored (audit against the --to API deltas by hand)")
        for e in sorted(census["authored"], key=lambda e: e.name):
            print(e.name)
    return 0


if __name__ == "__main__":
    sys.exit(main())
