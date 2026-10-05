"""decimal: the native fast paths agree with the Python methods.

WeavePy serves `decimal` operations on finite values natively (see
`stdlib/decimal_native.rs`) and falls back to the module's Python methods
for everything else: subclass operands, a `Context` subclass, special
values, trapped signals, unusual arguments. This test runs each operation
both ways (exact `Decimal` operands under an exact `Context`, and
subclass operands under a `Context` subclass, which the native code never
serves) over rounding modes, precisions, exponent limits, clamping and
the signals, and checks the results and the flags agree. Under CPython
both ways run the C implementation.
"""

import random
import threading

import decimal as C
from decimal import Decimal

MODES = [
    "ROUND_DOWN", "ROUND_HALF_UP", "ROUND_HALF_EVEN", "ROUND_CEILING",
    "ROUND_FLOOR", "ROUND_UP", "ROUND_HALF_DOWN", "ROUND_05UP",
]
SIGNALS = [
    "Clamped", "DivisionByZero", "Inexact", "Overflow", "Rounded",
    "Underflow", "InvalidOperation", "Subnormal", "FloatOperation",
]


class SubDecimal(Decimal):
    pass


class SubContext(C.Context):
    pass


def make_context(native, prec, rounding, emin=-999999, emax=999999, clamp=0):
    cls = C.Context if native else SubContext
    return cls(prec=prec, rounding=getattr(C, rounding), Emin=emin,
               Emax=emax, clamp=clamp, traps=[], flags=[])


def raised(ctx):
    return [s for s in SIGNALS if ctx.flags[getattr(C, s)]]


def show(r):
    if isinstance(r, tuple):
        return tuple(show(x) for x in r)
    if isinstance(r, Decimal):
        return str(r)
    return r


def run(native, ctx_args, op, *args):
    ctx = make_context(native, *ctx_args)
    C.setcontext(ctx)
    cls = Decimal if native else SubDecimal
    try:
        vals = [cls(a) if isinstance(a, str) else a for a in args]
        return show(op(ctx, *vals)), raised(ctx)
    except Exception as e:  # noqa: BLE001 - compared below
        return type(e).__name__, raised(ctx)


OPS = {
    "add": lambda c, a, b: a + b,
    "sub": lambda c, a, b: a - b,
    "mul": lambda c, a, b: a * b,
    "div": lambda c, a, b: a / b,
    "floordiv": lambda c, a, b: a // b,
    "mod": lambda c, a, b: a % b,
    "divmod": lambda c, a, b: divmod(a, b),
    "quantize": lambda c, a, b: a.quantize(b),
    "neg": lambda c, a, b: -a,
    "pos": lambda c, a, b: +a,
    "abs": lambda c, a, b: abs(a),
    "cmp": lambda c, a, b: (a < b, a <= b, a == b, a != b, a > b, a >= b),
    "compare": lambda c, a, b: a.compare(b),
    "max": lambda c, a, b: a.max(b),
    "min": lambda c, a, b: a.min(b),
    "normalize": lambda c, a, b: a.normalize(),
    "tiv": lambda c, a, b: a.to_integral_value(),
    "tie": lambda c, a, b: a.to_integral_exact(),
    "hash": lambda c, a, b: hash(a),
    "str": lambda c, a, b: (str(a), repr(a), a.to_eng_string()),
    "int": lambda c, a, b: (int(a), round(a), a.__floor__(), a.__ceil__()),
    "round2": lambda c, a, b: round(a, 2),
    "float": lambda c, a, b: float(a),
    "tuple": lambda c, a, b: tuple(a.as_tuple()),
    "ratio": lambda c, a, b: a.as_integer_ratio(),
    "preds": lambda c, a, b: (a.is_zero(), a.is_signed(), a.is_normal(),
                                 a.is_subnormal(), a.is_finite(),
                                 a.adjusted(), a.same_quantum(b)),
    "copies": lambda c, a, b: (a.copy_abs(), a.copy_negate(),
                                  a.copy_sign(b)),
    "cadd": lambda c, a, b: c.add(a, b),
    "cmul": lambda c, a, b: c.multiply(a, b),
    "cdiv": lambda c, a, b: c.divide(a, b),
    "cplus": lambda c, a, b: c.plus(a),
    "cquant": lambda c, a, b: c.quantize(a, b),
    "create": lambda c, a, b: c.create_decimal(str(a)),
    "addint": lambda c, a, b: (a + 7, 7 + a, a - 3, 3 - a, a * -2,
                                  a == 1, 2 < a),
    "pow": lambda c, a, b: (a ** 2, a ** 3, (-a) ** 5, a ** 1, 3 ** b.copy_abs()
                            .to_integral_value().min(7), a ** b.copy_abs()
                            .to_integral_value().min(9)),
}


