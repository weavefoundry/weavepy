"""RFC 0076 WS6 — closure-cell lanes (LOAD_DEREF/STORE_DEREF) under
the tier-2 JIT.

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

    # nonlocal int counter: the canonical closure-cell hot loop.
    def make_counter():
        c = 0

        def inc(n):
            nonlocal c
            i = 0
            while i < n:
                c = c + i
                i = i + 1
            return c

        return inc

    inc = make_counter()
    k = 0
    while k < 40:
        r = inc(200)
        k = k + 1
    print("counter", r)

    # Float accumulator through a cell.
    def make_acc():
        total = 0.0

        def add(n):
            nonlocal total
            i = 0
            while i < n:
                total = total + 0.25
                i = i + 1
            return total

        return add

    add = make_acc()
    k = 0
    while k < 40:
        f = add(200)
        k = k + 1
    print("acc", f)

    # Read-only freevar (no STORE_DEREF in the hot body).
    def make_scaler(factor):
        def scale(n):
            s = 0
            i = 0
            while i < n:
                s = s + factor
                i = i + 1
            return s

        return scale

    sc = make_scaler(7)
    k = 0
    while k < 40:
        v = sc(200)
        k = k + 1
    print("scaler", v)

    # Two closures sharing one cell: writes through either must be
    # visible to the other, tier changes notwithstanding.
    def make_pair():
        x = 0

        def bump(n):
            nonlocal x
            i = 0
            while i < n:
                x = x + 1
                i = i + 1
            return x

        def drain(n):
            nonlocal x
            i = 0
            while i < n:
                x = x - 1
                i = i + 1
            return x

        return bump, drain

    bump, drain = make_pair()
    k = 0
    while k < 40:
        bump(300)
        r = drain(100)
        k = k + 1
    print("shared", r)

    # Cell overflow: the int lane leaves i64 mid-loop; exactness must
    # be preserved through the deopt.
    def make_grower():
        g = 1

        def grow(n):
            nonlocal g
            i = 0
            while i < n:
                g = g * 3
                i = i + 1
            return g

        return grow

    grow = make_grower()
    k = 0
    while k < 40:
        big = grow(3)
        k = k + 1
    print("overflow", big)

    # Lane change while hot: rebinding the cell to a float between
    # calls must deopt the burned int lanes, never misread bits.
    def make_flip():
        x = 1

        def step(n):
            nonlocal x
            i = 0
            while i < n:
                x = x + 1
                i = i + 1
            return x

        def reset(v):
            nonlocal x
            x = v

        return step, reset

    step, reset = make_flip()
    k = 0
    while k < 40:
        step(200)
        k = k + 1
    reset(0.5)
    print("flipped", step(4))

    # Unbound cell: reading before the first write raises exactly,
    # from a body that tiered up on the bound path.
    def make_maybe():
        def get(n):
            t = 0
            i = 0
            while i < n:
                t = t + y
                i = i + 1
            return t

        y = 2
        return get

    g = make_maybe()
    k = 0
    while k < 40:
        r = g(50)
        k = k + 1
    print("bound", r)

    def make_unbound():
        def get():
            return z

        if False:
            z = 1
        return get

    try:
        make_unbound()()
        print("unbound", "no-raise")
    except NameError as e:
        print("unbound", type(e).__name__)

    # Promoted parameter: a cellvar aliasing a parameter (make_frame
    # moves the argument into the cell; every access derefs).
    def make_adder(base):
        def bump():
            nonlocal base
            base = base + 1

        def read(n):
            t = 0
            i = 0
            while i < n:
                t = t + base
                i = i + 1
            return t

        return bump, read

    bump2, read2 = make_adder(10)
    k = 0
    while k < 40:
        bump2()
        r = read2(100)
        k = k + 1
    print("promoted", r)

    # Object-lane cell: the payload is re-read (and freshly pinned)
    # per access — attribute traffic and calls ride the dyn lanes.
    class Box:
        def __init__(self, v):
            self.v = v

    def make_reader(box):
        def total(n):
            t = 0
            i = 0
            while i < n:
                t = t + box.v
                i = i + 1
            return t

        def swap(nb):
            nonlocal box
            box = nb

        return total, swap

    total, swap = make_reader(Box(3))
    k = 0
    while k < 40:
        r = total(100)
        k = k + 1
    swap(Box(5))
    print("objcell", r, total(100))

    # `super()` methods carry the `__class__` freevar — an object cell
    # (the canonical closure-in-a-method shape).
    class A2:
        def __init__(self, s):
            self.s = s

    class B2(A2):
        def __init__(self, s):
            super().__init__(s)
            self.t = s * 2

    k = 0
    while k < 40:
        b = B2(k)
        k = k + 1
    print("super", b.s, b.t)

    # None through an object cell (the nullable encoding).
    def make_opt():
        cur = None

        def put(v):
            nonlocal cur
            cur = v

        def take(n):
            c = 0
            i = 0
            while i < n:
                if cur is None:
                    c = c + 1
                i = i + 1
            return c

        return put, take

    put, take = make_opt()
    k = 0
    while k < 40:
        r = take(100)
        k = k + 1
    put(Box(1))
    print("optcell", r, take(100))

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
