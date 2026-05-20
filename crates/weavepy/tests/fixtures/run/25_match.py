def describe(value):
    match value:
        case 0:
            return "zero"
        case 1 | 2 | 3:
            return "small"
        case n if n < 0:
            return f"negative ({n})"
        case _:
            return "other"

for v in [0, 1, 3, -5, 100]:
    print(describe(v))


def shape(point):
    match point:
        case (0, 0):
            return "origin"
        case (x, 0):
            return f"on x-axis at {x}"
        case (0, y):
            return f"on y-axis at {y}"
        case (x, y):
            return f"point ({x}, {y})"
        case _:
            return "unknown"

print(shape((0, 0)))
print(shape((3, 0)))
print(shape((0, 4)))
print(shape((1, 2)))


def head_tail(xs):
    match xs:
        case []:
            return "empty"
        case [a]:
            return f"single: {a}"
        case [first, *rest]:
            return f"first={first}, rest={rest}"

print(head_tail([]))
print(head_tail([42]))
print(head_tail([1, 2, 3, 4]))
