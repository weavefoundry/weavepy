"""Values through warm and compiled fast paths.

Each case runs small operations on edge-case values (strings with lone
surrogates and non-ASCII text, bytes, integers at the machine-word and
float-precision boundaries, NaN and negative zero, mixed-type keys and
compares, exceptions, generators and coroutines, tracing switched on
mid-loop, recursion limits) in loops long enough for every tier to
compile them, inputs drawn from a fixed pseudo-random sequence, and prints
a digest of what it saw. The output must be CPython's in fresh
interpreters with every tier on, with a low compile threshold, with a low
frame-JIT threshold, with the method tier off and with the compilers off.
"""

import os
import subprocess
import sys
import unittest

PRELUDE = r'''
import zlib

N = 300


class Rand:
    """A fixed pseudo-random sequence (the same on every interpreter)."""

    def __init__(self, seed):
        self.x = seed

    def next(self, n):
        self.x = (self.x * 1103515245 + 12345) % 2147483648
        return self.x % n

    def pick(self, xs):
        return xs[self.next(len(xs))]


def report(name, results):
    text = repr(results)
    print(name, len(results), zlib.crc32(text.encode("utf-8", "surrogatepass")), text[:300])


def hot(name, work, n=N, seed=1):
    """`work(rng, i)` in a loop through a method call, then directly."""
    rng = Rand(seed)
    out = []
    for i in range(n):
        out.append(work(rng, i))
    report(name, out)
'''

CASES = {}

CASES["str_surrogates"] = r'''
POOL = ["", "a", "abc", "\udcc4", "x\udcc4y", "\udc80\udcff", "é", "naïve", "€", "日本",
        "\U0001f600", "a\U0001f600b", "\ud800", "ß", "İ", "\x00", "\t\n", "'\"", "\\"]
HANDLERS = ["strict", "surrogateescape", "surrogatepass", "replace", "ignore",
            "backslashreplace", "xmlcharrefreplace", "namereplace"]
DEC = ["strict", "surrogateescape", "replace", "ignore", "backslashreplace", "surrogatepass"]
BYTES = [b"abc", b"\xc4", b"a\xc4b", b"\xff\xfe", b"\xed\xb3\x84", b"\xe2\x82\xac", b"\xf0\x9f\x98\x80"]
TABLE = {ord("a"): "A", 0xDCC4: "<c4>", ord("é"): None}


def enc(s, h, codec):
    try:
        return s.encode(codec, h)
    except UnicodeError as e:
        return type(e).__name__


def dec(b, h, codec):
    try:
        return b.decode(codec, h)
    except UnicodeError as e:
        return type(e).__name__


def work(rng, i):
    a = rng.pick(POOL)
    b = rng.pick(POOL)
    h = rng.pick(HANDLERS)
    raw = rng.pick(BYTES)
    s = a + b
    t = a
    t += b
    t += "!"
    d = {a: 1, b: 2}
    parts = [a, b, "-"]
    return (
        s, t, len(s), s[1:3], s[::-1], s[-1:] if s else "", s[0] if s else None,
        "|".join(parts), f"{a}/{b!r}/{len(b):3d}", "%s:%r" % (a, b), "{0}{1}".format(a, b),
        enc(s, h, "utf-8"), enc(s, h, "ascii"), enc(s, h, "latin-1"),
        dec(raw, rng.pick(DEC), "utf-8"), dec(raw, rng.pick(DEC), "ascii"),
        s.translate(TABLE), repr(s), ascii(s), a == b, a < b, a <= b, d[a], d.get(b), a in s,
        s.find(b), s.startswith(a), s.upper(), s.lower(), s.split(b) if b else s.split(),
        s.replace(a, "R") if a else s, [ord(c) for c in s[:4]], str(s) is s or str(s) == s,
        s.encode("utf-8", "surrogatepass").decode("utf-8", "surrogatepass") == s,
        s.isascii(), s.isidentifier(), s.casefold(), s.center(9, "*"), s.zfill(5),
    )


hot("str_surrogates", work)
hot("str_surrogates_2", work, seed=7)
'''

CASES["os_fs_names"] = r'''
import os

NAMES = [b"plain", b"\xc4", b"caf\xc3\xa9", b"a\xffb", b"\xed\xa0\x80", b"\xe2\x82\xac.txt"]


def work(rng, i):
    raw = rng.pick(NAMES)
    name = os.fsdecode(raw)
    back = os.fsencode(name)
    joined = os.path.join("/tmp", name)
    return (name, repr(name), back == raw, joined, os.path.basename(joined),
            name.encode("utf-8", "surrogateescape") == raw, "%s" % name, f"{name}")


hot("os_fs_names", work)
'''

CASES["escaped_names"] = r'''
import io

RAW = [b"plain.txt\x00\x00", b"\xc4\xd6\xdc\x00pad", b"caf\xc3\xa9/\x00", b"a\xffb\x00", b"dir/\xe2\x82\xac\x00"]
CODECS = ["utf-8", "ascii", "latin-1", "iso8859-1"]


def nts(s, encoding, errors):
    p = s.find(b"\0")
    if p != -1:
        s = s[:p]
    return s.decode(encoding, errors)


def safe(s, encoding):
    return s.encode(encoding, "backslashreplace").decode(encoding)


def work(rng, i):
    raw = rng.pick(RAW)
    enc = rng.pick(CODECS)
    name = nts(raw, enc, "surrogateescape")
    buf = io.StringIO()
    print(safe(name, "ascii"), end=" ", file=buf)
    buf.write("%-12s|" % name)
    buf.write("{:>12}|".format(name))
    buf.write(name.rstrip("/") + "\n")
    stripped = name.rstrip("/")
    return (name, repr(name), buf.getvalue(), safe(name, "utf-8"), safe(name, "latin-1"),
            stripped, name.encode(enc, "surrogateescape") == raw.split(b"\0")[0],
            [hex(ord(c)) for c in name], name.ljust(14, "."), name[:3], len(name),
            name.isprintable(), "/" in name, name.split("/"), name.partition("."))


hot("escaped_names", work)
'''

