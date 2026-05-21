from enum import Enum


class Color(Enum):
    RED = 1
    GREEN = 2
    BLUE = 3


print(Color.RED)
print(Color(2))
print(Color["BLUE"])
print([c.name for c in Color])
