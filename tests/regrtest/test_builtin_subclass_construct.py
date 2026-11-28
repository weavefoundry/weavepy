"""Constructing subclasses of immutable built-ins in compiled code.

`C(x)` for a class that keeps its base's `__new__` and `object.__init__`,
with `x` already of the base's type, wraps `x` itself; other arguments
convert as the base's constructor does; a class with its own `__init__`
or a `__new__` added mid-loop runs them; slots, instance dicts and the
type behave as in CPython. Run it past the compile threshold.
"""


class S(str):
    __slots__ = ("tag",)


class I(int):
    pass


class F(float):
    __slots__ = ()


class T(tuple):
    pass


class B(bytes):
    pass


class FS(frozenset):
    pass


class WithInit(str):
    def __init__(self, v):
        self.seen = v


class Late(str):
    pass


def make(n):
    out = []
    for i in range(n):
        s = S("key")
        s.tag = i
        k = I(i)
        k.extra = -i
        out.append((s, k, F(1.5), T((i, 2)), B(b"ab"), FS(frozenset({1}))))
    return out


N = 3000
for j, (s, k, f, t, b, fs) in enumerate(make(N)):
    assert type(s) is S and s == "key" and s.tag == j
    assert type(k) is I and k == j and k.extra == -j and k + 1 == j + 1
    assert type(f) is F and f == 1.5
    assert type(t) is T and t == (j, 2) and len(t) == 2
    assert type(b) is B and b == b"ab"
    assert type(fs) is FS and fs == {1}


def convert(n):
    out = []
    for _ in range(n):
        out.append((I(True), I("7"), S(12), T([1, 2]), F(3), S(S("x"))))
    return out


for r in convert(N):
    assert r == (1, 7, "12", (1, 2), 3.0, "x")
    assert type(r[0]) is I and type(r[0]) is not bool and type(r[5]) is S


def inits(n):
    return [WithInit("v") for _ in range(n)]


assert all(w.seen == "v" and w == "v" for w in inits(N))


def lates(n):
    return [Late("a") for _ in range(n)]


assert lates(N)[-1] == "a"
Late.__new__ = lambda cls, v: str.__new__(cls, v * 2)
assert lates(3) == ["aa"] * 3
try:
    I(1, 2, 3)
except TypeError:
    pass
else:
    raise AssertionError("too many arguments")
print("ok")