CASES["surrogate_routes"] = r'''
import codecs

CPS = [0xDCC4, 0xD800, 0xDFFF, 0xE9, 0x20AC, 0x1F600, 0x61, 0xDC80, 0xF0000]


def routes(cp):
    c = chr(cp)
    lit = {0xDCC4: "\udcc4", 0xD800: "\ud800", 0xDFFF: "\udfff", 0xE9: "\xe9", 0x20AC: "€",
           0x1F600: "\U0001f600", 0x61: "a", 0xDC80: "\udc80", 0xF0000: "\U000f0000"}[cp]
    via_bytes = c.encode("utf-8", "surrogatepass").decode("utf-8", "surrogatepass")
    via_escape = codecs.decode(c.encode("unicode_escape"), "unicode_escape")
    via_join = "".join(["x", c, "y"])[1:2]
    via_fmt = ("%s" % c)
    via_f = f"{c}"
    via_slice = ("ab" + c + "cd")[2]
    via_list = "".join(list(c))
    via_mul = (c * 3)[1]
    via_repl = "Q".replace("Q", c)
    return [lit, c, via_bytes, via_escape, via_join, via_fmt, via_f, via_slice, via_list,
            via_mul, via_repl]


def work(rng, i):
    cp = rng.pick(CPS)
    rs = routes(cp)
    d = {}
    for k, r in enumerate(rs):
        d[r] = d.get(r, 0) + 1
    other = routes(rng.pick(CPS))
    s = set(rs) | set(other)
    return (len(d), sorted(map(ord, s)), all(r == rs[0] for r in rs), [len(r) for r in rs],
            [hash(r) == hash(rs[0]) for r in rs], d.get(rs[0]), rs[0] in other,
            sorted(rs + other)[:3], max(rs + other), [r.encode("utf-8", "surrogatepass") for r in rs[:2]],
            "".join(rs).count(rs[0]), "".join(rs).find(rs[0]), rs[0] < other[0], repr(rs[0]))


hot("surrogate_routes", work)
'''

CASES["bytes_ops"] = r'''
POOL = [b"", b"a", b"abc", b"\x00\xff", b"\xc4\x80", bytes(range(5)), b"spam eggs"]


def work(rng, i):
    a = rng.pick(POOL)
    b = rng.pick(POOL)
    ba = bytearray(a)
    ba += b
    ba.append(i % 256)
    ba.extend(b"!")
    c = a + b
    return (c, c[1:3], c[0] if c else None, c[-1:], b"-".join([a, b]), c.find(b), b in c,
            c.replace(a, b"R") if a else c, c.hex(), bytes.fromhex(c.hex()) == c, c.upper(),
            ba, bytes(ba), ba[1:], len(ba), a < b, a == b, b"%s|%d" % (a, i), c.split(b) if b else c.split(),
            c.decode("latin-1"), memoryview(c)[1:].tobytes(), int.from_bytes(c[:8] or b"\x00", "big"),
            c.count(b"a"), c.strip(), list(c[:3]), c * 2, ba.pop() if ba else None)


hot("bytes_ops", work)
'''

CASES["int_edges"] = r'''
EDGE = [0, 1, -1, 2, -2, 3, 7, -7, 2**31 - 1, -2**31, 2**31, 2**53 - 1, 2**53, 2**53 + 1,
        -(2**53) - 1, 2**62, 2**63 - 1, -2**63, 2**63, -2**63 - 1, 2**64, 10**20, -10**20,
        True, False]


def ops(a, b):
    out = [a + b, a - b, a * b, a == b, a < b, a & b, a | b, a ^ b, -a, abs(a), ~a]
    for f in (lambda: a / b, lambda: a // b, lambda: a % b, lambda: divmod(a, b),
              lambda: a ** (b % 5) if b >= 0 else a ** -1, lambda: pow(a, 3, b) if b else None,
              lambda: a << (b % 70) if b >= 0 else a >> 3, lambda: a >> (abs(b) % 70)):
        try:
            out.append(f())
        except (ZeroDivisionError, OverflowError, ValueError) as e:
            out.append(type(e).__name__)
    out.append(float(a) == a)
    out.append(a == float(a) if abs(a) < 2**70 else None)
    out.append(int(float(a)) if abs(a) < 2**70 else None)
    out.append(hash(a) == hash(float(a)) if abs(a) < 2**53 else None)
    return out


def work(rng, i):
    a = rng.pick(EDGE)
    b = rng.pick(EDGE)
    x = i - 150
    y = (i * 7919) % 97 - 48
    q = x // y if y else None
    r = x % y if y else None
    t = x / y if y else None
    return (ops(a, b), q, r, t, x ** 2, True + True, True * 3, -0.0 == 0, int(-0.0),
            (x * 2**40) // 3, (x * 2**40) % 7, (2**63 - 1) + x, x * (2**62))


hot("int_edges", work, n=600)
'''

CASES["float_edges"] = r'''
import math

VALS = [0.0, -0.0, 1.0, -1.5, 0.1, 1e16, 1e-7, 123456789.125, 2.5, 3.5, -2.5,
        float("inf"), float("-inf"), float("nan"), 1 / 3, 2**53 + 0.0, 5e-324]
SPECS = ["", ".2f", ".3e", "g", ".10g", "+.1f", "08.3f", "%", ".0f", "r"]


def fmt(v, spec):
    if spec == "r":
        return repr(v)
    try:
        return format(v, spec)
    except ValueError as e:
        return "ValueError"


def work(rng, i):
    a = rng.pick(VALS)
    b = rng.pick(VALS)
    xs = [rng.pick(VALS) for _ in range(5)]
    nan_free = [x for x in xs if x == x]
    return (fmt(a, rng.pick(SPECS)), fmt(b, rng.pick(SPECS)), a == b, a < b, a != b, a >= b,
            str(a), "%.3f|%g|%r" % (a, a, a), f"{a:.2f}", round(a, 2) if math.isfinite(a) else a,
            math.copysign(1.0, a), a == a, [a] == [a], (a,) == (a,), a in [a], sorted(nan_free),
            max(nan_free) if nan_free else None, min(nan_free) if nan_free else None,
            a * 0.0 if math.isfinite(a) else None, float(str(a)) == a or a != a)


hot("float_edges", work)
'''

