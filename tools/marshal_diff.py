#!/usr/bin/env python3
"""Cross-check WeavePy's ``marshal.dumps`` of code objects byte-for-byte
against a CPython oracle.

RFC 0077 WS9 acceptance tool (the RFC 0033 fixture, re-pointed at
3.14). For every source file in a corpus, both interpreters compile the
file with ``compile(src, path, "exec", dont_inherit=True)`` and print
``marshal.dumps(code).hex()``. The two hex streams must be identical:
that covers the ``TYPE_CODE`` field order, ``co_code``, the location
and exception tables, ``co_localsplus*``, constant interning, and the
``FLAG_REF`` / ``TYPE_REF`` sharing structure the compiler's constant
merging produces.

The code object is compiled inline so its reference count is one on
the CPython side (an unflagged root); that is the ``py_compile`` /
``importlib`` idiom's shape up to the root flag byte. The oracle runs
with ``-S -I`` by default: CPython's ``FLAG_REF`` layout depends on which
strings the process has already interned, and WeavePy's writer models a
stock startup (the frozen modules), not whatever ``site`` pulled in.

Usage::

    tools/marshal_diff.py [--oracle python3.14] [--weavepy target/release/weavepy]
                          [--limit N] [--verbose] [--stdlib] [FILE ...]

``--stdlib`` walks the oracle's own ``Lib/`` (via ``sysconfig``). Files
that fail to compile under either interpreter are counted separately.
On a divergence the tool prints the byte offset of the first difference
and, with ``--verbose``, a structural decode of both streams around it.
"""

from __future__ import annotations

import argparse
import os
import struct
import subprocess
import sys
import tempfile

# Runs under the interpreter being measured. ``sys.argv[1:]`` are the
# source paths; one line per path: ``<path>\t<hex or !ERROR ...>``.
DUMPER = r'''
import marshal, sys

for path in sys.argv[1:]:
    with open(path, "rb") as fh:
        src = fh.read()
    try:
        compile(src, path, "exec", dont_inherit=True)
    except Exception as exc:  # noqa: BLE001
        print(path + "\t!COMPILE-ERROR " + type(exc).__name__)
        continue
    try:
        # Compiled inline so the code object's only reference is the
        # argument slot (CPython leaves such a root unflagged).
        print(path + "\t" + marshal.dumps(compile(src, path, "exec", dont_inherit=True)).hex())
    except Exception as exc:  # noqa: BLE001
        print(path + "\t!DUMP-ERROR " + type(exc).__name__ + " " + str(exc)[:120])
'''


def run_dump(interp: str, flags: list[str], dumper_path: str, files: list[str]) -> dict[str, str]:
    env = dict(os.environ)
    env.setdefault("PYTHONHASHSEED", "0")
    env["PYTHONDONTWRITEBYTECODE"] = "1"
    try:
        proc = subprocess.run(
            [interp, *flags, "-X", "utf8", dumper_path, *files],
            capture_output=True,
            text=True,
            timeout=600,
            env=env,
        )
    except subprocess.TimeoutExpired:
        return {f: "!TIMEOUT" for f in files}
    out: dict[str, str] = {}
    for line in proc.stdout.splitlines():
        path, _, value = line.partition("\t")
        out[path] = value
    if proc.returncode != 0:
        tail = proc.stderr.strip().splitlines()[-3:] if proc.stderr else []
        for f in files:
            out.setdefault(f, "!CRASH rc=%d %s" % (proc.returncode, " | ".join(tail)))
    return out


