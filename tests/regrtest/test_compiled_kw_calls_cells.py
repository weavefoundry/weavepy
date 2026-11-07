"""Compiled keyword calls of functions with cell variables keep CPython's
binding: a parameter a nested function captures (passed by keyword, by
position, or left to its default), a keyword-only one, a cell the body
rebinds after the closure is made, a closure over the caller's own cells,
and a recursive call."""


def parse(qs, sep="&", encoding="utf-8", *, level=1):
    def unq(s):
        return s.upper() + encoding + str(level)
    return [unq(p) for p in qs.split(sep)]


def rebind(x, y=1):
    def get():
        return x + y
    first = get()
    x = 100
    return first, get()


def outer(n):
    base = n * 10

    def inner(a, k=0):
        def deep():
            return a + k + base
        return deep()
    return inner(1, k=n)


def fact(n, acc=1):
    def step():
        return n * acc
    return acc if n <= 1 else fact(n - 1, acc=step())


def calls(n):
    out = []
    for i in range(n):
        out.append(parse("a&b", encoding="x", sep="&", level=i % 3))
        out.append(parse("c;d", ";", level=2))
        out.append(parse(qs="e"))
        out.append(rebind(i, y=2))
        out.append(outer(i))
        out.append(fact(5, acc=1))
    return out


out = calls(3000)
assert out[0] == ["Ax0", "Bx0"] and out[6] == ["Ax1", "Bx1"]
assert out[1] == ["Cutf-82", "Dutf-82"] and out[2] == ["Eutf-81"]
assert out[3] == (2, 102) and out[-3] == (3001, 102)
assert out[4] == 1 and out[-2] == 1 + 2999 + 29990
assert out[5] == 120
print("ok")
