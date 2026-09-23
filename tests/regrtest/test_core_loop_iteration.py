"""The core loop's FOR_ITER fast arms match the full iteration protocol.

bytes, bytearray and str iterators step inside the core loop, and
``for i, x in enumerate(xs)`` hands its pair straight to the
``UNPACK_SEQUENCE 2`` that follows without building a tuple. Each case
below runs in a function called repeatedly so the fast arms (and the JIT
tiers above them) are the ones exercised, and compares against the
answer the generic protocol gives.
"""

import sys


def iterate(xs):
    out = []
    for x in xs:
        out.append(x)
    return out


def enumerated(xs, start=0):
    out = []
    for i, x in enumerate(xs, start):
        out.append((i, x))
    return out


def check(label, got, want):
    assert got == want, f"{label}: {got!r} != {want!r}"


for _ in range(300):
    check("bytes", iterate(b"\x00\x7f\x80\xff"), [0, 127, 128, 255])
    check("bytearray", iterate(bytearray(b"ab")), [97, 98])
    check("str ascii", iterate("abc"), ["a", "b", "c"])
    check("str wide", iterate("aé€𝄞"), ["a", "é", "€", "𝄞"])
    check("enumerate bytes", enumerated(b"ab"), [(0, 97), (1, 98)])
    check("enumerate str", enumerated("x€", 5), [(5, "x"), (6, "€")])
    check("enumerate list", enumerated([None, "z"]), [(0, None), (1, "z")])
    check("enumerate tuple", enumerated((1, 2)), [(0, 1), (1, 2)])
    check("enumerate range", enumerated(range(3, 0, -1)), [(0, 3), (1, 2), (2, 1)])
    check("enumerate bytearray", enumerated(bytearray(b"q")), [(0, 113)])
    check("enumerate big start", enumerated("ab", sys.maxsize), [(sys.maxsize, "a"), (sys.maxsize + 1, "b")])

# A one-character Latin-1 string is shared, as in CPython.
s = "héllo"
assert s[1] is iterate(s)[1]
assert iterate("é")[0] is iterate("é")[0]

# Growing a bytearray or list while iterating is seen by the loop.
ba = bytearray(b"ab")
seen = []
for c in ba:
    seen.append(c)
    if c == 97:
        ba.append(99)
check("bytearray growth", seen, [97, 98, 99])

xs = [1, 2]
pairs = []
for i, x in enumerate(xs):
    if i == 0:
        xs.append(3)
    pairs.append((i, x))
check("list growth", pairs, [(0, 1), (1, 2), (2, 3)])

# enumerate advances the iterator it wraps, and resumes where it stopped.
it = iter([1, 2, 3, 4])
for i, x in enumerate(it):
    if i == 1:
        break
check("shared inner", list(it), [3, 4])
e = enumerate("abc")
next(e)
check("resumed enumerate", [p for p in e], [(1, "b"), (2, "c")])

# A target that isn't a pair still gets the tuple (no fused unpack).
check("pair target", [p for p in enumerate("ab")], [(0, "a"), (1, "b")])
try:
    for i, x, y in enumerate("ab"):
        pass
except ValueError as exc:
    check("bad unpack", str(exc), "not enough values to unpack (expected 3, got 2)")
else:
    raise AssertionError("unpacking a pair into three targets must fail")

print("ok")
