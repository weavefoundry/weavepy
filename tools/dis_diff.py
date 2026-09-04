#!/usr/bin/env python3
"""Cross-check WeavePy's bytecode presentation against a CPython oracle.

RFC 0077 WS9 acceptance tool. For every source file in a corpus, both
interpreters compile the file and dump every code object it contains
(recursively) in a normalized text form: the ``dis`` listing with
offsets, the exception table, ``co_consts`` / ``co_names`` /
``co_varnames`` / ``co_stacksize`` / ``co_flags``, and the ``co_lines``
table. The tool diffs the two dumps per code object and reports the
first divergence, plus a corpus-wide tally of identical versus
divergent code objects.

Usage::

    tools/dis_diff.py [--oracle python3.14] [--weavepy target/release/weavepy]
                      [--limit N] [--verbose] [--stdlib] [FILE ...]

``--stdlib`` walks the oracle's own ``Lib/`` (via ``sysconfig``), which
is the largest ready corpus of idiomatic Python. Files that fail to
compile under either interpreter are counted separately, not diffed.

The dumper below runs unchanged under both interpreters, so anything
it prints that differs is a real presentation difference (or a genuine
compiler-shape difference), never a tool artifact.
"""

from __future__ import annotations

import argparse
import difflib
import os
import re
import subprocess
import sys
import tempfile

# Runs under the interpreter being measured. Kept as a string so both
# sides execute byte-identical code; ``sys.argv[1]`` is the source path
# and the dump goes to stdout.
DUMPER = r'''
import dis, io, sys, types, re

path = sys.argv[1]
with open(path, "rb") as fh:
    src = fh.read()
try:
    code = compile(src, path, "exec", dont_inherit=True)
except Exception as exc:  # noqa: BLE001
    print("!COMPILE-ERROR", type(exc).__name__, str(exc)[:200])
    sys.exit(0)

ADDR = re.compile(r" at 0x[0-9a-fA-F]+")
FROZENSET = re.compile(r"frozenset\(\{([^{}]*)\}\)")

def sort_frozensets(text):
    # Set iteration order follows str hashes, which differ between the
    # two runtimes (and between CPython runs without PYTHONHASHSEED):
    # normalize to a sorted element list so only membership counts.
    def fix(m):
        items = sorted(m.group(1).split(", "))
        return "frozenset({%s})" % ", ".join(items)
    return FROZENSET.sub(fix, text)

def const_repr(c):
    if isinstance(c, types.CodeType):
        return "<code %s>" % c.co_qualname
    if isinstance(c, (tuple, frozenset)):
        kind = "tuple" if isinstance(c, tuple) else "frozenset"
        items = c if isinstance(c, tuple) else sorted(c, key=repr)
        return "%s(%s)" % (kind, ", ".join(const_repr(x) for x in items))
    if isinstance(c, float):
        return repr(c)
    if isinstance(c, complex):
        return repr(c)
    return type(c).__name__ + ":" + repr(c)

def dump(co):
    out = io.StringIO()
    print("@@", co.co_qualname, file=out)
    print("flags", hex(co.co_flags), "stacksize", co.co_stacksize,
          "argcount", co.co_argcount, co.co_posonlyargcount, co.co_kwonlyargcount,
          "nlocals", co.co_nlocals, "firstlineno", co.co_firstlineno, file=out)
    print("consts", [const_repr(c) for c in co.co_consts], file=out)
    print("names", list(co.co_names), file=out)
    print("varnames", list(co.co_varnames), "cellvars", list(co.co_cellvars),
          "freevars", list(co.co_freevars), file=out)
    buf = io.StringIO()
    dis.dis(co, file=buf, show_caches=False, show_offsets=True, depth=0)
    text = sort_frozensets(ADDR.sub("", buf.getvalue()))
    out.write(text)
    if not text.endswith("\n"):
        out.write("\n")
    print("lines", list(co.co_lines()), file=out)
    print("positions", [tuple(p) for p in co.co_positions()], file=out)
    sys.stdout.write(out.getvalue())
    for c in co.co_consts:
        if isinstance(c, types.CodeType):
            dump(c)

dump(code)
'''


