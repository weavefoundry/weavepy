"""Smoke test: PEP 634 structural pattern matching."""

def kind(obj):
    match obj:
        case 0:
            return "zero"
        case int() if obj < 0:
            return "negative"
        case int():
            return "positive int"
        case [1, 2, *rest]:
            return f"list starting 1,2 with tail {rest}"
        case [a, b, c]:
            return f"triple ({a},{b},{c})"
        case (a, b):
            return f"pair ({a},{b})"
        case None:
            return "none"
        case _:
            return "other"

assert kind(0) == "zero"
assert kind(7) == "positive int"
assert kind(-2) == "negative"
assert kind([1, 2, 3, 4]) == "list starting 1,2 with tail [3, 4]"
assert kind([1, 2]) == "list starting 1,2 with tail []"
assert kind([4, 5, 6]) == "triple (4,5,6)"
assert kind((9, 10)) == "pair (9,10)"
assert kind(None) == "none"
assert kind(3.14) == "other"


class Color:
    __match_args__ = ("name", "rgb")

    def __init__(self, name, rgb):
        self.name = name
        self.rgb = rgb


def describe(c):
    match c:
        case Color("red", rgb):
            return f"red rgb={rgb}"
        case Color(name, _):
            return name
    return "unmatched"


assert describe(Color("red", (255, 0, 0))) == "red rgb=(255, 0, 0)"
assert describe(Color("blue", (0, 0, 255))) == "blue"
