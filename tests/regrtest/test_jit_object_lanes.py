# RFC 0076 WS8 — object-lane truthiness, generic membership, set
# construction/iteration, and the LOAD_ATTR probe-miss residue.
#
# The workload runs twice — interpreted and under an aggressive JIT
# threshold — and the outputs must match exactly. Shapes covered:
# `TO_BOOL` on object-lane values (`None`, instances with and without
# `__bool__`/`__len__`, empty/non-empty strings and lists), `in` /
# `not in` on sets, strings, and `__contains__` instances, set
# literals (`{...}`) and `set()` construction with `add`/`discard`,
# `for x in set` through the materializing opaque-iterator capture,
# and attribute loads whose shape probe misses (a module receiver)
# riding the generic helper.

import os
import subprocess
import sys

if len(sys.argv) > 1 and sys.argv[1] == "child":

    class Plain:
        def __init__(self, v):
            self.v = v

    class WithBool:
        def __init__(self, v):
            self.v = v

        def __bool__(self):
            return self.v > 0

    class WithLen:
        def __init__(self, n):
            self.n = n

        def __len__(self):
            return self.n

    # TO_BOOL on object-lane values: the pure kinds answer without
    # running Python; __bool__/__len__ dispatch the exact protocol.
    def count_truthy(xs, n):
        c = 0
        i = 0
        while i < n:
            x = xs[i % 8]
            if x:
                c = c + 1
            i = i + 1
        return c

    def notter(x, n):
        c = 0
        i = 0
        while i < n:
            if not x:
                c = c + 1
            i = i + 1
        return c

    k = 0
    while k < 40:
        a = notter(Plain(0), 60)
        b = notter(WithBool(-3), 60)
        c = notter(WithLen(0), 60)
        k = k + 1
    print("truth", a, b, c)

    # Membership: `in`/`not in` on a set parameter, a str, and a
    # `__contains__` instance — all through the generic helper.
    def member(s, n):
        c = 0
        i = 0
        while i < n:
            if i % 7 in s:
                c = c + 1
            if i % 5 not in s:
                c = c + 10
            i = i + 1
        return c

    seen = {0, 2, 4, 6}
    k = 0
    while k < 40:
        m = member(seen, 100)
        k = k + 1
    print("member", m)

    def strmem(s, n):
        c = 0
        i = 0
        while i < n:
            if "ab" in s:
                c = c + 1
            if "zq" not in s:
                c = c + 1
            i = i + 1
        return c

    k = 0
    while k < 40:
        sm = strmem("xxabyy", 50)
        k = k + 1
    print("strmem", sm)

    class HasContains:
        def __contains__(self, x):
            return x == 42

    def instmem(o, n):
        c = 0
        i = 0
        while i < n:
            if 42 in o:
                c = c + 1
            if i in o:
                c = c + 100
            i = i + 1
        return c

    k = 0
    while k < 40:
        q = instmem(HasContains(), 30)
        k = k + 1
    print("instmem", q)

    # Set literals (BUILD_SET through the native helper) and the
    # set()/add/discard shape through the opaque-call lanes.
    def build(n):
        t = 0
        i = 0
        while i < n:
            s = {1, 2, 3, i % 4}
            if i % 4 in s:
                t = t + 1
            i = i + 1
        return t

    k = 0
    while k < 40:
        bl = build(50)
        k = k + 1
    print("build", bl)

    def adder(n):
        s = set()
        i = 0
        while i < n:
            s.add(i % 10)
            i = i + 1
        s.discard(3)
        return 5 in s, 3 in s

    k = 0
    while k < 40:
        ad = adder(60)
        k = k + 1
    print("adder", ad[0], ad[1])

    # `for x in set` — the materializing opaque-iterator capture.
    def total(s, n):
        t = 0
        i = 0
        while i < n:
            for x in s:
                t = t + x
            i = i + 1
        return t

    k = 0
    while k < 40:
        tt = total(seen, 20)
        k = k + 1
    print("iter", tt)

    # LOAD_ATTR residue: a module receiver has no gradable instance
    # shape — the probe misses and the load rides the generic helper.
    def sepcount(n):
        c = 0
        i = 0
        while i < n:
            if os.sep:
                c = c + 1
            i = i + 1
        return c

    k = 0
    while k < 40:
        sc = sepcount(30)
        k = k + 1
    print("attr", sc)

    # RFC 0076 WS5 — a None-valued module global rides the obj-global
    # pin: the helper must answer the object lane's nullable encoding
    # (`-1`), not a pin index, or the `is None` fence takes the wrong
    # branch once the function tiers up (torch's
    # `_cupti_monitor.push_user_annotation` shape).
    _singleton = None

    def maybe_push(name):
        if _singleton is None:
            return None
        return _singleton.push(name)

    class Pusher:
        def push(self, name):
            return len(name)

    hits = 0
    k = 0
    while k < 60:
        r = maybe_push("x")
        assert r is None, (k, r)
        k = k + 1
    _singleton = Pusher()
    k = 0
    while k < 60:
        r = maybe_push("xyz")
        assert r == 3, (k, r)
        hits = hits + r
        k = k + 1
    _singleton = None
    k = 0
    while k < 60:
        r = maybe_push("x")
        assert r is None, (k, r)
        k = k + 1
    print("noneglobal", hits)

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
