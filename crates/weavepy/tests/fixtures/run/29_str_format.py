print("{} + {} = {}".format(1, 2, 3))
print("{0}-{1}-{0}".format("a", "b"))
print("{name} is {age}".format(name="Ada", age=37))

print("[{:>10}]".format("hi"))
print("[{:<10}]".format("hi"))
print("[{:^10}]".format("hi"))
print("[{:*^10}]".format("hi"))

print("{:.3f}".format(3.14159))
print("{:08.2f}".format(3.14))
print("{:+d}".format(42))
print("{:b}".format(42))
print("{:o}".format(42))
print("{:x}".format(255))
print("{:X}".format(255))

print("%d / %d = %d" % (10, 3, 10 // 3))
print("%s says %s" % ("Ada", "hello"))
print("%5.2f" % 3.14159)
print("%-10s|" % "left")
print("%(name)s/%(age)d" % {"name": "Ada", "age": 37})

class Point:
    def __init__(self, x, y):
        self.x = x
        self.y = y
    def __repr__(self):
        return "P(" + str(self.x) + "," + str(self.y) + ")"
    def __str__(self):
        return "(" + str(self.x) + "," + str(self.y) + ")"

p = Point(1, 2)
print("{!r}".format(p))
print("{!s}".format(p))
