from dataclasses import (
    dataclass,
    field,
    fields,
    asdict,
    astuple,
    replace,
    FrozenInstanceError,
)


@dataclass
class Point:
    x: int
    y: int = 0


p = Point(1, 2)
print(p)
print(asdict(p))
print(astuple(p))
print(replace(p, x=99))
print([f.name for f in fields(Point)])


@dataclass(frozen=True)
class Frozen:
    name: str
    value: int = 10


f = Frozen("hi")
print(f, f.name, f.value)
try:
    f.name = "oops"
except FrozenInstanceError as e:
    print("frozen rejected:", e)


@dataclass(order=True)
class Ord:
    x: int
    y: int


print(Ord(1, 2) < Ord(2, 3))
print(Ord(1, 2) == Ord(1, 2))
print(Ord(1, 2) > Ord(0, 0))


@dataclass
class Container:
    name: str
    items: list = field(default_factory=list)


c1 = Container("a")
c2 = Container("b")
c1.items.append(1)
print(c1.items, c2.items)
