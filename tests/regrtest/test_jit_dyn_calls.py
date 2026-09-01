# RFC 0076 WS7 — escaping callees and per-kind CallDyn fast paths.
#
# The workload runs twice — interpreted and under an aggressive JIT
# threshold — and the outputs must match exactly. Shapes covered:
# a burned global callee escaping into a local (demotion + opaque
# call), callables stored in a container, a callee passed as an
# argument, a bound method called through a local, a demoted class
# used both as a constructor and a static-method namespace, and a
# keyword call on an opaque callee.

import os
import subprocess
import sys

if len(sys.argv) > 1 and sys.argv[1] == "child":

    def add1(x):
        return x + 1

    def mul2(x):
        return x * 2

    # Escaping callee: the ternary select stores the function into a
    # local, so its loads demote to object pins and the call rides
    # the opaque lane (native fast path when the callee compiles).
    def pick(flag, n):
        g = add1 if flag else mul2
        r = None
        i = 0
        while i < n:
            r = g(i)
            i = i + 1
        return r

    k = 0
    while k < 40:
        a = pick(k % 2 == 0, 50)
        k = k + 1
    print("pick", a, pick(True, 50), pick(False, 50))

    # Callables stored into a container.
    def table(n):
        fns = [add1, mul2]
        r = None
        i = 0
        while i < n:
            r = fns[i % 2](7)
            i = i + 1
        return r

    k = 0
    while k < 40:
        b = table(9)
        k = k + 1
    print("table", b, table(10))

    # Callee as an argument: `f` arrives as an object parameter and
    # calls through the opaque lane.
    def apply_last(f, n):
        r = None
        i = 0
        while i < n:
            r = f(i)
            i = i + 1
        return r

    k = 0
    while k < 40:
        c = apply_last(add1, 30)
        k = k + 1
    print("apply", c, apply_last(mul2, 30))

    # Bound method through a local (the per-kind fast path's
    # bound-method direct entry when `bump` compiles).
    class Counter:
        def __init__(self, v):
            self.v = v

        def bump(self, d):
            self.v = self.v + d
            return self.v

    cnt = Counter(0)
    m = cnt.bump
    k = 0
    while k < 40:
        d = m(2)
        k = k + 1
    print("bound", d, cnt.v)

    # A demoted class: `Strength.stronger` forces the class load to
    # escape (static-method namespace), and the same class constructs
    # instances in the loop (the dyn ctor fast path).
    class Strength:
        def __init__(self, v):
            self.v = v

        @staticmethod
        def stronger(s1, s2):
            return s1.v < s2.v

    def strength_churn(n):
        t = 0
        i = 0
        while i < n:
            a = Strength(i)
            b = Strength(n - i)
            if Strength.stronger(a, b):
                t = t + 1
            i = i + 1
        return t

    k = 0
    while k < 40:
        e = strength_churn(20)
        k = k + 1
    print("strength", e)

    # Keyword call on an opaque callee (CallDynKw).
    def kwtarget(x, y=3, z=5):
        return x * 100 + y * 10 + z

    def kw_driver(f, n):
        r = 0
        i = 0
        while i < n:
            r = f(1, z=9)
            i = i + 1
        return r

    k = 0
    while k < 40:
        g = kw_driver(kwtarget, 25)
        k = k + 1
    print("kw", g)

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
