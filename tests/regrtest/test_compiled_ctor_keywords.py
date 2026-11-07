"""A class called with keywords from compiled code (`C(i, b=2)`), whose
`__init__` compiled code enters directly, keeps CPython's results: each
parameter bound by position, keyword or default, keyword-only parameters,
an `__init__` that collects `**kwargs`, a subclass's own `__init__`, and
the `TypeError`s of a missing, unknown or doubled argument."""


class C:
    def __init__(self, a, b=0, *, c=5):
        self.a = a
        self.b = b
        self.c = c


class K:
    def __init__(self, a, **kw):
        self.a = a
        self.kw = kw


class D(C):
    def __init__(self, a, b=1, *, c=7, d=9):
        super().__init__(a, b, c=c)
        self.d = d


class Raises:
    def __init__(self, x, y=0):
        if x < 0:
            raise ValueError(x)
        self.x = x + y


def run(i):
    c = C(i, b=2)
    assert (c.a, c.b, c.c) == (i, 2, 5)
    c = C(a=i, c=1)
    assert (c.a, c.b, c.c) == (i, 0, 1)
    c = C(i, c=3, b=4)
    assert (c.a, c.b, c.c) == (i, 4, 3)
    k = K(i, x=1, y=2)
    assert k.a == i and k.kw == {"x": 1, "y": 2}
    d = D(i, d=3)
    assert (d.a, d.b, d.c, d.d) == (i, 1, 7, 3)
    r = Raises(i, y=1)
    assert r.x == i + 1
    try:
        Raises(-1, y=1)
    except ValueError as e:
        assert e.args == (-1,)
    else:
        raise AssertionError("the __init__'s raise was lost")


for i in range(3000):
    run(i)

for bad in (lambda: C(b=2), lambda: C(1, z=2), lambda: C(1, a=2), lambda: C(1, 2, 3)):
    try:
        bad()
    except TypeError:
        pass
    else:
        raise AssertionError("a bad call bound")
print("ok")
