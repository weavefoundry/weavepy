"""Checked native arithmetic preserves Python integers and overflow recovery."""


def adding(n, value, delta):
    for _ in range(n):
        value += delta
    return value


def subtracting(n, value, delta):
    for _ in range(n):
        value -= delta
    return value


def multiplying(n, value, factor):
    for _ in range(n):
        value *= factor
    return value


# Compile each ordinary integer loop before exercising its overflow exits.
for _ in range(3):
    assert adding(10000, 17, 2) == 20017
    assert subtracting(10000, 17, 2) == -19983
    assert multiplying(10000, 17, 1) == 17

maximum = (1 << 63) - 1
minimum = -(1 << 63)
for n in (0, 1, 2, 5, 73):
    for value, delta in [(maximum - 2, 1), (minimum + 2, -1),
                         (maximum, maximum), (minimum, minimum),
                         (maximum, -maximum), (minimum, maximum),
                         (0, 0), (True, True), (-19, False)]:
        assert adding(n, value, delta) == value + n * delta, (n, value, delta)
        assert subtracting(n, value, delta) == value - n * delta, (n, value, delta)
    for value, factor in [(maximum, 2), (minimum, -1), (minimum, 2),
                          (3037000500, 3037000500), (-3037000500, 3037000500),
                          (17, -2), (0, minimum), (maximum, 0),
                          (minimum, 1), (True, True)]:
        assert multiplying(n, value, factor) == value * factor ** n, (n, value, factor)

# Begin inside i64 and cross its boundary after the loop has entered through OSR.
assert adding(20000, maximum - 15000, 1) == maximum + 5000
assert subtracting(20000, minimum + 15000, 1) == minimum - 5000
assert multiplying(200, 1, 2) == 1 << 200
# Calls after a deoptimization can still accept ordinary or wider integers.
assert adding(10000, 0, 1) == 10000
assert subtracting(10000, 0, 1) == -10000
assert multiplying(10000, 7, -1) == 7
assert adding(5, 1 << 100, 3) == (1 << 100) + 15
assert multiplying(5, -(1 << 100), -3) == (1 << 100) * 243


class Number:
    def __init__(self, value):
        self.value = value
        self.events = []

    def __iadd__(self, value):
        self.events.append(("add", value))
        self.value += value
        return self

    def __isub__(self, value):
        self.events.append(("sub", value))
        self.value -= value
        return self

    def __imul__(self, value):
        self.events.append(("mul", value))
        self.value *= value
        return self


for function, operation, expected in [(adding, "add", 19),
                                      (subtracting, "sub", -5),
                                      (multiplying, "mul", 567)]:
    number = Number(7)
    assert function(4, number, 3) is number
    assert number.events == [(operation, 3)] * 4
    assert number.value == expected

assert type(adding(1, True, True)) is int
assert type(subtracting(1, True, False)) is int
assert type(multiplying(1, True, True)) is int

print("checked integer arithmetic: ok")
