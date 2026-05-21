class Point:
    __slots__ = ("x", "y")

    def __init__(self, x, y):
        self.x = x
        self.y = y


p = Point(3, 4)
print(p.x, p.y)
try:
    p.z = 5
except AttributeError as e:
    print("ok")
