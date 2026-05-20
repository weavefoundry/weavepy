class Point:
    __match_args__ = ("x", "y")

    def __init__(self, x, y):
        self.x = x
        self.y = y


def classify(p):
    match p:
        case Point(x=0, y=0):
            return "origin"
        case Point(x=0, y=y):
            return f"y-axis @ {y}"
        case Point(x=x, y=0):
            return f"x-axis @ {x}"
        case Point(x=x, y=y) if x == y:
            return f"diagonal @ {x}"
        case Point(x, y):
            return f"({x},{y})"


print(classify(Point(0, 0)))
print(classify(Point(0, 5)))
print(classify(Point(7, 0)))
print(classify(Point(3, 3)))
print(classify(Point(1, 2)))


def kind(value):
    match value:
        case int() if value > 0:
            return "pos int"
        case str(s):
            return f"string of len {len(s)}"
        case list() as xs:
            return f"list with {len(xs)} elements"
        case _:
            return "other"

print(kind(42))
print(kind("hello"))
print(kind([1, 2, 3]))
print(kind(None))
