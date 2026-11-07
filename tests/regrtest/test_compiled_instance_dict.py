"""Compiled code's `o.__dict__` reads keep CPython's results: the
instance's own dict (the same object each time, live to later writes),
a class whose `__dict__` is a property or which gains `__getattr__` after
the site warms up, a `__slots__` class without one, and a site that reads
the attribute of a class as well as of its instances."""


class Plain:
    def __init__(self, v):
        self.v = v


def dicts(objs):
    return [o.__dict__ for o in objs]


objs = [Plain(i) for i in range(3000)]
ds = dicts(objs)
assert ds[5] == {"v": 5} and ds[5] is objs[5].__dict__
objs[5].w = 1
assert ds[5] == {"v": 5, "w": 1}
ds[6]["z"] = 2
assert objs[6].z == 2


class Prop:
    @property
    def __dict__(self):
        return "prop"


class Sub(Plain):
    pass


mixed = [Plain(1), Sub(2), Prop()]
for _ in range(3000):
    assert dicts(mixed) == [{"v": 1}, {"v": 2}, "prop"]


Sub.__getattr__ = lambda self, name: "fallback"
assert dicts([Sub(3)]) == [{"v": 3}]
Sub.__getattribute__ = lambda self, name: "custom"
assert dicts([Sub(3)]) == ["custom"]
del Sub.__getattribute__


class Slotted:
    __slots__ = ("a",)


try:
    dicts([Slotted()])
except AttributeError:
    pass
else:
    raise AssertionError("no AttributeError")

both = [Plain, Plain(4)] * 2000
out = dicts(both)
assert out[-1] == {"v": 4} and type(out[0]).__name__ == "mappingproxy"
assert "__init__" in out[0]
print("ok")
