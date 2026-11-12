"""The `AttributeError` compiled code raises for an attribute certainly
missing keeps CPython's message, `name`, `obj` and `args`: a plain
instance's, an unset `__slots__` member's (which names the class by its
qualified name), a nested class's, and a member assigned and deleted
after the site warms up."""


class Plain:
    pass


class Slotted:
    __slots__ = ("cached", "raw")

    def __init__(self):
        self.raw = 1


class Outer:
    class Inner:
        __slots__ = ("v",)


def miss(o):
    try:
        return o.cached
    except AttributeError as e:
        return e


def miss_v(o):
    try:
        return o.v
    except AttributeError as e:
        return e


for _ in range(3000):
    e = miss(Plain())
p, s = Plain(), Slotted()
e = miss(p)
assert str(e) == "'Plain' object has no attribute 'cached'", str(e)
assert e.name == "cached" and e.obj is p and e.args == (str(e),)
for _ in range(3000):
    e = miss(s)
assert str(e) == "'Slotted' object has no attribute 'cached'", str(e)
assert e.name == "cached" and e.obj is s and type(e) is AttributeError
s.cached = 5
assert miss(s) == 5
del s.cached
assert miss(s).name == "cached"
i = Outer.Inner()
for _ in range(3000):
    e = miss_v(i)
assert str(e) == "'Outer.Inner' object has no attribute 'v'", str(e)
i.v = 2
assert miss_v(i) == 2
print("ok")
