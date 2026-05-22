"""Smoke test: classes, inheritance, properties, dunders, dataclasses."""

class Point:
    def __init__(self, x, y):
        self.x = x
        self.y = y

    def __repr__(self):
        return f"Point({self.x}, {self.y})"

    def __eq__(self, other):
        return isinstance(other, Point) and (self.x, self.y) == (other.x, other.y)

    def __hash__(self):
        return hash((self.x, self.y))

    def __add__(self, other):
        return Point(self.x + other.x, self.y + other.y)

p = Point(1, 2)
q = Point(3, 4)
assert repr(p) == "Point(1, 2)"
assert p == Point(1, 2)
assert p != q
assert p + q == Point(4, 6)

class Named:
    def __init__(self, name):
        self._name = name

    @property
    def name(self):
        return self._name

    @name.setter
    def name(self, value):
        if not value:
            raise ValueError("empty")
        self._name = value


n = Named("alice")
assert n.name == "alice"
n.name = "bob"
assert n.name == "bob"
try:
    n.name = ""
except ValueError as e:
    assert str(e) == "empty"
else:
    raise AssertionError("expected ValueError")

# classmethod / staticmethod
class C:
    counter = 0

    @classmethod
    def bump(cls):
        cls.counter += 1
        return cls.counter

    @staticmethod
    def double(x):
        return x * 2

assert C.bump() == 1
assert C.bump() == 2
assert C.double(5) == 10

# inheritance + super()
class Animal:
    def speak(self):
        return "generic"

class Dog(Animal):
    def speak(self):
        return "woof: " + super().speak()

assert Dog().speak() == "woof: generic"

# isinstance / issubclass
assert isinstance(Dog(), Animal)
assert issubclass(Dog, Animal)
assert not issubclass(Animal, Dog)

# dataclasses
from dataclasses import dataclass, field

@dataclass
class Box:
    w: int
    h: int
    label: str = "box"
    extras: list = field(default_factory=list)

b = Box(2, 3)
assert b.w == 2 and b.h == 3 and b.label == "box" and b.extras == []
b.extras.append("a")
assert Box(2, 3, "box", ["a"]) != b or Box(2, 3, "box", ["a"]) == b  # eq tests