CASES["mixed_compares"] = r'''
import functools

ITEMS = [1, 1.0, True, 2, 2.5, -1, "a", "b", (1, 2), (1, 2.0), (1, "a"), [1], [1.0], None, b"a"]


def cmp(a, b):
    out = []
    for f in (lambda: a == b, lambda: a < b, lambda: a <= b, lambda: a > b):
        try:
            out.append(f())
        except TypeError:
            out.append("TypeError")
    return out


def work(rng, i):
    a = rng.pick(ITEMS)
    b = rng.pick(ITEMS)
    nums = [rng.pick([3, 1.5, True, -2, 0, 2.0, False]) for _ in range(5)]
    pairs = [(rng.pick([1, 1.0, 2]), rng.pick(["x", "y"])) for _ in range(4)]
    try:
        mixed = sorted([a, b])
    except TypeError:
        mixed = "TypeError"
    return (cmp(a, b), cmp((a, 1), (b, 2)), cmp([a], [b]), sorted(nums), sorted(nums, reverse=True),
            sorted(pairs), sorted(pairs, key=lambda p: (p[1], -p[0])), mixed, max(nums), min(nums),
            sorted(nums, key=functools.cmp_to_key(lambda x, y: (x > y) - (x < y))),
            [x is True for x in sorted(nums)])


hot("mixed_compares", work)
'''

CASES["equal_keys"] = r'''
KEYS = [1, 1.0, True, 0, 0.0, False, -0.0, 2, 2.0, "1", (1,), (1.0,), 10**20, float(10**20)]


def work(rng, i):
    d = {}
    s = set()
    for _ in range(6):
        k = rng.pick(KEYS)
        d[k] = i
        s.add(k)
    k = rng.pick(KEYS)
    items = sorted(((repr(key), repr(v)) for key, v in d.items()))
    first_types = sorted(type(key).__name__ for key in d)
    r = (items, first_types, len(s), sorted(map(repr, s)), k in d, k in s, d.get(k), repr(d.get(True)),
         repr(d.pop(1, None)), len(d), {1: "a", True: "b", 1.0: "c"}, {True: 1, 1: 2}, {0, False, 0.0},
         {1.0, 1} == {1}, dict.fromkeys([1, True, 1.0]), list({True: 0, 1: 1}.keys()),
         frozenset([1, True]) == frozenset([1.0]), {(1, 2): "t"}.get((1.0, 2.0)))
    return r


hot("equal_keys", work)
'''

CASES["exceptions"] = r'''
import sys
import traceback


class E1(Exception):
    pass


class E2(Exception):
    pass


def boom(i):
    if i % 3 == 0:
        raise E1(i)
    if i % 3 == 1:
        return {}["missing" + str(i % 2)]
    return [][i]


def chain(i):
    try:
        boom(i)
    except Exception as e:
        if i % 2:
            raise E2("cause") from e
        raise E2("context")


def group(i):
    eg = ExceptionGroup("g", [E1(i), E2(i), ValueError(i)])
    seen = []
    try:
        raise eg
    except* E1 as g:
        seen.append(("E1", len(g.exceptions)))
    except* (E2, ValueError) as g:
        seen.append(("rest", sorted(type(x).__name__ for x in g.exceptions)))
    return seen


def lines(tb):
    out = []
    while tb is not None:
        out.append((tb.tb_frame.f_code.co_name, tb.tb_lineno - tb.tb_frame.f_code.co_firstlineno))
        tb = tb.tb_next
    return out


def work(rng, i):
    try:
        chain(i)
    except E2 as e:
        cause = type(e.__cause__).__name__ if e.__cause__ else None
        ctx = type(e.__context__).__name__ if e.__context__ else None
        info = (str(e), cause, ctx, e.__suppress_context__, lines(e.__traceback__),
                lines(e.__context__.__traceback__) if e.__context__ else None)
    order = []
    try:
        try:
            order.append("try")
            boom(i)
        finally:
            order.append("finally")
    except Exception as e:
        order.append(type(e).__name__)
        order.append(sys.exc_info()[0].__name__)
        err = e
    order.append(sys.exc_info()[0])
    fmt = traceback.format_exception_only(type(err), err)
    return info, group(i), order, fmt


hot("exceptions", work)
'''

CASES["generators"] = r'''
def gen(n):
    total = 0
    try:
        for k in range(n):
            got = yield k
            if got is not None:
                total += got
    except ValueError as e:
        yield ("caught", str(e))
    finally:
        pass
    return total


def outer(n):
    r = yield from gen(n)
    yield ("returned", r)


def closer():
    try:
        yield 1
        yield 2
    except GeneratorExit:
        log.append("exit")
        raise
    finally:
        log.append("finally")


log = []


def work(rng, i):
    g = gen(4)
    out = [next(g), g.send(5), g.send(7)]
    try:
        out.append(g.throw(ValueError("boom%d" % (i % 3))))
    except StopIteration as e:
        out.append(("stop", e.value))
    o = outer(3)
    out.append(list(o))
    o2 = outer(5)
    out.append(next(o2))
    out.append(o2.send(10))
    try:
        while True:
            out.append(o2.send(1))
    except StopIteration as e:
        out.append(("stop", e.value))
    c = closer()
    out.append(next(c))
    c.close()
    out.append(list(log[-2:]))
    out.append(sum(x for x in range(i % 7)))
    out.append([y for y in (z * 2 for z in range(3))])
    return out


hot("generators", work)
'''

CASES["coroutines"] = r'''
import types


class Ready:
    def __init__(self, v):
        self.v = v

    def __await__(self):
        got = yield ("ready", self.v)
        return (self.v, got)


@types.coroutine
def tick(v):
    x = yield ("tick", v)
    return x


async def leaf(v):
    a = await Ready(v)
    b = await tick(v)
    return (a, b)


async def mid(v):
    try:
        r = await leaf(v)
    except KeyError as e:
        return ("caught", str(e))
    return ("mid", r)


def drive(coro, sends, throw_at=None):
    out = []
    try:
        out.append(coro.send(None))
        for k, s in enumerate(sends):
            if throw_at == k:
                out.append(coro.throw(KeyError("k%d" % k)))
            else:
                out.append(coro.send(s))
    except StopIteration as e:
        out.append(("done", e.value))
    return out


def work(rng, i):
    out = [drive(mid(i % 4), ["s1", "s2", "s3"]), drive(mid(1), ["a"], throw_at=0),
           drive(leaf(2), ["x", "y"])]
    c = mid(3)
    c.send(None)
    c.close()
    out.append(c.cr_frame is None)
    return out


hot("coroutines", work)
import asyncio


async def main():
    total = 0
    for i in range(N):
        total += await asyncio.sleep(0, result=i)
        await asyncio.sleep(0)
    return total


print("asyncio", asyncio.run(main()))
'''

