"""A `dict` subclass's `__init__` calling `super().__init__(...)` or
`dict.__init__(self, ...)`, and an `__init__` calling
`object.__setattr__(self, name, value)`, run from compiled constructors as
the full call runs them: positional and keyword sources, an overridden
`__setitem__` that `dict.__init__` doesn't call, the errors for a bad
argument and a wrong receiver, a raise inside a source's iteration, and
`__slots__` stores."""


class D(dict):
    def __init__(self, *a, **k):
        super().__init__(*a, **k)


class E(dict):
    def __init__(self, src):
        dict.__init__(self, src, z=0)

    def __setitem__(self, k, v):
        raise AssertionError("dict.__init__ doesn't call __setitem__")


class Plain(dict):
    def __init__(self):
        super().__init__()


class F:
    __slots__ = ("a", "b")

    def __init__(self, a, b):
        object.__setattr__(self, "a", a)
        object.__setattr__(self, "b", b)


def build(n):
    out = 0
    for i in range(n):
        d = D([(1, i)], x=i)
        out += d[1] + d["x"]
        e = E({"k": i})
        out += e["k"] + e["z"]
        p = Plain()
        out += len(p)
        f = F(i, -i)
        out += f.a + f.b
    return out


for _ in range(4):
    assert build(300) == sum(3 * i for i in range(300))


def bad(n):
    seen = 0
    for i in range(n):
        try:
            D(1)
        except TypeError:
            seen += 1
        try:
            D([1, 2, 3])
        except (TypeError, ValueError):
            seen += 1
        try:
            dict.__init__([], {})
        except TypeError:
            seen += 1

        def gen():
            yield ("a", 1)
            raise KeyError(i)

        try:
            D(gen())
        except KeyError as e:
            seen += e.args[0] == i
        try:
            object.__setattr__(F(0, 0), "c", 1)
        except AttributeError:
            seen += 1
    return seen


for _ in range(3):
    assert bad(200) == 1000
print("ok")
