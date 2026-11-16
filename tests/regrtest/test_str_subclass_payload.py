"""`str(x)` of a `str` subclass's instance in compiled code.

`email`'s `Terminal.value` is `str(self)`. An instance whose class keeps
`str`'s own `__str__` gives back its payload as an exact `str`; a class
that overrides `__str__`, directly, through a base, or after the fact,
is called. Surrogates in the payload survive. Run it past the compile
threshold.
"""


class S(str):
    pass


class Own(str):
    def __str__(self):
        return "own"


class FromBase(Own):
    pass


class Later(str):
    pass


def conv(x):
    return str(x)


for i in range(3000):
    r = conv(S("abc"))
    assert r == "abc" and type(r) is str
    assert conv(Own("x")) == "own" and conv(FromBase("y")) == "own"
    w = conv(S("a\udcacb"))
    assert w == "a\udcacb" and len(w) == 3
    assert conv(Later("l")) == ("later" if i >= 2000 else "l")
    if i == 1999:
        Later.__str__ = lambda self: "later"
    assert str(S("q")) == "q"