CASES["tracing_midloop"] = r'''
import sys


def f(x):
    y = x + 1
    return y * 2


def loop(n):
    t = 0
    for i in range(n):
        t += f(i)
    return t


events = []


def tracer(frame, event, arg):
    if frame.f_code.co_name == "f":
        events.append((event, frame.f_lineno - frame.f_code.co_firstlineno))
        return tracer
    return None


def prof(frame, event, arg):
    if frame.f_code.co_name == "f":
        events.append(("prof", event))


print(loop(N))
sys.settrace(tracer)
print(loop(3))
sys.settrace(None)
print(loop(N))
sys.setprofile(prof)
print(loop(2))
sys.setprofile(None)
print(loop(N))
print(events)
mon = sys.monitoring
TOOL = 3
mon.use_tool_id(TOOL, "test")
seen = []


def on_call(code, offset):
    if code.co_name == "f":
        seen.append("start")


mon.register_callback(TOOL, mon.events.PY_START, on_call)
mon.set_events(TOOL, mon.events.PY_START)
print(loop(4))
mon.set_events(TOOL, 0)
mon.free_tool_id(TOOL)
print(loop(N), seen)
'''

CASES["tracing_inside_hot_loop"] = r'''
import sys

mon = sys.monitoring


def g(x):
    return x * 3


events = []


def prof(frame, event, arg):
    if event in ("call", "return") and frame.f_code.co_name == "g":
        events.append(("prof", event))


def tracer(frame, event, arg):
    return None


def loop_profile(n):
    t = 0
    for i in range(n):
        if i == n - 3:
            sys.setprofile(prof)
        t += g(i)
    sys.setprofile(None)
    return t


def loop_lines(n):
    t = 0
    for i in range(n):
        if i == n - 2:
            mon.set_local_events(TOOL, loop_lines.__code__, mon.events.LINE)
        t += i
        t ^= 1
    mon.set_local_events(TOOL, loop_lines.__code__, 0)
    return t


def loop_calls(n):
    t = 0
    for i in range(n):
        if i == n - 4:
            mon.set_events(TOOL, mon.events.PY_START | mon.events.PY_RETURN)
        t += g(i)
    mon.set_events(TOOL, 0)
    return t


def loop_frame_trace(n):
    t = 0
    for i in range(n):
        if i == n - 2:
            frame = sys._getframe()
            frame.f_trace = lambda f, e, a: events.append(("ftrace", e, f.f_lineno - f.f_code.co_firstlineno))
            sys.settrace(tracer)
        t += i
    sys.settrace(None)
    return t


TOOL = 4
mon.use_tool_id(TOOL, "test")
lines = []
calls = []
mon.register_callback(TOOL, mon.events.LINE,
                      lambda code, line: lines.append(line - code.co_firstlineno) if code.co_name == "loop_lines" else None)
mon.register_callback(TOOL, mon.events.PY_START,
                      lambda code, off: calls.append(("start", code.co_name)) if code.co_name == "g" else None)
mon.register_callback(TOOL, mon.events.PY_RETURN,
                      lambda code, off, val: calls.append(("return", code.co_name, val)) if code.co_name == "g" else None)
print(loop_profile(N), events)
events.clear()
print(loop_lines(N), lines)
print(loop_calls(N), calls)
print(loop_frame_trace(N), events)
mon.free_tool_id(TOOL)
'''

CASES["recursion_limits"] = r'''
import sys


def depth(n):
    if n == 0:
        return 0
    return 1 + depth(n - 1)


def deep():
    return deep()


class R:
    def __repr__(self):
        return repr(self)


for i in range(N):
    depth(50)
print(depth(500))
old = sys.getrecursionlimit()
sys.setrecursionlimit(200)
results = []
for i in range(5):
    try:
        deep()
    except RecursionError as e:
        results.append("RecursionError")
    try:
        repr(R())
    except RecursionError:
        results.append("repr-RecursionError")
    results.append(depth(100))
sys.setrecursionlimit(old)
print(results, depth(900))
'''

CASES["leaf_helpers"] = r'''
STRS = ["a", "\udcc4", "é", "x\udcc4", "€uro", "\U0001f600", "", "ab\x00"]
NUMS = [0, 1, -1, 7, -7, 2**31, 2**62, -(2**63), 2**64, 1.5, -0.0, True, False]
KEYS = [1, 1.0, True, "1", (1,), 2]


def cat(a, b):
    return a + b


def sl(a):
    return a[1:]


def ix(a):
    return a[-1]


def up(a):
    return a.upper()


def rep(a):
    return repr(a)


def fmt(a):
    return f"<{a}>"


def pct(a):
    return "%s" % a


def enc(a):
    return a.encode("utf-8", "surrogateescape")


def esc(a):
    return a.encode("ascii", "backslashreplace")


def jn(a, b):
    return "".join([a, b])


def eq(a, b):
    return a == b


def lt(a, b):
    return a < b


def div(a, b):
    return a / b


def fdiv(a, b):
    return a // b


def mod(a, b):
    return a % b


def pw(a, b):
    return a ** b


def get(d, k):
    return d.get(k)


def has(d, k):
    return k in d


def tl(a):
    return len(a)


def run(rng):
    out = []
    d = {k: repr(k) for k in KEYS}
    for i in range(N * 2):
        a = rng.pick(STRS)
        b = rng.pick(STRS)
        x = rng.pick(NUMS)
        y = rng.pick(NUMS)
        k = rng.pick(KEYS)
        row = [cat(a, b), sl(a), up(a), rep(a), fmt(a), pct(a), enc(a), esc(a), jn(a, b),
               eq(a, b), lt(a, b), tl(a + b), get(d, k), has(d, k), eq(x, y)]
        if a:
            row.append(ix(a))
        try:
            row.append(lt(x, y))
            row.append(div(x, y))
            row.append(fdiv(x, y))
            row.append(mod(x, y))
            row.append(pw(x, 3))
        except (ZeroDivisionError, OverflowError) as e:
            row.append(type(e).__name__)
        out.append(row)
    return out


report("leaf_helpers", run(Rand(3)))
report("leaf_helpers_2", run(Rand(11)))
'''