def decode(data: bytes) -> list[tuple[int, str]]:
    """A structural listing of a marshal stream: ``(offset, text)`` per
    value, with ``FLAG_REF`` slots numbered the way the reader does."""
    pos = 0
    refs: list[object] = []
    out: list[tuple[int, str]] = []

    def rb() -> int:
        nonlocal pos
        b = data[pos]
        pos += 1
        return b

    def rl() -> int:
        nonlocal pos
        v = struct.unpack_from("<i", data, pos)[0]
        pos += 4
        return v

    def rd(depth: int) -> object:
        nonlocal pos
        start = pos
        raw = rb()
        flag = raw & 0x80
        t = chr(raw & 0x7F)
        idx = None
        if flag:
            idx = len(refs)
            refs.append(None)
        ind = "  " * depth
        tag = "[%d]" % idx if flag else "   "

        def emit(text: str, value: object) -> object:
            out.append((start, "%s%s %s" % (ind, tag, text)))
            if flag:
                refs[idx] = value
            return value

        if t == "N":
            return emit("None", None)
        if t == "T":
            return emit("True", True)
        if t == "F":
            return emit("False", False)
        if t == ".":
            return emit("Ellipsis", ...)
        if t == "S":
            return emit("StopIteration", StopIteration)
        if t == "i":
            return emit("int %d" % rl(), None)
        if t == "g":
            v = struct.unpack_from("<d", data, pos)[0]
            pos += 8
            return emit("float %r" % v, v)
        if t == "y":
            v = struct.unpack_from("<dd", data, pos)
            pos += 16
            return emit("complex %r" % (v,), v)
        if t == "l":
            n = rl()
            digits = [struct.unpack_from("<H", data, pos + 2 * i)[0] for i in range(abs(n))]
            pos += 2 * abs(n)
            return emit("long n=%d %r" % (n, digits), None)
        if t in "zZ":
            n = rb()
            s = data[pos : pos + n].decode("latin1")
            pos += n
            return emit("str%s %r" % ("(interned)" if t == "Z" else "", s), s)
        if t in "aAut":
            n = rl()
            s = data[pos : pos + n].decode("utf-8", "surrogatepass")
            pos += n
            return emit("str%s %r (%s)" % ("(interned)" if t in "At" else "", s, t), s)
        if t == "s":
            n = rl()
            s = data[pos : pos + n]
            pos += n
            return emit("bytes len=%d %s" % (n, s[:16].hex()), s)
        if t in ")(":
            n = rb() if t == ")" else rl()
            emit("tuple[%d]" % n, None)
            items = tuple(rd(depth + 1) for _ in range(n))
            if flag:
                refs[idx] = items
            return items
        if t in "<>":
            n = rl()
            emit("%s[%d]" % ("frozenset" if t == ">" else "set", n), None)
            for _ in range(n):
                rd(depth + 1)
            return None
        if t == "[":
            n = rl()
            emit("list[%d]" % n, None)
            for _ in range(n):
                rd(depth + 1)
            return None
        if t == "{":
            emit("dict", None)
            while data[pos] != 0x30:
                rd(depth + 1)
                rd(depth + 1)
            pos += 1
            return None
        if t == ":":
            emit("slice", None)
            rd(depth + 1)
            rd(depth + 1)
            rd(depth + 1)
            return None
        if t == "r":
            i = rl()
            target = refs[i] if i < len(refs) else "?"
            out.append((start, "%s    REF %d -> %.40r" % (ind, i, target)))
            return target
        if t == "c":
            emit("code", "<code>")
            hdr = [rl() for _ in range(5)]
            out.append(
                (
                    start + 1,
                    "%s  argcount=%d posonly=%d kwonly=%d stacksize=%d flags=%#x" % (ind, *hdr),
                )
            )
            for name in (
                "co_code",
                "consts",
                "names",
                "localsplusnames",
                "localspluskinds",
                "filename",
                "name",
                "qualname",
            ):
                out.append((pos, "%s  .%s:" % (ind, name)))
                rd(depth + 2)
            out.append((pos, "%s  firstlineno=%d" % (ind, rl())))
            for name in ("linetable", "exceptiontable"):
                out.append((pos, "%s  .%s:" % (ind, name)))
                rd(depth + 2)
            return "<code>"
        raise ValueError("unknown marshal type %r at %d" % (t, start))

    try:
        rd(0)
    except Exception as exc:  # noqa: BLE001
        out.append((pos, "!! decode stopped: %s" % exc))
    return out


def first_difference(a: bytes, b: bytes) -> int:
    n = min(len(a), len(b))
    for i in range(n):
        if a[i] != b[i]:
            return i
    return n


def describe(a_hex: str, b_hex: str, context: int) -> str:
    a, b = bytes.fromhex(a_hex), bytes.fromhex(b_hex)
    off = first_difference(a, b)
    lines = ["   first difference at byte %d (oracle %d bytes, weavepy %d bytes)" % (off, len(a), len(b))]
    for label, data in (("oracle", a), ("weavepy", b)):
        listing = decode(data)
        # The last `context` entries at or before the divergence, then a
        # few past it.
        before = [e for e in listing if e[0] <= off]
        after = [e for e in listing if e[0] > off]
        window = before[-context:] + after[:context]
        lines.append("   %s:" % label)
        for o, text in window:
            marker = ">>" if before and o == before[-1][0] else "  "
            lines.append("   %s %6d  %s" % (marker, o, text))
    return "\n".join(lines)


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
    ap.add_argument(
        "--oracle-flags",
        default="-S -I",
        help="interpreter flags for the oracle (default: a stock startup without site, "
        "the interned-string state WeavePy's writer models)",
    )
    ap.add_argument("--limit", type=int, default=0, help="stop after N files")
    ap.add_argument("--batch", type=int, default=64, help="files per interpreter process")
    ap.add_argument("--verbose", "-v", action="count", default=0)
    ap.add_argument("--context", type=int, default=6)
    ap.add_argument("files", nargs="*")
    args = ap.parse_args()

    files = list(args.files)
    if args.stdlib:
        files += stdlib_corpus(args.oracle)
    if args.limit:
        files = files[: args.limit]
    if not files:
        ap.error("no corpus: pass files or --stdlib")

    with tempfile.NamedTemporaryFile("w", suffix="_marshal_dump.py", delete=False) as fh:
        fh.write(DUMPER)
        dumper = fh.name

    same = diff = compile_err = crash = 0
    try:
        for i in range(0, len(files), args.batch):
            batch = files[i : i + args.batch]
            a = run_dump(args.oracle, args.oracle_flags.split(), dumper, batch)
            b = run_dump(args.weavepy, [], dumper, batch)
            for path in batch:
                x, y = a.get(path, "!MISSING"), b.get(path, "!MISSING")
                if x.startswith("!") or y.startswith("!"):
                    if x.startswith("!COMPILE") and y.startswith("!COMPILE"):
                        compile_err += 1
                        continue
                    crash += 1
                    print("XX %s\n   oracle: %s\n   weavepy: %s" % (path, x[:200], y[:200]))
                    continue
                if x == y:
                    same += 1
                    continue
                diff += 1
                print("-- %s" % path)
                if args.verbose:
                    print(describe(x, y, args.context))
                else:
                    print("   first difference at byte %d (oracle %d bytes, weavepy %d bytes)"
                          % (first_difference(bytes.fromhex(x), bytes.fromhex(y)), len(x) // 2, len(y) // 2))
    finally:
        os.unlink(dumper)

    total = same + diff
    print()
    print(
        "files: %d identical, %d divergent, %d compile-error (both), %d crash/timeout"
        % (same, diff, compile_err, crash)
    )
    if total:
        print("marshal streams: %.1f%% byte-identical" % (100.0 * same / total))
    return 0 if diff == 0 and crash == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
