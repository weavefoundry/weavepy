# __slots__ — both the basic memory-only form and the descriptor
# protocol it sets up under the hood.


class Point:
    __slots__ = ("x", "y")

    def __init__(self, x, y):
        self.x = x
        self.y = y


p = Point(3, 4)
print(p.x, p.y)
p.x = 10
print(p.x, p.y)

try:
    p.z = 5
except AttributeError as e:
    print("slot reject:", e)


# Slots interact correctly with inheritance: a subclass that *also*
# declares __slots__ stays slot-only, while a subclass without __slots__
# gets a normal __dict__.


class Vec3(Point):
    __slots__ = ("z",)

    def __init__(self, x, y, z):
        super().__init__(x, y)
        self.z = z


v = Vec3(1, 2, 3)
print(v.x, v.y, v.z)
try:
    v.w = 4
except AttributeError as e:
    print("nested slot reject:", e)


class Loose(Point):
    pass


lo = Loose(7, 8)
lo.extra = "fine"
print(lo.x, lo.y, lo.extra)
