"""RFC 0059 WS3b — on-stack replacement at loop back-edges.

Same two-leg subprocess scheme as test_jit_calls: the workloads are
single long-running activations (called once), so only OSR can tier
them up. Outputs must match the interpreted leg exactly; on non-jit
builds both legs interpret and the test degrades to a semantics check.
"""

import os
import subprocess
import sys

if len(sys.argv) > 1 and sys.argv[1] == "child":
    # ---- workload (runs in both legs) ----

    # While-loop kernel, one hot activation.
    def while_kernel(n):
        s = 0
        i = 0
        while i < n:
            s = s + i * 3 - (i % 5)
            i = i + 1
        return s

    print("while", while_kernel(120000))

    # Rewritten range loop: OSR must decompose the live iterator into
    # the synthetic counter slots mid-iteration.
    def range_kernel(n):
        total = 0
        for i in range(n):
            total = total + i * 2
        return total

    print("range", range_kernel(120000))

    # Deopt after OSR entry: the accumulator overflows to a big int
    # mid-loop, forcing a bail back to the interpreter with exact
    # state.
    def overflow_kernel(n):
        s = 1
        i = 0
        while i < n:
            s = s + s // 2 + 1
            i = i + 1
        return s % 100003

    print("overflow", overflow_kernel(300))

    # Exception thrown mid-loop after the frame is hot: unwinding out
    # of (possibly native) loop code must preserve try/except flow.
    def raising_kernel(n):
        s = 0
        i = 0
        while i < n:
            s = s + 10 // (n - 1 - i if i < n - 1 else 0)
            i = i + 1
        return s

    try:
        raising_kernel(50000)
        print("raise", "missed")
    except ZeroDivisionError:
        print("raise", "caught")

    # A loop with a break and a live range iterator on the stack.
    def break_kernel(n):
        acc = 0
        for i in range(n):
            if i == n - 7:
                break
            acc = acc + i
        return acc

    print("break", break_kernel(90000))

    sys.exit(0)


def run_leg(jit):
    env = dict(os.environ)
    env.pop("WEAVEPY_JIT", None)
    env.pop("WEAVEPY_JIT_THRESHOLD", None)
    if jit:
        env["WEAVEPY_JIT"] = "1"
        env["WEAVEPY_JIT_THRESHOLD"] = "3"
    out = subprocess.run(
        [sys.executable, os.path.abspath(__file__), "child"],
        env=env,
        capture_output=True,
        text=True,
        timeout=25,
    )
    assert out.returncode == 0, f"jit={jit} leg failed:\n{out.stderr}"
    return out.stdout


interp = run_leg(False)
jitted = run_leg(True)
assert interp.strip(), "workload produced no output"
assert interp == jitted, f"OSR leg diverged from interpreter:\n--- interp\n{interp}--- jit\n{jitted}"
print("ok")
