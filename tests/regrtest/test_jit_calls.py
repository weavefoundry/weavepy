"""RFC 0059 WS3a — Python-to-Python calls under the tier-2 JIT.

The workload below is run twice in subprocesses: once interpreted and
once with `WEAVEPY_JIT=1` at a low tier-up threshold. The outputs must
be byte-identical. On builds without the `jit` feature (or under
CPython) the env var is inert and both legs interpret — the test then
degrades to a plain semantics check, which is the intended auto-skip.
"""

import os
import subprocess
import sys

if len(sys.argv) > 1 and sys.argv[1] == "child":
    # ---- workload (runs in both legs) ----

    # Self-recursion through a burned-in global (return-lane fixpoint).
    def fib(n):
        if n < 2:
            return n
        return fib(n - 1) + fib(n - 2)

    r = 0
    k = 0
    while k < 30:
        r = fib(14)
        k = k + 1
    print("fib", r)

    # Non-self callee with an inferable scalar return lane.
    def half(x):
        return x // 2

    def driver(n):
        t = 0
        i = 0
        while i < n:
            t = t + half(i)
            i = i + 1
        return t

    k = 0
    while k < 40:
        r = driver(50)
        k = k + 1
    print("driver", r)

    # Overflow inside a native call: the callee's int result leaves
    # the machine lane mid-loop; exactness must be preserved.
    def grow(x):
        return x * 1000000000000000000

    def big_driver(n):
        t = 0
        i = 0
        while i < n:
            t = t + grow(i)
            i = i + 1
        return t

    k = 0
    while k < 40:
        r = big_driver(20)
        k = k + 1
    print("overflow", r)

    # Exception propagation out of a native call, caught by the
    # (interpreted) outer frame.
    def boom(x):
        return 10 // x

    def raising_driver(n):
        t = 0
        i = 0 - 1
        while i < n:
            t = t + boom(i)
            i = i + 1
        return t

    caught = 0
    k = 0
    while k < 40:
        try:
            raising_driver(5)
        except ZeroDivisionError:
            caught = caught + 1
        k = k + 1
    print("raised", caught)

    # Rebinding the burned-in callee between calls: the entry guards
    # must notice both a new *object* and a new *__code__* on the same
    # object.
    def double(x):
        return x * 2

    def triple(x):
        return x * 3

    helper = double

    def rebind_driver(n):
        t = 0
        i = 0
        while i < n:
            t = t + helper(i)
            i = i + 1
        return t

    k = 0
    while k < 40:
        r = rebind_driver(40)
        k = k + 1
    print("pre-rebind", r)
    helper = triple
    print("rebound-global", rebind_driver(40))
    helper.__code__ = double.__code__
    print("rebound-code", rebind_driver(40))

    # sys.settrace attach while hot: tracing must observe every call
    # (the observer gate forces interpretation regardless of tier).
    calls = []

    def tracer(frame, event, arg):
        if event == "call" and frame.f_code.co_name == "half":
            calls.append(1)
        return None

    sys.settrace(tracer)
    r = driver(30)
    sys.settrace(None)
    print("traced", r, len(calls))

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
assert interp == jitted, f"JIT diverged from interpreter:\n--- interp\n{interp}--- jit\n{jitted}"
print("ok")