CASES["typed_int_loops"] = r'''
def divs(n, step, base):
    acc = 0
    fl = 0.0
    out = []
    for i in range(n):
        a = base + i * step
        b = (i % 13) - 6
        if b == 0:
            b = 7
        q = a // b
        r = a % b
        t = a / b
        acc = acc + q - r
        acc = acc ^ (q & 0xFFFF)
        fl = fl + t
        out.append((q, r, t))
    return acc, fl, out[:3], out[-3:], len(out)


def shifts(n, step, base):
    acc = 0
    out = []
    for i in range(n):
        a = base + i * step
        b = (i % 13) - 6
        p = b ** 3
        s = a << (i % 5)
        u = a >> (i % 7)
        acc = acc + p + u
        acc = acc ^ (s & 0xFFFF)
        out.append((p, s, u))
    return acc, out[:3], out[-3:]


def overflow(n, start):
    x = start
    out = []
    for i in range(n):
        x = x + i
        y = x * 3
        z = y - x * 2
        w = -x
        v = x * x
        out.append((x, y, z, w, v % 1000003, abs(w)))
    return out[-4:], out[n // 2]


def boundary(n):
    big = 2**53
    out = []
    for i in range(n):
        a = big + i
        f = a * 1.0
        out.append((a == f, int(f) == a, f == a + 0.0, a < f, a // 2, a % 3, float(a) / 2))
    return out[:4], out[-2:]


def bools(n):
    acc = 0
    for i in range(n):
        t = i % 2 == 0
        acc = acc + t
        acc = acc + True * 2
        acc = acc - (t * i)
    return acc


def negzero(n):
    out = []
    for i in range(n):
        x = -0.0 * i
        y = 0.0 - x
        z = x / (i + 1)
        out.append((repr(x), repr(y), repr(z), x == 0, repr(-x), repr(x * -1)))
    return out[:2], out[-2:]


for step, base in ((1, 0), (7, -1000), (2**40, -(2**62)), (2**61, 2**62), (3, -(2**63))):
    print(divs(400, step, base))
    print(shifts(400, step, base))
for start in (0, 2**31, 2**62 - 1000, -(2**62), 2**63 - 50000, 3037000000):
    print(overflow(400, start))
print(boundary(400))
print(bools(400))
print(negzero(400))
'''

CASES["random_int_programs"] = r'''
CONSTS = ["0", "1", "-1", "3", "-7", "255", "2**31", "2**32 - 1", "2**53", "2**62", "2**63 - 1",
          "-(2**63)", "2**64", "10**18", "True", "False"]
OPS = ["+", "-", "*", "//", "%", "&", "|", "^"]


SMALL = ["0", "1", "-1", "3", "-7", "255", "2147483648", "9007199254740992", "4611686018427387904",
         "9223372036854775807", "-9223372036854775807"]


def expr(rng, depth, typed=False):
    if depth == 0 or rng.next(3) == 0:
        return rng.pick(["i", "a", "b", "acc"] + (SMALL if typed else CONSTS))
    k = rng.next(5 if typed else 8)
    left = expr(rng, depth - 1, typed)
    right = expr(rng, depth - 1, typed)
    if k < 5:
        op = rng.pick(OPS)
        if op in ("//", "%"):
            return "(%s %s ((%s) or 7))" % (left, op, right)
        return "(%s %s %s)" % (left, op, right)
    if k == 5:
        return "(-(%s))" % left
    if k == 6:
        return "((%s) >> (i %% 9))" % left
    return "((%s) << (i %% 5))" % left


def program(rng, idx, typed=False):
    e1 = expr(rng, 3, typed)
    e2 = expr(rng, 3, typed)
    e3 = expr(rng, 2, typed)
    return """
def f%d(n, a, b):
    acc = 0
    out = 0
    for i in range(n):
        x = %s
        y = %s
        acc = (acc + x - y) %% 1000000007
        out = out ^ (%s)
        acc = acc + (x > y) - (x < y)
    return acc, out
""" % (idx, e1, e2, e3)


rng = Rand(2024)
results = []
for idx in range(80):
    src = program(rng, idx, typed=idx % 2 == 0)
    ns = {}
    exec(src, ns)
    f = ns["f%d" % idx]
    for a, b in ((3, -5), (2**62, -(2**62)), (2**63 - 1, 1), (-(2**63), -1), (10**20, 7)):
        results.append(f(N, a, b))
report("random_int_programs", results)
'''

CASES["typed_float_loops"] = r'''
import math


def loop(n, seed):
    xs = []
    acc = 0.0
    nan = float("nan")
    for i in range(n):
        x = (i * seed % 97) / 7.0 - 6.5
        y = x * x - 3.25
        z = y / (x + 0.5) if x != -0.5 else 0.0
        acc = acc + z
        w = nan if i % 11 == 0 else z
        xs.append(w)
    finite = [x for x in xs if x == x]
    return (repr(acc), sorted(finite)[:3], max(finite), min(finite), len(finite),
            sum(1 for x in xs if x != x), "%.6f %.3e %g" % (acc, acc, acc), f"{acc:.4f}",
            round(acc, 3), math.floor(acc), math.ceil(acc), int(acc), acc // 1, acc % 1)


print(loop(400, 13))
print(loop(400, 31))
'''

ENVS = [
    {},
    {"WEAVEPY_JIT_THRESHOLD": "2"},
    {"WEAVEPY_FRAME_JIT_HOT": "50"},
    {"WEAVEPY_TIER2": "0"},
    {"WEAVEPY_JIT": "0"},
]

