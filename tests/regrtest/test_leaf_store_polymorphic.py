"""Attribute stores from frameless leaves keep Python's semantics when one
store site sees instances of several classes.

A setter (`self._w = v`) or a base `__init__` run on several classes'
instances stores through the plain instance-dict path for a class the
site's cache doesn't serve. That path must still yield to everything
that isn't a plain store: a property or other data descriptor for the
name, a `__setattr__` override, `__slots__`, and a class changed since.
"""


class Shape:
    def __init__(self, name):
        self.name = name

    @property
    def width(self):
        return self._w

    @width.setter
    def width(self, v):
        self._w = v


class Rect(Shape):
    def __init__(self, w):
        super().__init__("rect")
        self._w = w


class Square(Rect):
    def __init__(self, s):
        super().__init__(s)
        self.name = "square"


class Circle(Shape):
    def __init__(self, r):
        super().__init__("circle")
        self._w = r


class Described(Shape):
    """`_w` is a property here: stores go through its setter."""

    def __init__(self, w):
        super().__init__("described")
        self.stored = []
        self._w = w

    @property
    def _w(self):
        return self.stored[-1] if self.stored else None

    @_w.setter
    def _w(self, v):
        self.stored.append(v)


class Logged(Shape):
    log = []

    def __init__(self, w):
        super().__init__("logged")
        self._w = w

    def __setattr__(self, name, value):
        Logged.log.append(name)
        object.__setattr__(self, name, value)


class Slotted(Shape):
    __slots__ = ("_w", "name")

    def __init__(self, w):
        Shape.__init__(self, "slotted")
        self._w = w


def bump(shapes, n):
    for _ in range(n):
        for s in shapes:
            s.width = s.width + 1


plain = [Rect(1), Square(2), Circle(3)]
bump(plain, 3000)
assert [s.width for s in plain] == [3001, 3002, 3003]
assert [s.name for s in plain] == ["rect", "square", "circle"]
assert all("_w" in vars(s) for s in plain)

d = Described(5)
Logged.log.clear()
lg = Logged(7)
sl = Slotted(9)
mixed = plain + [d, lg, sl]
bump(mixed, 100)
assert [s.width for s in plain] == [3101, 3102, 3103]
assert d.width == 105 and d.stored[-3:] == [103, 104, 105] and "_w" not in vars(d)
assert lg.width == 107 and Logged.log.count("_w") == 101
assert sl.width == 109 and "_w" not in sl.__dict__

# A class changed since: a property added for the name takes over.
Circle._w = property(lambda self: 0, lambda self, v: None)
c = plain[2]
bump([c], 10)
assert c.width == 0
del Circle._w
assert c.width == 3103
bump(plain, 10)
assert [s.width for s in plain] == [3111, 3112, 3113]

print("ok")
