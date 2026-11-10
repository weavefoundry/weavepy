"""Keyword calls whose keywords aren't the next parameters in order bind as
the interpreter binds them from compiled loops: a keyword past a defaulted
parameter (`f(a, c=3)`), keyword-only parameters given by keyword, keywords
in another order, computed arguments, methods, defaults rebound
(`__defaults__`, `__kwdefaults__`) between calls, and the errors for a
positional-only parameter, a repeated parameter, an unknown keyword and a
missing argument."""


def f(a, b=10, c=20):
    return (a, b, len([c]) and c)


def g(a, *, x=1, y=2):
    return (a, x, y + len("ab"))


def h(p, /, q=5, *, r=6):
    return (p, q, r + 0 * len(""))


def k(s, keep=False, strict=False, encoding="utf-8", errors="replace", mx=None, sep="&", *, level=1):
    return (s, keep, strict, encoding, errors, mx, sep, level + len(""))


class C:
    def m(self, a, b=1, c=2):
        return (a, b, c + len(""))


def run(n):
    out = []
    o = C()
    for i in range(n):
        out.append(f(i, c=3))
        out.append(f(i, c=i, b=-i))
        out.append(f(c=i, a=1))
        out.append(g(i, y=3))
        out.append(g(i, y=i, x=-i))
        out.append(h(i, r=7))
        out.append(h(i, q=8, r=9))
        out.append(k(i, False, False, encoding="u", errors="r", mx=None, sep="&", level=2))
        out.append(k(i, sep=";", level=i))
        out.append(f(len(out), c=str(i)))
        out.append(o.m(i, c=4))
        out.append(o.m(c=i, a=0))
    return out


def expected(n):
    out = []
    for i in range(n):
        out.append((i, 10, 3))
        out.append((i, -i, i))
        out.append((1, 10, i))
        out.append((i, 1, 5))
        out.append((i, -i, i + 2))
        out.append((i, 5, 7))
        out.append((i, 8, 9))
        out.append((i, False, False, "u", "r", None, "&", 2))
        out.append((i, False, False, "utf-8", "replace", None, ";", i))
        out.append((len(out), 10, str(i)))
        out.append((i, 1, 4))
        out.append((0, 1, i))
    return out


for _ in range(4):
    assert run(300) == expected(300)

# Defaults rebound between calls of the compiled loop.
saved = f.__defaults__, g.__kwdefaults__
try:
    f.__defaults__ = (100, 200)
    g.__kwdefaults__ = {"x": 7, "y": 8}
    r = run(5)
    assert r[0] == (0, 100, 3), r[0]
    assert r[3] == (0, 7, 5), r[3]
    f.__defaults__ = None
    try:
        f(1, c=3)
    except TypeError as e:
        assert "b" in str(e), e
    else:
        raise AssertionError("expected TypeError")
    g.__kwdefaults__ = None
    try:
        g(1, y=3)
    except TypeError as e:
        assert "x" in str(e), e
    else:
        raise AssertionError("expected TypeError")
finally:
    f.__defaults__, g.__kwdefaults__ = saved
assert run(5) == expected(5)


def errors(n):
    seen = 0
    for i in range(n):
        try:
            h(p=i)
        except TypeError as e:
            seen += "positional" in str(e)
        try:
            f(i, a=1)
        except TypeError as e:
            seen += "multiple values" in str(e)
        try:
            f(i, d=1)
        except TypeError as e:
            seen += "unexpected keyword" in str(e)
        try:
            f(c=1)
        except TypeError as e:
            seen += "missing" in str(e)
    return seen


for _ in range(3):
    assert errors(300) == 1200
print("ok")