def run_dump(interp: str, dumper_path: str, src: str) -> str:
    env = dict(os.environ)
    env.setdefault("PYTHONHASHSEED", "0")
    env["PYTHONDONTWRITEBYTECODE"] = "1"
    try:
        proc = subprocess.run(
            [interp, "-X", "utf8", dumper_path, src],
            capture_output=True,
            text=True,
            timeout=120,
            env=env,
        )
    except subprocess.TimeoutExpired:
        return "!TIMEOUT\n"
    if proc.returncode != 0:
        tail = proc.stderr.strip().splitlines()[-3:] if proc.stderr else []
        return "!CRASH rc=%d %s\n" % (proc.returncode, " | ".join(tail))
    return proc.stdout


def split_objects(dump: str) -> dict[str, str]:
    objs: dict[str, str] = {}
    cur = None
    for line in dump.splitlines(keepends=True):
        if line.startswith("@@ "):
            cur = line[3:].strip()
            n = 2
            base = cur
            while cur in objs:
                cur = "%s#%d" % (base, n)
                n += 1
            objs[cur] = ""
        if cur is not None:
            objs[cur] += line
    return objs


INSTR_LINE = re.compile(r"^\s*(?:\d+\s+)?(?:-->\s+|>>\s+)?(?:L\d+:\s+)?(\d+)\s+([A-Z_]+)(?:\s+(.*))?$")


def instr_seq(text: str) -> list[tuple[str, str]]:
    """The ``(opname, oparg-repr)`` sequence of an object dump, offsets
    and labels stripped so a shape comparison ignores byte layout."""
    out = []
    for line in text.splitlines():
        m = INSTR_LINE.match(line)
        if not m:
            continue
        arg = (m.group(3) or "").strip()
        # Jump targets / labels differ whenever layout differs; keep
        # only the shape-relevant part of an argument.
        arg = re.sub(r"\s*\(to L\d+\)", "", arg)
        arg = re.sub(r"<code object .*", "<code>", arg)
        out.append((m.group(2), arg))
    return out


def first_shape_divergence(a: str, b: str) -> str | None:
    """``"ORACLE_OP arg | WEAVEPY_OP arg"`` at the first instruction
    where the two listings' shapes part, or ``None`` if the instruction
    streams agree (only metadata differs)."""
    sa, sb = instr_seq(a), instr_seq(b)
    for i in range(max(len(sa), len(sb))):
        x = sa[i] if i < len(sa) else ("<end>", "")
        y = sb[i] if i < len(sb) else ("<end>", "")
        if x[0] != y[0]:
            return "%s | %s" % (x[0], y[0])
        if x[1] != y[1]:
            return "%s %s | %s %s" % (x[0], x[1][:24], y[0], y[1][:24])
    return None


