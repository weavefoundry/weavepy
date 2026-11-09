"""Calls of builtin types (`int(x)`, `float(x)`, `str(x)`, `range(n)`, an
exception class) from a loop that compiles during its first call give what
the type gives: when the site's callee changes to another type or a Python
class, when a parse fails, with keyword arguments, and for the spellings
the parsers accept (underscores, surrounding space, `inf`, `nan`, Unicode
digits)."""

import math


def convert(fn, xs, n):
    out = None
    for _ in range(n):
        out = [fn(x) for x in xs]
    return out


ints = ["12", " 345 ", "-6789", "1_000", "0", "+7", "١٢", "  -0  "]
for _ in range(3):
    assert convert(int, ints, 300) == [12, 345, -6789, 1000, 0, 7, 12, 0]

floats = ["1.5", " -2.25 ", "1e3", "1_0.5", "inf", "-Infinity", "+nan", "٣.5"]
got = convert(float, floats, 300)
assert got[:6] == [1.5, -2.25, 1000.0, 10.5, math.inf, -math.inf]
assert math.isnan(got[6]) and got[7] == 3.5
neg_nan = float("-nan")
assert math.isnan(neg_nan) and math.copysign(1.0, neg_nan) == -1.0

# The same site with other callees.
assert convert(str, [1, True, None, "s"], 300) == ["1", "True", "None", "s"]


class Wrapped:
    def __init__(self, v):
        self.v = v

    def __eq__(self, other):
        return isinstance(other, Wrapped) and other.v == self.v


assert convert(Wrapped, [1, 2], 300) == [Wrapped(1), Wrapped(2)]
assert convert(float, ["2"], 300) == [2.0]


def total_ranges(n):
    t = 0
    for i in range(n):
        for j in range(i % 5):
            t += j
    return t


assert total_ranges(2000) == sum(sum(range(i % 5)) for i in range(2000))


# Failures raise as the full constructor does.
def bad_ints(xs):
    caught = []
    for x in xs:
        try:
            int(x)
        except ValueError as e:
            caught.append(str(e))
    return caught


for _ in range(200):
    msgs = bad_ints(["x1", "1__0", "_1", "", "0x10", "1.5"])
assert msgs[0] == "invalid literal for int() with base 10: 'x1'", msgs
assert msgs[1] == "invalid literal for int() with base 10: '1__0'"
assert len(msgs) == 6


def bad_floats(xs):
    caught = 0
    for x in xs:
        try:
            float(x)
        except ValueError:
            caught += 1
    return caught


for _ in range(200):
    assert bad_floats(["1_", "_1.0", "1..0", "infinit", "nan_", "1e"]) == 6

# Keyword arguments and bases.
for _ in range(300):
    assert int("ff", 16) == 255 and int("0x1f", base=0) == 31 and int("z", 36) == 35


# Exception classes construct as the full call does.
def make_errors(n):
    out = None
    for i in range(n):
        out = (ValueError("v%d" % i), KeyError(i), OSError(2, "missing"))
    return out


v, k, o = make_errors(400)
assert v.args == ("v399",) and k.args == (399,) and o.errno == 2 and o.strerror == "missing"
print("ok")