# CPython 3.14's output for each case.
EXPECTED = {
    'str_surrogates': 'str_surrogates 300 3047311255 [(\'日本\\x00\', \'日本\\x00!\', 3, \'本\\x00\', \'\\x00本日\', \'\\x00\', \'日\', \'日本|\\x00|-\', "日本/\'\\\\x00\'/  1", "日本:\'\\\\x00\'", \'日本\\x00\', b\'\\xe6\\x97\\xa5\\xe6\\x9c\\xac\\x00\', b\'\\x00\', b\'\\x00\', \'\\\\xed\\\\xb3\\\\x84\', \'\', \'日本\\x00\', "\'日本\\\\x00\'", "\'\\\\u65e5\\\\u672c\\\\x00\'", False, False, False, 1, 2, True, 2, True, \'日本\\x00\', \'日本\\x00\', [\'日\nstr_surrogates_2 300 52953394 [(\'€\\t\\n\', \'€\\t\\n!\', 3, \'\\t\\n\', \'\\n\\t€\', \'\\n\', \'€\', \'€|\\t\\n|-\', "€/\'\\\\t\\\\n\'/  2", "€:\'\\\\t\\\\n\'", \'€\\t\\n\', b\'\\xe2\\x82\\xac\\t\\n\', \'UnicodeEncodeError\', \'UnicodeEncodeError\', \'€\', \'\\udce2\\udc82\\udcac\', \'€\\t\\n\', "\'€\\\\t\\\\n\'", "\'\\\\u20ac\\\\t\\\\n\'", False, False, False, 1, 2, True, 1, True, \'€\\t\\n\', \'€\\t\\n\', [\'\n',
    'os_fs_names': 'os_fs_names 300 4002017692 [(\'plain\', "\'plain\'", True, \'/tmp/plain\', \'plain\', True, \'plain\', \'plain\'), (\'a\\udcffb\', "\'a\\\\udcffb\'", True, \'/tmp/a\\udcffb\', \'a\\udcffb\', True, \'a\\udcffb\', \'a\\udcffb\'), (\'\\udced\\udca0\\udc80\', "\'\\\\udced\\\\udca0\\\\udc80\'", True, \'/tmp/\\udced\\udca0\\udc80\', \'\\udced\\udca0\\udc80\', True, \'\\udced\\udca0\\udc80\n',
    'escaped_names': 'escaped_names 300 461174030 [(\'plain.txt\', "\'plain.txt\'", \'plain.txt plain.txt   |   plain.txt|plain.txt\\n\', \'plain.txt\', \'plain.txt\', \'plain.txt\', True, [\'0x70\', \'0x6c\', \'0x61\', \'0x69\', \'0x6e\', \'0x2e\', \'0x74\', \'0x78\', \'0x74\'], \'plain.txt.....\', \'pla\', 9, True, False, [\'plain.txt\'], (\'plain\', \'.\', \'txt\')), (\'dir/\\udce2\\udc82\\u\n',
    'surrogate_routes': 'surrogate_routes 300 3126620246 [(1, [97, 233], True, [1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1], [True, True, True, True, True, True, True, True, True, True, True], 11, False, [\'a\', \'a\', \'a\'], \'é\', [b\'a\', b\'a\'], 11, 0, True, "\'a\'"), (1, [8364], True, [1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1], [True, True, True, True, True, True, True, True, True,\n',
    'bytes_ops': "bytes_ops 300 2892209280 [(b'\\xc4\\x80\\x00\\x01\\x02\\x03\\x04', b'\\x80\\x00', 196, b'\\x04', b'\\xc4\\x80-\\x00\\x01\\x02\\x03\\x04', 2, True, b'R\\x00\\x01\\x02\\x03\\x04', 'c4800001020304', True, b'\\xc4\\x80\\x00\\x01\\x02\\x03\\x04', bytearray(b'\\xc4\\x80\\x00\\x01\\x02\\x03\\x04\\x00'), b'\\xc4\\x80\\x00\\x01\\x02\\x03\\x04\\x00!', bytearray(b'\\x80\\x00\\x01\\x\n",
    'int_edges': "int_edges 600 3950306163 [([4611686018427387904, 4611686018427387904, 0, False, False, 0, 4611686018427387904, 4611686018427387904, -4611686018427387904, 4611686018427387904, -4611686018427387905, 'ZeroDivisionError', 'ZeroDivisionError', 'ZeroDivisionError', 'ZeroDivisionError', 1, None, 4611686018427387904, 46116860184273\n",
    'float_edges': "float_edges 300 555764892 [('+inf', '00000inf', True, False, False, True, 'inf', 'inf|inf|inf', 'inf', inf, 1.0, True, True, True, True, [-2.5, 0.0, 0.0, 0.1, 3.5], 3.5, -2.5, None, True), ('0000.000', '0.1', False, True, True, False, '0.0', '0.000|0|0.0', '0.00', 0.0, 1.0, True, True, True, True, [-0.0, 2.5, 123456789.125, \n",
    'mixed_compares': "mixed_compares 300 2466693269 [([True, False, True, False], [False, True, True, False], [True, False, True, False], [False, 0, True, True, 2.0], [2.0, True, True, False, 0], [(1, 'x'), (1.0, 'x'), (2, 'x'), (2, 'x')], [(2, 'x'), (2, 'x'), (1, 'x'), (1.0, 'x')], [1, 1], 2.0, False, [False, 0, True, True, 2.0], [False, False, True\n",
    'equal_keys': 'equal_keys 300 241986941 [([(\'(1.0,)\', \'0\'), (\'0.0\', \'0\'), (\'True\', \'0\')], [\'bool\', \'float\', \'tuple\'], 3, [\'(1.0,)\', \'0.0\', \'True\'], True, True, 0, \'0\', \'0\', 2, {1: \'c\'}, {True: 2}, {0}, True, {1: None}, [True], True, \'t\'), ([("\'1\'", \'1\'), (\'(1,)\', \'1\'), (\'0.0\', \'1\'), (\'2\', \'1\'), (\'True\', \'1\')], [\'bool\', \'float\', \'int\', \'st\n',
    'exceptions': "exceptions 300 3550317893 [(('context', None, 'E1', False, [('work', 2), ('chain', 6)], [('chain', 2), ('boom', 2)]), [('E1', 1), ('rest', ['E2', 'ValueError'])], ['try', 'finally', 'E1', 'E1', None], ['E1: 0\\n']), (('cause', 'KeyError', 'KeyError', True, [('work', 2), ('chain', 5)], [('chain', 2), ('boom', 4)]), [('E1', 1),\n",
    'generators': "generators 300 1018507958 [[0, 1, 2, ('caught', 'boom0'), [0, 1, 2, ('returned', 0)], 0, 1, 2, 3, 4, ('returned', 14), ('stop', None), 1, ['exit', 'finally'], 0, [0, 2, 4]], [0, 1, 2, ('caught', 'boom1'), [0, 1, 2, ('returned', 0)], 0, 1, 2, 3, 4, ('returned', 14), ('stop', None), 1, ['exit', 'finally'], 0, [0, 2, 4]], [0, 1\n",
    'coroutines': 'coroutines 300 1254401997 [[[(\'ready\', 0), (\'tick\', 0), (\'done\', (\'mid\', ((0, \'s1\'), \'s2\')))], [(\'ready\', 1), (\'done\', (\'caught\', "\'k0\'"))], [(\'ready\', 2), (\'tick\', 2), (\'done\', ((2, \'x\'), \'y\'))], True], [[(\'ready\', 1), (\'tick\', 1), (\'done\', (\'mid\', ((1, \'s1\'), \'s2\')))], [(\'ready\', 1), (\'done\', (\'caught\', "\'k0\'"))], [(\'ready\nasyncio 44850\n',
    'tracing_midloop': "90300\n12\n90300\n6\n90300\n[('call', 0), ('line', 1), ('line', 2), ('return', 2), ('call', 0), ('line', 1), ('line', 2), ('return', 2), ('call', 0), ('line', 1), ('line', 2), ('return', 2), ('prof', 'call'), ('prof', 'return'), ('prof', 'call'), ('prof', 'return')]\n20\n90300 ['start', 'start', 'start', 'start']\n",
    'tracing_inside_hot_loop': "134550 [('prof', 'call'), ('prof', 'return'), ('prof', 'call'), ('prof', 'return'), ('prof', 'call'), ('prof', 'return')]\n44850 [5, 6, 2, 3, 5, 6, 2, 7]\n134550 [('start', 'g'), ('return', 'g', 888), ('start', 'g'), ('return', 'g', 891), ('start', 'g'), ('return', 'g', 894), ('start', 'g'), ('return', 'g', 897)]\n44850 [('ftrace', 'line', 7), ('ftrace', 'line', 2), ('ftrace', 'line', 3), ('ftrace', 'line', 7), ('ftrace', 'line', 2), ('ftrace', 'line', 8)]\n",
    'recursion_limits': "500\n['RecursionError', 'repr-RecursionError', 100, 'RecursionError', 'repr-RecursionError', 100, 'RecursionError', 'repr-RecursionError', 100, 'RecursionError', 'repr-RecursionError', 100, 'RecursionError', 'repr-RecursionError', 100] 900\n",
    'leaf_helpers': 'leaf_helpers 600 3641074864 [[\'a\\udcc4\', \'\', \'A\', "\'a\'", \'<a>\', \'a\', b\'a\', b\'a\', \'a\\udcc4\', False, True, 2, \'(1,)\', True, False, \'a\', True, 4.656612873077393e-10, 0, 1, 1], [\'😀é\', \'\', \'😀\', "\'😀\'", \'<😀>\', \'😀\', b\'\\xf0\\x9f\\x98\\x80\', b\'\\\\U0001f600\', \'😀é\', False, False, 2, \'True\', True, False, \'😀\', False, -1.0842021724855044e-19, -1\nleaf_helpers_2 600 1314736723 [[\'a\\udcc4\', \'\', \'A\', "\'a\'", \'<a>\', \'a\', b\'a\', b\'a\', \'a\\udcc4\', False, True, 2, \'True\', True, False, \'a\', False, 7.0, 7, 0, 343], [\'😀é\', \'\', \'😀\', "\'😀\'", \'<😀>\', \'😀\', b\'\\xf0\\x9f\\x98\\x80\', b\'\\\\U0001f600\', \'😀é\', False, False, 2, \'True\', True, False, \'😀\', False, -1.0842021724855044e-19, -1, -922337203685\n',
    'typed_int_loops': "(-302323, 1014.9428571428572, [(0, 0, -0.0), (-1, -4, -0.2), (-1, -2, -0.5)], [(397, 0, 397.0), (199, 0, 199.0), (133, 0, 133.0)], 400)\n(13411, [(-216, 0, 0), (-125, 2, 0), (-64, 8, 0)], [(1, 1588, 12), (8, 3184, 6), (27, 6384, 399)])\n(-540233, 3292.6952380952366, [(166, -4, 166.66666666666666), (198, -3, 198.6), (246, -2, 246.5)], [(1779, 0, 1779.0), (893, 0, 893.0), (597, 2, 597.6666666666666)], 400)\n(-7387, [(-216, -1000, -1000), (-125, -1986, -497), (-64, -3944, -247)], [(1, 7116, 55), (8, 14288, 27), (27, 28688, 1793)])\n(-17578191952580047213, -1.757819195258001e+19, [(768614336404564650, -4, 7.686143364045646e+17), (922336983783152025, -3, 9.22336983783152e+17), (1152920954851033088, 0, 1.1529209548510331e+18)], [(-4611249512311160832, 0, -4.611249512311161e+18), (-2305624206399766528, 0, -2.3056242063997665e+18), (-1537082437762635094, 2, -1.537082437762635e+18)], 400)\n(-526211677489230512533, [(-216, -4611686018427387904, -4611686018427387904), (-125, -9223369837831520256, -2305842459457880064), (-64, -18446735277616529408, -1152920954851033088)], [(1, -18444998049244643328, -144101547259723776), (8, -36889987302396264448, -72050756449992704), (27, -73779957012606484480, -4611247313287905280)])\n(2357878199788282858854, 2.357878199788283e+21, [(-768614336404564651, -2, -7.686143364045646e+17), (-1383505805528216372, -4, -1.3835058055282163e+18), (-2305843009213693952, 0, -2.305843009213694e+18)], [(920031360676263886848, 0, 9.200313606762639e+20), (461168601842738790400, 0, 4.611686018427388e+20), (308214348898230424917, 1, 3.082143488982304e+20)], 400)\n(52811947419119877357163, [(-216, 4611686018427387904, 4611686018427387904), (-125, 13835058055282163712, 3458764513820540928), (-64, 36893488147419103232, 2305843009213693952)], [(1, 3680125442705055547392, 28750980021133246464), (8, 7378697629483820646400, 14411518807585587200), (27, 14794288747115060396032, 924643046694691274752)])\n(-35158615788106174858, -3.515861578810593e+19, [(1537228672809129301, -2, 1.5372286728091292e+18), (1844674407370955161, 0, 1.8446744073709553e+18), (2305843009213693950, -2, 2.305843009213694e+18)], [(-9223372036854774617, 0, -9.223372036854775e+18), (-4611686018427387307, 0, -4.6116860184273874e+18), (-3074457345618258204, 1, -3.0744573456182584e+18)], 400)\n(-1052473218517975292580, [(-216, -9223372036854775808, -9223372036854775808), (-125, -18446744073709551610, -4611686018427387903), (-64, -36893488147419103208, -2305843009213693951)], [(1, -36893488147419098468, -288230376151711707), (8, -73786976294838196912, -144115188075855854), (27, -147573952589676393776, -9223372036854774611)])\n([(78606, 235818, 78606, -78606, 884702, 78606), (79003, 237009, 79003, -79003, 455286, 79003), (79401, 238203, 79401, -79401, 499889, 79401), (79800, 239400, 79800, -79800, 20896, 79800)], (20100, 60300, 20100, -20100, 8788, 20100))\n([(2147562254, 6442686762, 2147562254, -2147562254, 164188, 2147562254), (2147562651, 6442687953, 2147562651, -2147562651, 635996, 2147562651), (2147563049, 6442689147, 2147563049, -2147563049, 536231, 2147563049), (2147563448, 6442690344, 2147563448, -2147563448, 867284, 2147563448)], (2147503748, 6442511244, 2147503748, -2147503748, 510310, 2147503748))\n([(4611686018427465510, 13835058055282396530, 4611686018427465510, -4611686018427465510, 965198, 4611686018427465510), (4611686018427465907, 13835058055282397721, 4611686018427465907, -4611686018427465907, 852946, 4611686018427465907), (4611686018427466305, 13835058055282398915, 4611686018427466305, -4611686018427466305, 888055, 4611686018427466305), (4611686018427466704, 13835058055282400112, 4611686018427466704, -4611686018427466704, 72910, 4611686018427466704)], (4611686018427407004, 13835058055282221012, 4611686018427407004, -4611686018427407004, 484899, 4611686018427407004))\n([(-4611686018427309298, -13835058055281927894, -4611686018427309298, 4611686018427309298, 500078, 4611686018427309298), (-4611686018427308901, -13835058055281926703, -4611686018427308901, 4611686018427308901, 959504, 4611686018427308901), (-4611686018427308503, -13835058055281925509, -4611686018427308503, 4611686018427308503, 217601, 4611686018427308503), (-4611686018427308104, -13835058055281924312, -4611686018427308104, 4611686018427308104, 276763, 4611686018427308104)], (-4611686018427367804, -13835058055282103412, -4611686018427367804, 4611686018427367804, 240201, 4611686018427367804))\n([(9223372036854804414, 27670116110564413242, 9223372036854804414, -9223372036854804414, 523766, 9223372036854804414), (9223372036854804811, 27670116110564414433, 9223372036854804811, -9223372036854804811, 616792, 9223372036854804811), (9223372036854805209, 27670116110564415627, 9223372036854805209, -9223372036854805209, 434521, 9223372036854805209), (9223372036854805608, 27670116110564416824, 9223372036854805608, -9223372036854805608, 979344, 9223372036854805608)], (9223372036854745908, 27670116110564237724, 9223372036854745908, -9223372036854745908, 998234, 9223372036854745908))\n([(3037078606, 9111235818, 3037078606, -3037078606, 540538, 3037078606), (3037079003, 9111237009, 3037079003, -3037079003, 877012, 3037079003), (3037079401, 9111238203, 3037079401, -3037079401, 669280, 3037079401), (3037079800, 9111239400, 3037079800, -3037079800, 919733, 3037079800)], (3037020100, 9111060300, 3037020100, -3037020100, 757761, 3037020100))\n([(True, True, True, False, 4503599627370496, 2, 4503599627370496.0), (False, False, True, False, 4503599627370496, 0, 4503599627370496.0), (True, True, True, False, 4503599627370497, 1, 4503599627370497.0), (False, False, True, True, 4503599627370497, 2, 4503599627370498.0)], [(True, True, True, False, 4503599627370695, 1, 4503599627370695.0), (False, False, True, True, 4503599627370695, 2, 4503599627370696.0)])\n-38800\n([('-0.0', '0.0', '-0.0', True, '0.0', '0.0'), ('-0.0', '0.0', '-0.0', True, '0.0', '0.0')], [('-0.0', '0.0', '-0.0', True, '0.0', '0.0'), ('-0.0', '0.0', '-0.0', True, '0.0', '0.0')])\n",
    'random_int_programs': 'random_int_programs 400 2830801631 [(168674248, 0), (168742948, 0), (844361223, 0), (141269692, 0), (845923247, 0), (569614621, 0), (569614621, 0), (569614621, 0), (569614621, 0), (569614621, 0), (675878303, 0), (112679648, 0), (379122674, 0), (675877699, 0), (379121770, 0), (999953041, 0), (817064964, 0), (492865556, 0), (999954257,\n',
    'typed_float_loops': "('-85.02590312302489', [-21.8571428571428, -21.8571428571428, -21.8571428571428], 19.8571428571428, -21.8571428571428, 363, 37, '-85.025903 -8.503e+01 -85.0259', '-85.0259', -85.026, -86, -85, -85, -86.0, 0.9740968769751106)\n('-74.14680508035559', [-21.8571428571428, -21.8571428571428, -21.8571428571428], 19.8571428571428, -21.8571428571428, 363, 37, '-74.146805 -7.415e+01 -74.1468', '-74.1468', -74.147, -75, -74, -74, -75.0, 0.8531949196444089)\n",
}


def run_case(name, extra=None):
    env = dict(os.environ, PYTHONHASHSEED="0", **(extra or {}))
    result = subprocess.run(
        [sys.executable, "-c", PRELUDE + CASES[name]],
        capture_output=True,
        text=True,
        env=env,
        timeout=300,
    )
    return result.returncode, result.stdout, result.stderr


class ValueFastPathTests(unittest.TestCase):
    pass


def _make(name):
    def test(self):
        for extra in ENVS:
            with self.subTest(env=extra):
                code, out, err = run_case(name, extra)
                self.assertEqual(code, 0, err[-3000:])
                self.assertEqual(out, EXPECTED[name])

    return test


for _name in CASES:
    setattr(ValueFastPathTests, "test_" + _name, _make(_name))


if __name__ == "__main__":
    unittest.main()
