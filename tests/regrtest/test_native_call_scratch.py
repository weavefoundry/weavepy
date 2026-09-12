"""Keep small and larger native call buffers valid through nested calls and exits."""


def unary(value):
    return value + 4


def ternary(a, b, c):
    return a + b + c


def quotient(divisor):
    return 100 // divisor


class Counter:
    def __init__(self):
        self.value = 0

    def zero(self):
        self.value += 1
        return self.value

    def one(self):
        self.value = unary(self.value)
        return self.value

    def many(self):
        self.value = ternary(self.value, 1, 2)
        return self.value

    def divide(self, divisor):
        self.value += 1
        return quotient(divisor)

    def descend(self, depth):
        if depth == 0:
            return self.value
        return self.descend(depth - 1)


def exercise(n):
    counter = Counter()
    total = 0
    for _ in range(n):
        total += counter.zero()
        total += counter.one()
        total += counter.many()
    return total


for _ in range(40):
    assert exercise(200) == 480400

counter = Counter()
for _ in range(40):
    assert counter.divide(5) == 20
    assert counter.descend(4) == counter.value

before = counter.value
try:
    counter.divide(0)
except ZeroDivisionError:
    pass
else:
    raise AssertionError("a nested call must propagate its exception")
assert counter.value == before + 1
assert counter.divide(4) == 25
assert counter.descend(40) == counter.value

counter.value = (1 << 63) - 2
assert counter.zero() == (1 << 63) - 1
assert counter.one() == (1 << 63) + 3
assert counter.many() == (1 << 63) + 6
assert exercise(200) == 480400
print("native call scratch: ok")
