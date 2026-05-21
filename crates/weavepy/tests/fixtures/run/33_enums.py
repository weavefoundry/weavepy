from enum import Enum, IntEnum, Flag, IntFlag, auto, unique


class Color(Enum):
    RED = 1
    GREEN = 2
    BLUE = 3


print(Color.RED)
print(Color.RED.name, Color.RED.value)
print(Color(2))
print(Color["BLUE"])
print([c.name for c in Color])
print(Color.RED == Color.RED)
print(Color.RED == Color.GREEN)


class Priority(IntEnum):
    LOW = 1
    MEDIUM = 5
    HIGH = 10


print(Priority.LOW < Priority.HIGH)
print(Priority.MEDIUM <= Priority.HIGH)
print(Priority.MEDIUM + 1)


class Perm(Flag):
    READ = auto()
    WRITE = auto()
    EXEC = auto()


print(Perm.READ.value, Perm.WRITE.value, Perm.EXEC.value)
rw = Perm.READ | Perm.WRITE
print(rw)
print(Perm.READ in rw)
print(Perm.EXEC in rw)


@unique
class Day(Enum):
    MON = 1
    TUE = 2
    WED = 3


try:

    @unique
    class Bad(Enum):
        A = 1
        B = 1

except ValueError as e:
    print("unique rejected:", e)