def check(name, ctx_args, *args):
    want = run(False, ctx_args, OPS[name], *args)
    got = run(True, ctx_args, OPS[name], *args)
    assert got == want, (name, ctx_args, args, got, want)


def rand_dec(rng):
    kind = rng.random()
    if kind < 0.05:
        return rng.choice(["0", "-0", "0E+5", "0.000", "-0E-3"])
    digits = rng.choice([1, 2, 3, 5, 9, 15, 19, 20, 28, 30, 40])
    coef = str(rng.randrange(10 ** (digits - 1), 10 ** digits))
    exp = rng.choice([0, -1, -2, -5, 3, rng.randrange(-40, 40)])
    sign = rng.choice(["", "-"])
    return "%s%sE%d" % (sign, coef, exp)


rng = random.Random(1234)
names = list(OPS)
for i in range(400):
    prec = rng.choice([1, 2, 3, 5, 9, 16, 28, 34])
    mode = rng.choice(MODES)
    if i % 7 == 0:
        ctx_args = (prec, mode, -rng.randrange(1, 30), rng.randrange(1, 30),
                    rng.choice([0, 1]))
    else:
        ctx_args = (prec, mode)
    check(names[i % len(names)], ctx_args, rand_dec(rng), rand_dec(rng))

# Integral powers: exact results at their ideal exponents, results one
# digit too long, powers of ten, one, zero, and the range limits.
powers = lambda c, a, b: tuple(a ** n for n in (1, 2, 3, 4, 7, 10, 25))
for mode in MODES:
    for v in ["1.5", "-1.5", "2", "0.5", "1.10", "100", "1E+2", "1.000",
              "-1", "0", "-0.00", "9.99", "12345", "3E-5", "7E+50"]:
        check("pow", (5, mode), v, "3")
        check("pow", (9, mode, -20, 20, 0), v, "2")
        for args in [(5, mode), (28, mode), (3, mode, -6, 6, 1)]:
            want = run(False, args, powers, v)
            got = run(True, args, powers, v)
            assert got == want, (v, args, got, want)

# Ties in every rounding mode, for both signs.
for mode in MODES:
    for v in ["2.5", "3.5", "-2.5", "-3.5", "2.50001", "-2.49999", "0.05",
              "1.05", "1.15", "-1.25", "9.95", "99.95"]:
        check("quantize", (28, mode), v, "0.1")
        check("quantize", (28, mode), v, "1")
        check("pos", (2, mode), v, "1")
        check("tiv", (28, mode), v, "1")
        check("tie", (28, mode), v, "1")
    # The sign of an exact zero sum depends on ROUND_FLOOR.
    check("add", (28, mode), "1.5", "-1.5")
    check("add", (28, mode), "-0", "0")
    check("sub", (28, mode), "0", "0")
    check("neg", (28, mode), "0", "1")

# Subnormal results, underflow to zero, clamping and overflow.
small = (5, "ROUND_HALF_EVEN", -10, 10, 0)
clamped = (5, "ROUND_HALF_EVEN", -10, 10, 1)
for v in ["1E-12", "1.2345E-11", "9.9999E-11", "5E-15", "1E-16", "123E-16"]:
    check("pos", small, v, "1")
    check("mul", small, v, "0.1")
    check("div", small, v, "3")
for v in ["1E+9", "1.2E+10", "12345E+6", "99999E+6", "0E+20", "1E+10"]:
    check("pos", clamped, v, "1")
    check("pos", small, v, "1")
    check("mul", small, v, "10")
    check("add", small, v, v)
check("quantize", small, "1.5", "1E-15")
check("quantize", small, "123456", "0.1")

# Division corner cases: exact quotients keep the ideal exponent.
for a, b in [("1", "3"), ("2", "4"), ("100", "4"), ("1.20", "2"), ("1E+3", "8"),
             ("0", "5"), ("-0", "5"), ("7", "-0.5"), ("1", "0"), ("0", "0"),
             ("12345678901234567890123", "7"), ("1E+30", "3E-10")]:
    for name in ("div", "floordiv", "mod", "divmod", "cdiv"):
        check(name, (9, "ROUND_HALF_EVEN"), a, b)
        check(name, (28, "ROUND_DOWN"), a, b)

# Specials take the Python path and still compare and print the same.
for s in ["Inf", "-Inf", "NaN", "-NaN123", "sNaN"]:
    for name in ("add", "mul", "str", "preds", "copies", "cmp", "neg"):
        check(name, (28, "ROUND_HALF_EVEN"), s, "1.5")

