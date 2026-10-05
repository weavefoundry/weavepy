"""Profile one suite benchmark with macOS `sample` and print the hottest
functions by self and by inclusive time.

    python3 tools/pybench/profile.py BIN NAME [--secs 5] [--top 40]
        [--reps 1000] [--work N] [--focus SUBSTR]

The benchmark runs warm (``REPS`` calls after the first) while `sample`
records the main thread's call graph. ``--focus`` restricts the report to
stacks that pass through a function whose name contains SUBSTR.
"""

import argparse
import collections
import os
import re
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))

LINE = re.compile(r"^(?P<indent>[\s+!:|]*)(?P<count>\d+)\s+(?P<sym>.+?)(?:\s+\(in [^)]*\))?(?:\s+\+\s+\d+)?(?:\s+\[[^\]]*\])?(?:\s+[\w./-]+:\d+)?\s*$")


def simplify(sym):
    sym = re.sub(r"::h[0-9a-f]{16}", "", sym)
    sym = re.sub(r"\s+\(in .*$", "", sym)
    return sym.strip()


def parse(text):
    """Return (depth, count, symbol) nodes for the busiest thread that isn't
    just waiting (the CLI runs the interpreter on a spawned thread)."""
    start = text.find("Call graph:")
    end = text.find("Total number in stack", start)
    threads = []
    for raw in text[start:end].splitlines()[1:]:
        m = LINE.match(raw)
        if not m:
            continue
        depth = len(m.group("indent"))
        sym = simplify(m.group("sym"))
        if sym.startswith("Thread_") or "DispatchQueue" in sym:
            threads.append([int(m.group("count")), []])
            continue
        if threads:
            threads[-1][1].append((depth, int(m.group("count")), sym))

    waits = ("__ulock_wait", "__psynch_cvwait", "__semwait_signal", "mach_msg2_trap",
             "mach_msg_trap", "kevent", "__select", "__psynch_mutexwait")

    def busy(t):
        # Samples whose leaf isn't a wait: the next node is no deeper.
        nodes = t[1]
        active = 0
        for i, (depth, count, sym) in enumerate(nodes):
            leaf = i + 1 == len(nodes) or nodes[i + 1][0] <= depth
            if leaf and sym not in waits:
                active += count
        return active

    return max(threads, key=busy)[1] if threads else []


def report(nodes, top, focus):
    self_counts = collections.Counter()
    incl = collections.Counter()
    total = 0
    stack = []  # [depth, sym, count, children]

    def finish(entry, names):
        nonlocal total
        own = entry[2] - entry[3]
        if own <= 0 or (focus and not any(focus in s for s in names)):
            return
        self_counts[entry[1]] += own
        total += own
        for s in set(names):
            incl[s] += own

    for depth, count, sym in nodes + [(-1, 0, "")]:
        while stack and stack[-1][0] >= depth:
            names = [e[1] for e in stack]
            finish(stack.pop(), names)
        if depth < 0:
            break
        if stack:
            stack[-1][3] += count
        stack.append([depth, sym, count, 0])
    print("samples: %d" % total)
    print("\n%-8s %-6s  self" % ("count", "share"))
    for sym, n in self_counts.most_common(top):
        print("%-8d %5.1f%%  %s" % (n, 100.0 * n / max(total, 1), sym[:150]))
    print("\n%-8s %-6s  inclusive" % ("count", "share"))
    for sym, n in incl.most_common(top):
        print("%-8d %5.1f%%  %s" % (n, 100.0 * n / max(total, 1), sym[:150]))


def demangle(text):
    """Demangle Rust symbols with the filter named by `RUST_DEMANGLER` (any
    `rustfilt`-compatible command), when one is set."""
    tool = os.environ.get("RUST_DEMANGLER")
    if not tool:
        return text
    return subprocess.run([tool], input=text, capture_output=True, text=True).stdout


def callers(nodes, target, top):
    """Self samples of functions matching `target`, by immediate caller."""
    by_caller = collections.Counter()
    total = 0
    stack = []  # [depth, sym, count, children]

    def finish(entry, parent):
        nonlocal total
        own = entry[2] - entry[3]
        if own > 0 and target in entry[1]:
            by_caller[parent] += own
            total += own

    for depth, count, sym in nodes + [(-1, 0, "")]:
        while stack and stack[-1][0] >= depth:
            entry = stack.pop()
            finish(entry, stack[-1][1] if stack else "<root>")
        if depth < 0:
            break
        if stack:
            stack[-1][3] += count
        stack.append([depth, sym, count, 0])
    print("self samples in %r: %d" % (target, total))
    for sym, n in by_caller.most_common(top):
        print("%-8d %5.1f%%  %s" % (n, 100.0 * n / max(total, 1), sym[:150]))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("bin")
    ap.add_argument("name")
    ap.add_argument("--secs", type=int, default=5)
    ap.add_argument("--top", type=int, default=40)
    ap.add_argument("--reps", type=int, default=100000)
    ap.add_argument("--work", type=int)
    ap.add_argument("--focus")
    ap.add_argument("--save")
    ap.add_argument("--callers", help="attribute this function's self time to its callers")
    args = ap.parse_args()
    bench = os.path.join(HERE, "benchmarks", args.name + ".py")
    work = args.work
    if work is None:
        m = re.search(r"^WORK\s*=\s*(\d+)", open(bench).read(), re.M)
        work = int(m.group(1))
    proc = subprocess.Popen([args.bin, os.path.join(HERE, "harness.py"), args.name, str(work), str(args.reps)],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1.0)
    out = args.save or tempfile.mktemp(suffix=".sample.txt")
    subprocess.run(["sample", str(proc.pid), str(args.secs), "1", "-file", out],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    proc.kill()
    nodes = parse(demangle(open(out).read()))
    if args.callers:
        callers(nodes, args.callers, args.top)
    else:
        report(nodes, args.top, args.focus)


if __name__ == "__main__":
    sys.exit(main())
