from dataclasses import dataclass, field, asdict


@dataclass
class Point:
    x: int
    y: int = 0


p = Point(1, 2)
print(p)
print(asdict(p))
