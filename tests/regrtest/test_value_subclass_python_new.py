"""Subclasses of value types with a Python `__new__`.

`configparser`'s `_Line(str)` has a `__new__` that chains to
`str.__new__` and an `__init__` that sets its slots. The interpreter and
compiled code run such a `__new__` as an inline activation, then the
`__init__`: the payload is the base's, `__init__` runs exactly once and
only for an instance of the class, and errors propagate. Run it past the
compile threshold.
"""


class Line(str):
    __slots__ = ("clean", "inits")

    def __new__(cls, val, *args, **kwargs):
        return super().__new__(cls, val)

    def __init__(self, val, comments=None):
        self.clean = val.strip()
        self.inits = getattr(self, "inits", 0) + 1


class Num(int):
    def __new__(cls, v, scale=1):
        return super().__new__(cls, v * scale)


class Other(tuple):
    def __new__(cls, *items):
        if items and items[0] == "str":
            return "not an instance"
        return super().__new__(cls, items)

    def __init__(self, *items):
        self.seen = items


class Bad(str):
    def __new__(cls, val):
        raise ValueError(val)


def build(i):
    line = Line(" k = %d " % i, comments=None)
    assert line == " k = %d " % i and line.clean == "k = %d" % i, line
    assert line.inits == 1 and type(line) is Line
    n = Num(i, scale=2)
    assert n == 2 * i and type(n) is Num
    o = Other(1, 2)
    assert o == (1, 2) and o.seen == (1, 2)
    assert Other("str") == "not an instance"
    try:
        Bad("x")
    except ValueError as e:
        assert e.args == ("x",)
    else:
        raise AssertionError("no ValueError")
    return line


for i in range(3000):
    assert isinstance(build(i), str)
lines = list(map(lambda s: Line(s, comments=None), ["a ", " b"]))
assert [x.clean for x in lines] == ["a", "b"]
