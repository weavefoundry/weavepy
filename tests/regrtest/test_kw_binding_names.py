"""Keyword calls bound from the call site's names: callees with closures
(cells), bound methods, `**kwargs` and `*args`, `f(*a, **d)`, keywords out
of parameter order, defaults, and every binding error, each run enough
times to take the interpreter's warmed call paths."""

ROUNDS = 200


def make():
    secret = 7

    def cl(a, b=2, c=3, *, d=4, e=5):
        def inner():
            return secret + a
        return (a, b, c, d, e, inner())

    def cl_star(a, *rest, k=1, **kw):
        def inner():
            return secret
        return (a, rest, k, sorted(kw.items()), inner())

    def cl_posonly(a, b, /, c=0, **kw):
        def inner():
            return secret
        return (a, b, c, sorted(kw.items()), inner())

    return cl, cl_star, cl_posonly


cl, cl_star, cl_posonly = make()


class K:
    def __init__(self):
        self.tag = "k"

    def m(self, x, y=10, *, z=20):
        def inner():
            return self.tag
        return (x, y, z, inner())


def expect_error(fn, msg):
    try:
        fn()
    except TypeError as e:
        assert msg in str(e), (msg, str(e))
    else:
        raise AssertionError("no TypeError: " + msg)


k = K()
for _ in range(ROUNDS):
    assert cl(1, b=2) == (1, 2, 3, 4, 5, 8)
    assert cl(1, c=30, b=20) == (1, 20, 30, 4, 5, 8)
    assert cl(a=1) == (1, 2, 3, 4, 5, 8)
    assert cl(1, e=50, d=40) == (1, 2, 3, 40, 50, 8)
    assert cl(1, 2, 3, d=0) == (1, 2, 3, 0, 5, 8)
    assert cl(*(1, 2), **{"e": 9}) == (1, 2, 3, 4, 9, 8)
    assert cl_star(1, 2, 3, k=4, x=5, y=6) == (1, (2, 3), 4, [("x", 5), ("y", 6)], 7)
    assert cl_star(1, x=5) == (1, (), 1, [("x", 5)], 7)
    assert cl_star(a=1, k=2) == (1, (), 2, [], 7)
    assert cl_star(*[1, 2], **{"q": 3}) == (1, (2,), 1, [("q", 3)], 7)
    # A positional-only name given as a keyword lands in `**kw`.
    assert cl_posonly(1, 2, a=3, c=4) == (1, 2, 4, [("a", 3)], 7)
    assert k.m(1, z=3) == (1, 10, 3, "k")
    assert k.m(x=1, y=2) == (1, 2, 20, "k")
    m = k.m
    assert m(1, y=5) == (1, 5, 20, "k")
    # The `**kwargs` dictionary keeps the call's keys and order.
    r = cl_star(0, b=1, a2=2, c=3)
    assert r[3] == [("a2", 2), ("b", 1), ("c", 3)]

    expect_error(lambda: cl(1, a=2), "got multiple values for argument 'a'")
    expect_error(lambda: cl(1, f=2), "got an unexpected keyword argument 'f'")
    expect_error(lambda: cl(b=2), "missing 1 required positional argument: 'a'")
    expect_error(lambda: cl(1, 2, 3, 4, d=1), "takes from 1 to 3 positional arguments but 4 positional arguments (and 1 keyword-only argument) were given")
    expect_error(lambda: cl_posonly(1, c=2), "missing 1 required positional argument: 'b'")
    expect_error(lambda: k.m(1, x=2), "got multiple values for argument 'x'")
    expect_error(lambda: k.m(y=2), "missing 1 required positional argument: 'x'")
    expect_error(lambda: cl(**{"a": 1, "zz": 2}), "got an unexpected keyword argument 'zz'")


def posonly_only(a, /, b):
    def inner():
        return a
    return inner() + b


for _ in range(ROUNDS):
    assert posonly_only(1, b=2) == 3
    expect_error(lambda: posonly_only(a=1, b=2), "positional-only arguments passed as keyword arguments: 'a'")


# Replaced defaults are honored.
def with_defaults(a, b=1, *, c=2):
    def inner():
        return a
    return (inner(), b, c)


for i in range(ROUNDS):
    if i == ROUNDS // 2:
        with_defaults.__defaults__ = (100,)
        with_defaults.__kwdefaults__ = {"c": 200}
    want = (5, 1, 2) if i < ROUNDS // 2 else (5, 100, 200)
    assert with_defaults(a=5) == want, (i, with_defaults(a=5))
print("kw binding names ok")
