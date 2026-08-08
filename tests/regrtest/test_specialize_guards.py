"""RFC 0058 WS3 — inline-cache guard invalidation must be invisible.

Each block warms a specialized fast path (subscr / binop / call /
for-iter families), then mutates the guarded shape mid-loop and asserts
the deopt produces exactly the generic path's behaviour: same values,
same exception types, same messages.
"""

WARM = 80  # comfortably past specialization + cooldown cycles


# --- BINARY_SUBSCR / STORE_SUBSCR ------------------------------------

xs = [10, 20, 30]
t = (1, 2, 3)
s = "hello"
d = {"a": 1, 2: "b"}
for n in range(WARM):
    assert xs[1] == 20 and xs[-1] == 30 + n
    assert t[0] == 1 and t[-2] == 2
    assert s[1] == "e" and s[-1] == "o"
    assert d["a"] == 1 + n and d[2] == "b"
    xs[2] = xs[2] + 1
    d["a"] = d["a"] + 1
assert xs[2] == 30 + WARM and d["a"] == 1 + WARM

# A warmed site that goes polymorphic mid-loop keeps working.
containers = [[1, 2], (3, 4), "ab", {1: 5}]
got = []
for c in containers:
    for _ in range(WARM):
        got.append(c[1])
assert got[0] == 2 and got[-1] == 5

# Error paths keep CPython messages after warm-up.
try:
    xs[99]
    raise SystemExit("expected IndexError")
except IndexError as e:
    assert str(e) == "list index out of range", e
try:
    t[99]
    raise SystemExit("expected IndexError")
except IndexError as e:
    assert str(e) == "tuple index out of range", e
try:
    s[99]
    raise SystemExit("expected IndexError")
except IndexError as e:
    assert str(e) == "string index out of range", e
try:
    d["missing"]
    raise SystemExit("expected KeyError")
except KeyError as e:
    assert str(e) == "'missing'", e
try:
    xs[99] = 1
    raise SystemExit("expected IndexError")
except IndexError as e:
    assert str(e) == "list assignment index out of range", e
try:
    d[[1]] = 1
    raise SystemExit("expected TypeError")
except TypeError as e:
    assert "unhashable" in str(e), e

# Non-ASCII strings never take the byte-indexing shape.
u = "héllo"
for _ in range(WARM):
    assert u[1] == "é" and u[4] == "o"

# dict subclass with __missing__ stays on the generic path.
class D(dict):
    def __missing__(self, k):
        return "missed"

dd = D()
for _ in range(WARM):
    assert dd["nope"] == "missed"


# --- BINARY_OP division / modulo / power -----------------------------

acc = 0
for i in range(1, WARM):
    acc += 100 // i + 100 % i + i**2
assert acc == sum(100 // i + 100 % i + i**2 for i in range(1, WARM))

facc = 0.0
for i in range(1, WARM):
    facc += 100.0 / i + 100.0 % (i + 0.5) + float(i) ** 0.5 + 100.0 // (i + 1.0)

# Same site, overflow into bignum mid-loop (deopt, exact result).
big = 1
for i in range(WARM):
    big = big * 3 + 1
assert big % 3 == 1 and big > 2**63

# Error semantics survive warm caches.
for _ in range(WARM):
    q = 7 // 2
try:
    7 // 0
    raise SystemExit("expected ZeroDivisionError")
except ZeroDivisionError as e:
    assert str(e) == "integer division or modulo by zero", e
try:
    7.0 / 0.0
    raise SystemExit("expected ZeroDivisionError")
except ZeroDivisionError as e:
    assert str(e) == "float division by zero", e
try:
    0 ** -1
    raise SystemExit("expected ZeroDivisionError")
except ZeroDivisionError:
    pass


# --- CALL family ------------------------------------------------------

class Counter:
    def __init__(self):
        self.n = 0

    def bump(self, k):
        self.n += k
        return self.n


c = Counter()
for _ in range(WARM):
    c.bump(2)
assert c.n == 2 * WARM

# Rebinding the method on the class mid-loop is observed (attr_version
# guard on the LOAD_ATTR side; the call cache re-fingerprints).
class A:
    def m(self):
        return 1


a = A()
seen = []
for i in range(WARM):
    seen.append(a.m())
    if i == WARM // 2:
        A.m = lambda self: 2
assert seen[0] == 1 and seen[-1] == 2


def f(a, b=10, c=20):
    return a + b + c


total = 0
for _ in range(WARM):
    total += f(1) + f(1, 2)
assert total == WARM * (31 + 23)

# `f.__defaults__ = …` replaces the compiled tuple mid-loop.
def g(a, b=1):
    return a + b


vals = []
for i in range(WARM):
    vals.append(g(0))
    if i == WARM // 2:
        g.__defaults__ = (5,)
assert vals[0] == 1 and vals[-1] == 5

# `del f.__defaults__` clears them; calls then under-fill.
del g.__defaults__
try:
    g(0)
    raise SystemExit("expected TypeError")
except TypeError:
    pass

# Native calls: module-level and bound methods, mixed with a profiler
# so the observer deopt keeps firing c_call events.
import math

total = 0.0
for i in range(WARM):
    total += math.sqrt(i)

buf = []
for i in range(WARM):
    buf.append(i)
assert len(buf) == WARM and buf[-1] == WARM - 1

import sys

events = []


def prof(frame, event, arg):
    if event.startswith("c_"):
        events.append(event)


sys.setprofile(prof)
math.sqrt(4.0)
buf.append(1)
sys.setprofile(None)
assert "c_call" in events and "c_return" in events, events


# --- FOR_ITER str / dict ----------------------------------------------

out = []
for ch in "abc" * (WARM // 2):
    out.append(ch)
assert len(out) == 3 * (WARM // 2) and out[0] == "a"

d2 = {"x": 1, "y": 2}
ks = []
for _ in range(WARM):
    for k in d2:
        ks.append(k)
assert len(ks) == 2 * WARM

# Mutation during a warmed dict loop raises exactly like CPython.
try:
    for k in d2:
        d2["z"] = 3
    raise SystemExit("expected RuntimeError")
except RuntimeError as e:
    assert "changed size during iteration" in str(e), e
del d2["z"]

# `del` + reinsert (same size) trips the keys-changed guard.
try:
    for k in d2:
        del d2["x"]
        d2["x"] = 0
    raise SystemExit("expected RuntimeError")
except RuntimeError as e:
    assert "changed" in str(e), e

print("ok")