# Construction.
for v in ["1.23", "  -4.5e-3\n", "+.5", "5.", "1_000", "1e999999999999999999",
          "1e-9999999999999999999", "abc", "1e", "", "\u0661\u0662", "0e-100",
          0, -7, 2 ** 70, -(10 ** 40), True, 0.1, -0.0, 1e300, 5e-324, 2.5,
          float("inf"), float("nan"), (1, (2, 5), -1)]:
    want = run(False, (28, "ROUND_HALF_EVEN"), lambda c, a: str(SubDecimal(a)), v)
    got = run(True, (28, "ROUND_HALF_EVEN"), lambda c, a: str(Decimal(a)), v)
    assert got == want, (v, got, want)
    if isinstance(v, (int, float)):
        assert str(Decimal.from_float(v)) == str(SubDecimal.from_float(v)), v
assert C.Decimal(C.Decimal("1.5")) == C.Decimal("1.5")
assert str(C.Decimal(value="2.5")) == "2.5"
assert str(C.Context(prec=3).create_decimal("1.23456")) == "1.23"

# FloatOperation is recorded by Decimal(float) and raised when trapped.
c = C.Context(traps=[])
C.setcontext(c)
C.Decimal(0.5)
assert c.flags[C.FloatOperation]
c.traps[C.FloatOperation] = True
try:
    C.Decimal(0.5)
except C.FloatOperation:
    pass
else:
    raise AssertionError("FloatOperation not raised")

# Trapped signals raise from the Python path, after the flags it sets.
for m in (C,):
    ctx = m.Context(prec=3, traps=[m.Inexact])
    m.setcontext(ctx)
    try:
        m.Decimal(1) / m.Decimal(3)
    except m.Inexact:
        pass
    else:
        raise AssertionError("Inexact not raised")
    ctx = m.Context(prec=3, traps=[m.Rounded])
    m.setcontext(ctx)
    try:
        m.Decimal("1.2345").quantize(m.Decimal("0.1"))
    except m.Rounded:
        pass
    else:
        raise AssertionError("Rounded not raised")
    m.setcontext(m.Context(prec=3))
    try:
        m.Decimal("1.5").quantize(m.Decimal("1E-10"))
    except m.InvalidOperation:
        pass
    else:
        raise AssertionError("InvalidOperation not raised")
    m.setcontext(m.Context())
    try:
        m.Decimal(1) / 0
    except m.DivisionByZero:
        pass
    else:
        raise AssertionError("DivisionByZero not raised")

# The context changes mid-computation: every operation reads it afresh.
C.setcontext(C.Context())
x = C.Decimal(1) / C.Decimal(7)
assert str(x) == "0.1428571428571428571428571429"
C.getcontext().prec = 5
assert str(C.Decimal(1) / C.Decimal(7)) == "0.14286"
C.getcontext().rounding = C.ROUND_DOWN
assert str(C.Decimal(1) / C.Decimal(7)) == "0.14285"
with C.localcontext() as lc:
    lc.prec = 2
    assert str(C.Decimal(1) / C.Decimal(7)) == "0.14"
    lc.clear_flags()
    C.Decimal(1) / C.Decimal(4)
    assert not lc.flags[C.Inexact]
    C.Decimal(1) / C.Decimal(3)
    assert lc.flags[C.Inexact] and lc.flags[C.Rounded]
assert str(C.Decimal(1) / C.Decimal(7)) == "0.14285"
with C.localcontext(prec=3, rounding=C.ROUND_UP):
    assert str(C.Decimal(1) / C.Decimal(7)) == "0.143"
    assert str(x + 0) == "0.143"
assert C.getcontext().prec == 5
assert str(x.quantize(C.Decimal("0.01"), rounding=C.ROUND_UP)) == "0.15"
assert str(x.quantize(C.Decimal("0.01"), C.ROUND_HALF_EVEN,
                      C.Context(prec=3))) == "0.14"
C.getcontext().capitals = 0
assert str(C.Decimal("1E+10")) == "1e+10"
C.getcontext().capitals = 1
assert str(C.Decimal("1E+10")) == "1E+10"

# Each thread has its own context.
results = {}


def worker():
    C.getcontext().prec = 3
    results["thread"] = str(C.Decimal(2) / C.Decimal(3))


C.setcontext(C.Context(prec=10))
t = threading.Thread(target=worker)
t.start()
t.join()
assert results["thread"] == "0.667"
assert str(C.Decimal(2) / C.Decimal(3)) == "0.6666666667"


# Subclasses take the Python path, and their overrides win.
class MyDec(C.Decimal):
    def __add__(self, other, context=None):
        return "my add"

    def __radd__(self, other, context=None):
        return "my radd"


class Plain(C.Decimal):
    pass


C.setcontext(C.Context())
assert MyDec(1) + C.Decimal(2) == "my add"
assert C.Decimal(2) + MyDec(1) == "my radd"
assert 2 + MyDec(1) == "my radd"
p = Plain("1.5")
assert type(p) is Plain and str(p) == "1.5"
assert type(p + 1) is C.Decimal and str(p + 1) == "2.5"
assert type(p * p) is C.Decimal and p * p == C.Decimal("2.25")
assert type(Plain.from_float(0.5)) is Plain
assert hash(p) == hash(C.Decimal("1.5"))
assert str(C.getcontext().add(Plain(1), 2)) == "3"


