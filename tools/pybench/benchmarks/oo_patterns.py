"""Everyday object-oriented code: properties, super() chains, class
attributes read through instances, isinstance against ABCs and tuples,
__eq__/__hash__/__lt__, dunder dispatch, and getattr/hasattr."""

import abc

WORK = 30000


class Shape(abc.ABC):
    unit = "cm"
    registry = []

    def __init__(self, name):
        self.name = name

    @abc.abstractmethod
    def area(self):
        ...

    def describe(self):
        return "%s:%s" % (self.name, self.unit)

    def __lt__(self, other):
        return self.area() < other.area()

    def __eq__(self, other):
        return isinstance(other, Shape) and self.area() == other.area()

    def __hash__(self):
        return hash(self.area())


class Rect(Shape):
    sides = 4

    def __init__(self, w, h):
        super().__init__("rect")
        self._w = w
        self._h = h

    @property
    def width(self):
        return self._w

    @width.setter
    def width(self, v):
        self._w = v

    def area(self):
        return self._w * self._h


class Square(Rect):
    def __init__(self, s):
        super().__init__(s, s)
        self.name = "square"


class Circle(Shape):
    sides = 0

    def __init__(self, r):
        super().__init__("circle")
        self.r = r

    def area(self):
        return 3.14159 * self.r * self.r


def bench(n):
    total = 0.0
    shapes = []
    for i in range(n):
        k = i % 3
        s = Rect(i % 7 + 1, 2) if k == 0 else Square(i % 5 + 1) if k == 1 else Circle(i % 4 + 1)
        shapes.append(s)
        if isinstance(s, Rect):
            s.width = s.width + 1
            total += s.sides
        if isinstance(s, (Circle, Square)):
            total += 1
        if isinstance(s, Shape):
            total += len(s.describe())
        total += s.area()
        total += getattr(s, "r", 0) + hasattr(s, "_w")
    shapes.sort()
    total += len(set(shapes[:200]))
    total += sum(1 for a, b in zip(shapes, shapes[1:]) if a == b)
    return round(total, 3)