def positions_divergence(a: str, b: str) -> str | None:
    """``"<pos:OP oracle-pos | weavepy-pos>"`` for the first instruction
    whose PEP 657 position differs (the instruction streams agree)."""
    pa = next((l for l in a.splitlines() if l.startswith("positions ")), None)
    pb = next((l for l in b.splitlines() if l.startswith("positions ")), None)
    if pa is None or pb is None:
        return None
    try:
        la = eval(pa[len("positions "):], {}, {})
        lb = eval(pb[len("positions "):], {}, {})
    except Exception:
        return None
    # co_positions() has one entry per code unit (caches included), so
    # map back through the listing's offsets rather than its ordinal.
    by_unit: dict[int, str] = {}
    for line in a.splitlines():
        m = INSTR_LINE.match(line)
        if m:
            by_unit[int(m.group(1)) // 2] = m.group(2)
    for i, (x, y) in enumerate(zip(la, lb)):
        if x != y:
            op = by_unit.get(i)
            while op is None and i > 0:
                i -= 1
                op = by_unit.get(i)
            return "<pos:%s %s | %s>" % (op or "?", x, y)
    return None


def stdlib_corpus(oracle: str) -> list[str]:
    out = subprocess.run(
        [oracle, "-c", "import sysconfig; print(sysconfig.get_paths()['stdlib'])"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    files = []
    for root, dirs, names in os.walk(out):
        dirs[:] = sorted(
            d
            for d in dirs
            if d not in ("test", "tests", "site-packages", "__pycache__", "lib2to3", "idlelib", "turtledemo")
        )
        for n in sorted(names):
            if n.endswith(".py"):
                files.append(os.path.join(root, n))
    return files


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--oracle", default="python3.14")
    ap.add_argument("--weavepy", default=os.path.join("target", "release", "weavepy"))
    ap.add_argument("--stdlib", action="store_true", help="use the oracle's Lib/ as the corpus")
    ap.add_argument("--limit", type=int, default=0, help="stop after N files")
    ap.add_argument("--verbose", "-v", action="count", default=0)
    ap.add_argument("--context", type=int, default=3)
    ap.add_argument("--only", help="restrict the verbose diff to code objects whose qualname contains this")
    ap.add_argument("files", nargs="*")
    args = ap.parse_args()

    files = list(args.files)
    if args.stdlib:
        files += stdlib_corpus(args.oracle)
    if args.limit:
        files = files[: args.limit]
    if not files:
        ap.error("no corpus: pass files or --stdlib")

    with tempfile.NamedTemporaryFile("w", suffix="_dis_dump.py", delete=False) as fh:
        fh.write(DUMPER)
        dumper = fh.name

    same = diff = 0
    files_clean = files_dirty = 0
    compile_err = crash = 0
    first_diff_lines: dict[str, int] = {}
    shape_tally: dict[str, list[str]] = {}
    try:
        for path in files:
            a = run_dump(args.oracle, dumper, path)
            b = run_dump(args.weavepy, dumper, path)
            if a.startswith("!") or b.startswith("!"):
                if a.startswith("!COMPILE") and b.startswith("!COMPILE"):
                    compile_err += 1
                    continue
                crash += 1
                print("XX %s\n   oracle: %s\n   weavepy: %s" % (path, a.strip()[:200], b.strip()[:200]))
                continue
            oa, ob = split_objects(a), split_objects(b)
            file_dirty = False
            for name, text in oa.items():
                other = ob.get(name)
                if other == text:
                    same += 1
                    continue
                diff += 1
                file_dirty = True
                if other is None:
                    print("-- %s :: %s (missing in weavepy)" % (path, name))
                    continue
                dl = list(
                    difflib.unified_diff(
                        text.splitlines(),
                        other.splitlines(),
                        "oracle",
                        "weavepy",
                        n=args.context,
                        lineterm="",
                    )
                )
                # The first real divergence line drives the tally.
                key = next((l[1:].strip() for l in dl[2:] if l.startswith("-")), "?")
                key = key.split(None, 3)[-1] if key[:1].isdigit() else key
                first_diff_lines[key[:60]] = first_diff_lines.get(key[:60], 0) + 1
                shape = first_shape_divergence(text, other)
                if shape is None:
                    # Same instruction stream: metadata only (table,
                    # consts order, lines, positions, stacksize...).
                    meta = next(
                        (l[1:].split(None, 1)[0] for l in dl[2:] if l.startswith("-") and l[1:].split(None, 1)),
                        "?",
                    )
                    shape = "<meta:%s>" % meta
                    if meta == "positions":
                        shape = positions_divergence(text, other) or shape
                shape_tally.setdefault(shape, []).append("%s::%s" % (os.path.basename(path), name))
                if args.verbose and (not args.only or args.only in name):
                    print("-- %s :: %s" % (path, name))
                    for l in dl[2:]:
                        print("   " + l)
            for name in ob:
                if name not in oa:
                    diff += 1
                    file_dirty = True
                    print("++ %s :: %s (extra in weavepy)" % (path, name))
            if file_dirty:
                files_dirty += 1
                if not args.verbose:
                    print("-- %s" % path)
            else:
                files_clean += 1
    finally:
        os.unlink(dumper)

    total = same + diff
    print()
    print(
        "files: %d clean, %d divergent, %d compile-error (both), %d crash/timeout"
        % (files_clean, files_dirty, compile_err, crash)
    )
    if total:
        print("code objects: %d identical, %d divergent (%.1f%% identical)" % (same, diff, 100.0 * same / total))
    if first_diff_lines:
        print("\nmost common first divergence (oracle side):")
        for k, v in sorted(first_diff_lines.items(), key=lambda kv: -kv[1])[:25]:
            print("  %5d  %s" % (v, k))
    if shape_tally:
        print("\nmost common first shape divergence (oracle | weavepy), with an example:")
        for k, v in sorted(shape_tally.items(), key=lambda kv: -len(kv[1]))[:40]:
            print("  %5d  %-56s %s" % (len(v), k, v[0]))
    return 0 if diff == 0 and crash == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
