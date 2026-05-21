from dataclasses import dataclass, FrozenInstanceError


@dataclass(frozen=True)
class Point:
    x: int
    y: int


p = Point(1, 2)
print(p)
try:
    p.x = 3
except FrozenInstanceError:
    print("ok")
