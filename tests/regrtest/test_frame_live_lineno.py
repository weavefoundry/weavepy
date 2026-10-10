"""A frame object of a live activation reports the line its activation
is at, however the activation runs (interpreted, inline, or compiled once
hot), and a frame object outliving its activation keeps the line it left
at. Each check runs cold and again once its loops and callees are hot."""

import sys

N = 30000

with open(__file__) as _f:
    _SRC = _f.read().splitlines()


def L(tag):
    """The line carrying the marker comment `tag`."""
    marker = "# " + tag
    (line,) = [i + 1 for i, s in enumerate(_SRC) if s.endswith(marker)]
    return line


def self_reads(n):
    fr = sys._getframe()
    bad = 0
    for i in range(n):
        a = fr.f_lineno  # sr-a
        x = i + 1
        b = fr.f_lineno  # sr-b
        if (a, b) != (L("sr-a"), L("sr-b")):
            bad += 1
    return bad


def self_getattr(n):
    fr = sys._getframe()
    seen = set()
    for i in range(n):
        seen.add(getattr(fr, "f_lineno"))  # sg-a
        seen.add(getattr(fr, "f_lineno"))  # sg-b
    return sorted(seen)


def keep():
    return sys._getframe(1)


def held_by_callee_then_read(a):
    fr = keep()
    x = a + 1
    first = fr.f_lineno  # hc-a
    y = x * 2
    return fr, first, fr.f_lineno  # hc-b


F = None


def stores_own_frame(a):
    global F
    F = sys._getframe()
    x = a + 1
    return x  # so-ret


def returns_own_frame(a):
    fr = sys._getframe()
    x = a + 1
    y = x * 2
    return fr  # ro-ret


def walk():
    f = sys._getframe(1)
    out = []
    while f is not None and f.f_code.co_name != "<module>":
        out.append((f.f_code.co_name, f.f_lineno))
        f = f.f_back
    return out


def d1():
    return walk()  # d1-call


def d2():
    r = d1()  # d2-call
    return r


class Getattr:
    def __getattr__(self, name):
        return sys._getframe(1).f_lineno


class Prop:
    @property
    def line(self):
        return sys._getframe(1).f_lineno


def escapes(n):
    g, p = Getattr(), Prop()
    seen = set()
    for i in range(n):
        seen.add(g.anything)  # es-a
        z = 0
        seen.add(p.line)  # es-b
        sorted([2, 1], key=lambda v: seen.add(sys._getframe(1).f_lineno) or v)  # es-c
    return sorted(seen)


def raiser(a):
    if a >= 0:
        raise ValueError(a)  # ra-raise


def catches(n):
    lines = set()
    for i in range(n):
        try:
            raiser(i)  # ca-call
        except ValueError as e:
            tb = e.__traceback__
            lines.add((tb.tb_lineno, tb.tb_frame.f_lineno))  # ca-add
            lines.add((tb.tb_next.tb_lineno, tb.tb_next.tb_frame.f_lineno))
    return sorted(lines)


def gen():
    yield 1  # ge-a
    x = 2
    yield x  # ge-b


def gen_lines(n):
    seen = set()
    for i in range(n):
        g = gen()
        next(g)
        seen.add(g.gi_frame.f_lineno)
        next(g)
        seen.add(g.gi_frame.f_lineno)
    return sorted(seen)


def run(n):
    assert self_reads(n) == 0
    got = self_getattr(n)
    assert got == [L("sg-a"), L("sg-b")], got
    seen = set()
    for i in range(n):
        fr, first, second = held_by_callee_then_read(i)
        seen.add((first, second, fr.f_lineno, fr.f_back.f_code.co_name))
        stores_own_frame(i)
        seen.add(("F", F.f_lineno))
        seen.add(("R", returns_own_frame(i).f_lineno))
    want = {
        (L("hc-a"), L("hc-b"), L("hc-b"), "run"),
        ("F", L("so-ret")),
        ("R", L("ro-ret")),
    }
    assert seen == want, seen
    for i in range(n // 10):
        w = d2()  # run-d2
    assert w == [("d1", L("d1-call")), ("d2", L("d2-call")), ("run", L("run-d2"))], w
    got = escapes(n)
    assert got == [L("es-a"), L("es-b"), L("es-c")], got
    got = catches(n // 10)
    assert got == [(L("ra-raise"), L("ra-raise")), (L("ca-call"), L("ca-add"))], got
    got = gen_lines(n // 10)
    assert got == [L("ge-a"), L("ge-b")], got


run(10)
run(N)
run(N)
