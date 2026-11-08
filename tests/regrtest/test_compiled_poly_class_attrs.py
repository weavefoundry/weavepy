"""A compiled read of a class attribute through instances of several
classes (`self.unit` in a base class's method) gives each class's current
value: after the class or a base changes it, when an instance shadows it,
when it's deleted, and for a mutable container the class holds."""

import gc
import weakref


class Shape:
    unit = "cm"
    scale = 2
    table = [1]

    def describe(self):
        return self.unit, self.scale, len(self.table)


class Rect(Shape):
    scale = 3


class Square(Rect):
    unit = "in"


class Circle(Shape):
    pass


def run(objs, n):
    out = []
    for i in range(n):
        out.append(objs[i % len(objs)].describe())
    return out[-len(objs):]


objs = [Rect(), Square(), Circle(), Shape()]
expect = [("cm", 3, 1), ("in", 3, 1), ("cm", 2, 1), ("cm", 2, 1)]
for _ in range(3):
    assert run(objs, 400) == expect, run(objs, 400)

# A base's change reaches the subclasses that inherit it.
Shape.unit = "mm"
assert run(objs, 400) == [("mm", 3, 1), ("in", 3, 1), ("mm", 2, 1), ("mm", 2, 1)]
Rect.scale = 5
assert run(objs, 400) == [("mm", 5, 1), ("in", 5, 1), ("mm", 2, 1), ("mm", 2, 1)]

# An instance's own value shadows the class's.
objs[2].unit = "px"
assert run(objs, 400)[2] == ("px", 2, 1)
del objs[2].unit
assert run(objs, 400)[2] == ("mm", 2, 1)

# A deleted override falls back to the base's.
del Square.unit
assert run(objs, 400)[1] == ("mm", 5, 1)

# A mutable container's contents are read afresh.
Shape.table.append(2)
assert run(objs, 400)[0] == ("mm", 5, 2)

# A replaced value is freed when the class lets it go.
class Payload(list):
    pass


Shape.table = Payload([1, 2, 3])
assert run(objs, 400)[3] == ("mm", 2, 3)
ref = weakref.ref(Shape.table)
Shape.table = [0]
gc.collect()
assert ref() is None, "a replaced class attribute is still alive"
assert run(objs, 400)[3] == ("mm", 2, 1)

# A class that gains a custom `__getattribute__` keeps the say.
class Odd(Shape):
    pass


objs.append(Odd())
assert run(objs, 500)[4] == ("mm", 2, 1)


def custom(self, name):
    if name == "unit":
        return "custom"
    return object.__getattribute__(self, name)


Odd.__getattribute__ = custom
assert run(objs, 500)[4] == ("custom", 2, 1)

print("ok")
