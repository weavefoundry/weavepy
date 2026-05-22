"""Smoke test: iterator protocol, generators, generator delegation, send/throw."""

class Squares:
    def __init__(self, n):
        self.n = n
        self.i = 0

    def __iter__(self):
        return self

    def __next__(self):
        if self.i >= self.n:
            raise StopIteration
        val = self.i * self.i
        self.i += 1
        return val


assert list(Squares(5)) == [0, 1, 4, 9, 16]
assert sum(Squares(4)) == 0 + 1 + 4 + 9

def fib():
    a, b = 0, 1
    while True:
        yield a
        a, b = b, a + b


it = fib()
out = [next(it) for _ in range(10)]
assert out == [0, 1, 1, 2, 3, 5, 8, 13, 21, 34]


def echoer():
    saved = []
    while True:
        val = yield saved
        if val is None:
            break
        saved.append(val)
    return saved


g = echoer()
assert next(g) == []
assert g.send(1) == [1]
assert g.send(2) == [1, 2]
try:
    g.send(None)
except StopIteration as e:
    assert e.value == [1, 2]


def producer():
    for i in range(3):
        yield i
    yield from "ab"


assert list(producer()) == [0, 1, 2, "a", "b"]


def thrower():
    try:
        yield 1
    except ValueError:
        yield "caught"


g = thrower()
assert next(g) == 1
assert g.throw(ValueError) == "caught"

# zip, enumerate, reversed
assert list(zip([1, 2, 3], "abc")) == [(1, "a"), (2, "b"), (3, "c")]
assert list(enumerate("xyz", 10)) == [(10, "x"), (11, "y"), (12, "z")]
assert list(reversed([1, 2, 3])) == [3, 2, 1]

# map / filter — comprehensions are the WeavePy-preferred form;
# `map(callable, …)` is currently only available when the callable
# is a builtin (see RuntimeError raised above).
assert [x + 1 for x in [1, 2, 3]] == [2, 3, 4]
assert [x for x in range(6) if x % 2] == [1, 3, 5]