class MyContext(C.Context):
    pass


mc = MyContext(prec=2)
assert str(mc.divide(C.Decimal(1), C.Decimal(3))) == "0.33"
assert mc.flags[C.Inexact]

# Mixed int operations, hashing and equality with ints.
assert C.Decimal(5) == 5 and 5 == C.Decimal("5.0") and C.Decimal("5.5") != 5
assert hash(C.Decimal("5.000")) == hash(5)
assert hash(C.Decimal("0.5")) == hash(0.5)
assert hash(C.Decimal("-1")) == hash(-1) == -2
assert {C.Decimal("1.0"): "a"}[1] == "a"
assert C.Decimal(2) ** 2 == 4
assert sum([C.Decimal("0.1")] * 10) == 1

# Formatting and the namedtuple from as_tuple.
t = C.Decimal("-1.20").as_tuple()
assert t == (1, (1, 2, 0), -2) and t.sign == 1 and t.exponent == -2
assert type(t).__name__ == "DecimalTuple"
for spec in ["", ".2f", "10.3e", ",.2f", "+.1%", ">12", "g", ".3g", "E"]:
    assert format(C.Decimal("12345.6789"), spec) == \
        format(SubDecimal("12345.6789"), spec), spec

# Signatures of replaced methods (inspect reads `__text_signature__`).
import inspect

assert list(inspect.signature(C.Decimal.quantize).parameters) == \
    ["self", "exp", "rounding", "context"]

# Formatting: every specifier part, both ways, under both capitals.
C.setcontext(C.Context())
values = ["0", "-0", "1.5", "-2.675", "123456789.987654321", "1E+30", "1.2E-9",
          "0.000", "-0.0001", "99999.5", "5E+2", "0E+3"]
specs = ["", "f", ".0f", ".3f", "e", ".2e", "E", "g", ".4g", "G", "%", ".1%",
         ",", ",.2f", "_.3f", ".3,f", ".6_f", "+", " .2f", "z.1f", "#.0f",
         "012.3f", "*^15.2f", "<10", ">10.1e", "=+12.2f", "^9", "08,.1f",
         "→<12.2f", "z#012.0%"]
for caps in (1, 0):
    for v in values:
        for spec in specs:
            def fmt(c, a, s=spec, caps=caps):
                c.capitals = caps
                return format(a, s)
            want = run(False, (28, "ROUND_HALF_EVEN"), fmt, v)
            got = run(True, (28, "ROUND_HALF_EVEN"), fmt, v)
            assert got == want, (v, spec, caps, got, want)
for v in ["1.23", "-4.5"]:
    for spec in ["n", "Q", "=010", ".f", "10.2.f"]:
        want = run(False, (28, "ROUND_UP"), lambda c, a, s=spec: format(a, s), v)
        got = run(True, (28, "ROUND_UP"), lambda c, a, s=spec: format(a, s), v)
        assert got == want, (v, spec, got, want)

# Keyword calls take the same paths as positional ones.
C.setcontext(C.Context())
x = C.Decimal("2.675")
assert str(x.quantize(C.Decimal("0.01"), rounding=C.ROUND_HALF_UP)) == "2.68"
assert str(x.quantize(exp=C.Decimal("0.01"), rounding=C.ROUND_DOWN)) == "2.67"
assert str(x.quantize(C.Decimal("0.01"),
                      context=C.Context(rounding=C.ROUND_UP))) == "2.68"
assert str(x.to_integral_value(rounding=C.ROUND_CEILING)) == "3"
for bad in ({"rounding": "nonsense"}, {"bogus": 1}):
    try:
        x.quantize(C.Decimal("0.01"), **bad)
    except TypeError:
        pass
    else:
        raise AssertionError("bad keyword call accepted: %r" % bad)

# Hashing in sets and dicts agrees with numeric equality.
s = {C.Decimal("1.0"), C.Decimal("1.00"), 1, 1.0, C.Decimal("0.5"), 0.5}
assert len(s) == 2, s
d = {C.Decimal("2.50"): "x"}
assert d[C.Decimal("2.5")] == "x" and d[2.5] == "x"

# `getcontext()` hands back the same context until it is replaced.
c1 = C.getcontext()
assert C.getcontext() is c1
c2 = C.Context(prec=3)
C.setcontext(c2)
assert C.getcontext() is c2
with C.localcontext() as lc:
    assert C.getcontext() is lc
assert C.getcontext() is c2
try:
    C.getcontext(1)
except TypeError:
    pass
else:
    raise AssertionError("getcontext() took an argument")
